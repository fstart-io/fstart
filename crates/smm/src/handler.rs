#[cfg(any(feature = "stage-bin", test))]
use core::arch::asm;
#[cfg(any(feature = "stage-bin", test))]
use core::mem::{align_of, size_of};
#[cfg(any(feature = "stage-bin", test))]
use core::sync::atomic::Ordering;

#[cfg(any(feature = "stage-bin", test))]
use crate::runtime::{HANDLER_CONFIG_ALIGNMENT, HANDLER_CONFIG_CAPACITY};
pub use crate::runtime::{SmmEntryParams, SmmRuntime};

/// Debug console port used by [`debug_trace`].
pub const DEBUGCON: u16 = 0x0402;

/// Per-entry view handed to the concrete SMM handler.
pub struct SmmContext<'a> {
    params: &'a mut SmmEntryParams,
}

/// A concrete permanent SMM handler, typically a southbridge SMI dispatcher.
pub trait SmmHandler {
    /// Immutable configuration written by the normal-mode installer.
    ///
    /// The installer and the handler must name the same type; the driver
    /// that owns the handler also owns this type.
    type Config: Copy;

    /// Handle one SMI.
    ///
    /// # Safety
    ///
    /// Called only by the framework-owned SMM entry after it has validated
    /// the loader parameters and acquired the permanent rendezvous.
    unsafe fn handle(ctx: &mut SmmContext<'_>, config: &Self::Config);
}

impl<'a> SmmContext<'a> {
    /// Build a context from the raw entry-parameter pointer.
    ///
    /// # Safety
    ///
    /// `params` must be null or the current CPU's loader-filled entry block.
    #[inline(always)]
    pub unsafe fn from_raw(params: *mut SmmEntryParams) -> Option<Self> {
        if params.is_null() {
            return None;
        }
        let params = unsafe { &mut *params };
        Some(Self { params })
    }

    /// Logical CPU index of the current entry.
    #[inline(always)]
    pub const fn cpu(&self) -> u32 {
        self.params.cpu
    }

    #[cfg(any(feature = "stage-bin", test))]
    #[inline(always)]
    fn runtime_ptr(&self) -> Option<*mut SmmRuntime> {
        (self.params.runtime != 0).then_some(self.params.runtime as *mut SmmRuntime)
    }

    /// Copy the installer-written handler configuration.
    ///
    /// Returns `None` unless the runtime records exactly `size_of::<T>()`
    /// bytes at an aligned offset inside SMRAM.
    ///
    /// # Safety
    ///
    /// `T` must be the type the installer wrote, and `params.runtime` must
    /// point to the live runtime block inside locked SMRAM.
    #[cfg(any(feature = "stage-bin", test))]
    #[inline(always)]
    pub(crate) unsafe fn handler_config<T: Copy>(&self) -> Option<T> {
        let runtime = self.runtime_ptr()?;
        // SAFETY: the runtime pointer was written by the installer and the
        // block lives in locked SMRAM for the whole SMI.
        let (offset, size, smram_base, smram_size) = unsafe {
            (
                (*runtime).handler_config_offset as usize,
                (*runtime).handler_config_size as usize,
                (*runtime).smram_base as usize,
                (*runtime).smram_size as usize,
            )
        };
        let start = (runtime as usize).checked_add(offset)?;
        let end = start.checked_add(size)?;
        if size != size_of::<T>()
            || size > HANDLER_CONFIG_CAPACITY
            || align_of::<T>() > HANDLER_CONFIG_ALIGNMENT
            || !offset.is_multiple_of(HANDLER_CONFIG_ALIGNMENT)
            || offset < size_of::<SmmRuntime>()
            || start < smram_base
            || end > smram_base.checked_add(smram_size)?
        {
            return None;
        }
        // SAFETY: the range was validated above and holds a `T` written by
        // the installer.
        Some(unsafe { core::ptr::read(start as *const T) })
    }
}

#[cfg(any(feature = "stage-bin", test))]
#[inline(always)]
fn claim_owner(owner: &core::sync::atomic::AtomicU32, token: u32) -> bool {
    owner
        .compare_exchange(0, token, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
}

/// Enter the permanent SMI rendezvous.
///
/// Exactly one CPU owns shared chipset dispatch. Every other CPU stays in SMM
/// until the owner releases the lock, then returns through RSM. This matches
/// coreboot's `smi_obtain_lock()` handling in `smm_module_handler.c`.
///
/// # Safety
///
/// `ctx.params.runtime` must point to a live runtime block in locked SMRAM.
#[cfg(any(feature = "stage-bin", test))]
#[inline(always)]
pub(crate) unsafe fn enter_rendezvous(ctx: &SmmContext<'_>) -> bool {
    let Some(runtime) = ctx.runtime_ptr() else {
        return false;
    };
    let owner = unsafe { &(*runtime).owner };
    if claim_owner(owner, ctx.cpu().wrapping_add(1)) {
        true
    } else {
        while owner.load(Ordering::Acquire) != 0 {
            unsafe { asm!("pause", options(nomem, nostack, preserves_flags)) };
        }
        false
    }
}

/// Release the rendezvous taken by [`enter_rendezvous`].
///
/// # Safety
///
/// The caller must own the current SMI's rendezvous, and
/// `ctx.params.runtime` must point to its live SMRAM runtime block.
#[cfg(any(feature = "stage-bin", test))]
#[inline(always)]
pub(crate) unsafe fn leave_rendezvous(ctx: &SmmContext<'_>) {
    let Some(runtime) = ctx.runtime_ptr() else {
        return;
    };
    let owner = unsafe { &(*runtime).owner };
    owner.store(0, Ordering::Release);
}

/// Emit a minimal SMM debug trace for a CPU number.
///
/// # Safety
///
/// The caller must run where `DEBUGCON` port I/O is permitted.
#[inline(always)]
pub unsafe fn debug_trace(cpu: u32) {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn params(cpu: u32, runtime: u64) -> SmmEntryParams {
        SmmEntryParams {
            cpu,
            stack_size: 0,
            stack_top: 0,
            common_entry: 0,
            runtime,
            coreboot_module_args: 0,
            cr3: 0,
            entry_base: 0,
        }
    }

    #[test]
    fn owner_enter_leave_and_failed_claim_preserve_the_token() {
        let mut runtime = SmmRuntime::new(0, 0, 1, 0x400, 0, 0);
        let mut entry = params(0, core::ptr::addr_of_mut!(runtime) as u64);
        let ctx = SmmContext { params: &mut entry };
        assert!(unsafe { enter_rendezvous(&ctx) });
        assert_eq!(runtime.owner.load(Ordering::Acquire), 1);
        assert!(!claim_owner(&runtime.owner, 2));
        assert_eq!(runtime.owner.load(Ordering::Acquire), 1);
        unsafe { leave_rendezvous(&ctx) };
        assert_eq!(runtime.owner.load(Ordering::Acquire), 0);
    }

    #[repr(C, align(16))]
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct Config([u16; 4]);

    #[repr(C)]
    struct Fixture {
        runtime: SmmRuntime,
        config: Config,
    }

    #[test]
    #[allow(unused_assignments)]
    fn handler_config_requires_exact_size_inside_smram() {
        let mut fixture = Fixture {
            runtime: SmmRuntime::new(0, 0, 1, 0x400, 0, 0),
            config: Config([0x600, 0x20, 2, 0]),
        };
        let base = core::ptr::addr_of!(fixture) as u64;
        let offset = core::mem::offset_of!(Fixture, config) as u32;
        fixture.runtime = SmmRuntime::new(
            base,
            size_of::<Fixture>() as u64,
            1,
            0x400,
            offset,
            size_of::<Config>() as u32,
        );
        let mut entry = params(0, base);
        let ctx = SmmContext { params: &mut entry };
        assert_eq!(
            unsafe { ctx.handler_config::<Config>() },
            Some(Config([0x600, 0x20, 2, 0]))
        );
        assert_eq!(unsafe { ctx.handler_config::<[u16; 2]>() }, None);

        fixture.runtime.smram_size = 8;
        let ctx = SmmContext { params: &mut entry };
        assert_eq!(unsafe { ctx.handler_config::<Config>() }, None);
    }
}
