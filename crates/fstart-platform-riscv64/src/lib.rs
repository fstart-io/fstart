//! RISC-V 64-bit platform support.
//!
//! Provides the reset vector entry point, stack setup, BSS clearing,
//! and architecture-specific helpers.

#![no_std]

pub mod entry;

/// Halt the processor.
#[inline(always)]
pub fn halt() -> ! {
    loop {
        unsafe {
            core::arch::asm!("wfi");
        }
    }
}
