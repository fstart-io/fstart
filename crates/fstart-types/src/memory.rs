//! Memory map types.

use heapless::String as HString;
use serde::{Deserialize, Serialize};

/// Complete memory map for a board.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryMap {
    /// Named memory regions (ROM, RAM only — not per-device MMIO)
    pub regions: heapless::Vec<MemoryRegion, 16>,
    /// Optional physical flash partition map.
    ///
    /// Intel descriptor based systems split the SPI flash into descriptor,
    /// GbE, ME, BIOS, and other regions.  fstart executes from and reads only
    /// the BIOS region, but host-side image assembly can still describe and
    /// optionally populate the non-BIOS regions.
    #[serde(default)]
    pub flash_layout: Option<FlashLayout>,
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
/// which  is detected at runtime using cpuid
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct CarConfig {
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

/// Physical flash layout for platforms with non-BIOS firmware regions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FlashLayout {
    /// Intel Firmware Descriptor controlled SPI flash.
    IntelIfd(IntelIfdFlashLayout),
}

/// Intel Firmware Descriptor flash layout.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntelIfdFlashLayout {
    /// Physical address where the entire SPI flash aperture is memory-mapped.
    pub base: u64,
    /// Total flash size in bytes.
    pub size: u32,
    /// Regions described by the descriptor.
    pub regions: heapless::Vec<IntelIfdRegionConfig, 8>,
}

impl IntelIfdFlashLayout {
    /// Return the configured BIOS region.
    pub fn bios_region(&self) -> Option<&IntelIfdRegionConfig> {
        self.regions
            .iter()
            .find(|region| region.kind == IntelIfdRegion::Bios)
    }

    /// Memory-mapped BIOS base address.
    pub fn bios_base(&self) -> Option<u64> {
        self.bios_region()
            .map(|region| self.base + u64::from(region.offset))
    }

    /// Memory-mapped end of the whole flash aperture.
    pub fn end(&self) -> u64 {
        self.base + u64::from(self.size)
    }
}

/// One Intel IFD flash region declared in board RON.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntelIfdRegionConfig {
    /// Descriptor region kind.
    pub kind: IntelIfdRegion,
    /// Offset from the start of the physical flash image.
    pub offset: u32,
    /// Region size in bytes.  Zero means the region is unused.
    pub size: u32,
    /// Optional binary blob to place in this region when a full flash image is
    /// generated.  Paths are resolved relative to the board directory.
    #[serde(default)]
    pub file: Option<HString<128>>,
}

/// Intel IFD region identifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum IntelIfdRegion {
    /// Flash descriptor.
    Descriptor,
    /// Host-visible BIOS region.
    Bios,
    /// Intel Management Engine region.
    Me,
    /// Intel GbE region.
    Gbe,
    /// Platform data region.
    Pdr,
    /// Reserved or unsupported region number.
    Reserved,
}

impl IntelIfdRegion {
    /// Numeric FLREG index used by Intel descriptors.
    pub fn flreg_index(self) -> Option<usize> {
        match self {
            IntelIfdRegion::Descriptor => Some(0),
            IntelIfdRegion::Bios => Some(1),
            IntelIfdRegion::Me => Some(2),
            IntelIfdRegion::Gbe => Some(3),
            IntelIfdRegion::Pdr => Some(4),
            IntelIfdRegion::Reserved => None,
        }
    }

    /// Conventional lower-case region name.
    pub fn as_str(self) -> &'static str {
        match self {
            IntelIfdRegion::Descriptor => "descriptor",
            IntelIfdRegion::Bios => "bios",
            IntelIfdRegion::Me => "me",
            IntelIfdRegion::Gbe => "gbe",
            IntelIfdRegion::Pdr => "pdr",
            IntelIfdRegion::Reserved => "reserved",
        }
    }
}
