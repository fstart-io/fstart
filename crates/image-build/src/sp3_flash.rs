// SPDX-License-Identifier: MIT OR Apache-2.0
//! AMD SP3 (Naples, Rome, Milan) flash image: one build composed into the
//! vendor ROM (FIRMWARE_IMAGE §1.1–§2.1, §4, §7 of the SP3 analysis).
//!
//! Written from the SP3 reverse engineering and AMD #55758 rev 1.13 (facts
//! only). It is to be folded into the shared AMD assembly ([`crate::amd`]).
//!
//! Every supported vendor half keeps its vendor EFS and its PSP/BIOS
//! directories and blobs in place (`[0, prefix)` of the half, the vendor's
//! "Two 16M in 32MB" layout, AMD #55758 rev 1.13 §4.1.7); only its `0x62`
//! entries change, to a copy of the reset image at the half's own
//! `0xde0000`.
//!
//! ```text
//! [0, prefix_lo)              lower vendor prefix (EFS, directories, blobs)
//! [ffs, 0xde0000)             fstart FFS (ffs = max(0x600000, prefix end)),
//!                             then erased
//! [0xde0000, 16 MiB)          reset image (lower half's 0x62)
//! [16M, 16M + prefix_hi)      upper vendor prefix, if the board supports
//!                             its generation (else the upper half is kept)
//! [16M + 0xde0000, 32 MiB)    reset image copy (upper half's 0x62)
//! ```
//!
//! The FFS exists once, in the lower half. The reset image reads it through
//! ROM2 when ROM2 shows the lower half and through ROM3 otherwise
//! (`fstart_driver_zen::fch::rom3`), located by the flash anchor
//! (`fstart_amdfw_format::anchor`).
//!
//! A single multi-generation EFS with a `2PSP` combo directory and the
//! upper trees rebased to 32 MiB offsets (§4.1.7 "Single 32M in 32MB",
//! §4.1.4.2) would need one reset image only; it is not implemented until
//! the server boot ROMs are seen to accept it.
//!
//! Every `0x62` entry (L1 and L2, every generation) describes the reset
//! image as linked: `[0x75de0000, 0x76000000)`, `0x220000` bytes,
//! reset + copy, uncompressed. The reset image carries the stage's trust
//! policy, the FFS locator and the SP3 flash anchor (FFS flash offset and
//! size) as patch sites, which [`patch_reset_image`] fills from the
//! assembled FFS. A composed image is re-read with `fstart-amdfw-format` (as
//! the reset image reads it) and the boot ROM's EFS rule before it is
//! returned.

use core::fmt;
use core::ops::Range;
use fstart_amdfw_format::anchor::{ANCHOR_SIZE, FlashAnchor};
use fstart_amdfw_format::directory::{
    BiosEntry, Combo, Directory, Entry, Levels, PspEntry, bios, modify_entries,
};
use fstart_amdfw_format::efs::{self, BiosDirectorySlot, Efs};
use fstart_amdfw_format::{Error as DirError, Flash, Profile, ROM2_BASE as MMIO_BASE};
use fstart_core::ffs::locator::{LOCATOR_SIZE, LocatorBlock};
use fstart_core::ffs::trust::{TRUST_SIZE, TrustBlock};
use serde::{Deserialize, Serialize};
use zerocopy::IntoBytes;

pub use fstart_amdfw_format::HALF_SIZE;

/// An SP3 generation as the image knows it: the BIOS directory slot its
/// boot ROM reads. The silicon's generation maps to it through its
/// profile (`fstart_driver_zen::soc::Generation::profile`).
pub type Generation = BiosDirectorySlot;
pub const NAPLES: Generation = BiosDirectorySlot::Family17hModel00;
pub const ROME: Generation = BiosDirectorySlot::Family17hModel30;
pub const MILAN: Generation = BiosDirectorySlot::Family19h;

use crate::build_plan::{BuildPlan, FfsAssembly, UnitOutput};

/// The generation of a vendor EFS word: exactly the word whose bits a
/// generation's boot ROM requires clear (FIRMWARE_IMAGE §1.1).
pub fn generation_of(word: u32) -> Result<Generation, ImageError> {
    GENERATIONS
        .into_iter()
        .find(|g| !Profile::for_slot(*g).generation_mask == word)
        .ok_or(ImageError::UnknownGeneration(word))
}

const GENERATIONS: [Generation; 3] = [NAPLES, ROME, MILAN];

/// The plan name of a generation.
pub const fn name(generation: Generation) -> &'static str {
    match generation {
        NAPLES => "naples",
        ROME => "rome",
        MILAN => "milan",
        BiosDirectorySlot::Family17hModel10 => "picasso",
    }
}

/// The flash offsets of the 16 MiB halves a generation's boot ROM searches
/// for its EFS, in order: Naples only the lower one (FIRMWARE_IMAGE §1.2,
/// AMD #55758 §4.1.7).
const fn boot_halves(generation: Generation) -> &'static [usize] {
    match generation {
        NAPLES => &[0],
        _ => &[0, HALF_SIZE],
    }
}

/// `bytes` from the start of a 16 MiB half, as the boot ROM maps the half
/// below 4 GiB.
const fn flash(generation: Generation, bytes: &[u8]) -> Flash<'_> {
    Flash::region(bytes, 0, MMIO_BASE, Profile::for_slot(generation))
}

/// Where the SP3 halves have their EFS.
const EFS_OFFSET: usize = 0x2_0000;

/// One 16 MiB half of a 32 MiB flash (a 16 MiB flash is its lower half).
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

/// A `0x62` slot: where a reset image sits in its half and where the PSP
/// copies it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResetSlot {
    pub flash_offset: u32,
    pub size: u32,
    pub destination: u64,
}

impl ResetSlot {
    /// A nonempty slot ending at the top of the half, because the 16-bit
    /// entry may run from the flash copy at the top of ROM2 (FIRMWARE_IMAGE
    /// §5). DRAM placement is checked by [`FlashPlan`].
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
    /// The `0x62` entries are missing or not a copied reset image.
    ResetEntry(&'static str),
    InvalidSlot(ResetSlot),
    /// The vendor prefix reaches into the FFS or reset image.
    PrefixOverlap {
        prefix_end: usize,
        limit: usize,
    },
    FfsTooLarge {
        size: usize,
        capacity: usize,
    },
    /// The reset image does not match the plan or has no reset vector.
    ResetImage(&'static str),
    /// Trust, locator or flash anchor patching of the reset image failed.
    Patch(&'static str),
    /// A generation would boot another half (or none).
    BootOrder {
        half: FlashHalf,
        generation: Generation,
        booted: Option<FlashHalf>,
    },
    /// The flash plan itself is inconsistent.
    Plan(&'static str),
    /// The vendor ROM does not fit the layout.
    Layout(&'static str),
    /// An EFS signature at a boot ROM search offset other than the EFS.
    StrayEfs {
        offset: usize,
    },
    /// The composed image does not resolve as planned.
    Verify(&'static str),
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

const fn align_up(v: usize, a: usize) -> usize {
    v.div_ceil(a) * a
}

/// A parsed vendor half.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VendorLayout {
    pub generation: Generation,
    /// The vendor `0x62` slot (identical in L1 and L2).
    pub reset: ResetSlot,
    /// End of everything the PSP/ABL reference: directories (including
    /// their reserved `max_size`) and every entry's data except `0x62`.
    pub prefix_end: usize,
}

/// A half's EFS, wherever a boot ROM of any generation finds it.
fn vendor_efs(half: &[u8]) -> Result<&Efs, ImageError> {
    let any = Profile {
        generation_mask: 0,
        ..Profile::for_slot(NAPLES)
    };
    Ok(Flash::region(half, 0, MMIO_BASE, any).efs_checked()?.1)
}

/// A vendor half's generation and its PSP and BIOS directories, checksums
/// verified.
struct Vendor<'a> {
    generation: Generation,
    psp: Levels<'a, PspEntry>,
    bios: Levels<'a, BiosEntry>,
}

impl<'a> Vendor<'a> {
    fn read(half: &'a [u8]) -> Result<Self, ImageError> {
        let efs = vendor_efs(half)?;
        let generation = generation_of(efs.generation.get())?;
        let flash = flash(generation, half);
        let psp_root = flash.resolve(efs.new_psp_directory.get().into(), 0, 0)?;
        if Combo::read(flash, psp_root)?.is_some() {
            return Err(ImageError::Layout("vendor half with a combo directory"));
        }
        Ok(Self {
            generation,
            psp: flash.psp_directories()?,
            bios: flash.bios_directories_checked(generation)?,
        })
    }

    /// Every `0x62` entry: its data range and DRAM destination.
    fn resets(&self) -> impl Iterator<Item = Result<(Range<usize>, u64), DirError>> + '_ {
        self.bios.iter().flat_map(|dir| {
            dir.entries
                .iter()
                .filter(|e| e.kind == bios::RESET_IMAGE && e.has_data())
                .map(move |e| Ok((dir.range(e)?, e.destination())))
        })
    }

    /// End of everything the PSP/ABL read: the directories with their
    /// reserved size and every file except the reset image.
    fn prefix_end(&self) -> Result<usize, DirError> {
        fn end<E: Entry>(
            dir: Directory<'_, E>,
            kept: impl Fn(&E) -> bool,
        ) -> Result<usize, DirError> {
            dir.entries
                .iter()
                .filter(|e| e.has_data() && kept(e))
                .try_fold(dir.extent()?.end, |end, e| Ok(end.max(dir.range(e)?.end)))
        }
        let psp = self.psp.iter().map(|dir| end(dir, |_| true));
        let bios = self
            .bios
            .iter()
            .map(|dir| end(dir, |e: &BiosEntry| e.kind != bios::RESET_IMAGE));
        psp.chain(bios).try_fold(0, |max, end| Ok(max.max(end?)))
    }
}

impl VendorLayout {
    /// Parse one 16 MiB half: EFS generation, every PSP and BIOS directory
    /// (checksums verified), the `0x62` slot and the prefix end.
    pub fn parse(half: &[u8]) -> Result<Self, ImageError> {
        exact_size(half, HALF_SIZE)?;
        let vendor = Vendor::read(half)?;
        let resets = vendor.resets().collect::<Result<Vec<_>, _>>()?;
        let (target, destination) = resets
            .first()
            .cloned()
            .ok_or(ImageError::ResetEntry("no 0x62 entry"))?;
        if resets.iter().any(|r| *r != (target.clone(), destination)) {
            return Err(ImageError::ResetEntry("L1 and L2 0x62 entries differ"));
        }
        let reset = ResetSlot {
            flash_offset: target.start as u32,
            size: target.len() as u32,
            destination,
        };
        Ok(Self {
            generation: vendor.generation,
            reset,
            prefix_end: vendor.prefix_end()?,
        })
    }
}

/// The 16 MiB halves of a vendor ROM: one for a 16 MiB flash, two for a
/// 32 MiB one.
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

/// The generation of a half's vendor EFS, `None` without a recognized EFS.
pub fn half_generation(half: &[u8]) -> Option<Generation> {
    vendor_efs(half)
        .ok()
        .and_then(|efs| generation_of(efs.generation.get()).ok())
}

/// How a build is composed into the vendor ROM.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlashPlan {
    /// Generations the board supports; an upper half of another generation
    /// keeps its vendor bytes.
    #[serde(with = "generation_names")]
    pub generations: Vec<Generation>,
    /// The reset image as linked: its size, DRAM link address and its place
    /// at the top of the lower half.
    pub reset: ResetSlot,
    /// Lowest flash offset of the FFS (it starts higher if the lower vendor
    /// prefix reaches further).
    pub ffs_offset: u32,
    /// Lowest DRAM address the reset image may be copied to: the end of the
    /// platform's early region, which the reset image needs intact.
    pub dram_floor: u64,
}

/// What happens to one half of the vendor ROM.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HalfAction {
    /// Composed: vendor prefix kept, its `0x62` entries rewritten.
    Compose(VendorLayout),
    /// Kept byte-identical: an upper half without a recognized EFS (`None`)
    /// or of a generation the board does not support.
    Keep(Option<Generation>),
}

/// The plan applied to a vendor ROM, before any byte is written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Survey {
    pub halves: Vec<(FlashHalf, HalfAction)>,
    /// The FFS window, absolute flash offsets in the lower half.
    pub ffs: Range<u32>,
}

impl Survey {
    pub fn ffs_capacity(&self) -> usize {
        self.ffs.len()
    }

    /// The composed halves, lower first.
    pub fn composed(&self) -> impl Iterator<Item = (FlashHalf, VendorLayout)> + '_ {
        self.halves
            .iter()
            .filter_map(|(half, action)| match action {
                HalfAction::Compose(layout) => Some((*half, *layout)),
                HalfAction::Keep(_) => None,
            })
    }
}

impl FlashPlan {
    /// The plan's own consistency.
    pub fn check(&self) -> Result<(), ImageError> {
        self.reset.check()?;
        if self.generations.is_empty() {
            return Err(ImageError::Plan("no supported generation"));
        }
        if self.reset.destination < self.dram_floor {
            return Err(ImageError::Plan("reset image below the DRAM floor"));
        }
        if self.ffs_offset == 0 || self.ffs_offset >= self.reset.flash_offset {
            return Err(ImageError::Plan("FFS offset outside the lower half"));
        }
        Ok(())
    }

    /// Decide every half of `vendor_rom` and the FFS window.
    pub fn survey(&self, vendor_rom: &[u8]) -> Result<Survey, ImageError> {
        self.check()?;
        let reset_at = self.reset.flash_offset as usize;
        let halves = halves(vendor_rom)?
            .into_iter()
            .map(|(half, bytes)| {
                let action = match half_generation(bytes) {
                    Some(g) if self.generations.contains(&g) => {
                        let layout = VendorLayout::parse(bytes)?;
                        if layout.prefix_end > reset_at {
                            return Err(ImageError::PrefixOverlap {
                                prefix_end: layout.prefix_end,
                                limit: reset_at,
                            });
                        }
                        HalfAction::Compose(layout)
                    }
                    other => HalfAction::Keep(other),
                };
                Ok((half, action))
            })
            .collect::<Result<Vec<_>, ImageError>>()?;
        let lower = match halves[0].1 {
            HalfAction::Compose(layout) => layout,
            HalfAction::Keep(_) if halves.iter().all(|(_, a)| matches!(a, HalfAction::Keep(_))) => {
                return Err(ImageError::NoSupportedHalf);
            }
            // The FFS lives in the lower half.
            HalfAction::Keep(_) => {
                return Err(ImageError::Layout(
                    "the board does not support the lower half's generation",
                ));
            }
        };
        let composed: Vec<_> = halves
            .iter()
            .filter_map(|(_, a)| match a {
                HalfAction::Compose(l) => Some(l.generation),
                HalfAction::Keep(_) => None,
            })
            .collect();
        if composed.len() == 2 && composed[0] == composed[1] {
            return Err(ImageError::Layout("both halves carry the same generation"));
        }
        let start = (self.ffs_offset as usize).max(align_up(lower.prefix_end, 0x1000));
        if start >= reset_at {
            return Err(ImageError::PrefixOverlap {
                prefix_end: lower.prefix_end,
                limit: reset_at,
            });
        }
        Ok(Survey {
            halves,
            ffs: start as u32..self.reset.flash_offset,
        })
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

/// Point every `0x62` of one composed half (`bytes`, the vendor's
/// directories at `tables`) at the reset image copy at the half's
/// `plan.reset.flash_offset` (MMIO form for MMIO entries).
fn rewrite_half(bytes: &mut [u8], tables: &[usize], plan: &FlashPlan) -> Result<(), ImageError> {
    const MODE: u64 = 3 << 62;
    let rewrite = |e: &mut BiosEntry| {
        if e.kind != bios::RESET_IMAGE || !e.has_data() {
            return;
        }
        let source = e.source.get();
        let mmio = (MMIO_BASE..1 << 32).contains(&(source & !MODE));
        let offset = u64::from(plan.reset.flash_offset) + if mmio { MMIO_BASE } else { 0 };
        e.source.set(source & MODE | offset);
        e.size.set(plan.reset.size);
        e.destination.set(plan.reset.destination);
        // Reset + copy, not compressed; keep read-only/instance.
        e.flags = e.flags & !(BiosEntry::RESET | BiosEntry::COPY | BiosEntry::COMPRESSED)
            | BiosEntry::RESET
            | BiosEntry::COPY;
    };
    tables.iter().try_for_each(|&offset| {
        modify_entries(bytes, offset, rewrite).ok_or(ImageError::Verify("a BIOS directory moved"))
    })
}

/// Build the image from the survey. `reset` is already patched.
fn build(
    vendor_rom: &[u8],
    plan: &FlashPlan,
    survey: &Survey,
    reset: &[u8],
    ffs: &[u8],
) -> Result<Vec<u8>, ImageError> {
    let mut out = vendor_rom.to_vec();
    for (half, layout) in survey.composed() {
        let vendor = Vendor::read(&vendor_rom[half.range()])?;
        let tables: Vec<_> = vendor.bios.iter().map(|dir| dir.offset).collect();
        let bytes = &mut out[half.range()];
        bytes[layout.prefix_end..].fill(0xff);
        rewrite_half(bytes, &tables, plan)?;
        let reset_at = half.offset() + plan.reset.flash_offset as usize;
        out[reset_at..reset_at + reset.len()].copy_from_slice(reset);
    }
    let start = survey.ffs.start as usize;
    out[start..start + ffs.len()].copy_from_slice(ffs);
    Ok(out)
}

/// Re-read the composed image as the boot ROMs and the reset image will:
/// every composed generation boots the planned half, resolves its PSP and
/// BIOS directories (checksums), and every `0x62` names the reset image;
/// no stray EFS signature sits at another search offset.
fn verify(image: &[u8], plan: &FlashPlan, survey: &Survey) -> Result<(), ImageError> {
    for (half, layout) in survey.composed() {
        let generation = layout.generation;
        let planned = half;
        let booted = booted_half(image, generation);
        if booted != Some(planned) {
            return Err(ImageError::BootOrder {
                half: planned,
                generation,
                booted,
            });
        }
        let flash = flash(generation, &image[planned.offset()..]);
        flash.psp_directories()?;
        let dirs = flash.bios_directories_checked(generation)?;
        let expected = planned.offset() + plan.reset.flash_offset as usize;
        let resets = dirs
            .iter()
            .flat_map(|d| d.entries.iter().map(move |e| (d, e)))
            .filter(|(_, e)| e.kind == bios::RESET_IMAGE);
        for (dir, entry) in resets {
            let range = dir.range(entry)?;
            if planned.offset() + range.start != expected
                || range.len() != plan.reset.size as usize
                || entry.destination() != plan.reset.destination
                || !(entry.reset() && entry.copy() && !entry.compressed())
            {
                return Err(ImageError::Verify("a 0x62 entry misses the reset image"));
            }
        }
        for &offset in efs::PROBES.iter().filter(|&&o| o as usize != EFS_OFFSET) {
            let at = half.offset() + offset as usize;
            if image[at..at + 4] == efs::SIGNATURE.to_le_bytes() {
                return Err(ImageError::StrayEfs { offset: at });
            }
        }
    }
    Ok(())
}

/// The half a generation's PSP boot ROM boots from (lower, then upper;
/// Naples only lower).
pub fn booted_half(image: &[u8], generation: Generation) -> Option<FlashHalf> {
    let halves = boot_halves(generation);
    let profile = Profile::for_slot(generation);
    fstart_amdfw_format::booted_half(image, MMIO_BASE, profile, halves).map(|offset| {
        if offset == 0 {
            FlashHalf::Lower
        } else {
            FlashHalf::Upper
        }
    })
}

/// Check a 32 MiB vendor-style (dual) image: every half with an EFS is
/// booted by its own generation (so Rome must sit below Milan, Naples
/// below Rome).
pub fn check_dual(image: &[u8]) -> Result<(), ImageError> {
    exact_size(image, 2 * HALF_SIZE)?;
    [FlashHalf::Lower, FlashHalf::Upper]
        .into_iter()
        .filter_map(|half| {
            vendor_efs(&image[half.range()])
                .ok()
                .map(|efs| (half, efs.generation.get()))
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
    /// The FFS locator (image-relative offsets, size).
    pub locator: bool,
    /// The SP3 flash anchor (FFS flash offset and size); without it the
    /// reset image cannot find its FFS.
    pub flash: bool,
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

/// Fill the reset image's patch sites: trust policy and FFS locator from
/// the assembled FFS (its offsets are FFS-relative, valid wherever the FFS
/// sits), and `flash` (where the FFS sits in the flash part). A site the
/// stage does not link is skipped; a site with no FFS counterpart is an
/// error.
pub fn patch_reset_image(
    reset: &mut [u8],
    ffs: &[u8],
    flash: FlashAnchor,
) -> Result<Patched, ImageError> {
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
    if FlashAnchor::parse(flash.as_bytes()).is_none() {
        return Err(ImageError::Patch("invalid flash anchor"));
    }
    let flash = site(reset, FlashAnchor::placeholder().as_bytes())?
        .map(|at| reset[at..at + ANCHOR_SIZE].copy_from_slice(flash.as_bytes()))
        .is_some();
    Ok(Patched {
        trust,
        locator,
        flash,
    })
}

/// The flash image of one build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sp3Flash {
    /// The composed ROM, the vendor ROM's size (16 or 32 MiB).
    pub image: Vec<u8>,
    pub survey: Survey,
    pub patched: Patched,
}

/// Compose a build: patch the reset image, then lay out the vendor ROM as
/// the plan's layout says and verify the result.
pub fn compose(
    vendor_rom: &[u8],
    plan: &FlashPlan,
    reset_image: &[u8],
    ffs: &[u8],
) -> Result<Sp3Flash, ImageError> {
    let survey = plan.survey(vendor_rom)?;
    if ffs.len() > survey.ffs_capacity() {
        return Err(ImageError::FfsTooLarge {
            size: ffs.len(),
            capacity: survey.ffs_capacity(),
        });
    }
    check_reset_image(plan, reset_image)?;
    let mut reset = reset_image.to_vec();
    let anchor = FlashAnchor::new(vendor_rom.len() as u32, survey.ffs.start, ffs.len() as u32);
    let patched = patch_reset_image(&mut reset, ffs, anchor)?;
    let image = build(vendor_rom, plan, &survey, &reset, ffs)?;
    verify(&image, plan, &survey)?;
    Ok(Sp3Flash {
        image,
        survey,
        patched,
    })
}

/// The SP3 image operation: an FFS (the payload and the reset unit's
/// trust-finalized copy) plus the reset unit, composed into the vendor ROM.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Sp3FlashAssembly {
    pub ffs: FfsAssembly,
    /// Vendor ROM: one 16 MiB half or the 32 MiB dual image, workspace
    /// relative or absolute. Its directories and blobs are kept in place.
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
    use super::{GENERATIONS, Generation};
    use serde::{Deserialize, Deserializer, Serializer, de::Error};

    pub fn serialize<S: Serializer>(gs: &[Generation], s: S) -> Result<S::Ok, S::Error> {
        s.collect_seq(gs.iter().map(|&g| super::name(g)))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<Generation>, D::Error> {
        Vec::<String>::deserialize(d)?
            .iter()
            .map(|name| {
                GENERATIONS
                    .into_iter()
                    .find(|&g| super::name(g).eq_ignore_ascii_case(name))
                    .ok_or_else(|| D::Error::custom(format!("unknown SP3 generation {name}")))
            })
            .collect()
    }
}

#[cfg(test)]
#[path = "sp3_flash_tests.rs"]
mod tests;
