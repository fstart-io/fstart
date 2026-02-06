//! Memory map types.

use heapless::String as HString;
use serde::{Deserialize, Serialize};

/// Complete memory map for a board.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryMap {
    /// Named memory regions (ROM, RAM only — not per-device MMIO)
    pub regions: heapless::Vec<MemoryRegion, 16>,
    /// Base address where the firmware flash image is mapped in memory.
    ///
    /// For XIP flash, this is the flash memory-mapped region start.
    /// For QEMU `-bios`, this is the address where QEMU loads the image
    /// (typically the RAM base, e.g., `0x80000000` for riscv64 virt).
    ///
    /// Used by `SigVerify`, `StageLoad`, and `PayloadLoad` to locate
    /// the FFS anchor and file data in the firmware image. If `None`,
    /// these capabilities fall back to scanning or use the bootblock's
    /// own load address.
    #[serde(default)]
    pub flash_base: Option<u64>,
    /// Total size of the firmware flash image in bytes.
    ///
    /// Used to bound the `FfsReader`'s view of the flash image.
    /// If `None`, the total image size from the FFS anchor is used instead.
    #[serde(default)]
    pub flash_size: Option<u64>,
}

/// A single memory region.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryRegion {
    /// Region name (e.g., "rom", "ram", "mmio")
    pub name: HString<32>,
    /// Base physical address
    pub base: u64,
    /// Size in bytes
    pub size: u64,
    /// What kind of memory this is
    pub kind: RegionKind,
}

/// Type of memory region.
///
/// Device MMIO ranges do not belong here — they go in `DeviceConfig::resources`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RegionKind {
    /// Read-only memory (flash, ROM)
    Rom,
    /// Read-write memory (DRAM, SRAM)
    Ram,
    /// Reserved (firmware-owned, not passed to OS)
    Reserved,
}
