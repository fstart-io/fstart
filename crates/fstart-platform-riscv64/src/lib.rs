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
            "jr {sbi}",
            sbi = in(reg) sbi_addr,
            in("a0") hart_id,
            in("a1") dtb_addr,
            in("a2") info as *const FwDynamicInfo as u64,
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
