//! RISC-V 64-bit platform support.
//!
//! Provides the reset vector entry point, stack setup, BSS clearing,
//! and architecture-specific helpers. Captures boot parameters (hart ID,
//! DTB address) passed by QEMU at reset.

#![no_std]

pub mod entry;

// ---------------------------------------------------------------------------
// Boot parameters — stored in CSRs, immune to stack overflow
// ---------------------------------------------------------------------------

/// Return the boot hart ID passed by QEMU at reset (`a0`).
///
/// Reads directly from the `mhartid` CSR (always available in M-mode).
pub fn boot_hart_id() -> u64 {
    let id: u64;
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
    unsafe {
        core::arch::asm!(
            "jr {0}",
            in(reg) addr,
            options(noreturn),
        );
    }
}
