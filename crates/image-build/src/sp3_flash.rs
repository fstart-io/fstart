// SPDX-License-Identifier: MIT OR Apache-2.0
//! AMD SP3 (Naples, Rome, Milan) flash image: one build composed into the
//! vendor ROM (FIRMWARE_IMAGE §1.1–§2.1, §4, §7 of the SP3 analysis).
//!
//! Written from the SP3 reverse engineering and AMD #55758 rev 1.13 (facts
//! only).
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
//! (`fstart_driver_zen::fch::rom3`).
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
//! assembled FFS. A composed image is re-read with the runtime directory
//! parser and the boot ROM's EFS rule before it is returned.

use core::fmt;
use core::ops::Range;
use std::collections::BTreeSet;

use fstart_core::ffs::locator::{LOCATOR_SIZE, LocatorBlock};
use fstart_core::ffs::trust::{TRUST_SIZE, TrustBlock};
use fstart_driver_zen::fch::rom3::{ANCHOR_SIZE, FlashAnchor};
use fstart_driver_zen::psp::directory::{
    Combo, DirError, EFS_OFFSET, EFS_SEARCH, EFS_SIGNATURE, Efs, Entry, MMIO_BASE, bios,
    booted_half as booted_half_offset, fletcher32, psp,
};
use serde::{Deserialize, Serialize};
use zerocopy::IntoBytes;

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

/// The generation of a vendor EFS word (exact words only).
pub fn generation_of(word: u32) -> Result<Generation, ImageError> {
    GENERATIONS
        .into_iter()
        .find(|&g| generation_word(g) == word)
        .ok_or(ImageError::UnknownGeneration(word))
}

const GENERATIONS: [Generation; 3] = [Generation::Naples, Generation::Rome, Generation::Milan];

/// EFS field of a generation's BIOS directory (AMD #55758 Table 2).
const fn bios_field(generation: Generation) -> usize {
    match generation {
        Generation::Naples => 0x18,
        Generation::Rome => 0x20,
        Generation::Milan => 0x28,
    }
}
const EFS_PSP: usize = 0x14;
const EFS_WORD: usize = 0x24;

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
    /// An address mode the composer does not implement (3, partition
    /// relative), or a directory with a base address field.
    AddressMode {
        offset: usize,
        mode: u8,
    },
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

fn get32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}
fn get64(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().unwrap())
}
fn put32(b: &mut [u8], at: usize, v: u32) {
    b[at..at + 4].copy_from_slice(&v.to_le_bytes());
}
fn put64(b: &mut [u8], at: usize, v: u64) {
    b[at..at + 8].copy_from_slice(&v.to_le_bytes());
}
const fn align_up(v: usize, a: usize) -> usize {
    v.div_ceil(a) * a
}

// ---- directory trees as raw bytes ----

/// PSP or BIOS directory, by cookie.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Psp,
    Bios,
}

impl Kind {
    fn of(cookie: &[u8]) -> Option<Self> {
        match cookie {
            b"$PSP" | b"$PL2" => Some(Self::Psp),
            b"$BHD" | b"$BL2" => Some(Self::Bios),
            _ => None,
        }
    }

    const fn entry_size(self) -> usize {
        match self {
            Self::Psp => 16,
            Self::Bios => 24,
        }
    }
}

/// One directory of a tree, offsets within its half.
#[derive(Debug, Clone, Copy)]
struct Dir {
    offset: usize,
    kind: Kind,
    count: usize,
    info: u32,
}

impl Dir {
    /// Gen2 address mode (AMD #55758 Table 4); gen1 headers have none.
    fn mode(&self, gen1: bool) -> u8 {
        if gen1 { 0 } else { (self.info >> 29 & 3) as u8 }
    }

    fn table_end(&self) -> usize {
        self.offset + 16 + self.count * self.kind.entry_size()
    }

    /// The table, or its reserved area (gen2 Max Size, 4 KiB units).
    fn end(&self, gen1: bool) -> usize {
        let reserved = if gen1 {
            0
        } else {
            self.offset + (self.info & 0x3ff) as usize * 0x1000
        };
        self.table_end().max(reserved)
    }

    fn entry(&self, index: usize) -> usize {
        self.offset + 16 + index * self.kind.entry_size()
    }
}

/// An entry that locates flash data (not a value entry).
#[derive(Debug, Clone)]
struct Located {
    /// Offset of the entry in the half.
    entry: usize,
    dir: Kind,
    kind: u8,
    /// The raw location field (address mode bits included).
    value: u64,
    /// Effective address mode (the directory's or the entry's).
    mode: u8,
    /// Data range in the half.
    target: Range<usize>,
}

impl Located {
    fn is_reset(&self) -> bool {
        self.dir == Kind::Bios && self.kind == bios::RESET_IMAGE
    }

    fn mmio(&self) -> bool {
        (MMIO_BASE..1 << 32).contains(&(self.value & ADDRESS_MASK))
    }
}

const ADDRESS_MASK: u64 = (1 << 62) - 1;

/// Every directory reachable from the roots (L1, L2 and any entry pointing
/// at a directory cookie) and every located entry, checksums verified.
struct Tree {
    gen1: bool,
    dirs: Vec<Dir>,
    located: Vec<Located>,
}

impl Tree {
    fn walk(half: &[u8], roots: &[usize], gen1: bool) -> Result<Self, ImageError> {
        let mut queue: Vec<usize> = roots.to_vec();
        let mut seen = BTreeSet::new();
        let (mut dirs, mut located) = (Vec::new(), Vec::new());
        while let Some(offset) = queue.pop() {
            if !seen.insert(offset) {
                continue;
            }
            let dir = Self::directory(half, offset)?;
            for index in 0..dir.count {
                let at = dir.entry(index);
                let (kind, size, value) = (half[at], get32(half, at + 4), get64(half, at + 8));
                if matches!(size, 0 | u32::MAX) || (dir.kind == Kind::Psp && kind == psp::SOFT_FUSE)
                {
                    continue;
                }
                let mode = dir.mode(gen1).max((value >> 62) as u8);
                let address = value & ADDRESS_MASK;
                let start = match mode {
                    0 | 1 if (MMIO_BASE..1 << 32).contains(&address) => address - MMIO_BASE,
                    0 | 1 if !gen1 => address,
                    0 | 1 => return Err(DirError::BadPointer(value).into()),
                    2 => offset as u64 + address,
                    _ => return Err(ImageError::AddressMode { offset: at, mode }),
                };
                let target = usize::try_from(start)
                    .ok()
                    .and_then(|s| Some(s..s.checked_add(size as usize)?))
                    .filter(|r| r.end <= half.len())
                    .ok_or(DirError::BadPointer(value))?;
                if half
                    .get(target.start..target.start + 4)
                    .and_then(Kind::of)
                    .is_some()
                {
                    queue.push(target.start);
                }
                located.push(Located {
                    entry: at,
                    dir: dir.kind,
                    kind,
                    value,
                    mode,
                    target,
                });
            }
            dirs.push(dir);
        }
        Ok(Self {
            gen1,
            dirs,
            located,
        })
    }

    fn directory(half: &[u8], offset: usize) -> Result<Dir, ImageError> {
        let truncated = DirError::Truncated { offset };
        let header = half.get(offset..offset + 16).ok_or(truncated)?;
        let kind = Kind::of(&header[..4]).ok_or(DirError::Cookie { offset })?;
        let dir = Dir {
            offset,
            kind,
            count: get32(header, 8) as usize,
            info: get32(header, 12),
        };
        let body = half.get(offset + 8..dir.table_end()).ok_or(truncated)?;
        let (stored, computed) = (get32(header, 4), fletcher32(body));
        if stored != computed {
            return Err(DirError::Checksum {
                offset,
                stored,
                computed,
            }
            .into());
        }
        Ok(dir)
    }

    /// End of everything the PSP/ABL read: directories with their reserved
    /// area and every located entry except the reset image.
    fn prefix_end(&self) -> usize {
        let dirs = self.dirs.iter().map(|d| d.end(self.gen1));
        let data = self
            .located
            .iter()
            .filter(|l| !l.is_reset())
            .map(|l| l.target.end);
        dirs.chain(data).max().unwrap_or(0)
    }

    fn resets(&self) -> impl Iterator<Item = &Located> {
        self.located.iter().filter(|l| l.is_reset())
    }

    /// Recompute every directory checksum in `half`.
    fn fix_checksums(&self, half: &mut [u8]) {
        for dir in &self.dirs {
            let sum = fletcher32(&half[dir.offset + 8..dir.table_end()]);
            put32(half, dir.offset + 4, sum);
        }
    }
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

/// A vendor half's PSP and BIOS directory trees.
fn vendor_trees(half: &[u8]) -> Result<(Generation, Tree), ImageError> {
    let efs = Efs::new(half)?;
    let generation = generation_of(efs.generation_word())?;
    let gen1 = generation == Generation::Naples;
    let pointer = |value: u32| {
        let v = u64::from(value);
        match v {
            v if (MMIO_BASE..1 << 32).contains(&v) => Ok((v - MMIO_BASE) as usize),
            v if !gen1 && (v as usize) < half.len() => Ok(v as usize),
            v => Err(DirError::BadPointer(v)),
        }
    };
    let psp_root = pointer(efs.psp_pointer())?;
    if Combo::parse(half, psp_root)?.is_some() {
        return Err(ImageError::Layout("vendor half with a combo directory"));
    }
    let bios_root = pointer(efs.bios_pointer(generation))?;
    Ok((generation, Tree::walk(half, &[psp_root, bios_root], gen1)?))
}

impl VendorLayout {
    /// Parse one 16 MiB half: EFS generation, every PSP and BIOS directory
    /// (checksums verified), the `0x62` slot and the prefix end.
    pub fn parse(half: &[u8]) -> Result<Self, ImageError> {
        exact_size(half, HALF_SIZE)?;
        let (generation, tree) = vendor_trees(half)?;
        let resets: Vec<_> = tree.resets().collect();
        let first = resets
            .first()
            .ok_or(ImageError::ResetEntry("no 0x62 entry"))?;
        let destination = |l: &Located| get64(half, l.entry + 16);
        if resets
            .iter()
            .any(|l| l.target != first.target || destination(l) != destination(first))
        {
            return Err(ImageError::ResetEntry("L1 and L2 0x62 entries differ"));
        }
        let reset = ResetSlot {
            flash_offset: first.target.start as u32,
            size: first.target.len() as u32,
            destination: destination(first),
        };
        Ok(Self {
            generation,
            reset,
            prefix_end: tree.prefix_end(),
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
    Efs::new(half)
        .ok()
        .and_then(|efs| generation_of(efs.generation_word()).ok())
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

/// Point every `0x62` of one composed half in `out` (the whole image) at
/// the reset image copy at the half's `plan.reset.flash_offset` (MMIO form
/// for MMIO entries).
fn rewrite_half(out: &mut [u8], half: FlashHalf, tree: &Tree, plan: &FlashPlan) {
    let bytes = &mut out[half.range()];
    for l in tree.resets() {
        let source = u64::from(plan.reset.flash_offset) + if l.mmio() { MMIO_BASE } else { 0 };
        put64(bytes, l.entry + 8, l.value & !ADDRESS_MASK | source);
        put32(bytes, l.entry + 4, plan.reset.size);
        put64(bytes, l.entry + 16, plan.reset.destination);
        // Flags: reset + copy, not compressed; keep read-only/instance.
        bytes[l.entry + 2] = bytes[l.entry + 2] & !0x0b | 0x03;
    }
    tree.fix_checksums(bytes);
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
        let (_, tree) = vendor_trees(&vendor_rom[half.range()])?;
        out[half.range()][layout.prefix_end..].fill(0xff);
        rewrite_half(&mut out, half, &tree, plan);
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
        let efs = Efs::new(&image[planned.offset()..])?;
        efs.psp_directories(generation)?;
        let dirs = efs.bios_directories(generation)?;
        let expected = planned.offset() + plan.reset.flash_offset as usize;
        let resets = dirs
            .levels()
            .flat_map(|d| d.entries().iter().map(move |e| (d, e)))
            .filter(|(_, e)| e.kind() == bios::RESET_IMAGE);
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
        for &offset in EFS_SEARCH.iter().filter(|&&o| o != EFS_OFFSET) {
            let at = half.offset() + offset;
            if get32(image, at) == EFS_SIGNATURE {
                return Err(ImageError::StrayEfs { offset: at });
            }
        }
    }
    Ok(())
}

/// The half a generation's PSP boot ROM boots from (lower, then upper;
/// Naples only lower).
pub fn booted_half(image: &[u8], generation: Generation) -> Option<FlashHalf> {
    booted_half_offset(image, generation).map(|offset| {
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
        s.collect_seq(gs.iter().map(|g| g.name().to_ascii_lowercase()))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<Generation>, D::Error> {
        Vec::<String>::deserialize(d)?
            .iter()
            .map(|name| {
                GENERATIONS
                    .into_iter()
                    .find(|g| g.name().eq_ignore_ascii_case(name))
                    .ok_or_else(|| D::Error::custom(format!("unknown SP3 generation {name}")))
            })
            .collect()
    }
}

#[cfg(test)]
#[path = "sp3_flash_tests.rs"]
mod tests;
