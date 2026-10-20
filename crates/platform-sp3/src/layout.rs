//! The DRAM layout of the reset image and the early region (FIRMWARE_IMAGE
//! §2.1, §5; PSP_APOB §3.6, §3.7).
//!
//! The PSP copies BIOS directory entry `0x62` from the top of the flash half
//! to DRAM and releases the BSP at its last 16 bytes. On every SP3 donor the
//! copy ends at `0x7600_0000`; only the start (and size) differs:
//!
//! | Donor | flash | size | destination |
//! |---|---|---|---|
//! | Naples S8026 v2.02, Rome S8030 v4.04/v4.00, Rome S8026, Rome S8253 | `0xdb0000` | `0x250000` | `0x75db0000` |
//! | Naples KNPP-D32 1801 | `0xde0000` | `0x220000` | `0x75de0000` |
//! | Milan S8030 v4.04, Milan S8253 v2.03 | `0xce0000` | `0x320000` | `0x75ce0000` |
//! | Milan S8030 v4.00 | `0xd00000` | `0x300000` | `0x75d00000` |
//!
//! A build links the reset image for one [`ResetGeometry`] (the per
//! generation constants below); the host checks it against the vendor
//! directory and refuses a mismatch unless the board opts into rewriting the
//! `0x62` entries ([`crate::host`]).
//!
//! Everything the reset image needs besides its own bytes lives in the
//! **early region** `[0x7500_0000, 0x75c0_0000)`, below the lowest observed
//! destination and inside the 16 MiB write-back window the vendor SEC opens
//! with variable MTRR 7 (`0x7500_0006` / mask `0xffff_ff00_0800`,
//! instruction-checked in all seven donors, e.g. Rome S8030 `0x75ffefe1`).
//! The same layout therefore serves every generation and vendor geometry.
//! That the PSP/ABL leave this DRAM alone is inference: the vendor caches it
//! from its first instructions, and the only PSP-written x86-visible range is
//! the APOB at `0x3000_0000` (PSP_APOB §3.5).

use fstart_driver_zen::soc::Generation;

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

/// One flash half (16 MiB), the unit the PSP boots from.
pub const HALF_SIZE: u64 = 16 << 20;
/// The booted half as mapped below 4 GiB.
pub const FLASH_MMIO: Span = Span::new(0xff00_0000, HALF_SIZE);
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
    /// may be fetched from either copy.
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

/// Naples S8026 v2.02 (FIRMWARE_IMAGE §2.1).
pub const NAPLES_RESET: ResetGeometry = ResetGeometry {
    flash_offset: 0xdb_0000,
    size: 0x25_0000,
    destination: 0x75db_0000,
};
/// Rome S8030 v4.04/v4.00, S8026 v2.02, S8253 v2.03.
pub const ROME_RESET: ResetGeometry = NAPLES_RESET;
/// Milan S8030 v4.04, S8253 v2.03 (v4.00 used `0xd00000`/`0x300000`).
pub const MILAN_RESET: ResetGeometry = ResetGeometry {
    flash_offset: 0xce_0000,
    size: 0x32_0000,
    destination: 0x75ce_0000,
};

/// The reset geometry a build of `generation` links for.
pub const fn reset_geometry(generation: Generation) -> ResetGeometry {
    match generation {
        Generation::Naples => NAPLES_RESET,
        Generation::Rome => ROME_RESET,
        Generation::Milan => MILAN_RESET,
    }
}

/// First flash offset of the fstart FFS. Above the vendor prefix of every
/// donor of the generation (prefix ends: Naples `0x273000`/`0x4ba000`,
/// Rome `0x50b000`/`0x54b000`, Milan `0x586000`; FIRMWARE_IMAGE §4) with
/// room to spare for prefix growth in later vendor releases.
pub const fn ffs_offset(generation: Generation) -> u32 {
    match generation {
        Generation::Naples => 0x50_0000,
        Generation::Rome | Generation::Milan => 0x60_0000,
    }
}

/// The FFS window of the booted half in the MMIO view: from
/// [`ffs_offset`] up to the reset image.
pub const fn ffs_window(generation: Generation) -> Span {
    let offset = ffs_offset(generation) as u64;
    let end = reset_geometry(generation).flash_offset as u64;
    Span::new(FLASH_MMIO.base + offset, end - offset)
}

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

/// The generation this firmware build serves (exactly one generation
/// feature; host builds may enable several and pass the generation
/// explicitly).
#[cfg(all(feature = "naples", not(any(feature = "rome", feature = "milan"))))]
pub const GENERATION: Generation = Generation::Naples;
#[cfg(all(feature = "rome", not(any(feature = "naples", feature = "milan"))))]
pub const GENERATION: Generation = Generation::Rome;
#[cfg(all(feature = "milan", not(any(feature = "naples", feature = "rome"))))]
pub const GENERATION: Generation = Generation::Milan;

/// This build's reset geometry.
#[cfg(any(
    all(feature = "naples", not(any(feature = "rome", feature = "milan"))),
    all(feature = "rome", not(any(feature = "naples", feature = "milan"))),
    all(feature = "milan", not(any(feature = "naples", feature = "rome")))
))]
pub const RESET: ResetGeometry = reset_geometry(GENERATION);

#[cfg(any(
    all(feature = "naples", not(any(feature = "rome", feature = "milan"))),
    all(feature = "rome", not(any(feature = "naples", feature = "milan"))),
    all(feature = "milan", not(any(feature = "naples", feature = "rome")))
))]
const _: () = assert!(RESET.check().is_ok());

/// Which 16 MiB half of a 32 MiB dual-image flash a board variant occupies.
/// The PSP boot ROM tries the lower half first, so Rome must be below
/// Milan (FIRMWARE_IMAGE §1.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlashHalf {
    Lower,
    Upper,
}

impl FlashHalf {
    pub const fn offset(self) -> usize {
        match self {
            Self::Lower => 0,
            Self::Upper => HALF_SIZE as usize,
        }
    }
}

/// Board facts the SP3 build plan and flash image need. A board crate
/// implements this for its `Board` type once per variant; the variant's
/// generation feature selects the impl, e.g. for the Tyan S8030:
///
/// ```ignore
/// #[cfg(feature = "rome")]
/// impl Sp3BoardFacts for Board {
///     const GENERATION: Generation = Generation::Rome;
///     const VENDOR_IMAGE: &'static str = "boards/tyan/s8030/vendor/8030V404.ROM";
///     const FLASH_HALF: FlashHalf = FlashHalf::Lower;
///     const MAX_CPUS: u16 = 128;
/// }
/// ```
///
/// It is a plain trait (not behind the `host` feature) because the host
/// plan runner builds the board crate without its own `host` feature.
pub trait Sp3BoardFacts {
    /// Generation of this variant; must equal the platform's enabled
    /// generation feature.
    const GENERATION: Generation;
    /// Workspace-relative vendor ROM: the 32 MiB dual image or one 16 MiB
    /// half. Its prefix (EFS, PSP/BIOS directories, blobs, APCB) is kept
    /// byte-identical.
    const VENDOR_IMAGE: &'static str;
    /// Half of the 32 MiB flash this variant occupies (and, for a 32 MiB
    /// vendor image, the half its prefix is taken from).
    const FLASH_HALF: FlashHalf;
    /// Logical CPU ceiling (all sockets); sizes the AP stacks and SMM.
    const MAX_CPUS: u16;
    /// Accept a vendor `0x62` geometry different from [`reset_geometry`]
    /// by rewriting the BIOS directory entries. Untested on hardware
    /// (FIRMWARE_IMAGE §9); keep `false` unless proven.
    const REWRITE_RESET_GEOMETRY: bool = false;
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [Generation; 3] = [Generation::Naples, Generation::Rome, Generation::Milan];

    #[test]
    fn every_generation_geometry_ends_at_both_tops_above_the_early_region() {
        for g in ALL {
            let r = reset_geometry(g);
            assert_eq!(r.check(), Ok(()), "{g:?}");
            assert_eq!(r.reset_vector(), 0x75ff_fff0);
            assert!(CACHE_WINDOW.contains(&r.dram()));
            let ffs = ffs_window(g);
            assert!(ffs.size > 0x40_0000, "{g:?} FFS window {ffs:x?}");
        }
        // The other vendor geometries seen on donors are valid too.
        for (flash, size, dest) in [
            (0xde_0000, 0x22_0000, 0x75de_0000),
            (0xd0_0000, 0x30_0000, 0x75d0_0000),
        ] {
            let r = ResetGeometry {
                flash_offset: flash,
                size,
                destination: dest,
            };
            assert_eq!(r.check(), Ok(()));
        }
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
