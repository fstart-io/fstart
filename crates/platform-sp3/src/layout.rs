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
//! fstart links one reset image for every generation at the smallest
//! geometry ([`RESET`]) and the image builder points every `0x62` entry of
//! every composed generation at it: one copy at the top of the lower half
//! (unified layout), or one per half (dual layout), see
//! `fstart_image_build::sp3_flash`. The FFS is in the lower half only; the
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

use fstart_driver_amd::sp3::fch::rom3;
use fstart_driver_amd::sp3::soc::Generation;

/// A physical address range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub base: u64,
    pub size: u64,
}

impl Span {
    pub const fn new(base: u64, size: u64) -> Self {
        Self { base, size }
    }

    pub const fn end(&self) -> u64 {
        self.base + self.size
    }

    pub const fn contains(&self, other: &Span) -> bool {
        other.base >= self.base && other.end() <= self.end()
    }

    pub const fn overlaps(&self, other: &Span) -> bool {
        self.base < other.end() && other.base < self.end()
    }
}

/// One flash half (16 MiB), the unit an EFS describes in the dual layout.
pub const HALF_SIZE: u64 = 16 << 20;
/// ROM2: the 16 MiB flash page `SPIx5C` selects, below 4 GiB.
pub const ROM2: Span = Span::new(fstart_amdfw_format::ROM2_BASE, HALF_SIZE);
/// ROM3: the whole flash (up to 64 MiB) above 4 GiB, in the 12 GiB hole
/// below 1 TiB; identity-mapped by the reset image's page tables.
pub const ROM3: Span = Span::new(rom3::BASE, rom3::SIZE);
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
    pub const fn dram(&self) -> Span {
        Span::new(self.destination, self.size as u64)
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

/// The reset image of every build, at the smallest donor geometry (Naples
/// KNPP-D32 1801, FIRMWARE_IMAGE §2.1); every `0x62` entry is rewritten to
/// it. A release Naples+Rome+Milan image uses about `0x57000`.
pub const RESET: ResetGeometry = ResetGeometry {
    flash_offset: 0xde_0000,
    size: 0x22_0000,
    destination: 0x75de_0000,
};

const _: () = assert!(RESET.check().is_ok());

/// Lowest flash offset of the fstart FFS (lower half): above the vendor
/// prefix of every donor (prefix ends: Naples `0x294000`/`0x4e0000`, Rome
/// `0x50b000`/`0x54b000`, Milan `0x586000`; FIRMWARE_IMAGE §4) with room
/// for prefix growth and the combo directory. The image builder moves it up
/// if a vendor prefix reaches further.
pub const FFS_OFFSET: u32 = 0x60_0000;

/// Upper bound of the FFS in the ROM3 view, the window the FFS is built
/// for: `[FFS_OFFSET, RESET.flash_offset)` of the lower half. The runtime
/// reads only what the flash anchor and FFS locator describe.
pub const FFS_WINDOW: Span = Span::new(
    ROM3.base + FFS_OFFSET as u64,
    (RESET.flash_offset - FFS_OFFSET) as u64,
);

/// The 16 MiB write-back window opened before anything is written (the
/// vendor's MTRR 7 window): early region plus reset image.
pub const CACHE_WINDOW: Span = Span::new(0x7500_0000, 0x100_0000);
/// Firmware-owned DRAM below the reset image.
pub const EARLY_REGION: Span = Span::new(0x7500_0000, 0xc0_0000);

/// Number of page-directory-pointer tables: each maps 512 GiB with 1 GiB
/// pages (every Zen core has `PDPE1GB`). 256 cover the whole lower
/// canonical half (128 TiB), so DRAM of a full 2P system and the DF's
/// 64-bit MMIO windows are mapped without later table growth.
pub const PDPT_COUNT: u64 = 256;
/// PML4 followed by [`PDPT_COUNT`] PDPTs, built by `reset.S`.
pub const PAGE_TABLES: Span = Span::new(0x7500_0000, 0x1000 * (1 + PDPT_COUNT));
/// `.data` stays in the image (it is already in DRAM); this is BSS and
/// the arch crate's per-AP stacks (4 KiB × CPUs).
pub const DATA: Span = Span::new(0x7520_0000, 0x40_0000);
/// Stage heap (`_FSTART_HEAP`).
pub const HEAP: Span = Span::new(0x7560_0000, 0x40_0000);
/// BSP stack.
pub const STACK: Span = Span::new(0x75a0_0000, 0x4_0000);
/// Firmware store window (ACPI/SMBIOS tables, records); only its used part
/// needs to stay reserved, but [`EARLY_REGION`] is reserved as a whole.
pub const STORE: Span = Span::new(0x75b0_0000, 0x10_0000);

/// Board facts the SP3 build plan and flash image need. A board crate
/// implements this for its `Board` type, e.g. for the Tyan S8030:
///
/// ```ignore
/// impl Sp3BoardFacts for Board {
///     const GENERATIONS: &'static [Generation] = &[Generation::Rome, Generation::Milan];
///     const VENDOR_IMAGE: &'static str = "boards/tyan/s8030/vendor/8030V404.ROM";
///     const MAX_CPUS: u16 = 128;
/// }
/// ```
///
/// It is a plain trait (not behind the `host` feature) because the host
/// plan runner builds the board crate without its own `host` feature.
pub trait Sp3BoardFacts {
    /// Generations the board supports. The platform must be built with
    /// each one's feature; one firmware build serves all of them.
    const GENERATIONS: &'static [Generation];
    /// Workspace-relative vendor ROM: the 32 MiB dual image or one 16 MiB
    /// half. Its PSP/BIOS directories, blobs and APCB stay in place.
    const VENDOR_IMAGE: &'static str;
    /// Logical CPU ceiling over every supported generation (all sockets);
    /// sizes the AP stacks and SMM.
    const MAX_CPUS: u16;
}

/// Whether this build carries `generation`'s silicon code (its platform
/// feature).
pub const fn compiled(generation: Generation) -> bool {
    match generation {
        Generation::Naples => cfg!(feature = "naples"),
        Generation::Rome => cfg!(feature = "rome"),
        Generation::Milan => cfg!(feature = "milan"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reset_image_and_ffs_window() {
        assert_eq!(RESET.reset_vector(), 0x75ff_fff0);
        assert!(CACHE_WINDOW.contains(&RESET.dram()));
        assert_eq!(FFS_WINDOW.base, 0xfd_0060_0000);
        assert_eq!(FFS_WINDOW.end(), ROM3.base + u64::from(RESET.flash_offset));
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
        assert!(ROM3.end() <= (PDPT_COUNT * 512) << 30);
    }

    #[test]
    fn early_regions_are_disjoint_inside_the_early_region_and_cache_window() {
        let parts = [PAGE_TABLES, DATA, HEAP, STACK, STORE];
        assert!(CACHE_WINDOW.contains(&EARLY_REGION));
        // One variable MTRR: naturally aligned power of two.
        assert!(
            CACHE_WINDOW.size.is_power_of_two()
                && CACHE_WINDOW.base.is_multiple_of(CACHE_WINDOW.size)
        );
        for (i, a) in parts.iter().enumerate() {
            assert!(EARLY_REGION.contains(a), "{a:x?}");
            assert_eq!(a.base % 0x1000, 0);
            for b in &parts[i + 1..] {
                assert!(!a.overlaps(b), "{a:x?} {b:x?}");
            }
        }
        // The page tables cover the canonical lower half with 1 GiB pages.
        assert_eq!((PDPT_COUNT * 512) << 30, 1 << 47);
    }
}
