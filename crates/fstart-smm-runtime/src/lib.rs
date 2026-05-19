#![no_std]

use core::arch::asm;

pub use fstart_smm::runtime::{
    SmmEntryParams, SmmRuntime, MAX_SMM_CPUS, SMM_PLATFORM_DATA_ICH_GPE0_STS_OFFSET,
    SMM_PLATFORM_DATA_ICH_PM_BASE, SMM_PLATFORM_FLAG_ICH_GPE0_64BIT, SMM_PLATFORM_INTEL_ICH,
    SMM_PLATFORM_NONE,
};

pub const SMM_RUNTIME_FLAG_FINALIZED: u32 = 1;
pub const APM_CNT: u16 = 0x00b2;
pub const DEBUGCON: u16 = 0x0402;

pub struct SmmContext<'a> {
    pub params: &'a mut SmmEntryParams,
    pub apm_command: u8,
}

pub trait SmmHandler {
    unsafe fn handle(ctx: &mut SmmContext<'_>);
}

pub trait SmmBoardHandler {
    unsafe fn on_apmc(_ctx: &mut SmmContext<'_>, _command: u8) {}

    unsafe fn on_gpe(_ctx: &mut SmmContext<'_>, _gpe_status: u64) {}

    unsafe fn on_tco_command(_ctx: &mut SmmContext<'_>, _command: u8) -> Option<u8> {
        None
    }
}

pub struct NoBoardSmmHandler;

impl SmmBoardHandler for NoBoardSmmHandler {}

impl<'a> SmmContext<'a> {
    pub unsafe fn from_raw(params: *mut SmmEntryParams) -> Option<Self> {
        if params.is_null() {
            return None;
        }
        let params = &mut *params;
        let apm_command = fstart_pio::inb(APM_CNT);
        Some(Self {
            params,
            apm_command,
        })
    }

    pub unsafe fn runtime_mut(&mut self) -> Option<&'static mut SmmRuntime> {
        runtime_mut(self.params)
    }

    pub unsafe fn record_entry(&mut self) {
        let cpu = self.params.cpu as usize;
        let apm_command = self.apm_command;
        let Some(runtime) = self.runtime_mut() else {
            return;
        };

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

    pub unsafe fn set_runtime_flags(&mut self, flags: u32) {
        if let Some(runtime) = self.runtime_mut() {
            runtime.flags |= flags;
        }
    }
}

pub unsafe fn runtime_mut(params: &mut SmmEntryParams) -> Option<&'static mut SmmRuntime> {
    if params.runtime == 0 {
        None
    } else {
        Some(&mut *(params.runtime as *mut SmmRuntime))
    }
}

pub unsafe fn obtain_handler_lock(runtime: &mut SmmRuntime) -> bool {
    let lock = &mut runtime.handler_lock as *mut u32;
    let mut old: u32 = 1;
    asm!(
        "xchg dword ptr [{lock}], {old:e}",
        lock = in(reg) lock,
        old = inout(reg) old,
        options(nostack, preserves_flags)
    );
    old == 0
}

pub unsafe fn wait_for_handler_unlock(runtime: &SmmRuntime) {
    while core::ptr::read_volatile(&runtime.handler_lock) != 0 {
        asm!("pause", options(nomem, nostack, preserves_flags));
    }
}

pub unsafe fn release_handler_lock(runtime: &mut SmmRuntime) {
    core::ptr::write_volatile(&mut runtime.handler_lock, 0);
}

pub unsafe fn debug_trace(cpu: u32) {
    fstart_pio::outb(DEBUGCON, b'S');
    let mut digit = (cpu & 0x0f) as u8;
    if digit > 9 {
        digit = digit.wrapping_add(7);
    }
    fstart_pio::outb(DEBUGCON, digit.wrapping_add(b'0'));
    fstart_pio::outb(DEBUGCON, b'\n');
}
