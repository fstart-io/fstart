//! AArch64 platform support.

#![no_std]

pub mod entry;

/// Halt the processor.
#[inline(always)]
pub fn halt() -> ! {
    loop {
        unsafe {
            core::arch::asm!("wfe");
        }
    }
}

/// Jump to an address, transferring control unconditionally.
///
/// Used by `StageLoad` and `PayloadLoad` to transfer control to the
/// next stage or payload after loading it into memory.
///
/// # Safety
///
/// The caller must ensure:
/// - `addr` points to valid executable code
/// - The stack and BSS will be set up by the target code (its own `_start`)
/// - This function never returns
#[inline(always)]
pub fn jump_to(addr: u64) -> ! {
    unsafe {
        core::arch::asm!(
            "br {0}",
            in(reg) addr,
            options(noreturn),
        );
    }
}
