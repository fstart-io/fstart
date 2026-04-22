//! Shared boot protocol types.
//!
//! [`BootLinuxParams`] provides a uniform interface for platform crates'
//! `boot_linux()` functions.  Each platform reads the fields it needs
//! and ignores the rest — the struct is the common denominator across
//! RISC-V (SBI), AArch64 (ATF), ARMv7 (direct jump), and x86_64
//! (zero-page protocol).
//!
//! Codegen emits a single `fstart_platform::boot_linux(&params)` call
//! regardless of platform, eliminating the per-platform `match` in
//! `board_gen::platform_boot_protocol_stmts`.

use crate::memory_detect::E820Entry;

/// Parameters for the Linux boot protocol.
///
/// Fields are `Option` where not universally applicable.  Platform
/// crates document which fields they require vs ignore.
///
/// Not `Copy` because `bootargs` and `e820_entries` are references.
#[derive(Debug)]
pub struct BootLinuxParams<'a> {
    /// Kernel entry point (load address).
    pub kernel_addr: u64,
    /// Flattened device tree address.
    pub dtb_addr: u64,
    /// Firmware entry point (SBI on RISC-V, BL31 on AArch64).
    /// Ignored on ARMv7 and x86_64.
    pub fw_addr: u64,
    /// ACPI RSDP physical address.  x86_64 only; ignored elsewhere.
    pub rsdp_addr: u64,
    /// Kernel command line.  x86_64 copies it into the zero page;
    /// other platforms pass it via the DTB `/chosen/bootargs` node
    /// (already patched by `fdt_prepare`).
    pub bootargs: &'a str,
    /// e820 memory map.  x86_64 only; ignored elsewhere.
    pub e820_entries: &'a [E820Entry],
    /// Address for the zero page (boot_params).  x86_64 only.
    pub zero_page_addr: u64,
    /// Platform-specific hart ID (RISC-V only; ignored elsewhere).
    pub hart_id: u64,
}

impl<'a> BootLinuxParams<'a> {
    /// Minimal params — all optional fields zeroed / empty.
    pub const fn new(kernel_addr: u64, dtb_addr: u64) -> Self {
        Self {
            kernel_addr,
            dtb_addr,
            fw_addr: 0,
            rsdp_addr: 0,
            bootargs: "",
            e820_entries: &[],
            zero_page_addr: 0,
            hart_id: 0,
        }
    }
}
