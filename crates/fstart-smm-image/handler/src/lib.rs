#![no_std]
#![no_main]

use core::arch::asm;
use core::panic::PanicInfo;

mod abi {
    include!("../../../fstart-smm/src/runtime_abi.rs");
}

mod intel_ich;

use abi::{SmmEntryParams, SmmRuntime, MAX_SMM_CPUS, SMM_PLATFORM_INTEL_ICH, SMM_PLATFORM_NONE};

const SMM_RUNTIME_FLAG_FINALIZED: u32 = 1;

const APM_CNT: u16 = 0x00b2;
const DEBUGCON: u16 = 0x0402;

#[no_mangle]
pub extern "C" fn fstart_smm_handler(params: *mut SmmEntryParams) {
    if params.is_null() {
        return;
    }

    // SAFETY: The SMM entry stub passes the loader-patched per-entry parameter
    // block.  The handler only performs volatile MMIO/PIO-style side effects and
    // updates the runtime block if the loader provided one.
    unsafe {
        let params = &mut *params;
        let apm_command = pio_read8(APM_CNT);

        record_entry(params, apm_command);

        if let Some(runtime) = runtime_mut(params) {
            if !obtain_handler_lock(runtime) {
                wait_for_handler_unlock(runtime);
                debug_trace(params.cpu);
                return;
            }
        }

        match params.platform_kind {
            SMM_PLATFORM_NONE => {}
            SMM_PLATFORM_INTEL_ICH => intel_ich::dispatch(params, apm_command),
            _ => {}
        }

        debug_trace(params.cpu);

        if let Some(runtime) = runtime_mut(params) {
            release_handler_lock(runtime);
        }
    }
}

unsafe fn runtime_mut(params: &mut SmmEntryParams) -> Option<&'static mut SmmRuntime> {
    if params.runtime == 0 {
        None
    } else {
        Some(&mut *(params.runtime as *mut SmmRuntime))
    }
}

unsafe fn record_entry(params: &mut SmmEntryParams, apm_command: u8) {
    let Some(runtime) = runtime_mut(params) else {
        return;
    };

    let cpu = params.cpu as usize;
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

pub(crate) unsafe fn set_runtime_flags(params: &mut SmmEntryParams, flags: u32) {
    if let Some(runtime) = runtime_mut(params) {
        runtime.flags |= flags;
    }
}

unsafe fn obtain_handler_lock(runtime: &mut SmmRuntime) -> bool {
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

unsafe fn wait_for_handler_unlock(runtime: &SmmRuntime) {
    while core::ptr::read_volatile(&runtime.handler_lock) != 0 {
        asm!("pause", options(nomem, nostack, preserves_flags));
    }
}

unsafe fn release_handler_lock(runtime: &mut SmmRuntime) {
    core::ptr::write_volatile(&mut runtime.handler_lock, 0);
}

unsafe fn debug_trace(cpu: u32) {
    pio_write8(DEBUGCON, b'S');
    let mut digit = (cpu & 0x0f) as u8;
    if digit > 9 {
        digit = digit.wrapping_add(7);
    }
    pio_write8(DEBUGCON, digit.wrapping_add(b'0'));
    pio_write8(DEBUGCON, b'\n');
}

unsafe fn pio_read8(port: u16) -> u8 {
    let value: u8;
    asm!(
        "in al, dx",
        in("dx") port,
        out("al") value,
        options(nomem, nostack, preserves_flags)
    );
    value
}

pub(crate) unsafe fn pio_read32(port: u16) -> u32 {
    let value: u32;
    asm!(
        "in eax, dx",
        in("dx") port,
        out("eax") value,
        options(nomem, nostack, preserves_flags)
    );
    value
}

unsafe fn pio_write8(port: u16, value: u8) {
    asm!(
        "out dx, al",
        in("dx") port,
        in("al") value,
        options(nomem, nostack, preserves_flags)
    );
}

pub(crate) unsafe fn pio_write16(port: u16, value: u16) {
    asm!(
        "out dx, ax",
        in("dx") port,
        in("ax") value,
        options(nomem, nostack, preserves_flags)
    );
}

pub(crate) unsafe fn pio_write32(port: u16, value: u32) {
    asm!(
        "out dx, eax",
        in("dx") port,
        in("eax") value,
        options(nomem, nostack, preserves_flags)
    );
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {}
}
