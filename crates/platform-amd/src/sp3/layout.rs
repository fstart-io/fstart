//! The DRAM layout of the reset image and the early region, and where the
//! firmware sits in the SPI flash (FIRMWARE_IMAGE §1.2a, §2.1, §5; PSP_APOB
//! §3.6, §3.7).
//!
//! The PSP copies BIOS directory entry `0x62` from flash to DRAM and
//! releases the BSP at its last 16 bytes. On every SP3 donor the copy ends
//! at `0x7600_0000`; only the start (and size) differs:
//!
//! | Donor | flash | size | destination |
//! |---|---|---|---|
//! | Naples S8026 v2.02, Rome S8030 v4.04/v4.00, Rome S8026, Rome S8253 | `0xdb0000` | `0x250000` | `0x75db0000` |
//! | Naples KNPP-D32 1801 (smallest) | `0xde0000` | `0x220000` | `0x75de0000` |
//! | Milan S8030 v4.04, Milan S8253 v2.03 | `0xce0000` | `0x320000` | `0x75ce0000` |
//! | Milan S8030 v4.00 | `0xd00000` | `0x300000` | `0x75d00000` |
//!
//! fstart links one 3 MiB reset image for every generation ([`RESET`]),
//! keeping the common image end, and the builder points every `0x62` entry of
//! every composed generation at it: one copy at the top of the lower half
//! (unified layout), or one per half (dual layout), see
//! `fstart_image_build::amd::halved_flash`. The FFS is in the lower half only; the
//! reset image finds it through the FCH ROM3 window by absolute flash
//! offset (the flash anchor the builder patches), whichever half the ROM2
//! window at `0xff00_0000` shows.
//!
//! Everything the reset image needs besides its own bytes lives in the
//! **early region** `[0x7500_0000, 0x75c0_0000)`, below the lowest observed
//! destination and inside the 16 MiB write-back window the vendor SEC opens
//! with variable MTRR 7 (`0x7500_0006` / mask `0xffff_ff00_0800`,
//! instruction-checked in all seven donors, e.g. Rome S8030 `0x75ffefe1`).
//! That the PSP/ABL leave this DRAM alone is inference: the vendor caches it
//! from its first instructions, and the only PSP-written x86-visible range is
//! the APOB at `0x3000_0000` (PSP_APOB §3.5).

use crate::layout::{Layout, Region};
use fstart_driver_amd::fch::hw::rom3;

/// One flash half (16 MiB), the unit an EFS describes in the dual layout.
pub const HALF_SIZE: u64 = 16 << 20;
/// ROM2: the 16 MiB flash page `SPIx5C` selects, below 4 GiB.
pub const ROM2: Region = Region::new(fstart_amdfw_format::ROM2_BASE, HALF_SIZE);
/// ROM3: the whole flash (up to 64 MiB) above 4 GiB, in the 12 GiB hole
/// below 1 TiB; identity-mapped by the reset image's page tables.
pub const ROM3: Region = Region::new(rom3::BASE, rom3::SIZE);
/// End of every SP3 reset image in DRAM (exclusive).
pub const IMAGE_END: u64 = 0x7600_0000;
/// Real-mode segment the PSP releases x86 in: CS base = image end − 64 KiB,
/// IP = `0xfff0`. Instruction-checked only indirectly: the vendor's 16-bit
/// code addresses its GDT descriptor as `cs:0xfb50` (Rome S8030, flash
/// `0xfffffb50`), which is right only for this base (or for `0xffff_0000`
/// with the identical flash copy on top); AGESA patches the AP reset vector
/// in the DRAM copy (CCX §3.7).
pub const RESET_SEGMENT_BASE: u64 = IMAGE_END - 0x1_0000;

/// Where the PSP puts the reset image (BIOS directory entry `0x62`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResetGeometry {
    /// Offset of the image in the 16 MiB half.
    pub flash_offset: u32,
    pub size: u32,
    /// DRAM destination (copy target and link address).
    pub destination: u64,
}

/// Why a reset geometry is not usable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GeometryError {
    /// The DRAM copy must end at [`IMAGE_END`].
    DramEnd,
    /// The flash copy must end at the top of the half: the vendor SEC jumps
    /// into the flash copy first (FIRMWARE_IMAGE §5), and our 16-bit entry
    /// may be fetched from either copy (ROM2 shows the top of the half).
    FlashEnd,
    /// Destination and size must be 64 KiB multiples and cover the 64 KiB
    /// real-mode segment.
    Alignment,
    /// The image would overlap the early region.
    EarlyRegion,
}

impl ResetGeometry {
    pub const fn dram(&self) -> Region {
        Region::new(self.destination, self.size as u64)
    }

    /// Where the PSP releases the BSP (and the SMU the APs).
    pub const fn reset_vector(&self) -> u64 {
        self.destination + self.size as u64 - 0x10
    }

    pub const fn check(&self) -> Result<(), GeometryError> {
        if self.destination + self.size as u64 != IMAGE_END {
            Err(GeometryError::DramEnd)
        } else if self.flash_offset as u64 + self.size as u64 != HALF_SIZE {
            Err(GeometryError::FlashEnd)
        } else if !self.destination.is_multiple_of(0x1_0000)
            || !self.size.is_multiple_of(0x1_0000)
            || self.size < 0x1_0000
        {
            Err(GeometryError::Alignment)
        } else if self.destination < EARLY_REGION.end() {
            Err(GeometryError::EarlyRegion)
        } else {
            Ok(())
        }
    }
}

/// The common 3 MiB reset-image budget, including the graphical UEFI
/// payload. Every composed `0x62` entry is rewritten to this geometry.
/// The reset vector and the firmware's early-RAM reservations stay fixed;
/// the settings store and FFS limit follow the lower flash start.
pub const RESET: ResetGeometry = ResetGeometry {
    flash_offset: 0xd0_0000,
    size: 0x30_0000,
    destination: 0x75d0_0000,
};

const _: () = assert!(RESET.check().is_ok());

/// Lowest flash offset of the fstart FFS (lower half): above the vendor
/// prefix of every donor (prefix ends: Naples `0x294000`/`0x4e0000`, Rome
/// `0x50b000`/`0x54b000`, Milan `0x586000`; FIRMWARE_IMAGE §4) with room
/// for prefix growth and the combo directory. The image builder moves it up
/// if a vendor prefix reaches further.
pub const FFS_OFFSET: u32 = 0x60_0000;

/// A range of the SPI flash, in flash offsets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FlashRange {
    pub offset: u32,
    pub size: u32,
}

impl FlashRange {
    pub const fn end(&self) -> u32 {
        self.offset + self.size
    }
}

/// The settings store: the EFI variable store CrabEFI writes, which also
/// holds the option overrides the boot reads (`fstart_options`). It sits
/// at the top of the FFS window of the lower half, below the reset image,
/// in 4 KiB sectors the SPI100 command engine erases; a new image has it
/// erased. Inside the low 16 MiB, so 3-byte SPI commands reach it.
pub const NVRAM: FlashRange = FlashRange {
    offset: RESET.flash_offset - NVRAM_SIZE,
    size: NVRAM_SIZE,
};
/// CrabEFI's default variable store size.
const NVRAM_SIZE: u32 = 0x4_0000;

/// Upper bound of the FFS in the ROM3 view, the window the FFS is built
/// for: `[FFS_OFFSET, NVRAM.offset)` of the lower half. The runtime reads
/// only what the flash anchor and FFS locator describe.
pub const FFS_WINDOW: Region = Region::new(
    ROM3.base + FFS_OFFSET as u64,
    (NVRAM.offset - FFS_OFFSET) as u64,
);

const _: () = assert!(NVRAM.end() == RESET.flash_offset && NVRAM.offset.is_multiple_of(0x1000));

/// Firmware-owned DRAM below the reset image: the page tables, BSS,
/// heap, stack and store of [`LAYOUT`].
pub const EARLY_REGION: Region = Region::new(0x7500_0000, 0xc0_0000);

/// The reset image in DRAM and everything it needs besides its own bytes,
/// in the vendor's 16 MiB write-back window (its MTRR 7 window, the
/// layout's cache window). 256 PDPTs of 1 GiB pages (every Zen core has
/// `PDPE1GB`) cover the whole lower canonical half (128 TiB), so DRAM of a
/// full 2P system and the DF's 64-bit MMIO windows are mapped without
/// later table growth. `.data` stays in the image (it is already in
/// DRAM); `data` is BSS and the arch crate's per-AP stacks (4 KiB × CPUs).
/// The store holds the ACPI/SMBIOS tables and records; only its used part
/// needs to stay reserved, but the early region is reserved as a whole.
/// The FFS is read through ROM3, the whole flash above 4 GiB.
pub const LAYOUT: Layout = Layout {
    early: Region::new(EARLY_REGION.base, IMAGE_END - EARLY_REGION.base),
    page_tables: EARLY_REGION.base,
    pdpts: 256,
    data: Region::new(0x7520_0000, 0x40_0000),
    heap: Region::new(0x7560_0000, 0x40_0000),
    stack: Region::new(0x75a0_0000, 0x4_0000),
    store: Some(Region::new(0x75b0_0000, 0x10_0000)),
    image: RESET.dram(),
    flash: ROM3,
    ffs: Some(FFS_WINDOW),
}
.check();

const _: () = assert!(EARLY_REGION.contains(&LAYOUT.store.unwrap()));

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reset_image_and_ffs_window() {
        assert_eq!(RESET.reset_vector(), 0x75ff_fff0);
        assert!(LAYOUT.cache_window().contains(&RESET.dram()));
        assert_eq!(FFS_WINDOW.base, 0xfd_0060_0000);
        assert_eq!(FFS_WINDOW.end(), ROM3.base + u64::from(NVRAM.offset));
        assert_eq!(NVRAM.offset, 0xcc_0000);
        let low = ResetGeometry {
            flash_offset: 0xb0_0000,
            size: 0x50_0000,
            destination: 0x75b0_0000,
        };
        assert_eq!(low.check(), Err(GeometryError::EarlyRegion));
        let unaligned = ResetGeometry {
            size: 0x25_8000,
            destination: 0x75da_8000,
            flash_offset: 0xda_8000,
        };
        assert_eq!(unaligned.check(), Err(GeometryError::Alignment));
        // ROM3 lies in the hole below 1 TiB, inside the identity map.
        assert!(ROM3.base >= 0xfd_0000_0000 && ROM3.end() <= 1 << 40);
        assert!(ROM3.end() <= (LAYOUT.pdpts * 512) << 30);
    }

    #[test]
    fn the_layout_is_the_vendor_window() {
        assert_eq!(LAYOUT.cache_window(), Region::new(0x7500_0000, 0x100_0000));
        assert!(EARLY_REGION.end() <= RESET.destination);
        // The page tables cover the canonical lower half with 1 GiB pages.
        assert_eq!((LAYOUT.pdpts * 512) << 30, 1 << 47);
    }
}
