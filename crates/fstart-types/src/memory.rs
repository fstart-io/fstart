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
    /// Cache-as-RAM (CAR) region for pre-DRAM x86 stages.
    ///
    /// On x86 platforms, bootblock (and optionally romstage) runs
    /// before DRAM is initialized. CAR uses the CPU's L1/L2 cache as
    /// temporary writable RAM by programming MTRRs and entering
    /// Non-Evict Mode (NEM) or a similar cache-locking mechanism.
    ///
    /// When this field is set, the linker **automatically** places
    /// `.data`, `.bss`, and the stack of every XIP stage
    /// (`runs_from: Rom` with `load_addr` in a ROM region) into this
    /// CAR region instead of the first RAM region. RAM-loaded stages
    /// (`runs_from: Ram`) are unaffected.
    ///
    /// `None` for boards that don't need CAR (ARM / RISC-V, where
    /// DRAM is live at reset; QEMU virt; etc.).
    #[serde(default)]
    pub car: Option<CarConfig>,
}

/// Cache-as-RAM (CAR) configuration for pre-DRAM x86 stages.
///
/// Describes a region of cache-locked memory used as temporary writable
/// storage before the DRAM controller is programmed. The firmware's
/// bootblock enters this mode via MTRR programming + a CPU-specific
/// mechanism (see [`CarMethod`]).
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct CarConfig {
    /// How to enable CAR on this CPU.
    pub method: CarMethod,
    /// Base physical address of the CAR region.
    ///
    /// Typically in the 0xFEF0_0000 range for Intel Atom-class parts,
    /// or a cache-sized window below 4 GiB for other CPUs.
    pub base: u64,
    /// Size of the CAR region in bytes.
    ///
    /// Must not exceed the cache size. For Intel Atom D4xx/D5xx
    /// (Pineview), L2 cache is 512 KiB, so `size <= 0x8_0000`.
    pub size: u64,
}

/// Mechanism used to enable Cache-as-RAM on the target CPU.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CarMethod {
    /// Intel Non-Evict Mode (NEM) — MSR `0x2E0` setup + cache fill.
    ///
    /// Used on Atom (Pineview, Cedarview), Core 2, early Nehalem, and
    /// similar pre-NEM-deprecation Intel parts. Not supported on
    /// Skylake and later.
    NonEvictMode,
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
