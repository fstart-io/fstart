//! SP3 flash image composition (FIRMWARE_IMAGE §1.2, §2.1, §4, §7).
//!
//! One 16 MiB half per generation:
//!
//! ```text
//! [0, prefix_end)                 vendor bytes, byte-identical (EFS at 0x20000,
//!                                 PSP/BIOS directories, PSP blobs, APCB,
//!                                 AMI NVRAM; everything the PSP/ABL read)
//! [prefix_end, ffs_offset)        erased (0xff)
//! [ffs_offset, reset offset)      fstart FFS, then erased
//! [reset offset, 16 MiB)          fstart reset image (BIOS directory 0x62)
//! ```
//!
//! The vendor AMI FV_DATA/FV_MAIN/FV_BB are dropped. The `0x62` geometry
//! stays the vendor's unless the board opts into rewriting it, in which
//! case every `0x62` entry (L1 and L2) gets the build's geometry and the
//! directory Fletcher-32 checksums are recomputed.
//!
//! Two halves form the 32 MiB dual image. The PSP boot ROM of each
//! generation searches the lower half first (Naples only the lower one) and
//! takes the first EFS whose generation word it accepts, so the order is
//! checked by simulating that rule for every half's generation.

use core::fmt;
use core::ops::Range;
use std::vec::Vec;

use fstart_driver_zen::psp::directory::{
    BiosEntry, DirError, Directories, Directory, Efs, Entry, HALF_SIZE, PspEntry, bios, fletcher32,
    psp,
};
use fstart_driver_zen::soc::Generation;

use crate::layout::{FlashHalf, GeometryError, ResetGeometry};

/// EFS generation words of the three generations (FIRMWARE_IMAGE §1.1).
pub const fn generation_word(generation: Generation) -> u32 {
    match generation {
        Generation::Naples => 0xffff_ffff,
        Generation::Rome => 0xffff_fffe,
        Generation::Milan => 0xffff_fffc,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImageError {
    /// A half must be exactly 16 MiB (a full image 32 MiB).
    Size {
        expected: usize,
        actual: usize,
    },
    Directory(DirError),
    /// The EFS generation word is none of the three.
    UnknownGeneration(u32),
    /// The half is for another generation than the build.
    WrongGeneration {
        expected: Generation,
        found: Generation,
    },
    /// The `0x62` entries are missing, inconsistent or not a copied reset
    /// image.
    ResetEntry(&'static str),
    InvalidGeometry(GeometryError),
    /// The vendor's `0x62` geometry differs from the build's and the board
    /// did not allow rewriting it.
    GeometryMismatch {
        vendor: ResetGeometry,
        build: ResetGeometry,
    },
    /// The vendor prefix reaches into the FFS or reset image.
    PrefixOverlap {
        prefix_end: usize,
        limit: usize,
    },
    FfsTooLarge {
        size: usize,
        capacity: usize,
    },
    /// The reset image is not a full slot or has no reset vector.
    ResetImage(&'static str),
    /// A generation would boot the other half (or none).
    BootOrder {
        half: FlashHalf,
        generation: Generation,
        booted: Option<FlashHalf>,
    },
}

impl From<DirError> for ImageError {
    fn from(e: DirError) -> Self {
        Self::Directory(e)
    }
}

impl fmt::Display for ImageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SP3 flash image: {self:x?}")
    }
}

impl std::error::Error for ImageError {}

/// The generation an EFS word belongs to (exact words only).
pub fn generation_of(word: u32) -> Result<Generation, ImageError> {
    [Generation::Naples, Generation::Rome, Generation::Milan]
        .into_iter()
        .find(|&g| generation_word(g) == word)
        .ok_or(ImageError::UnknownGeneration(word))
}

/// A parsed vendor half.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VendorLayout {
    pub generation: Generation,
    /// The `0x62` geometry (identical in L1 and L2).
    pub reset: ResetGeometry,
    /// End of everything the PSP/ABL reference: directories (including
    /// their reserved `max_size`) and every entry's data except `0x62`.
    pub prefix_end: usize,
}

fn exact_size(bytes: &[u8], expected: usize) -> Result<(), ImageError> {
    if bytes.len() == expected {
        Ok(())
    } else {
        Err(ImageError::Size {
            expected,
            actual: bytes.len(),
        })
    }
}

/// End of a directory's table, or of its reserved area (gen2 header
/// additional info bits 9:0, 4 KiB units).
fn directory_end<E: Entry>(image: &[u8], dir: &Directory<'_, E>, gen1: bool) -> usize {
    let table = dir.offset() + 16 + core::mem::size_of_val(dir.entries());
    let info = u32::from_le_bytes(
        image[dir.offset() + 12..dir.offset() + 16]
            .try_into()
            .unwrap(),
    );
    let reserved = if gen1 {
        0
    } else {
        dir.offset() + ((info & 0x3ff) as usize) * 0x1000
    };
    table.max(reserved)
}

/// Data range of an entry that has flash data, `None` for value entries.
fn data_range<E: Entry>(
    dir: &Directory<'_, E>,
    entry: &E,
) -> Result<Option<Range<usize>>, ImageError> {
    match dir.range(entry) {
        Ok(range) => Ok(Some(range)),
        Err(DirError::NoData) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

fn prefix_end_of<E: Entry>(
    image: &[u8],
    dirs: &Directories<'_, E>,
    gen1: bool,
    skip: impl Fn(&E) -> bool,
) -> Result<usize, ImageError> {
    dirs.levels().try_fold(0, |end, dir| {
        let data = dir
            .entries()
            .iter()
            .filter(|e| !skip(e))
            .map(|e| data_range(dir, e))
            .try_fold(0, |end, r| r.map(|r| r.map_or(end, |r| end.max(r.end))))?;
        Ok(end.max(data).max(directory_end(image, dir, gen1)))
    })
}

/// Every `0x62` entry with its byte offset in the half.
fn reset_entries<'a>(
    dirs: &'a Directories<'a, BiosEntry>,
) -> impl Iterator<Item = (usize, &'a Directory<'a, BiosEntry>, &'a BiosEntry)> + 'a {
    dirs.levels().flat_map(|dir| {
        dir.entries()
            .iter()
            .enumerate()
            .filter(|(_, e)| e.kind() == bios::RESET_IMAGE)
            .map(move |(i, e)| {
                (
                    dir.offset() + 16 + i * core::mem::size_of::<BiosEntry>(),
                    dir,
                    e,
                )
            })
    })
}

impl VendorLayout {
    /// Parse one 16 MiB half: EFS generation, BIOS and PSP directories
    /// (checksums verified), the `0x62` geometry and the prefix end.
    pub fn parse(half: &[u8]) -> Result<Self, ImageError> {
        exact_size(half, HALF_SIZE)?;
        let efs = Efs::new(half)?;
        let generation = generation_of(efs.generation_word())?;
        let gen1 = efs.is_gen1();
        let bios_dirs = efs.bios_directories(generation)?;
        let psp_dirs = efs.psp_directories()?;

        let resets = reset_entries(&bios_dirs)
            .map(|(_, dir, e)| {
                let range = dir.range(e)?;
                Ok((
                    range,
                    e.destination(),
                    e.reset() && e.copy() && !e.compressed(),
                ))
            })
            .collect::<Result<Vec<_>, ImageError>>()?;
        let (range, destination, _) = resets
            .first()
            .cloned()
            .ok_or(ImageError::ResetEntry("no 0x62 entry"))?;
        if resets
            .iter()
            .any(|(r, d, _)| *r != range || *d != destination)
        {
            return Err(ImageError::ResetEntry("L1 and L2 0x62 entries differ"));
        }
        if resets.iter().any(|(_, _, flags)| !flags) {
            return Err(ImageError::ResetEntry(
                "0x62 is not an uncompressed reset+copy image",
            ));
        }
        let reset = ResetGeometry {
            flash_offset: range.start as u32,
            size: range.len() as u32,
            destination,
        };
        reset.check().map_err(ImageError::InvalidGeometry)?;

        let bios_end = prefix_end_of(half, &bios_dirs, gen1, |e: &BiosEntry| {
            matches!(e.kind(), bios::RESET_IMAGE | bios::APOB)
        })?;
        let psp_end = prefix_end_of(half, &psp_dirs, gen1, |e: &PspEntry| {
            e.kind() == psp::SOFT_FUSE
        })?;
        let prefix_end = bios_end.max(psp_end);
        if prefix_end > reset.flash_offset as usize {
            return Err(ImageError::PrefixOverlap {
                prefix_end,
                limit: reset.flash_offset as usize,
            });
        }
        Ok(Self {
            generation,
            reset,
            prefix_end,
        })
    }
}

/// The vendor half a board variant uses: the image itself if it is one
/// 16 MiB half, else `half` of a 32 MiB dual image.
pub fn select_half(image: &[u8], half: FlashHalf) -> Result<&[u8], ImageError> {
    match image.len() {
        HALF_SIZE => Ok(image),
        n if n == 2 * HALF_SIZE => Ok(&image[half.offset()..half.offset() + HALF_SIZE]),
        actual => Err(ImageError::Size {
            expected: 2 * HALF_SIZE,
            actual,
        }),
    }
}

/// How one half is composed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HalfPlan {
    pub generation: Generation,
    /// The geometry the reset image was linked for.
    pub reset: ResetGeometry,
    /// Flash offset of the FFS.
    pub ffs_offset: usize,
    /// Rewrite the vendor's `0x62` entries if their geometry differs.
    pub rewrite_reset_geometry: bool,
}

/// Compose one half: vendor prefix, FFS, reset image, `0x62` entries.
/// The result is re-parsed: its directories verify and point at the new
/// reset image.
pub fn compose_half(
    vendor: &[u8],
    plan: &HalfPlan,
    reset_image: &[u8],
    ffs: &[u8],
) -> Result<Vec<u8>, ImageError> {
    let layout = VendorLayout::parse(vendor)?;
    if layout.generation != plan.generation {
        return Err(ImageError::WrongGeneration {
            expected: plan.generation,
            found: layout.generation,
        });
    }
    plan.reset.check().map_err(ImageError::InvalidGeometry)?;
    let rewrite = layout.reset != plan.reset;
    if rewrite && !plan.rewrite_reset_geometry {
        return Err(ImageError::GeometryMismatch {
            vendor: layout.reset,
            build: plan.reset,
        });
    }
    if reset_image.len() != plan.reset.size as usize {
        return Err(ImageError::ResetImage("not exactly the 0x62 slot"));
    }
    let vector = &reset_image[reset_image.len() - 16..];
    if vector.iter().all(|&b| b == 0) || vector.iter().all(|&b| b == 0xff) {
        return Err(ImageError::ResetImage(
            "no reset vector in the last 16 bytes",
        ));
    }
    let reset_offset = plan.reset.flash_offset as usize;
    if layout.prefix_end > plan.ffs_offset {
        return Err(ImageError::PrefixOverlap {
            prefix_end: layout.prefix_end,
            limit: plan.ffs_offset,
        });
    }
    let capacity = reset_offset.saturating_sub(plan.ffs_offset);
    if ffs.len() > capacity {
        return Err(ImageError::FfsTooLarge {
            size: ffs.len(),
            capacity,
        });
    }

    let mut out = std::vec![0xff; HALF_SIZE];
    out[..layout.prefix_end].copy_from_slice(&vendor[..layout.prefix_end]);
    out[plan.ffs_offset..plan.ffs_offset + ffs.len()].copy_from_slice(ffs);
    out[reset_offset..].copy_from_slice(reset_image);
    if rewrite {
        rewrite_reset_entries(&mut out, plan.reset)?;
    }

    let composed = VendorLayout::parse(&out)?;
    if composed.reset != plan.reset || composed.prefix_end != layout.prefix_end {
        return Err(ImageError::ResetEntry(
            "composed directories do not describe the image",
        ));
    }
    Ok(out)
}

/// Point every `0x62` entry at `geometry` and fix the checksums of the
/// directories that hold one. The source keeps the vendor's encoding: an
/// MMIO address on gen1 (Naples), an offset with the entry's address-mode
/// bits 63:62 on gen2.
fn rewrite_reset_entries(half: &mut [u8], geometry: ResetGeometry) -> Result<(), ImageError> {
    let (gen1, patches) = {
        let efs = Efs::new(half)?;
        let generation = generation_of(efs.generation_word())?;
        let dirs = efs.bios_directories(generation)?;
        let patches = reset_entries(&dirs)
            .map(|(at, dir, e)| (at, dir.offset(), dir.entries().len(), e.source()))
            .collect::<Vec<_>>();
        (efs.is_gen1(), patches)
    };
    for &(at, _, _, source) in &patches {
        let offset = u64::from(geometry.flash_offset);
        let source = if gen1 {
            fstart_driver_zen::psp::directory::MMIO_BASE + offset
        } else {
            source & (3 << 62) | offset
        };
        half[at + 4..at + 8].copy_from_slice(&geometry.size.to_le_bytes());
        half[at + 8..at + 16].copy_from_slice(&source.to_le_bytes());
        half[at + 16..at + 24].copy_from_slice(&geometry.destination.to_le_bytes());
    }
    for &(_, dir, count, _) in &patches {
        let body = dir + 8..dir + 16 + count * core::mem::size_of::<BiosEntry>();
        let sum = fletcher32(&half[body]);
        half[dir + 4..dir + 8].copy_from_slice(&sum.to_le_bytes());
    }
    Ok(())
}

/// The half a generation's PSP boot ROM boots from a 32 MiB image: the
/// first (lower, then upper; Naples only lower) whose EFS it accepts.
pub fn booted_half(image: &[u8], generation: Generation) -> Option<FlashHalf> {
    let halves: &[FlashHalf] = match generation {
        Generation::Naples => &[FlashHalf::Lower],
        Generation::Rome | Generation::Milan => &[FlashHalf::Lower, FlashHalf::Upper],
    };
    halves.iter().copied().find(|h| {
        image
            .get(h.offset()..h.offset() + HALF_SIZE)
            .and_then(|half| Efs::new(half).ok())
            .is_some_and(|efs| efs.matches(generation))
    })
}

/// Check a 32 MiB image: every half with an EFS is booted by its own
/// generation (so Rome must sit below Milan, Naples below Rome).
pub fn check_dual(image: &[u8]) -> Result<(), ImageError> {
    exact_size(image, 2 * HALF_SIZE)?;
    [FlashHalf::Lower, FlashHalf::Upper]
        .into_iter()
        .filter_map(|half| {
            Efs::new(&image[half.offset()..half.offset() + HALF_SIZE])
                .ok()
                .map(|efs| (half, efs.generation_word()))
        })
        .try_for_each(|(half, word)| {
            let generation = generation_of(word)?;
            let booted = booted_half(image, generation);
            if booted == Some(half) {
                Ok(())
            } else {
                Err(ImageError::BootOrder {
                    half,
                    generation,
                    booted,
                })
            }
        })
}

/// The 32 MiB dual image of two composed halves.
pub fn compose_dual(lower: &[u8], upper: &[u8]) -> Result<Vec<u8>, ImageError> {
    exact_size(lower, HALF_SIZE)?;
    exact_size(upper, HALF_SIZE)?;
    let image = [lower, upper].concat();
    check_dual(&image)?;
    Ok(image)
}

/// Replace one half of a 32 MiB image (e.g. the vendor dual ROM) with a
/// composed half, keeping the other, and check the boot order.
pub fn replace_half(image: &[u8], half: FlashHalf, composed: &[u8]) -> Result<Vec<u8>, ImageError> {
    exact_size(image, 2 * HALF_SIZE)?;
    exact_size(composed, HALF_SIZE)?;
    let mut out = image.to_vec();
    out[half.offset()..half.offset() + HALF_SIZE].copy_from_slice(composed);
    check_dual(&out)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{MILAN_RESET, ROME_RESET, ffs_offset};
    use std::path::Path;

    /// A minimal gen2 half: EFS, `$BHD` with APOB and 0x62, `$PSP` with a
    /// blob and a soft fuse value entry.
    fn synthetic(generation: Generation, reset: ResetGeometry) -> Vec<u8> {
        let mut h = std::vec![0xff; HALF_SIZE];
        let put32 =
            |h: &mut Vec<u8>, at: usize, v: u32| h[at..at + 4].copy_from_slice(&v.to_le_bytes());
        let put64 =
            |h: &mut Vec<u8>, at: usize, v: u64| h[at..at + 8].copy_from_slice(&v.to_le_bytes());
        let (psp_dir, bios_dir) = (0x10_0000usize, 0x20_0000usize);
        put32(&mut h, 0x2_0000, 0x55aa_55aa);
        put32(&mut h, 0x2_0014, psp_dir as u32);
        put32(&mut h, 0x2_0024, generation_word(generation));
        let bios_ptr = match generation {
            Generation::Naples => unreachable!(),
            Generation::Rome => 0x2_0020,
            Generation::Milan => 0x2_0028,
        };
        put32(&mut h, bios_ptr, bios_dir as u32);
        // $PSP: blob 0x01 at 0x11_0000 (0x8000 B), soft fuse value.
        h[psp_dir..psp_dir + 4].copy_from_slice(b"$PSP");
        put32(&mut h, psp_dir + 8, 2);
        put32(&mut h, psp_dir + 12, 0x10); // 64 KiB reserved
        let e = psp_dir + 16;
        h[e] = 0x01;
        put32(&mut h, e + 4, 0x8000);
        put64(&mut h, e + 8, 0x11_0000);
        h[e + 16] = 0x0b;
        put32(&mut h, e + 16 + 4, 0xffff_ffff);
        put64(&mut h, e + 16 + 8, 1);
        let sum = fletcher32(&h[psp_dir + 8..psp_dir + 16 + 32]);
        put32(&mut h, psp_dir + 4, sum);
        // $BHD: APOB value entry, reset image.
        h[bios_dir..bios_dir + 4].copy_from_slice(b"$BHD");
        put32(&mut h, bios_dir + 8, 2);
        put32(&mut h, bios_dir + 12, 0x1 | 1 << 29);
        let e = bios_dir + 16;
        h[e] = bios::APOB;
        put32(&mut h, e + 4, 0);
        put64(&mut h, e + 8, 0);
        put64(&mut h, e + 16, 0x3000_0000);
        let e = e + 24;
        h[e] = bios::RESET_IMAGE;
        h[e + 2] = 0x3; // reset + copy
        put32(&mut h, e + 4, reset.size);
        put64(&mut h, e + 8, 1 << 62 | u64::from(reset.flash_offset));
        put64(&mut h, e + 16, reset.destination);
        let sum = fletcher32(&h[bios_dir + 8..bios_dir + 16 + 48]);
        put32(&mut h, bios_dir + 4, sum);
        h[0x11_0000..0x11_8000].fill(0x5a);
        h
    }

    fn reset_image(geometry: ResetGeometry) -> Vec<u8> {
        let mut r = std::vec![0u8; geometry.size as usize];
        let n = r.len();
        r[n - 16..n - 12].copy_from_slice(&[0xfa, 0xe9, 0x00, 0x00]);
        r
    }

    fn plan(generation: Generation, reset: ResetGeometry) -> HalfPlan {
        HalfPlan {
            generation,
            reset,
            ffs_offset: ffs_offset(generation) as usize,
            rewrite_reset_geometry: false,
        }
    }

    #[test]
    fn synthetic_half_prefix_geometry_and_composition() {
        let vendor = synthetic(Generation::Milan, MILAN_RESET);
        let layout = VendorLayout::parse(&vendor).unwrap();
        assert_eq!(layout.reset, MILAN_RESET);
        // The BIOS directory reserves 4 KiB (its max_size), beyond the PSP
        // blob at [0x110000, 0x118000).
        assert_eq!(layout.prefix_end, 0x20_1000);

        let ffs = std::vec![0xa5; 0x1000];
        let out = compose_half(
            &vendor,
            &plan(Generation::Milan, MILAN_RESET),
            &reset_image(MILAN_RESET),
            &ffs,
        )
        .unwrap();
        assert_eq!(out[..layout.prefix_end], vendor[..layout.prefix_end]);
        assert!(out[layout.prefix_end..0x60_0000].iter().all(|&b| b == 0xff));
        assert_eq!(out[0x60_0000..0x60_1000], ffs[..]);
        assert_eq!(out[HALF_SIZE - 16], 0xfa);
        // Unchanged geometry: directories are byte-identical.
        assert_eq!(out[0x20_0000..0x20_0040], vendor[0x20_0000..0x20_0040]);

        assert!(matches!(
            compose_half(
                &vendor,
                &plan(Generation::Rome, ROME_RESET),
                &reset_image(ROME_RESET),
                &ffs
            ),
            Err(ImageError::WrongGeneration { .. })
        ));
        let big = std::vec![0; MILAN_RESET.flash_offset as usize - 0x60_0000 + 1];
        assert!(matches!(
            compose_half(
                &vendor,
                &plan(Generation::Milan, MILAN_RESET),
                &reset_image(MILAN_RESET),
                &big
            ),
            Err(ImageError::FfsTooLarge { .. })
        ));
        assert!(matches!(
            compose_half(
                &vendor,
                &plan(Generation::Milan, MILAN_RESET),
                &std::vec![0; MILAN_RESET.size as usize],
                &ffs
            ),
            Err(ImageError::ResetImage(_))
        ));
    }

    #[test]
    fn geometry_rewrite_is_opt_in_and_fixes_checksums() {
        // Milan v4.00 geometry in the vendor half, v4.04 in the build.
        let old = ResetGeometry {
            flash_offset: 0xd0_0000,
            size: 0x30_0000,
            destination: 0x75d0_0000,
        };
        let vendor = synthetic(Generation::Milan, old);
        let mut p = plan(Generation::Milan, MILAN_RESET);
        assert_eq!(
            compose_half(&vendor, &p, &reset_image(MILAN_RESET), &[]),
            Err(ImageError::GeometryMismatch {
                vendor: old,
                build: MILAN_RESET
            })
        );
        p.rewrite_reset_geometry = true;
        let out = compose_half(&vendor, &p, &reset_image(MILAN_RESET), &[]).unwrap();
        let layout = VendorLayout::parse(&out).unwrap();
        assert_eq!(layout.reset, MILAN_RESET);
        // Address-mode bits of the source survive.
        let e = 0x20_0000 + 16 + 24;
        assert_eq!(
            u64::from_le_bytes(out[e + 8..e + 16].try_into().unwrap()) >> 62,
            1
        );
    }

    #[test]
    fn dual_image_order_follows_the_boot_rom_rule() {
        let rome = synthetic(Generation::Rome, ROME_RESET);
        let milan = synthetic(Generation::Milan, MILAN_RESET);
        let dual = compose_dual(&rome, &milan).unwrap();
        assert_eq!(booted_half(&dual, Generation::Rome), Some(FlashHalf::Lower));
        assert_eq!(
            booted_half(&dual, Generation::Milan),
            Some(FlashHalf::Upper)
        );
        // A Milan EFS below Rome would be taken by Rome's boot ROM.
        assert_eq!(
            compose_dual(&milan, &rome),
            Err(ImageError::BootOrder {
                half: FlashHalf::Upper,
                generation: Generation::Rome,
                booted: Some(FlashHalf::Lower)
            })
        );
        assert!(compose_dual(&rome, &rome).is_err());
        let erased = std::vec![0xff; HALF_SIZE];
        assert!(compose_dual(&rome, &erased).is_ok());
        assert!(replace_half(&dual, FlashHalf::Upper, &rome).is_err());
    }

    // ---- donor ROMs (skipped when the analysis workspace is absent) ----

    const DONORS: &str = "../../../sp3/firmware";

    fn donor(path: &str) -> Option<Vec<u8>> {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join(DONORS)
            .join(path);
        std::fs::read(&path).ok().or_else(|| {
            std::eprintln!("skipping: {} not present", path.display());
            None
        })
    }

    #[test]
    fn donor_halves_match_the_firmware_image_report() {
        // (donor, generation, reset geometry, prefix end) per FIRMWARE_IMAGE
        // §2.1/§4 and tools.build_firmware_image.
        let rows = [
            (
                "naples-s8026-v202.bin",
                Generation::Naples,
                (0xdb_0000, 0x25_0000, 0x75db_0000),
                0x27_3000,
            ),
            (
                "naples2p-knppd32-1801.bin",
                Generation::Naples,
                (0xde_0000, 0x22_0000, 0x75de_0000),
                0x4b_a000,
            ),
            (
                "rome-s8026-v202.bin",
                Generation::Rome,
                (0xdb_0000, 0x25_0000, 0x75db_0000),
                0x50_b000,
            ),
            (
                "rome-s8030-v404.bin",
                Generation::Rome,
                (0xdb_0000, 0x25_0000, 0x75db_0000),
                0x54_b000,
            ),
            (
                "rome2p-s8253-v203.bin",
                Generation::Rome,
                (0xdb_0000, 0x25_0000, 0x75db_0000),
                0x54_b000,
            ),
            (
                "milan-s8030-v404.bin",
                Generation::Milan,
                (0xce_0000, 0x32_0000, 0x75ce_0000),
                0x58_6000,
            ),
            (
                "milan2p-s8253-v203.bin",
                Generation::Milan,
                (0xce_0000, 0x32_0000, 0x75ce_0000),
                0x58_6000,
            ),
        ];
        for (name, generation, (flash_offset, size, destination), prefix_end) in rows {
            let Some(half) = donor(&std::format!("donors/{name}")) else {
                continue;
            };
            let layout = VendorLayout::parse(&half).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(layout.generation, generation, "{name}");
            assert_eq!(
                layout.reset,
                ResetGeometry {
                    flash_offset,
                    size,
                    destination
                },
                "{name}"
            );
            assert_eq!(layout.prefix_end, prefix_end, "{name}");
            assert!(prefix_end <= ffs_offset(generation) as usize, "{name}");
        }
    }

    #[test]
    fn donor_composition_keeps_the_prefix_and_dual_order() {
        let Some(rom) = donor("tyan-s8030gm2ne/S8030GM2NE_BIOS_V4.04/8030V404.ROM") else {
            return;
        };
        check_dual(&rom).unwrap();
        let rome = select_half(&rom, FlashHalf::Lower).unwrap();
        let milan = select_half(&rom, FlashHalf::Upper).unwrap();
        assert_eq!(
            VendorLayout::parse(rome).unwrap().generation,
            Generation::Rome
        );
        assert_eq!(
            VendorLayout::parse(milan).unwrap().generation,
            Generation::Milan
        );
        let swapped = [milan, rome].concat();
        assert!(matches!(
            check_dual(&swapped),
            Err(ImageError::BootOrder {
                generation: Generation::Rome,
                ..
            })
        ));

        let ffs = std::vec![0x11; 0x10_0000];
        let halves = [
            (rome, Generation::Rome, ROME_RESET),
            (milan, Generation::Milan, MILAN_RESET),
        ]
        .map(|(v, g, r)| {
            let out = compose_half(v, &plan(g, r), &reset_image(r), &ffs).unwrap();
            let prefix = VendorLayout::parse(v).unwrap().prefix_end;
            assert_eq!(out[..prefix], v[..prefix]);
            // The AMI multi-tank descriptor (Rome half, 0x21000) is in the prefix.
            assert_eq!(out[0x2_1000..0x2_1058], v[0x2_1000..0x2_1058]);
            out
        });
        let dual = compose_dual(&halves[0], &halves[1]).unwrap();
        assert_eq!(dual.len(), 2 * HALF_SIZE);
        // Replacing only the Milan half of the vendor ROM keeps AMI Rome.
        replace_half(&rom, FlashHalf::Upper, &halves[1]).unwrap();

        // Milan v4.00: different 0x62 geometry, refused unless rewritten.
        let Some(v400) = donor("tyan-s8030gm2ne/S8030GM2NE_bios_v400/8030V400.ROM") else {
            return;
        };
        check_dual(&v400).unwrap();
        let milan400 = select_half(&v400, FlashHalf::Upper).unwrap();
        assert!(matches!(
            compose_half(
                milan400,
                &plan(Generation::Milan, MILAN_RESET),
                &reset_image(MILAN_RESET),
                &ffs
            ),
            Err(ImageError::GeometryMismatch { .. })
        ));
    }

    #[test]
    fn donor_naples_rome_dual_image() {
        let Some(rom) = donor("tyan-s8026/x/8026V202.ROM") else {
            return;
        };
        check_dual(&rom).unwrap();
        assert_eq!(
            booted_half(&rom, Generation::Naples),
            Some(FlashHalf::Lower)
        );
        assert_eq!(booted_half(&rom, Generation::Rome), Some(FlashHalf::Upper));
        assert_eq!(booted_half(&rom, Generation::Milan), None);
    }
}
