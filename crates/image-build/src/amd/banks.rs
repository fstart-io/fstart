// SPDX-License-Identifier: MIT OR Apache-2.0
//! AMD vendor-bank geometry and boot selection. This module discovers which
//! banks to compose or preserve; byte composition lives in `assembly` for both
//! fixed-prefix images and multi-generation/mirrored vendor images.
use core::{fmt, ops::Range};
use fstart_amdfw_format::directory::{BiosEntry, Combo, Directory, Entry, Levels, PspEntry, bios};
use fstart_amdfw_format::efs::{self, BiosDirectorySlot, Efs};
use fstart_amdfw_format::{Error as DirError, Flash, Profile, ROM2_BASE as MMIO_BASE};
use serde::{Deserialize, Serialize};

pub use fstart_amdfw_format::HALF_SIZE;
pub type Generation = BiosDirectorySlot;
pub const NAPLES: Generation = BiosDirectorySlot::Family17hModel00;
pub const ROME: Generation = BiosDirectorySlot::Family17hModel30;
pub const MILAN: Generation = BiosDirectorySlot::Family19h;
const GENERATIONS: [Generation; 3] = [NAPLES, ROME, MILAN];
const EFS_OFFSET: usize = 0x2_0000;

pub fn generation_of(word: u32) -> Result<Generation, ImageError> {
    GENERATIONS
        .into_iter()
        .find(|g| !Profile::for_slot(*g).generation_mask == word)
        .ok_or(ImageError::UnknownGeneration(word))
}
pub const fn name(generation: Generation) -> &'static str {
    match generation {
        NAPLES => "naples",
        ROME => "rome",
        MILAN => "milan",
        BiosDirectorySlot::Family17hModel10 => "picasso",
    }
}
const fn flash(generation: Generation, bytes: &[u8]) -> Flash<'_> {
    Flash::region(bytes, 0, MMIO_BASE, Profile::for_slot(generation))
}

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

/// Reset-image placement within one boot bank; all addresses are explicit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResetSlot {
    pub flash_offset: u32,
    pub size: u32,
    pub destination: u64,
}
impl ResetSlot {
    pub const fn dram_end(self) -> u64 {
        self.destination + self.size as u64
    }
    fn check(self) -> Result<(), ImageError> {
        if self.size < 0x1000 || self.flash_offset.checked_add(self.size) != Some(HALF_SIZE as u32)
        {
            return Err(ImageError::InvalidSlot(self));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImageError {
    Size {
        expected: usize,
        actual: usize,
    },
    Directory(DirError),
    UnknownGeneration(u32),
    NoSupportedHalf,
    ResetEntry(&'static str),
    InvalidSlot(ResetSlot),
    PrefixOverlap {
        prefix_end: usize,
        limit: usize,
    },
    BootOrder {
        half: FlashHalf,
        generation: Generation,
        booted: Option<FlashHalf>,
    },
    Plan(&'static str),
    Layout(&'static str),
    StrayEfs {
        offset: usize,
    },
}
impl From<DirError> for ImageError {
    fn from(e: DirError) -> Self {
        Self::Directory(e)
    }
}
impl fmt::Display for ImageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "AMD bank layout: {self:x?}")
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VendorLayout {
    pub generation: Generation,
    pub reset: ResetSlot,
    /// Every reachable directory (including reserved extent) and file except reset.
    pub prefix_end: usize,
}
fn vendor_efs(half: &[u8]) -> Result<&Efs, ImageError> {
    let any = Profile {
        generation_mask: 0,
        ..Profile::for_slot(NAPLES)
    };
    Ok(Flash::region(half, 0, MMIO_BASE, any).efs_checked()?.1)
}
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
            return Err(ImageError::Layout("vendor bank with a combo directory"));
        }
        Ok(Self {
            generation,
            psp: flash.psp_directories()?,
            bios: flash.bios_directories_checked(generation)?,
        })
    }
    fn resets(&self) -> impl Iterator<Item = Result<(Range<usize>, u64), DirError>> + '_ {
        self.bios.iter().flat_map(|dir| {
            dir.entries
                .iter()
                .filter(|e| e.kind == bios::RESET_IMAGE && e.has_data())
                .map(move |e| Ok((dir.range(e)?, e.destination())))
        })
    }
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
        self.psp
            .iter()
            .map(|dir| end(dir, |_| true))
            .chain(
                self.bios
                    .iter()
                    .map(|dir| end(dir, |e: &BiosEntry| e.kind != bios::RESET_IMAGE)),
            )
            .try_fold(0, |max, end| Ok(max.max(end?)))
    }
}
impl VendorLayout {
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
        Ok(Self {
            generation: vendor.generation,
            reset: ResetSlot {
                flash_offset: target.start as u32,
                size: target.len() as u32,
                destination,
            },
            prefix_end: vendor.prefix_end()?,
        })
    }
}
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
pub fn half_generation(half: &[u8]) -> Option<Generation> {
    vendor_efs(half)
        .ok()
        .and_then(|efs| generation_of(efs.generation.get()).ok())
}

/// Vendor banks described by EFS generation words, with optional boot-bank
/// mirrors in a larger readout. No board identity is selected here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BankLayout {
    #[serde(with = "generation_names")]
    pub generations: Vec<Generation>,
    pub reset: ResetSlot,
    pub ffs_offset: u32,
    pub dram_floor: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mirrors: Vec<u32>,
}
/// One generation's two-level PSP partition, before common AMD composition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackedHalf {
    #[serde(with = "generation_name")]
    pub generation: Generation,
    pub files: crate::amd::assembly::PspFiles,
    pub apob_destination: u64,
}
impl PackedHalf {
    pub fn pack(
        &self,
        plan: &BankLayout,
        read: &dyn Fn(&str) -> Result<Vec<u8>, String>,
    ) -> Result<Vec<u8>, String> {
        let files = &self.files;
        if files.flash_size as usize != HALF_SIZE
            || files.efs_offset as usize != EFS_OFFSET
            || files.partition_end > plan.ffs_offset
        {
            return Err(format!(
                "{}: the bank partition must run from the EFS to below the FFS",
                name(self.generation)
            ));
        }
        let reset = crate::amd::levels::Reset {
            offset: plan.reset.flash_offset,
            size: plan.reset.size,
            destination: plan.reset.destination,
            compressed: false,
        };
        let packed = files
            .two_level(
                self.generation,
                |file| read(&file.path),
                reset,
                self.apob_destination,
            )?
            .pack()?;
        let mut half = vec![0xff; HALF_SIZE];
        half[packed.offset as usize..][..packed.bytes.len()].copy_from_slice(&packed.bytes);
        Ok(half)
    }
}
pub fn packed_rom(
    halves: &[PackedHalf],
    plan: &BankLayout,
    read: &dyn Fn(&str) -> Result<Vec<u8>, String>,
) -> Result<Vec<u8>, String> {
    if halves.is_empty() || halves.len() > 2 {
        return Err("a banked flash has one or two halves".into());
    }
    let mut rom = Vec::with_capacity(halves.len() * HALF_SIZE);
    for half in halves {
        if !plan.generations.contains(&half.generation) {
            return Err(format!(
                "the board doesn't support {}",
                name(half.generation)
            ));
        }
        rom.extend(half.pack(plan, read)?);
    }
    if rom.len() == 2 * HALF_SIZE {
        check_dual(&rom)?;
    }
    Ok(rom)
}
mod generation_name {
    use super::Generation;
    use serde::{Deserialize, Deserializer, Serializer};
    pub fn serialize<S: Serializer>(g: &Generation, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(super::name(*g))
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Generation, D::Error> {
        let name = String::deserialize(d)?;
        super::GENERATIONS
            .into_iter()
            .find(|&g| super::name(g).eq_ignore_ascii_case(&name))
            .ok_or_else(|| serde::de::Error::custom(format!("unknown bank generation {name}")))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HalfAction {
    Compose(VendorLayout),
    Keep(Option<Generation>),
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Survey {
    pub halves: Vec<(FlashHalf, HalfAction)>,
    pub ffs: Range<u32>,
}
impl Survey {
    pub fn ffs_capacity(&self) -> usize {
        self.ffs.len()
    }
    pub fn composed(&self) -> impl Iterator<Item = (FlashHalf, VendorLayout)> + '_ {
        self.halves
            .iter()
            .filter_map(|(half, action)| match action {
                HalfAction::Compose(layout) => Some((*half, *layout)),
                HalfAction::Keep(_) => None,
            })
    }
}
impl BankLayout {
    pub fn check(&self) -> Result<(), ImageError> {
        self.reset.check()?;
        if self.generations.is_empty() || self.generations.iter().any(|g| !GENERATIONS.contains(g))
        {
            return Err(ImageError::Plan(
                "no supported generation or unknown bank profile",
            ));
        }
        if self.reset.destination < self.dram_floor {
            return Err(ImageError::Plan("reset image below the DRAM floor"));
        }
        if self.ffs_offset == 0 || self.ffs_offset >= self.reset.flash_offset {
            return Err(ImageError::Plan("FFS offset outside the lower half"));
        }
        let mut mirrors = self.mirrors.clone();
        mirrors.sort_unstable();
        mirrors.dedup();
        if mirrors.len() != self.mirrors.len()
            || mirrors
                .iter()
                .any(|&m| !(m as usize).is_multiple_of(HALF_SIZE) || (m as usize) < HALF_SIZE)
        {
            return Err(ImageError::Plan(
                "mirrors are not distinct banks above the boot bank",
            ));
        }
        Ok(())
    }
    pub fn boot_window<'a>(&self, vendor_rom: &'a [u8]) -> Result<&'a [u8], ImageError> {
        if self.mirrors.is_empty() || vendor_rom.len() == HALF_SIZE {
            return Ok(vendor_rom);
        }
        let end = self.mirrors.iter().map(|&m| m as usize + HALF_SIZE).max();
        match end {
            Some(end) if vendor_rom.len() >= end && vendor_rom.len().is_multiple_of(HALF_SIZE) => {
                Ok(&vendor_rom[..HALF_SIZE])
            }
            _ => Err(ImageError::Layout(
                "the vendor readout does not hold the board's bank mirrors",
            )),
        }
    }
    pub(super) fn expand(&self, vendor_rom: &[u8], window: Vec<u8>) -> Vec<u8> {
        if window.len() == vendor_rom.len() {
            return window;
        }
        let mut out = vendor_rom.to_vec();
        for at in core::iter::once(0).chain(self.mirrors.iter().map(|&m| m as usize)) {
            out[at..at + window.len()].copy_from_slice(&window);
        }
        out
    }
    pub fn survey(&self, vendor_rom: &[u8]) -> Result<Survey, ImageError> {
        self.check()?;
        let reset_at = self.reset.flash_offset as usize;
        let halves = halves(self.boot_window(vendor_rom)?)?
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
        let start = (self.ffs_offset as usize).max(lower.prefix_end.div_ceil(0x1000) * 0x1000);
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
    /// Boot-order and stray-EFS checks supplement the common composer's
    /// directory/dependency/reset-byte readback checks.
    pub(super) fn verify_boot_order(
        &self,
        image: &[u8],
        survey: &Survey,
    ) -> Result<(), ImageError> {
        for (half, layout) in survey.composed() {
            let booted = booted_half(image, layout.generation);
            if booted != Some(half) {
                return Err(ImageError::BootOrder {
                    half,
                    generation: layout.generation,
                    booted,
                });
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
}
pub fn booted_half(image: &[u8], generation: Generation) -> Option<FlashHalf> {
    let origins: &[usize] = if generation == NAPLES {
        &[0]
    } else {
        &[0, HALF_SIZE]
    };
    fstart_amdfw_format::booted_half(image, MMIO_BASE, Profile::for_slot(generation), origins).map(
        |offset| {
            if offset == 0 {
                FlashHalf::Lower
            } else {
                FlashHalf::Upper
            }
        },
    )
}
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
                    .ok_or_else(|| D::Error::custom(format!("unknown AMD bank generation {name}")))
            })
            .collect()
    }
}

#[cfg(test)]
#[path = "banks_tests.rs"]
mod tests;
