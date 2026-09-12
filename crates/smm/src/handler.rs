use core::arch::asm;

pub use crate::runtime::{
    MAX_SMM_CPUS, SMM_PLATFORM_DATA_ICH_GPE0_STS_OFFSET, SMM_PLATFORM_DATA_ICH_PM_BASE,
    SMM_PLATFORM_FLAG_ICH_GPE0_64BIT, SMM_PLATFORM_INTEL_ICH, SMM_PLATFORM_NONE, SmmEntryParams,
    SmmRuntime,
};

pub const SMM_RUNTIME_FLAG_FINALIZED: u32 = 1;
pub const APM_CNT: u16 = 0x00b2;
pub const DEBUGCON: u16 = 0x0402;

pub struct SmmContext<'a> {
    pub params: &'a mut SmmEntryParams,
    pub apm_command: u8,
}

pub trait SmmHandler {
    /// Handle one SMI entry.
    ///
    /// # Safety
    ///
    /// The caller must ensure `ctx.params` points at the firmware-provided SMM
    /// entry parameter block for the current SMI, that the handler is running
    /// in SMM with the expected CPU state, and that any platform MMIO/PIO
    /// accesses performed by the implementation are valid for the board.
    unsafe fn handle(ctx: &mut SmmContext<'_>);
}

pub trait SmmBoardHandler {
    /// Handle an APMC software SMI command.
    ///
    /// # Safety
    ///
    /// The caller must ensure `ctx` describes the current SMI entry and that
    /// invoking board-specific APMC handling is valid for the platform state.
    unsafe fn on_apmc(_ctx: &mut SmmContext<'_>, _command: u8) {}

    /// Handle pending GPE status bits.
    ///
    /// # Safety
    ///
    /// The caller must ensure `ctx` describes the current SMI entry and that
    /// `gpe_status` was read from the platform's active GPE status register.
    unsafe fn on_gpe(_ctx: &mut SmmContext<'_>, _gpe_status: u64) {}

    /// Handle a TCO command byte, optionally returning a response byte.
    ///
    /// # Safety
    ///
    /// The caller must ensure `ctx` describes the current SMI entry and that
    /// the command was obtained from the platform's TCO/SMI source.
    unsafe fn on_tco_command(_ctx: &mut SmmContext<'_>, _command: u8) -> Option<u8> {
        None
    }
}

pub struct NoBoardSmmHandler;

impl SmmBoardHandler for NoBoardSmmHandler {}

impl<'a> SmmContext<'a> {
    /// Build an SMM context from the raw entry-parameter pointer.
    ///
    /// # Safety
    ///
    /// `params` must be either null or a valid, uniquely borrowed pointer to an
    /// [`SmmEntryParams`] block for the current SMI. The caller must be running
    /// in an environment where reading the APM control port is valid.
    #[inline(always)]
    pub unsafe fn from_raw(params: *mut SmmEntryParams) -> Option<Self> {
        if params.is_null() {
            return None;
        }
        // SAFETY: caller guarantees `params` is a valid, uniquely borrowed
        // entry-parameter block and port I/O is valid here.
        unsafe {
            let params = &mut *params;
            let apm_command = fstart_core::pio::inb(APM_CNT);
            Some(Self {
                params,
                apm_command,
            })
        }
    }

    /// Return the mutable SMM runtime pointed to by this context.
    ///
    /// # Safety
    ///
    /// The runtime pointer inside `self.params` must either be zero or point to
    /// the single live [`SmmRuntime`] instance. The caller must ensure exclusive
    /// access to the runtime for the duration of the returned borrow.
    #[inline(always)]
    pub unsafe fn runtime_mut(&mut self) -> Option<&'static mut SmmRuntime> {
        // SAFETY: caller guarantees the runtime pointer contract.
        unsafe { runtime_mut(self.params) }
    }

    /// Record per-CPU and per-APM-command SMI entry counters.
    ///
    /// # Safety
    ///
    /// The context runtime pointer must satisfy [`Self::runtime_mut`]'s safety
    /// requirements. The caller must ensure concurrent SMM handlers serialize
    /// access appropriately if multiple CPUs may update counters.
    #[inline(always)]
    pub unsafe fn record_entry(&mut self) {
        let cpu = self.params.cpu as usize;
        let apm_command = self.apm_command;
        // SAFETY: caller guarantees the runtime pointer contract and that
        // concurrent counter updates are serialized appropriately.
        let runtime = unsafe { self.runtime_mut() };
        let Some(runtime) = runtime else {
            return;
        };

        unsafe {
            if cpu < MAX_SMM_CPUS {
                let count = runtime.cpu_entry_counts.as_mut_ptr().add(cpu);
                count.write(count.read().wrapping_add(1));
            }

            runtime.last_apm_command = apm_command as u32;
            let count = runtime
                .apm_command_counts
                .as_mut_ptr()
                .add(apm_command as usize);
            count.write(count.read().wrapping_add(1));
        }
    }

    /// OR runtime flags into the SMM runtime block.
    ///
    /// # Safety
    ///
    /// The context runtime pointer must satisfy [`Self::runtime_mut`]'s safety
    /// requirements. The caller must ensure the flag update is serialized with
    /// other SMM runtime users when needed.
    #[inline(always)]
    pub unsafe fn set_runtime_flags(&mut self, flags: u32) {
        // SAFETY: caller guarantees the runtime pointer contract and
        // serialization of flag updates.
        if let Some(runtime) = unsafe { self.runtime_mut() } {
            runtime.flags |= flags;
        }
    }
}

/// Return the mutable SMM runtime referenced by an entry-parameter block.
///
/// # Safety
///
/// `params.runtime` must either be zero or point to the single live
/// [`SmmRuntime`] allocation. The caller must ensure exclusive access to that
/// runtime for the duration of the returned borrow.
#[inline(always)]
pub unsafe fn runtime_mut(params: &mut SmmEntryParams) -> Option<&'static mut SmmRuntime> {
    if params.runtime == 0 {
        None
    } else {
        // SAFETY: caller guarantees `params.runtime` points to the single live
        // SmmRuntime with exclusive access.
        Some(unsafe { &mut *(params.runtime as *mut SmmRuntime) })
    }
}

/// Try to acquire the SMM handler lock.
///
/// # Safety
///
/// `runtime` must point to the shared SMM runtime block and its `handler_lock`
/// field must be accessible with atomic x86 `xchg` semantics. The caller must
/// release the lock with [`release_handler_lock`] after a successful acquire.
#[inline(always)]
pub unsafe fn obtain_handler_lock(runtime: &mut SmmRuntime) -> bool {
    let lock = &mut runtime.handler_lock as *mut u32;
    let mut old: u32 = 1;
    // SAFETY: caller guarantees `lock` is accessible with atomic xchg semantics.
    unsafe {
        asm!(
            "xchg dword ptr [{lock}], {old:e}",
            lock = in(reg) lock,
            old = inout(reg) old,
            options(nostack, preserves_flags)
        );
    }
    old == 0
}

/// Spin until the SMM handler lock becomes free.
///
/// # Safety
///
/// `runtime` must point to the shared SMM runtime block and its `handler_lock`
/// field must remain valid while this function spins.
#[inline(always)]
pub unsafe fn wait_for_handler_unlock(runtime: &SmmRuntime) {
    // SAFETY: caller guarantees the lock field stays valid while spinning.
    while unsafe { core::ptr::read_volatile(&runtime.handler_lock) } != 0 {
        unsafe { asm!("pause", options(nomem, nostack, preserves_flags)) };
    }
}

/// Release the SMM handler lock.
///
/// # Safety
///
/// The caller must have successfully acquired the lock for `runtime` and must
/// not release a lock owned by another CPU.
#[inline(always)]
pub unsafe fn release_handler_lock(runtime: &mut SmmRuntime) {
    // SAFETY: caller owns the lock acquired via obtain_handler_lock.
    unsafe { core::ptr::write_volatile(&mut runtime.handler_lock, 0) };
}

/// Emit a minimal SMM debug trace for a CPU number.
///
/// # Safety
///
/// The caller must be running on a platform where `DEBUGCON` is decoded and
/// writing bytes to it is safe for the current firmware phase.
#[inline(always)]
pub unsafe fn debug_trace(cpu: u32) {
    // SAFETY: caller guarantees DEBUGCON is decoded and safe to write.
    unsafe {
        fstart_core::pio::outb(DEBUGCON, b'S');
        let mut digit = (cpu & 0x0f) as u8;
        if digit > 9 {
            digit = digit.wrapping_add(7);
        }
        fstart_core::pio::outb(DEBUGCON, digit.wrapping_add(b'0'));
        fstart_core::pio::outb(DEBUGCON, b'\n');
    }
}
