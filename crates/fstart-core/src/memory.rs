//! Memory map types.

use core::fmt;

use heapless::String as HString;
use serde::{Deserialize, Serialize};

/// Complete memory map for a board.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryMap {
    /// Named memory regions (ROM, RAM only — not per-device MMIO)
    pub regions: heapless::Vec<MemoryRegion, 16>,
    /// Optional physical flash partition map.
    ///
    /// Descriptor-based systems can split SPI flash into descriptor, GbE, ME,
    /// BIOS, and other regions. Host-side image assembly can describe and
    /// optionally populate non-firmware regions.
    #[serde(default)]
    pub flash_layout: Option<FlashLayout>,
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

impl MemoryMap {
    /// Return the firmware aperture declared by the memory map.
    ///
    /// Boot media does not consume this directly; firmware-image mappings come
    /// from Rust providers. Linker/setup code still uses the ROM aperture for
    /// placement and x86 cache/MTRR setup.
    pub fn firmware_window(&self) -> Option<(u64, u64)> {
        match &self.flash_layout {
            Some(FlashLayout::IntelIfd(layout)) => {
                let bios = layout.bios_region()?;
                Some((layout.base + u64::from(bios.offset), u64::from(bios.size)))
            }
            Some(FlashLayout::Legacy(layout)) => Some((layout.base, u64::from(layout.size))),
            None => self.contiguous_rom_window(),
        }
    }

    /// Derive redundant firmware-image facts.
    ///
    /// A declared flash layout provides the linker-visible firmware window;
    /// this helper ensures the corresponding ROM region exists.
    pub fn normalize_derived_flash(&mut self) -> Result<(), MemoryMapError> {
        if let Some((base, size)) = self.ifd_bios_window()? {
            return self.ensure_rom_region(base, size);
        }

        Ok(())
    }

    fn ifd_bios_window(&self) -> Result<Option<(u64, u64)>, MemoryMapError> {
        let Some(layout) = &self.flash_layout else {
            return Ok(None);
        };
        match layout {
            FlashLayout::IntelIfd(layout) => {
                let bios = layout
                    .bios_region()
                    .ok_or(MemoryMapError::MissingBiosRegion)?;
                Ok(Some((
                    layout.base + u64::from(bios.offset),
                    u64::from(bios.size),
                )))
            }
            FlashLayout::Legacy(layout) => Ok(Some((layout.base, u64::from(layout.size)))),
        }
    }

    fn contiguous_rom_window(&self) -> Option<(u64, u64)> {
        let mut count = 0usize;
        let mut base = u64::MAX;
        let mut end = 0u64;
        let mut total_size = 0u64;

        for region in self
            .regions
            .iter()
            .filter(|region| region.kind == RegionKind::Rom)
        {
            let region_end = region.base.checked_add(region.size)?;
            count += 1;
            base = base.min(region.base);
            end = end.max(region_end);
            total_size = total_size.checked_add(region.size)?;
        }

        if count == 0 || end.checked_sub(base)? != total_size {
            return None;
        }

        Some((base, total_size))
    }

    fn ensure_rom_region(
        &mut self,
        expected_base: u64,
        expected_size: u64,
    ) -> Result<(), MemoryMapError> {
        let expected_end = expected_base.saturating_add(expected_size);
        for region in &self.regions {
            if region.kind != RegionKind::Rom {
                continue;
            }
            if region.base == expected_base && region.size == expected_size {
                return Ok(());
            }
            let region_end = region.base.saturating_add(region.size);
            if region.base < expected_end && expected_base < region_end {
                return Err(MemoryMapError::RomRegionOverlap {
                    expected_base,
                    expected_size,
                    actual_base: region.base,
                    actual_size: region.size,
                });
            }
        }

        self.regions
            .push(MemoryRegion {
                name: HString::try_from("flash").map_err(|_| MemoryMapError::RegionsFull)?,
                base: expected_base,
                size: expected_size,
                kind: RegionKind::Rom,
            })
            .map_err(|_| MemoryMapError::RegionsFull)
    }
}

/// Error while deriving redundant memory-map facts from a flash layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryMapError {
    /// A descriptor-based layout lacks a BIOS region entry.
    MissingBiosRegion,
    /// A ROM memory region overlaps the derived firmware window without matching it.
    RomRegionOverlap {
        /// Expected ROM base.
        expected_base: u64,
        /// Expected ROM size.
        expected_size: u64,
        /// Actual configured ROM base.
        actual_base: u64,
        /// Actual configured ROM size.
        actual_size: u64,
    },
    /// `memory.regions` has no room for the derived ROM region.
    RegionsFull,
}

impl fmt::Display for MemoryMapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::MissingBiosRegion => f.write_str("descriptor flash_layout requires a BIOS region"),
            Self::RomRegionOverlap {
                expected_base,
                expected_size,
                actual_base,
                actual_size,
            } => write!(
                f,
                "ROM memory region overlaps the firmware image region but does not match it: \
                 expected base={expected_base:#x} size={expected_size:#x}, got base={actual_base:#x} size={actual_size:#x}"
            ),
            Self::RegionsFull => f.write_str(
                "memory.regions is full; cannot add derived firmware ROM region",
            ),
        }
    }
}

/// Cache-as-RAM (CAR) configuration for pre-DRAM x86 stages.
///
/// Describes a region of cache-locked memory used as temporary writable
/// storage before the DRAM controller is programmed. The firmware's
/// bootblock enters this mode via MTRR programming + a CPU-specific
/// which  is detected at runtime using cpuid
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
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
/// Device MMIO ranges do not belong here — keep them in typed driver/platform config.
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
    /// Legacy contiguous flash without an Intel Firmware Descriptor.
    Legacy(LegacyFlashLayout),
}

/// Legacy contiguous flash layout without an Intel Firmware Descriptor.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LegacyFlashLayout {
    /// Physical address where the flash is memory-mapped.
    pub base: u64,
    /// Total flash size in bytes.
    pub size: u32,
}

/// Intel Firmware Descriptor flash layout.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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

/// One Intel IFD flash region declared in board metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
