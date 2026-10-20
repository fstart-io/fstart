// SPDX-License-Identifier: MIT OR Apache-2.0
//! AMD SP3 (Naples, Rome, Milan) flash image: one 16 MiB half per
//! generation, and the 32 MiB dual image (FIRMWARE_IMAGE §1.2, §2.1, §4, §7
//! of the SP3 analysis).
//!
//! Written from the SP3 reverse engineering.
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
//! stays the vendor's unless the plan opts into rewriting it, in which case
//! every `0x62` entry (L1 and L2) gets the build's geometry and the
//! directory Fletcher-32 checksums are recomputed.
//!
//! Two halves form the 32 MiB dual image. The PSP boot ROM of each
//! generation searches the lower half first (Naples only the lower one) and
//! takes the first EFS whose generation word it accepts, so the order is
//! checked by simulating that rule for every half's generation.
//!
//! The PSP copies the reset image to DRAM; it is not loaded from the FFS.
//! As an initial image it carries the stage's constant trust policy and the
//! FFS locator as patch sites, which [`patch_reset_image`] fills from the
//! assembled FFS.

use core::fmt;
use core::ops::Range;

use fstart_core::ffs::locator::{LOCATOR_SIZE, LocatorBlock};
use fstart_core::ffs::trust::{TRUST_SIZE, TrustBlock};
use fstart_driver_zen::psp::directory::{
    self, BiosEntry, DirError, Directories, Directory, Efs, Entry, PspEntry, bios, fletcher32, psp,
};
use serde::{Deserialize, Serialize};

pub use fstart_driver_zen::psp::directory::HALF_SIZE;
pub use fstart_driver_zen::soc::Generation;

use crate::build_plan::{BuildPlan, FfsAssembly, UnitOutput};

/// EFS generation words of the three generations (FIRMWARE_IMAGE §1.1).
pub const fn generation_word(generation: Generation) -> u32 {
    match generation {
        Generation::Naples => 0xffff_ffff,
        Generation::Rome => 0xffff_fffe,
        Generation::Milan => 0xffff_fffc,
    }
}

/// The generation an EFS word belongs to (exact words only).
pub fn generation_of(word: u32) -> Result<Generation, ImageError> {
    [Generation::Naples, Generation::Rome, Generation::Milan]
        .into_iter()
        .find(|&g| generation_word(g) == word)
        .ok_or(ImageError::UnknownGeneration(word))
}

/// Which 16 MiB half of a 32 MiB dual-image flash a build occupies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FlashHalf {
    Lower,
    Upper,
}

impl FlashHalf {
    pub const fn offset(self) -> usize {
        match self {
            Self::Lower => 0,
            Self::Upper => HALF_SIZE,
        }
    }

    fn range(self) -> Range<usize> {
        self.offset()..self.offset() + HALF_SIZE
    }
}

/// The `0x62` slot: where the reset image sits in the half and where the
/// PSP copies it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResetSlot {
    pub flash_offset: u32,
    pub size: u32,
    pub destination: u64,
}

impl ResetSlot {
    /// The image-side rules: a nonempty slot ending at the top of the half,
    /// because the vendor-style 16-bit entry may run from the flash copy
    /// (FIRMWARE_IMAGE §5). DRAM placement is the platform's business.
    pub fn check(&self) -> Result<(), ImageError> {
        let end = self.flash_offset as usize + self.size as usize;
        if self.size < 0x1000 || end != HALF_SIZE {
            return Err(ImageError::InvalidSlot(*self));
        }
        Ok(())
    }

    fn range(&self) -> Range<usize> {
        self.flash_offset as usize..HALF_SIZE
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
    InvalidSlot(ResetSlot),
    /// The vendor's `0x62` geometry differs from the build's and the plan
    /// did not allow rewriting it.
    GeometryMismatch {
        vendor: ResetSlot,
        build: ResetSlot,
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
    /// Trust or locator patching of the reset image failed.
    Patch(&'static str),
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

impl From<ImageError> for String {
    fn from(e: ImageError) -> Self {
        e.to_string()
    }
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

/// A parsed vendor half.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VendorLayout {
    pub generation: Generation,
    /// The `0x62` slot (identical in L1 and L2).
    pub reset: ResetSlot,
    /// End of everything the PSP/ABL reference: directories (including
    /// their reserved `max_size`) and every entry's data except `0x62`.
    pub prefix_end: usize,
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
    /// (checksums verified), the `0x62` slot and the prefix end.
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
        let reset = ResetSlot {
            flash_offset: range.start as u32,
            size: range.len() as u32,
            destination,
        };
        reset.check()?;

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

/// The vendor half a build uses: the image itself if it is one 16 MiB half,
/// else `half` of a 32 MiB dual image.
pub fn select_half(image: &[u8], half: FlashHalf) -> Result<&[u8], ImageError> {
    match image.len() {
        HALF_SIZE => Ok(image),
        n if n == 2 * HALF_SIZE => Ok(&image[half.range()]),
        actual => Err(ImageError::Size {
            expected: 2 * HALF_SIZE,
            actual,
        }),
    }
}

/// How one half is composed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct HalfPlan {
    #[serde(with = "generation_name")]
    pub generation: Generation,
    /// The slot the reset image was linked for.
    pub reset: ResetSlot,
    /// Flash offset of the FFS (start of the runtime firmware window).
    pub ffs_offset: u32,
    /// Rewrite the vendor's `0x62` entries if their geometry differs.
    pub rewrite_reset_geometry: bool,
}

impl HalfPlan {
    /// Check a vendor half against the plan without composing: generation,
    /// `0x62` geometry (or the rewrite opt-in) and room for the FFS above
    /// the vendor prefix.
    pub fn check_vendor(&self, vendor: &[u8]) -> Result<VendorLayout, ImageError> {
        let layout = VendorLayout::parse(vendor)?;
        if layout.generation != self.generation {
            return Err(ImageError::WrongGeneration {
                expected: self.generation,
                found: layout.generation,
            });
        }
        self.reset.check()?;
        if layout.reset != self.reset && !self.rewrite_reset_geometry {
            return Err(ImageError::GeometryMismatch {
                vendor: layout.reset,
                build: self.reset,
            });
        }
        if layout.prefix_end > self.ffs_offset as usize {
            return Err(ImageError::PrefixOverlap {
                prefix_end: layout.prefix_end,
                limit: self.ffs_offset as usize,
            });
        }
        if self.ffs_offset > self.reset.flash_offset {
            return Err(ImageError::PrefixOverlap {
                prefix_end: self.ffs_offset as usize,
                limit: self.reset.flash_offset as usize,
            });
        }
        Ok(layout)
    }

    /// FFS capacity: from the FFS offset up to the reset image.
    pub fn ffs_capacity(&self) -> usize {
        (self.reset.flash_offset as usize).saturating_sub(self.ffs_offset as usize)
    }
}

/// Compose one half: vendor prefix, FFS, reset image, `0x62` entries.
/// `reset_image` must already be patched ([`patch_reset_image`]). The
/// result is re-parsed: its directories verify and point at the new reset
/// image.
pub fn compose_half(
    vendor: &[u8],
    plan: &HalfPlan,
    reset_image: &[u8],
    ffs: &[u8],
) -> Result<Vec<u8>, ImageError> {
    let layout = plan.check_vendor(vendor)?;
    if reset_image.len() != plan.reset.size as usize {
        return Err(ImageError::ResetImage("not exactly the 0x62 slot"));
    }
    let vector = &reset_image[reset_image.len() - 16..];
    if vector.iter().all(|&b| b == 0) || vector.iter().all(|&b| b == 0xff) {
        return Err(ImageError::ResetImage(
            "no reset vector in the last 16 bytes",
        ));
    }
    let capacity = plan.ffs_capacity();
    if ffs.len() > capacity {
        return Err(ImageError::FfsTooLarge {
            size: ffs.len(),
            capacity,
        });
    }

    let ffs_offset = plan.ffs_offset as usize;
    let mut out = vec![0xff; HALF_SIZE];
    out[..layout.prefix_end].copy_from_slice(&vendor[..layout.prefix_end]);
    out[ffs_offset..ffs_offset + ffs.len()].copy_from_slice(ffs);
    out[plan.reset.range()].copy_from_slice(reset_image);
    if layout.reset != plan.reset {
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

/// Point every `0x62` entry at `slot` and fix the checksums of the
/// directories that hold one. The source keeps the vendor's encoding: an
/// MMIO address on gen1 (Naples), an offset with the entry's address-mode
/// bits 63:62 on gen2.
fn rewrite_reset_entries(half: &mut [u8], slot: ResetSlot) -> Result<(), ImageError> {
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
        let offset = u64::from(slot.flash_offset);
        let source = if gen1 {
            directory::MMIO_BASE + offset
        } else {
            source & (3 << 62) | offset
        };
        half[at + 4..at + 8].copy_from_slice(&slot.size.to_le_bytes());
        half[at + 8..at + 16].copy_from_slice(&source.to_le_bytes());
        half[at + 16..at + 24].copy_from_slice(&slot.destination.to_le_bytes());
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
            .get(h.range())
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
            Efs::new(&image[half.range()])
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
/// composed half, keeping the other byte-identical, and check the boot
/// order.
pub fn replace_half(image: &[u8], half: FlashHalf, composed: &[u8]) -> Result<Vec<u8>, ImageError> {
    exact_size(image, 2 * HALF_SIZE)?;
    exact_size(composed, HALF_SIZE)?;
    let mut out = image.to_vec();
    out[half.range()].copy_from_slice(composed);
    check_dual(&out)?;
    Ok(out)
}

/// What [`patch_reset_image`] filled in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Patched {
    /// The stage's constant trust policy (expected keys, image family).
    pub trust: bool,
    /// The FFS locator; without it the reset image cannot find its FFS.
    pub locator: bool,
}

/// Offsets (8-byte aligned, as the patch sites are) where `needle` occurs.
fn sites(haystack: &[u8], needle: &[u8]) -> Vec<usize> {
    (0..haystack.len().saturating_sub(needle.len() - 1))
        .step_by(8)
        .filter(|&at| haystack[at..at + needle.len()] == *needle)
        .collect()
}

/// The single patch site of `placeholder`, if any.
fn site(haystack: &[u8], placeholder: &[u8]) -> Result<Option<usize>, ImageError> {
    match sites(haystack, placeholder)[..] {
        [] => Ok(None),
        [at] => Ok(Some(at)),
        _ => Err(ImageError::Patch("more than one patch site")),
    }
}

/// The FFS's own locator: the block whose image size is the FFS and whose
/// anchor offset is where it sits.
fn ffs_locator(ffs: &[u8]) -> Option<&[u8]> {
    (0..ffs.len().saturating_sub(LOCATOR_SIZE - 1))
        .step_by(8)
        .map(|at| (at, &ffs[at..at + LOCATOR_SIZE]))
        .find(|(at, bytes)| {
            LocatorBlock::parse(bytes).is_some_and(|l| {
                l.total_image_size.get() as usize == ffs.len()
                    && l.anchor_offset.get() as usize == *at
            })
        })
        .map(|(_, bytes)| bytes)
}

/// The trust policy the FFS builder finalized into an FFS-resident copy of
/// a stage (the reset unit's FFS file).
fn ffs_trust(ffs: &[u8], placeholder: &[u8]) -> Option<[u8; TRUST_SIZE]> {
    (0..ffs.len().saturating_sub(TRUST_SIZE - 1))
        .step_by(8)
        .map(|at| &ffs[at..at + TRUST_SIZE])
        .filter(|bytes| *bytes != placeholder)
        .find_map(|bytes| TrustBlock::parse(bytes).filter(|t| t.key_count.get() > 0))
        .map(|t| {
            let mut out = [0; TRUST_SIZE];
            t.write_to(&mut out);
            out
        })
}

/// Fill the reset image's trust and locator patch sites from the assembled
/// FFS (the FFS sits at the start of the runtime firmware window, so its
/// locator is valid as is). A site the stage does not link is skipped; a
/// site with no FFS counterpart is an error.
pub fn patch_reset_image(reset: &mut [u8], ffs: &[u8]) -> Result<Patched, ImageError> {
    let mut trust_placeholder = [0; TRUST_SIZE];
    TrustBlock::placeholder().write_to(&mut trust_placeholder);
    let mut locator_placeholder = [0; LOCATOR_SIZE];
    LocatorBlock::placeholder().write_to(&mut locator_placeholder);

    let trust = site(reset, &trust_placeholder)?
        .map(|at| {
            let trust = ffs_trust(ffs, &trust_placeholder)
                .ok_or(ImageError::Patch("FFS carries no finalized trust policy"))?;
            reset[at..at + TRUST_SIZE].copy_from_slice(&trust);
            Ok::<_, ImageError>(())
        })
        .transpose()?
        .is_some();
    let locator = site(reset, &locator_placeholder)?
        .map(|at| {
            let locator = ffs_locator(ffs).ok_or(ImageError::Patch("FFS has no locator"))?;
            reset[at..at + LOCATOR_SIZE].copy_from_slice(locator);
            Ok::<_, ImageError>(())
        })
        .transpose()?
        .is_some();
    Ok(Patched { trust, locator })
}

/// The flash images of one build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sp3Flash {
    /// The composed 16 MiB half.
    pub half: Vec<u8>,
    /// The 32 MiB image: the vendor dual ROM with only this build's half
    /// replaced (when the vendor ROM is 32 MiB).
    pub dual: Option<Vec<u8>>,
    pub patched: Patched,
}

/// Compose a build: patch the reset image from the FFS, compose the half
/// from the vendor ROM's `half` and, for a 32 MiB vendor ROM, put it back
/// into that ROM.
pub fn compose(
    vendor_rom: &[u8],
    half: FlashHalf,
    plan: &HalfPlan,
    reset_image: &[u8],
    ffs: &[u8],
) -> Result<Sp3Flash, ImageError> {
    let mut reset = reset_image.to_vec();
    let patched = patch_reset_image(&mut reset, ffs)?;
    let composed = compose_half(select_half(vendor_rom, half)?, plan, &reset, ffs)?;
    let dual = (vendor_rom.len() == 2 * HALF_SIZE)
        .then(|| replace_half(vendor_rom, half, &composed))
        .transpose()?;
    Ok(Sp3Flash {
        half: composed,
        dual,
        patched,
    })
}

/// The SP3 image operation: an FFS (the payload and the reset unit's
/// trust-finalized copy) plus the reset unit, composed into a vendor
/// flash half.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Sp3FlashAssembly {
    pub ffs: FfsAssembly,
    /// Vendor ROM: one 16 MiB half or the 32 MiB dual image, workspace
    /// relative or absolute. Its prefix (EFS, PSP/BIOS directories, blobs,
    /// APCB) is kept byte-identical.
    pub vendor_image: String,
    /// Half this build occupies (and takes from a 32 MiB vendor ROM).
    pub half: FlashHalf,
    /// Unit whose flat output is the `0x62` image.
    pub reset_unit: String,
    pub compose: HalfPlan,
}

impl Sp3FlashAssembly {
    /// The reset unit is the plan's first stage and is linked for exactly
    /// the `0x62` slot: stored and run at the destination, entered at its
    /// last 16 bytes.
    pub fn validate(&self, plan: &BuildPlan) -> Result<(), String> {
        let slot = self.compose.reset;
        slot.check()?;
        if self.vendor_image.is_empty() || self.vendor_image.contains('\0') {
            return Err("invalid SP3 vendor image path".into());
        }
        if self.compose.ffs_capacity() == 0 {
            return Err("SP3 FFS window is empty".into());
        }
        if plan.stages.first() != Some(&self.reset_unit) {
            return Err("SP3 reset unit must be the first image stage".into());
        }
        let UnitOutput::Executable {
            expectations,
            load_address,
            flat_capacity,
        } = &plan.unit(&self.reset_unit)?.output
        else {
            return Err("SP3 reset producer is not executable".into());
        };
        let size = u64::from(slot.size);
        if *load_address != slot.destination
            || *flat_capacity != size
            || !matches!(expectations.architecture, crate::elf::Architecture::X86_64)
            || expectations.stored.first().map(|s| (s.base, s.size))
                != Some((slot.destination, size))
            || expectations.entry != Some(slot.destination + size - 16)
        {
            return Err("SP3 reset producer differs from the 0x62 slot".into());
        }
        Ok(())
    }
}

/// `Generation` as its lower-case name in plans.
mod generation_name {
    use super::Generation;
    use serde::{Deserialize, Deserializer, Serializer, de::Error};

    const ALL: [Generation; 3] = [Generation::Naples, Generation::Rome, Generation::Milan];

    pub fn serialize<S: Serializer>(g: &Generation, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&g.name().to_ascii_lowercase())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Generation, D::Error> {
        let name = String::deserialize(d)?;
        ALL.into_iter()
            .find(|g| g.name().eq_ignore_ascii_case(&name))
            .ok_or_else(|| D::Error::custom(format!("unknown SP3 generation {name}")))
    }
}

#[cfg(test)]
#[path = "sp3_flash_tests.rs"]
mod tests;
