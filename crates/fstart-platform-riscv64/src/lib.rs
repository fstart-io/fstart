//! RISC-V 64-bit platform support.
//!
//! Provides the reset vector entry point, stack setup, BSS clearing,
//! and architecture-specific helpers. Captures boot parameters (hart ID,
//! DTB address) passed by QEMU at reset.
//!
//! Two entry paths are supported:
//!
//! - **Default** (`entry.rs`): Standard RISC-V entry for platforms that
//!   start with DTB in `a1` (e.g., QEMU virt).
//!
//! - **Sunxi** (`entry_sunxi.rs`, behind `sunxi` feature): Entry for
//!   Allwinner D1/T113 SoCs that boot from the BROM via eGON. The D1
//!   starts directly in RV64 M-mode — no mode switch is needed.

#![no_std]

#[cfg(not(feature = "sunxi"))]
pub mod entry;

#[cfg(feature = "sunxi")]
pub mod entry_sunxi;

// ---------------------------------------------------------------------------
// Boot parameters — stored in CSRs, immune to stack overflow
// ---------------------------------------------------------------------------

/// Return the boot hart ID passed by QEMU at reset (`a0`).
///
/// Reads directly from the `mhartid` CSR (always available in M-mode).
pub fn boot_hart_id() -> u64 {
    let id: u64;
    // SAFETY: reading mhartid is a read-only CSR access, valid at any privilege level.
    unsafe {
        core::arch::asm!("csrr {}, mhartid", out(reg) id);
    }
    id
}

// ---------------------------------------------------------------------------
// SBI copy trampoline (position-independent)
// ---------------------------------------------------------------------------
//
// Copies firmware from a3 (src) to a6 (dst/entry), a5 bytes, then
// issues fence + fence.i and jumps to a6.
// a0/a1/a2 are preserved for the SBI entry convention.
core::arch::global_asm!(
    r#"
    .section .text
    .balign 4
    .global _sbi_copy_trampoline
    .global _sbi_copy_trampoline_end
_sbi_copy_trampoline:
    mv   a4, a6          // a4 = write pointer (starts at entry/dst)
1:
    beqz a5, 2f           // if remaining == 0, done
    lbu  t0, 0(a3)        // load byte from src
    sb   t0, 0(a4)        // store byte to dst
    addi a3, a3, 1
    addi a4, a4, 1
    addi a5, a5, -1
    j    1b
2:
    fence rw, rw
    fence.i
    jr   a6               // jump to firmware entry point
_sbi_copy_trampoline_end:
    "#
);

/// Return the DTB address passed by QEMU at reset (`a1`).
///
/// The `_start` assembly saves `a1` into the `mscratch` CSR (following
/// coreboot's approach). This is immune to stack overflow — unlike a
/// BSS global which can be clobbered when debug-build crypto operations
/// exhaust the stack.
pub fn boot_dtb_addr() -> u64 {
    let addr: u64;
    // SAFETY: reading mscratch retrieves the DTB address saved by _start; always valid
    // in M-mode context.
    unsafe {
        core::arch::asm!("csrr {}, mscratch", out(reg) addr);
    }
    addr
}

// ---------------------------------------------------------------------------
// OpenSBI / RustSBI fw_dynamic protocol
// ---------------------------------------------------------------------------

/// `fw_dynamic_info` structure for the SBI fw_dynamic boot protocol.
///
/// Passed in `a2` when jumping to OpenSBI or RustSBI. Tells the SBI
/// firmware where the next boot stage (Linux) lives and what mode to
/// run it in.
///
/// Reference: OpenSBI `include/sbi/fw_dynamic.h`
#[derive(Debug)]
#[repr(C)]
pub struct FwDynamicInfo {
    /// Magic value: `0x4942534f` ("OSBI").
    pub magic: u64,
    /// Version of the info struct (currently 2).
    pub version: u64,
    /// Entry address of the next boot stage (Linux kernel).
    pub next_addr: u64,
    /// Privilege mode for the next stage: 1 = Supervisor.
    pub next_mode: u64,
    /// Options (reserved, set to 0).
    pub options: u64,
    /// Hart ID to boot on (usually 0).
    pub boot_hart: u64,
}

impl FwDynamicInfo {
    /// Magic value identifying this struct to the SBI firmware.
    pub const MAGIC: u64 = 0x4942534f; // "OSBI"
    /// Current version of the fw_dynamic_info protocol.
    pub const VERSION: u64 = 2;
    /// Supervisor mode constant for `next_mode`.
    pub const MODE_S: u64 = 1;

    /// Create a new `FwDynamicInfo` for booting Linux.
    pub fn new(kernel_addr: u64, boot_hart: u64) -> Self {
        Self {
            magic: Self::MAGIC,
            version: Self::VERSION,
            next_addr: kernel_addr,
            next_mode: Self::MODE_S,
            options: 0,
            boot_hart,
        }
    }
}

/// Jump to an SBI firmware (OpenSBI / RustSBI) using the fw_dynamic
/// protocol, which then boots Linux in S-mode.
///
/// Register convention on entry to SBI firmware:
/// - `a0` = boot hart ID
/// - `a1` = DTB address (for Linux, passed through by SBI)
/// - `a2` = pointer to `FwDynamicInfo`
///
/// # Safety
///
/// The caller must ensure all addresses are valid and the SBI binary
/// is loaded at `sbi_addr`.
pub fn boot_linux_sbi(sbi_addr: u64, hart_id: u64, dtb_addr: u64, info: &FwDynamicInfo) -> ! {
    // SAFETY: caller guarantees all addresses are valid mapped memory and firmware is
    // at sbi_addr; this is a non-returning M-mode to S-mode transition.
    unsafe {
        // Use explicit register constraints for a0/a1/a2 so the compiler
        // places values directly — avoids clobbering `{sbi}` with `mv`
        // instructions (the compiler is free to pick any register for
        // `{sbi}`, guaranteed not to be a0/a1/a2).
        core::arch::asm!(
            // Ensure stores to the SBI region are visible and the
            // I-cache fetches the new instructions (not stale FFS data).
            "fence rw, rw",
            "fence.i",
            "jr {sbi}",
            sbi = in(reg) sbi_addr,
            in("a0") hart_id,
            in("a1") dtb_addr,
            in("a2") info as *const FwDynamicInfo as u64,
            options(noreturn),
        );
    }
}

/// Unified Linux boot entry point.
///
/// Builds an [`FwDynamicInfo`] from `params.kernel_addr` and
/// `params.hart_id`, then jumps to the SBI firmware at `params.fw_addr`
/// which will `mret` into the kernel in S-mode.
///
/// Required fields: `kernel_addr`, `dtb_addr`, `fw_addr`, `hart_id`.
/// Ignored fields: `rsdp_addr`, `bootargs`, `e820_entries`, `zero_page_addr`.
pub fn boot_linux(params: &fstart_services::boot::BootLinuxParams<'_>) -> ! {
    let info = FwDynamicInfo::new(params.kernel_addr, params.hart_id);
    boot_linux_sbi(params.fw_addr, params.hart_id, params.dtb_addr, &info)
}

/// Copy an SBI firmware blob to its load address, then jump to it
/// using the fw_dynamic protocol.
///
/// This is needed when the firmware's load address overlaps with the
/// currently-executing code (e.g., both at 0x80000000). The copy +
/// jump is performed from a position-independent trampoline that is
/// first copied to a safe location (near the stack top) so it
/// survives the firmware overwrite.
///
/// # Safety
///
/// - `fw_src` and `fw_dst` must point to valid memory.
/// - `fw_len` must be the exact size of the firmware binary.
/// - The trampoline destination (`trampoline_addr`) must be in safe
///   RAM that won't be overwritten by the firmware copy.
/// - All other payload files must already be loaded.
pub fn copy_and_boot_sbi(
    fw_src: *const u8,
    fw_dst: u64,
    fw_len: usize,
    hart_id: u64,
    dtb_addr: u64,
    info: &FwDynamicInfo,
    trampoline_addr: u64,
) -> ! {
    // Copy the trampoline (defined in assembly below) to a safe
    // high-memory location, then jump to it.  The trampoline copies
    // the firmware blob from FFS to its load address and jumps.

    extern "C" {
        fn _sbi_copy_trampoline();
        fn _sbi_copy_trampoline_end();
    }
    let tramp_src = _sbi_copy_trampoline as *const u8;
    let tramp_end = _sbi_copy_trampoline_end as *const u8;
    let tramp_size = tramp_end as usize - tramp_src as usize;
    let tramp_dst = trampoline_addr as *mut u8;

    // Copy trampoline to safe location.
    // SAFETY: trampoline_addr is in safe high RAM (near stack top).
    unsafe {
        core::ptr::copy_nonoverlapping(tramp_src, tramp_dst, tramp_size);
    }

    // Flush I-cache for the trampoline region.
    // SAFETY: fence instructions are always safe.
    unsafe {
        core::arch::asm!("fence rw, rw");
        core::arch::asm!("fence.i");
    }

    // Jump to the trampoline with all parameters in registers.
    // a3 = src, a4 = dst, a5 = len, a6 = entry point (same as dst)
    // SAFETY: all addresses are valid, trampoline is at trampoline_addr.
    unsafe {
        core::arch::asm!(
            "jr {tramp}",
            tramp = in(reg) trampoline_addr,
            in("a0") hart_id,
            in("a1") dtb_addr,
            in("a2") info as *const FwDynamicInfo as u64,
            in("a3") fw_src as u64,
            in("a4") fw_dst,
            in("a5") fw_len,
            in("a6") fw_dst, // entry point = destination base
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
        // SAFETY: wfi is a hint instruction that is always safe to execute; it waits
        // for the next interrupt.
        unsafe {
            core::arch::asm!("wfi");
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
    // SAFETY: caller guarantees entry is a valid code address in mapped memory; this
    // is a non-returning jump.
    unsafe {
        core::arch::asm!(
            "jr {0}",
            in(reg) addr,
            options(noreturn),
        );
    }
}

/// Jump to an address, passing a handoff address in `a0`.
///
/// Same as [`jump_to`], plus: `handoff_addr` must point to a valid
/// serialized `StageHandoff` in DRAM (or be 0 for no handoff).
///
/// On RISC-V the convention is to pass the handoff address in `a0`.
/// The next stage's `_start` saves `a0` and makes it available via
/// [`boot_hart_id`] or a dedicated handoff reader.
#[inline(always)]
pub fn jump_to_with_handoff(addr: u64, handoff_addr: usize) -> ! {
    // SAFETY: caller guarantees entry and handoff_ptr are valid addresses in mapped
    // memory.
    unsafe {
        core::arch::asm!(
            "jr {addr}",
            addr = in(reg) addr,
            in("a0") handoff_addr,
            options(noreturn),
        );
    }
}
