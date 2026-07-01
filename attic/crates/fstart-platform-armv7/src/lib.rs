//! ARMv7-A platform support.
//!
//! Provides the reset vector entry point, stack setup, BSS clearing,
//! and architecture-specific helpers for ARMv7-A targets (Cortex-A7,
//! Cortex-A8, Cortex-A9, Cortex-A15, etc.).
//!
//! This crate no longer contains SoC-specific code (like Allwinner eGON
//! headers). For sunxi-specific support, enable the `sunxi` feature and
//! depend on `fstart-soc-sunxi`.
//!
//! The `udelay`, `sdelay`, and `halt` functions are re-exported from
//! `fstart-arch` for backward compatibility.

#![no_std]

#[cfg(target_arch = "arm")]
pub mod entry;

// Re-export architecture utilities from fstart-arch.
// This preserves the old API while centralizing arch-specific code.
pub use fstart_arch::{halt, sdelay, udelay};

// ---------------------------------------------------------------------------
// ARMv7-specific jump and boot helpers (not sunxi-specific)
// ---------------------------------------------------------------------------

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
#[cfg(target_arch = "arm")]
#[inline(always)]
pub fn jump_to(addr: u64) -> ! {
    // ARMv7 is 32-bit — truncate the u64 address to u32.
    let addr32 = addr as u32;
    unsafe {
        core::arch::asm!(
            "bx {0}",
            in(reg) addr32,
            options(noreturn),
        );
    }
}

/// Jump to an address with a handoff pointer in `r0`.
///
/// Used by `LoadNextStage` to pass a serialized [`StageHandoff`] to the
/// next stage. The next stage's `_start` saves `r0` to `r6` before the
/// CRT0 clobbers it, then passes it to `fstart_main(handoff_ptr)`.
///
/// # Safety
///
/// Same as [`jump_to`], plus: `handoff_addr` must point to a valid
/// serialized `StageHandoff` in DRAM (or be 0 for no handoff).
#[cfg(target_arch = "arm")]
#[inline(always)]
pub fn jump_to_with_handoff(addr: u64, handoff_addr: usize) -> ! {
    let addr32 = addr as u32;
    let handoff32 = handoff_addr as u32;
    unsafe {
        // IMPORTANT: `in("r0")` binds the handoff address directly to r0,
        // ensuring the compiler does NOT allocate `addr` to r0.  The
        // previous code used `in(reg)` for both + an explicit `mov r0`,
        // which allowed the compiler to place `addr` in r0 — the mov then
        // clobbered it, causing `bx` to jump to the handoff buffer instead
        // of the stage entry point.
        core::arch::asm!(
            "bx {addr}",
            addr = in(reg) addr32,
            in("r0") handoff32,
            options(noreturn),
        );
    }
}

/// Write the ARM Generic Timer frequency to CNTFRQ (CP15 c14,c0,0).
///
/// **Note:** In the normal boot flow, CNTFRQ is programmed by the CCU
/// driver's `init()` via [`fstart_arch::set_cntfrq`] during the
/// `ClockInit` capability — this matches U-Boot's `board_init()` timing.
/// This function remains available for manual / non-codegen use.
///
/// Must be called from secure mode — CNTFRQ is a banked secure register
/// and writes from non-secure state are ignored.
#[cfg(target_arch = "arm")]
pub fn set_arch_timer_freq(freq: u32) {
    fstart_arch::set_cntfrq(freq);
}

/// Clean up CPU state before jumping to the Linux kernel.
///
/// Performs the ARM-side subset of U-Boot's `cleanup_before_linux()`:
/// - Disables and invalidates I-cache (enabled by our entry.rs)
/// - Invalidates branch predictor array
/// - DSB + ISB barriers
///
/// D-cache and MMU are already off (never enabled by our firmware).
///
/// The ARM Linux boot protocol (`Documentation/arm/booting.rst`) permits
/// I-cache to be on, but U-Boot disables it for a clean handoff and we
/// follow suit.
#[cfg(target_arch = "arm")]
pub fn cleanup_before_linux() {
    // SAFETY: cache/TLB maintenance operations are safe from secure SVC.
    unsafe {
        core::arch::asm!(
            // Disable I-cache: clear SCTLR.I (bit 12)
            "mrc p15, 0, r0, c1, c0, 0",
            "bic r0, r0, #(1 << 12)",
            "mcr p15, 0, r0, c1, c0, 0",
            "isb",

            // Invalidate entire I-cache
            "mov r0, #0",
            "mcr p15, 0, r0, c7, c5, 0",

            // Invalidate branch predictor array
            "mcr p15, 0, r0, c7, c5, 6",

            "dsb",
            "isb",
            out("r0") _,
            options(nomem, nostack),
        );
    }
}

/// Unified Linux boot entry point.
///
/// Calls [`cleanup_before_linux`] then delegates to the ARM boot
/// protocol jump.
///
/// Required fields: `kernel_addr`, `dtb_addr`.
/// Ignored fields: `fw_addr`, `rsdp_addr`, `bootargs`, `e820_entries`,
/// `zero_page_addr`, `hart_id`.
/// Unified Linux boot entry point.
///
/// Calls [`cleanup_before_linux`] then jumps to the kernel.
///
/// Required fields: `kernel_addr`, `dtb_addr`.
/// Ignored fields: `fw_addr`, `rsdp_addr`, `bootargs`, `e820_entries`,
/// `zero_page_addr`, `hart_id`.
#[cfg(target_arch = "arm")]
pub fn boot_linux(params: &fstart_services::boot::BootLinuxParams<'_>) -> ! {
    cleanup_before_linux();
    boot_linux_direct(params.kernel_addr, params.dtb_addr)
}

/// Boot a Linux kernel using the ARM boot protocol.
///
/// Sets up the registers per the ARM Linux boot protocol:
/// - `r0` = 0
/// - `r1` = machine type (0xFFFF_FFFF for device-tree-only boot)
/// - `r2` = physical address of the DTB
///
/// Then jumps to the kernel entry point. This function never returns.
///
/// The caller should call [`cleanup_before_linux`] before this function.
/// CNTFRQ should already be programmed by the CCU driver during clock init.
///
/// # Safety
///
/// The caller must ensure:
/// - `kernel_addr` points to a valid ARM Linux zImage/Image
/// - `dtb_addr` points to a valid flattened device tree blob
/// - MMU is off, D-cache is off/clean
#[cfg(target_arch = "arm")]
#[inline(always)]
pub fn boot_linux_direct(kernel_addr: u64, dtb_addr: u64) -> ! {
    let kernel = kernel_addr as u32;
    let dtb = dtb_addr as u32;
    unsafe {
        // IMPORTANT: Use explicit register bindings (`in("r0")` etc.) for
        // the ARM boot protocol registers.  This prevents the compiler
        // from allocating `kernel` to r0/r1/r2 — which would be clobbered
        // by explicit `mov` instructions before `bx`.  See the fix for
        // `jump_to_with_handoff` for the full explanation.
        core::arch::asm!(
            // Disable IRQ/FIQ and switch to SVC mode (should already be).
            "cpsid aif, #0x13",
            // Jump to kernel — r0, r1, r2 are already set by the compiler
            // from the explicit register bindings below.
            "bx {kernel}",
            kernel = in(reg) kernel,
            in("r0") 0u32,
            in("r1") 0xFFFF_FFFFu32,       // DT-only boot (no ATAGS)
            in("r2") dtb,
            options(noreturn),
        );
    }
}
