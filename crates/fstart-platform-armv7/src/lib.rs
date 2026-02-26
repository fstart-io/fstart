//! ARMv7 (32-bit ARM) platform support.
//!
//! Provides the reset vector entry point, stack setup, BSS clearing,
//! and architecture-specific helpers. Captures the DTB address passed
//! by QEMU at reset.
//!
//! On QEMU ARM virt with `-bios`, the CPU starts in SVC mode at the
//! flash base address (0x0). No ATF or SBI layer is needed — the
//! firmware jumps directly to the Linux kernel.

#![no_std]

use core::sync::atomic::{AtomicU32, Ordering};

pub mod entry;

// ---------------------------------------------------------------------------
// Boot parameters — written by _start assembly, read by Rust code
// ---------------------------------------------------------------------------

/// DTB address saved from `r2` at reset (written by `_start` assembly).
///
/// QEMU ARM virt passes the DTB pointer in `r2` when booting with `-bios`.
/// On 32-bit ARM, `r0` = 0, `r1` = machine type (~0 for DT-only), `r2` = DTB.
#[no_mangle]
static BOOT_DTB_ADDR: AtomicU32 = AtomicU32::new(0);

/// Return the DTB address passed by QEMU/firmware at reset (`r2`).
pub fn boot_dtb_addr() -> u32 {
    BOOT_DTB_ADDR.load(Ordering::Relaxed)
}

// ---------------------------------------------------------------------------
// Linux boot protocol (32-bit ARM)
// ---------------------------------------------------------------------------

/// Jump directly to Linux kernel on 32-bit ARM.
///
/// ARM Linux boot protocol:
/// - `r0` = 0
/// - `r1` = machine type (0xFFFFFFFF for device-tree-only boot)
/// - `r2` = pointer to the DTB
///
/// No intermediate firmware (ATF/SBI) is needed on 32-bit ARM QEMU.
/// The kernel is entered in SVC mode with MMU and caches off.
///
/// # Safety
///
/// The caller must ensure the kernel is loaded at `kernel_addr` and the
/// DTB at `dtb_addr` is valid.
pub fn boot_linux_direct(kernel_addr: u32, dtb_addr: u32) -> ! {
    unsafe {
        // Data/instruction synchronisation barriers to ensure all stores
        // (loaded kernel + DTB) are visible before jumping.
        //
        // ARM Linux boot protocol: r0=0, r1=~0 (DT-only), r2=DTB.
        //
        // We bind kernel_addr to r4 and dtb_addr to r5 (callee-saved,
        // won't conflict with the r0/r1/r2 protocol registers) to
        // guarantee the `mov r0/r1/r2` sequence never clobbers our
        // input operands.
        core::arch::asm!(
            "dsb sy",
            "isb",
            "mov r0, #0",          // r0 = 0
            "mvn r1, #0",          // r1 = 0xFFFFFFFF (DT-only machine type)
            "mov r2, r5",          // r2 = DTB pointer
            "bx r4",              // jump to kernel
            in("r4") kernel_addr,
            in("r5") dtb_addr,
            options(noreturn),
        );
    }
}

// ---------------------------------------------------------------------------
// Basic helpers
// ---------------------------------------------------------------------------

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
            "bx {0}",
            in(reg) addr as u32,
            options(noreturn),
        );
    }
}
