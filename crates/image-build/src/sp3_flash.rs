// SPDX-License-Identifier: MIT OR Apache-2.0
//! AMD SP3 (Naples, Rome, Milan) flash image: one build composed into
//! every 16 MiB half of the vendor ROM whose generation the board supports
//! (FIRMWARE_IMAGE §1.2, §2.1, §4, §7 of the SP3 analysis).
//!
//! Written from the SP3 reverse engineering.
//!
//! ```text
//! [0, prefix_end)                 vendor bytes, byte-identical (EFS at 0x20000,
//!                                 PSP/BIOS directories, PSP blobs, APCB,
//!                                 AMI NVRAM; everything the PSP/ABL read)
//! [prefix_end, ffs_offset)        erased (0xff)
//! [ffs_offset, ffs_end)           fstart FFS, then erased; same offsets and
//!                                 bytes in every composed half
//! [slot, 16 MiB - image)          erased: the front of the vendor 0x62 slot
//! [16 MiB - image, 16 MiB)        fstart reset image, identical in every half
//! ```
//!
//! The vendor `0x62` slots of all SP3 donors end at the top of the half and
//! at the same DRAM address; only their start differs. The reset image is
//! linked once for the smallest slot and put at the end of each half's
//! vendor slot: the PSP copies the whole slot, so the image lands at its
//! link address whatever the slot size, and the vendor directories stay
//! byte-identical. `ffs_end` is the lowest slot start among the composed
//! halves, so one FFS (and one locator patched into the reset image) fits
//! every half.
//!
//! The PSP boot ROM of each generation searches the lower half first
//! (Naples only the lower one) and takes the first EFS whose generation
//! word it accepts, so a 32 MiB image is checked by simulating that rule
//! for every half's generation. Halves of a generation the board does not
//! support keep their vendor bytes.
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
    BiosEntry, DirError, Directories, Directory, Efs, Entry, PspEntry, bios, psp,
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

/// One 16 MiB half of a 32 MiB dual-image flash (a 16 MiB flash is its
/// lower half).
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

    pub fn range(self) -> Range<usize> {
        self.offset()..self.offset() + HALF_SIZE
    }
}

/// A `0x62` slot: where a reset image sits in the half and where the PSP
/// copies it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResetSlot {
    pub flash_offset: u32,
    pub size: u32,
    pub destination: u64,
}

impl ResetSlot {
    /// The image-side rules: a nonempty slot ending at the top of the half,
    /// because the vendor-style 16-bit entry may run from the flash copy
    /// (FIRMWARE_IMAGE §5). DRAM placement is checked by [`FlashPlan`].
    pub fn check(&self) -> Result<(), ImageError> {
        let end = self.flash_offset as usize + self.size as usize;
        if self.size < 0x1000 || end != HALF_SIZE {
            return Err(ImageError::InvalidSlot(*self));
        }
        Ok(())
    }

    /// End of the DRAM copy (exclusive).
    pub const fn dram_end(&self) -> u64 {
        self.destination + self.size as u64
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImageError {
    /// A half must be exactly 16 MiB, a full image 16 or 32 MiB.
    Size {
        expected: usize,
        actual: usize,
    },
    Directory(DirError),
    /// The EFS generation word is none of the three.
    UnknownGeneration(u32),
    /// No half of the vendor ROM has a generation the board supports.
    NoSupportedHalf,
    /// The `0x62` entries are missing, inconsistent or not a copied reset
    /// image.
    ResetEntry(&'static str),
    InvalidSlot(ResetSlot),
    /// A vendor `0x62` slot cannot carry the reset image at its link
    /// address.
    Slot {
        half: FlashHalf,
        vendor: ResetSlot,
        image: ResetSlot,
        reason: &'static str,
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
    /// The reset image does not match its slot or has no reset vector.
    ResetImage(&'static str),
    /// Trust or locator patching of the reset image failed.
    Patch(&'static str),
    /// A generation would boot the other half (or none).
    BootOrder {
        half: FlashHalf,
        generation: Generation,
        booted: Option<FlashHalf>,
    },
    /// The flash plan itself is inconsistent.
    Plan(&'static str),
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

/// Every `0x62` entry with its directory.
fn reset_entries<'a>(
    dirs: &'a Directories<'a, BiosEntry>,
) -> impl Iterator<Item = (&'a Directory<'a, BiosEntry>, &'a BiosEntry)> + 'a {
    dirs.levels().flat_map(|dir| {
        dir.entries()
            .iter()
            .filter(|e| e.kind() == bios::RESET_IMAGE)
            .map(move |e| (dir, e))
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
            .map(|(dir, e)| {
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

/// The 16 MiB halves of a vendor ROM: one for a 16 MiB flash, two for a
/// 32 MiB dual image.
pub fn halves(image: &[u8]) -> Result<Vec<(FlashHalf, &[u8])>, ImageError> {
    let halves: &[FlashHalf] = match image.len() {
        HALF_SIZE => &[FlashHalf::Lower],
        n if n == 2 * HALF_SIZE => &[FlashHalf::Lower, FlashHalf::Upper],
        actual => {
            return Err(ImageError::Size {
                expected: 2 * HALF_SIZE,
                actual,
            });
        }
    };
    Ok(halves.iter().map(|&h| (h, &image[h.range()])).collect())
}

/// The generation of a half's EFS, `None` without a recognized EFS.
pub fn half_generation(half: &[u8]) -> Option<Generation> {
    Efs::new(half)
        .ok()
        .and_then(|efs| generation_of(efs.generation_word()).ok())
}

/// How a build is composed into the vendor ROM.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlashPlan {
    /// Generations the board supports; halves of other generations keep
    /// their vendor bytes.
    #[serde(with = "generation_names")]
    pub generations: Vec<Generation>,
    /// The reset image as linked: its size, its DRAM link address and its
    /// place at the top of a half (the smallest vendor slot).
    pub reset: ResetSlot,
    /// Flash offset of the FFS in every half (start of the runtime firmware
    /// window).
    pub ffs_offset: u32,
    /// Lowest DRAM address a vendor slot may be copied to: the end of the
    /// platform's early region, which the reset image needs intact.
    pub dram_floor: u64,
}

/// What happens to one half of the vendor ROM.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HalfAction {
    /// Composed: vendor prefix, FFS, the reset image at the end of `slot`.
    Compose(VendorLayout),
    /// Kept byte-identical: no recognized EFS (`None`) or a generation the
    /// board does not support.
    Keep(Option<Generation>),
}

/// The plan applied to a vendor ROM, before any byte is written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Survey {
    pub halves: Vec<(FlashHalf, HalfAction)>,
    /// End of the common FFS window: the lowest composed slot start.
    pub ffs_end: u32,
}

impl Survey {
    /// FFS capacity of the common window.
    pub fn ffs_capacity(&self, plan: &FlashPlan) -> usize {
        (self.ffs_end as usize).saturating_sub(plan.ffs_offset as usize)
    }
}

impl FlashPlan {
    /// The plan's own consistency: the image at the top of a half, above
    /// the DRAM floor, and room for an FFS below it.
    pub fn check(&self) -> Result<(), ImageError> {
        self.reset.check()?;
        if self.generations.is_empty() {
            return Err(ImageError::Plan("no supported generation"));
        }
        if self.reset.destination < self.dram_floor {
            return Err(ImageError::Plan("reset image below the DRAM floor"));
        }
        if self.ffs_offset == 0 || self.ffs_offset >= self.reset.flash_offset {
            return Err(ImageError::Plan("FFS offset outside the half"));
        }
        Ok(())
    }

    /// Check a supported vendor half: its slot can carry the image at the
    /// image's link address (same top in flash and DRAM, large enough,
    /// above the DRAM floor) and its prefix ends below the FFS.
    pub fn check_half(&self, half: FlashHalf, vendor: &[u8]) -> Result<VendorLayout, ImageError> {
        let layout = VendorLayout::parse(vendor)?;
        let slot = layout.reset;
        let fail = |reason| ImageError::Slot {
            half,
            vendor: slot,
            image: self.reset,
            reason,
        };
        if slot.dram_end() != self.reset.dram_end() {
            return Err(fail("DRAM copy ends elsewhere"));
        }
        if slot.size < self.reset.size {
            return Err(fail("slot smaller than the reset image"));
        }
        if slot.destination < self.dram_floor {
            return Err(fail("slot reaches into the early region"));
        }
        if slot.flash_offset <= self.ffs_offset {
            return Err(fail("slot starts below the FFS"));
        }
        if layout.prefix_end > self.ffs_offset as usize {
            return Err(ImageError::PrefixOverlap {
                prefix_end: layout.prefix_end,
                limit: self.ffs_offset as usize,
            });
        }
        Ok(layout)
    }

    /// Decide every half of `vendor_rom` and the common FFS window.
    pub fn survey(&self, vendor_rom: &[u8]) -> Result<Survey, ImageError> {
        self.check()?;
        let halves = halves(vendor_rom)?
            .into_iter()
            .map(|(half, bytes)| {
                let action = match half_generation(bytes) {
                    Some(g) if self.generations.contains(&g) => {
                        HalfAction::Compose(self.check_half(half, bytes)?)
                    }
                    other => HalfAction::Keep(other),
                };
                Ok((half, action))
            })
            .collect::<Result<Vec<_>, ImageError>>()?;
        let ffs_end = halves
            .iter()
            .filter_map(|(_, a)| match a {
                HalfAction::Compose(layout) => Some(layout.reset.flash_offset),
                HalfAction::Keep(_) => None,
            })
            .min()
            .ok_or(ImageError::NoSupportedHalf)?;
        Ok(Survey { halves, ffs_end })
    }
}

/// The reset image must be exactly the plan's image with a reset vector.
fn check_reset_image(plan: &FlashPlan, reset_image: &[u8]) -> Result<(), ImageError> {
    if reset_image.len() != plan.reset.size as usize {
        return Err(ImageError::ResetImage("not the linked image size"));
    }
    let vector = &reset_image[reset_image.len() - 16..];
    if vector.iter().all(|&b| b == 0) || vector.iter().all(|&b| b == 0xff) {
        return Err(ImageError::ResetImage(
            "no reset vector in the last 16 bytes",
        ));
    }
    Ok(())
}

/// Compose one supported half: vendor prefix, FFS at the plan's offset
/// (below this half's slot), the reset image at the end of the vendor
/// slot with the slot's front erased. `reset_image` must already be
/// patched ([`patch_reset_image`]). The directories are not touched; the
/// result is re-parsed to prove they still describe it.
pub fn compose_half(
    half: FlashHalf,
    vendor: &[u8],
    plan: &FlashPlan,
    reset_image: &[u8],
    ffs: &[u8],
) -> Result<Vec<u8>, ImageError> {
    plan.check()?;
    let layout = plan.check_half(half, vendor)?;
    check_reset_image(plan, reset_image)?;
    let ffs_offset = plan.ffs_offset as usize;
    let capacity = layout.reset.flash_offset as usize - ffs_offset;
    if ffs.len() > capacity {
        return Err(ImageError::FfsTooLarge {
            size: ffs.len(),
            capacity,
        });
    }

    let mut out = vec![0xff; HALF_SIZE];
    out[..layout.prefix_end].copy_from_slice(&vendor[..layout.prefix_end]);
    out[ffs_offset..ffs_offset + ffs.len()].copy_from_slice(ffs);
    out[HALF_SIZE - reset_image.len()..].copy_from_slice(reset_image);

    let composed = VendorLayout::parse(&out)?;
    if composed != layout {
        return Err(ImageError::ResetEntry(
            "composed directories do not describe the image",
        ));
    }
    Ok(out)
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

/// The flash image of one build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sp3Flash {
    /// The composed ROM, the vendor ROM's size (16 or 32 MiB).
    pub image: Vec<u8>,
    pub survey: Survey,
    pub patched: Patched,
}

/// Compose a build: patch the reset image from the FFS and put FFS and
/// reset image into every supported half of the vendor ROM; other halves
/// stay vendor bytes. A 32 MiB result is checked for the boot order.
pub fn compose(
    vendor_rom: &[u8],
    plan: &FlashPlan,
    reset_image: &[u8],
    ffs: &[u8],
) -> Result<Sp3Flash, ImageError> {
    let survey = plan.survey(vendor_rom)?;
    let capacity = survey.ffs_capacity(plan);
    if ffs.len() > capacity {
        return Err(ImageError::FfsTooLarge {
            size: ffs.len(),
            capacity,
        });
    }
    check_reset_image(plan, reset_image)?;
    let mut reset = reset_image.to_vec();
    let patched = patch_reset_image(&mut reset, ffs)?;
    let mut image = vendor_rom.to_vec();
    for (half, action) in &survey.halves {
        if let HalfAction::Compose(_) = action {
            let composed = compose_half(*half, &vendor_rom[half.range()], plan, &reset, ffs)?;
            image[half.range()].copy_from_slice(&composed);
        }
    }
    if image.len() == 2 * HALF_SIZE {
        check_dual(&image)?;
    }
    Ok(Sp3Flash {
        image,
        survey,
        patched,
    })
}

/// The SP3 image operation: an FFS (the payload and the reset unit's
/// trust-finalized copy) plus the reset unit, composed into every
/// supported half of the vendor ROM.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Sp3FlashAssembly {
    pub ffs: FfsAssembly,
    /// Vendor ROM: one 16 MiB half or the 32 MiB dual image, workspace
    /// relative or absolute. Its prefix (EFS, PSP/BIOS directories, blobs,
    /// APCB) is kept byte-identical.
    pub vendor_image: String,
    /// Unit whose flat output is the reset image.
    pub reset_unit: String,
    pub compose: FlashPlan,
}

impl Sp3FlashAssembly {
    /// The reset unit is the plan's first stage and is linked for exactly
    /// the plan's image: stored and run at its destination, entered at its
    /// last 16 bytes.
    pub fn validate(&self, plan: &BuildPlan) -> Result<(), String> {
        self.compose.check()?;
        let slot = self.compose.reset;
        if self.vendor_image.is_empty() || self.vendor_image.contains('\0') {
            return Err("invalid SP3 vendor image path".into());
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
            return Err("SP3 reset producer differs from the reset image geometry".into());
        }
        Ok(())
    }
}

/// `Generation`s as their lower-case names in plans.
mod generation_names {
    use super::Generation;
    use serde::{Deserialize, Deserializer, Serializer, de::Error};

    const ALL: [Generation; 3] = [Generation::Naples, Generation::Rome, Generation::Milan];

    pub fn serialize<S: Serializer>(gs: &[Generation], s: S) -> Result<S::Ok, S::Error> {
        s.collect_seq(gs.iter().map(|g| g.name().to_ascii_lowercase()))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<Generation>, D::Error> {
        Vec::<String>::deserialize(d)?
            .iter()
            .map(|name| {
                ALL.into_iter()
                    .find(|g| g.name().eq_ignore_ascii_case(name))
                    .ok_or_else(|| D::Error::custom(format!("unknown SP3 generation {name}")))
            })
            .collect()
    }
}

#[cfg(test)]
#[path = "sp3_flash_tests.rs"]
mod tests;
