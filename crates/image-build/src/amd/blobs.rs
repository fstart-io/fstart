// SPDX-License-Identifier: MIT OR Apache-2.0
//! The AMD firmware an image is packed from, as two Cargo.toml tables:
//!
//! - the SoC's ([`SocBlobs`], `[package.metadata.fstart.amd.<soc>]` of the
//!   platform crate): the PSP partition's geometry and settings, and which
//!   file of a blob directory each PSP and BIOS directory entry (in which
//!   levels of a two-level partition), microcode patch and video BIOS is;
//! - the board's ([`BoardBlobs`], `[package.metadata.fstart.amd]` of the
//!   board crate): its SoC, its APCB instances and whether its PSP needs
//!   fstart's PSP application.
//!
//! [`resolve`] turns both and a blob directory into the files an image is
//! packed from. [`extract`] fills a blob directory from a vendor flash image:
//! directory entries by their type, microcode patches and video BIOSes by
//! what they are, wherever the image keeps them.
use super::{
    Key,
    assembly::{ApcbFiles, FileBlob, Fixed, FixedEntry, PspFiles},
    ish::IshSlot,
    levels::Level,
};
use fstart_amdfw_format::{Flash, Profile, directory::bios, efs::BiosDirectorySlot, microcode};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// A SoC's AMD firmware: `[package.metadata.fstart.amd.<soc>]`. File paths
/// are relative to the blob directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct SocBlobs {
    /// The EFS field the SoC's PSP takes the BIOS directory from.
    pub bios_directory: BiosDirectorySlot,
    /// `$PSP` + `$PL2` and `$BHD` + `$BL2` instead of a single level.
    #[serde(default = "one")]
    pub levels: u8,
    /// The EFS generation bits (+0x24) the SoC's boot ROM requires clear,
    /// if not those of its BIOS directory slot: Siena's vendor images keep
    /// bit 1 of the Family 19h slot's set.
    #[serde(default)]
    pub generation_mask: Option<u32>,
    /// Platform uses PSP L1 recovery entries (`0x48`/`0x4A`) -> 32-byte ISH A/B
    /// slots -> `$PL2` -> `0x49` -> `$BL2` with dual-origin mode-1 BIOS entries.
    #[serde(default)]
    pub ish: bool,
    /// Flash offset of the 16 MiB bank holding the EFS and ISH A/B slots.
    #[serde(default)]
    pub image_origin: u32,
    /// Flash offset of the 16 MiB bank used as the mode-1 origin for BIOS
    /// entries on dual-origin ISH images.
    #[serde(default)]
    pub bios_origin: Option<u32>,
    /// Expected PSP ID in ISH headers (and combo directories).
    #[serde(default)]
    pub psp_id: Option<u32>,
    /// Flash offset of the EFS, the start of the PSP partition.
    pub efs_offset: u32,
    /// End of the PSP partition.
    pub partition_end: u32,
    /// The PSP's SPI read mode and speed (EFS), erased if absent.
    #[serde(default)]
    pub spi_read_mode: Option<u8>,
    #[serde(default)]
    pub spi_speed: Option<u8>,
    #[serde(default)]
    pub micron: bool,
    /// PSP directory entry 0x0b of a single-level partition; a two-level
    /// one lists it with its `value`.
    #[serde(default)]
    pub soft_fuse: Option<u64>,
    /// The APOB's DRAM destination (BIOS entry 0x61), for platforms that
    /// read it from the directory rather than fixing it.
    #[serde(default)]
    pub apob_destination: Option<u64>,
    pub psp: Vec<DirectoryFile>,
    pub bios: Vec<DirectoryFile>,
    /// Patches the FFS carries for MP init.
    #[serde(default)]
    pub microcode: Vec<MicrocodeFile>,
    /// Video BIOSes the FFS carries, by the PCI device they are for.
    #[serde(default)]
    pub vbios: Vec<VbiosFile>,
}

const fn one() -> u8 {
    1
}

impl SocBlobs {
    /// How the SoC's boot ROM reads a flash image.
    pub fn profile(&self) -> Profile {
        let mut profile = Profile::for_slot(self.bios_directory);
        if let Some(mask) = self.generation_mask {
            profile.generation_mask = mask;
        }
        if self.ish {
            profile.ish = true;
            profile.psp_id = self.psp_id;
        }
        profile
    }

    /// A [`Flash`] view of `rom` configured for this SoC's EFS/BIOS origins.
    pub fn flash<'a>(&self, rom: &'a [u8]) -> Flash<'a> {
        if self.ish {
            let mut flash =
                Flash::region(rom, self.image_origin as usize, 0xff00_0000, self.profile());
            if let Some(bo) = self.bios_origin {
                flash = flash.with_bios_origin(bo as usize);
            }
            flash
        } else {
            Flash::new(rom).with_profile(self.profile())
        }
    }
}

/// A PSP or BIOS directory entry: its file, or the erased flash or value it
/// describes instead.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct DirectoryFile {
    #[serde(rename = "type")]
    pub kind: u8,
    #[serde(default)]
    pub subprogram: u8,
    #[serde(default)]
    pub instance: u8,
    /// The level of a two-level partition, 1 or 2; both if absent.
    #[serde(default)]
    pub level: Option<u8>,
    #[serde(default)]
    pub file: Option<String>,
    /// Bytes of erased flash the firmware writes (NV data).
    #[serde(default)]
    pub erased: Option<u32>,
    /// The entry's value (the PSP soft fuse chain).
    #[serde(default)]
    pub value: Option<u64>,
    /// Restrict an ISH entry to slot `"a"` (`0x48`) or `"b"` (`0x4A`); both if absent.
    #[serde(default)]
    pub slot: Option<IshSlot>,
    /// Mark the directory entry writable and 4 KiB-align its allocation (ISH).
    #[serde(default)]
    pub writable: bool,
}
impl DirectoryFile {
    pub fn key(&self) -> Key {
        Key::new(self.kind, self.subprogram, self.instance)
    }
    /// What the entry describes: exactly one of a file, erased flash or a
    /// value.
    fn content(&self) -> Result<Content<'_>, String> {
        match (&self.file, self.erased, self.value) {
            (Some(file), None, None) => Ok(Content::File(file)),
            (None, Some(size), None) => Ok(Content::Fixed(Fixed::Erased(size))),
            (None, None, Some(value)) => Ok(Content::Fixed(Fixed::Value(value))),
            _ => Err(format!(
                "AMD entry {:?} needs exactly one of file, erased or value",
                self.key()
            )),
        }
    }
}
enum Content<'a> {
    File(&'a str),
    Fixed(Fixed),
}

/// A microcode patch, by the CPU equivalence ID it is for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct MicrocodeFile {
    pub equivalence_id: u16,
    pub file: String,
}

/// A video BIOS (legacy option ROM), by its PCI vendor and device ID.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct VbiosFile {
    #[serde(default = "amd_vendor")]
    pub vendor: u16,
    pub device: u16,
    pub file: String,
}
const fn amd_vendor() -> u16 {
    0x1002
}
impl VbiosFile {
    /// The name the firmware looks it up by in the FFS.
    pub fn asset_name(&self) -> String {
        format!("pci{:04x},{:04x}.rom", self.vendor, self.device)
    }
}

/// A board's part: `[package.metadata.fstart.amd]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct BoardBlobs {
    /// The SoC table of the platform crate. A board of several SoCs (the
    /// SP3 generations) has one table per SoC, named after it, instead.
    #[serde(default)]
    pub soc: String,
    /// Pack fstart's PSP application ([`super::psp_app`]).
    #[serde(default)]
    pub psp_app: bool,
    pub apcb: BoardApcb,
    /// The APOB's DRAM destination (BIOS entry 0x61), if the board's
    /// firmware moves it from the platform's.
    #[serde(default)]
    pub apob_destination: Option<u64>,
    /// The eSPI configuration the board's PSP applies before x86 runs
    /// (EFS +0x50: eSPI0 config 0, eSPI1 config 0, eSPI0 config 1, eSPI1
    /// config 1), e.g. for a BMC on eSPI. Erased if absent.
    #[serde(default)]
    pub efs_espi: Option<[u8; 4]>,
    /// Opaque board-qualified Venice EFS byte following the ROM layout selector.
    #[serde(default)]
    pub efs_byte_59: Option<u8>,
}

/// The board tables of `table` (`[package.metadata.fstart.amd]`): itself
/// with its `soc`, or one per SoC, named after it.
pub fn board_tables(table: &serde_json::Value) -> Result<Vec<BoardBlobs>, String> {
    if table.get("soc").is_some() {
        let board: BoardBlobs = serde_json::from_value(table.clone())
            .map_err(|e| format!("board [package.metadata.fstart.amd]: {e}"))?;
        return Ok(vec![board]);
    }
    let tables = table
        .as_object()
        .filter(|tables| !tables.is_empty())
        .ok_or("the board has no [package.metadata.fstart.amd] table")?;
    tables
        .iter()
        .map(|(soc, table)| {
            let mut board: BoardBlobs = serde_json::from_value(table.clone())
                .map_err(|e| format!("board [package.metadata.fstart.amd.{soc}]: {e}"))?;
            if !board.soc.is_empty() && board.soc != *soc {
                return Err(format!("board table {soc} names SoC {}", board.soc));
            }
            board.soc = soc.clone();
            Ok(board)
        })
        .collect()
}

/// The 16 MiB image in `rom` (a flash of several, or the 16 MiB of a
/// smaller flash's mapping) whose EFS the SoC's boot ROM takes
/// ([`Flash::efs_checked`]: the generation word's required-clear bits) with
/// the SoC's BIOS directory. In a flash of several generations a later
/// generation's EFS can satisfy an earlier one's boot ROM too: the half
/// with exactly the SoC's generation word wins.
pub fn donor_image<'a>(rom: &'a [u8], soc: &SocBlobs) -> Option<&'a [u8]> {
    let profile = soc.profile();
    if soc.ish {
        let flash = soc.flash(rom);
        flash.efs_checked().ok()?;
        let (_, count) = flash.ish_slots_checked().ok()?;
        return (count > 0).then_some(rom);
    }
    let candidates = rom
        .chunks(fstart_amdfw_format::HALF_SIZE)
        .filter_map(|image| {
            let (_, efs) = Flash::new(image).with_profile(profile).efs_checked().ok()?;
            efs.bios_directory(soc.bios_directory)?;
            Some((image, efs.generation.get() == !profile.generation_mask))
        });
    let mut first = None;
    for (image, exact) in candidates {
        if exact {
            return Some(image);
        }
        first.get_or_insert(image);
    }
    first
}

/// Where a board's APCB instances come from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum BoardApcb {
    /// `instances` files, one per RAM ID, as the board's vendor ROM has
    /// them: `<board>/apcb/<n>.bin` in the blob directory ([`extract`]
    /// writes them), each the APCB (0x60) and its backup (0x68).
    Files { instances: u8 },
    /// BIOS directory entries with the board's files, for the APCB sets
    /// of the server SoCs (several types, subprograms and levels).
    Entries { entries: Vec<DirectoryFile> },
    /// Generated from a template ([`ApcbFiles`]), paths in the blob
    /// directory.
    Template(ApcbFiles),
}

/// The path of a board's APCB instance in a blob directory.
pub fn apcb_file(board: &str, instance: u8) -> String {
    format!("{board}/apcb/{instance}.bin")
}

/// What an image is packed from: the PSP partition's files (absolute
/// paths), the microcode files and the data assets of the FFS.
pub struct Resolved {
    pub files: PspFiles,
    pub microcode: Vec<String>,
}

/// The files of `soc` and `board` in the blob directory `blobs`, for a
/// flash of `flash_size` bytes.
pub fn resolve(
    soc: &SocBlobs,
    board: &BoardBlobs,
    board_name: &str,
    blobs: &Path,
    flash_size: u32,
) -> Result<Resolved, String> {
    let path = |file: &str| -> Result<String, String> {
        if file.is_empty() || Path::new(file).is_absolute() || file.contains("..") {
            return Err(format!(
                "blob path {file:?} is not relative to the blob directory"
            ));
        }
        Ok(blobs.join(file).to_string_lossy().into_owned())
    };
    if !(1..=2).contains(&soc.levels) {
        return Err(format!("{} directory levels", soc.levels));
    }
    if !soc.ish && board.efs_byte_59.is_some() {
        return Err("efs-byte-59 requires a Venice ISH image".into());
    }
    if soc.ish {
        if soc.levels != 2 {
            return Err("ISH partitions require levels = 2".into());
        }
        let bo = soc
            .bios_origin
            .ok_or("ISH partitions require an explicit bios-origin")?;
        if fstart_amdfw_format::efs::Efs::venice_layout_selector_for(soc.image_origin, bo).is_none()
        {
            return Err(format!(
                "unsupported Venice ISH origins (image={:#x}, bios={:#x})",
                soc.image_origin, bo
            ));
        }
    }
    let mut fixed = Vec::new();
    let mut directory = |files: &[DirectoryFile], psp: bool| -> Result<Vec<FileBlob>, String> {
        let mut out = Vec::new();
        for f in files {
            let level = Level::from_table(f.level)?;
            if soc.levels == 1 && f.level.is_some() {
                return Err(format!(
                    "AMD entry {:?} has a level in a single-level partition",
                    f.key()
                ));
            }
            if soc.ish && f.level.is_some() {
                return Err(format!(
                    "ISH entry {:?} cannot set level; use slot = \"a\"|\"b\" instead",
                    f.key()
                ));
            }
            if !soc.ish && f.slot.is_some() {
                return Err(format!("non-ISH entry {:?} cannot set slot", f.key()));
            }
            match f.content()? {
                Content::File(file) => out.push(FileBlob {
                    key: f.key(),
                    path: path(file)?,
                    level,
                    slot: f.slot,
                    writable: f.writable,
                }),
                Content::Fixed(content) => fixed.push(FixedEntry {
                    psp,
                    key: f.key(),
                    level,
                    slot: f.slot,
                    writable: f.writable,
                    content,
                }),
            }
        }
        Ok(out)
    };
    let psp = directory(&soc.psp, true)?;
    let mut bios = directory(&soc.bios, false)?;
    let apcb = match &board.apcb {
        BoardApcb::Files { instances } => {
            for instance in 0..*instances {
                let file = path(&apcb_file(board_name, instance))?;
                for kind in [bios::APCB, bios::APCB_BACKUP] {
                    bios.push(FileBlob {
                        key: Key::new(kind, 0, instance),
                        path: file.clone(),
                        level: Level::Both,
                        slot: None,
                        writable: false,
                    });
                }
            }
            None
        }
        BoardApcb::Entries { entries } => {
            bios.extend(directory(entries, false)?);
            None
        }
        BoardApcb::Template(apcb) => Some(ApcbFiles {
            template: path(&apcb.template)?,
            spds: apcb
                .spds
                .iter()
                .map(|spd| path(spd))
                .collect::<Result<_, _>>()?,
            ..apcb.clone()
        }),
    };
    if !fixed.is_empty() && soc.levels == 1 {
        return Err("erased and value entries need a two-level partition".into());
    }
    let assets = soc
        .vbios
        .iter()
        .map(|vbios| {
            Ok(fstart_core::board::FileAsset {
                name: vbios
                    .asset_name()
                    .as_str()
                    .try_into()
                    .map_err(|_| "video BIOS name too long")?,
                path: path(&vbios.file)?
                    .as_str()
                    .try_into()
                    .map_err(|_| "video BIOS path too long")?,
            })
        })
        .collect::<Result<_, String>>()?;
    Ok(Resolved {
        files: PspFiles {
            flash_size,
            efs_offset: soc.efs_offset,
            partition_end: soc.partition_end,
            spi_read_mode: soc.spi_read_mode,
            spi_speed: soc.spi_speed,
            micron: soc.micron,
            soft_fuse: soc.soft_fuse,
            two_level: soc.levels == 2 && !soc.ish,
            ish: soc.ish,
            image_origin: soc.image_origin,
            bios_origin: soc.bios_origin.unwrap_or(soc.image_origin),
            psp_id: soc.psp_id,
            generation_mask: soc.generation_mask,
            espi: board.efs_espi,
            efs_byte_59: board.efs_byte_59,
            psp,
            bios,
            fixed,
            apob_nv: None,
            psp_nv: None,
            apcb,
            assets,
            psp_app: board.psp_app,
        },
        microcode: soc
            .microcode
            .iter()
            .map(|m| path(&m.file))
            .collect::<Result<_, _>>()?,
    })
}

/// A file [`extract`] found: where it goes in the blob directory and where
/// it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Extracted {
    pub file: String,
    pub bytes: Vec<u8>,
    pub origin: String,
}

/// The blob directory's files of `soc` and `board`, taken from the vendor
/// flash image `rom`.
/// Returns what was found, and the files that were not.
pub fn extract(
    soc: &SocBlobs,
    board: &BoardBlobs,
    board_name: &str,
    rom: &[u8],
) -> Result<(Vec<Extracted>, Vec<String>), String> {
    let slot = soc.bios_directory;
    let flash = soc.flash(rom);
    let mut found = Vec::new();
    let mut missing = Vec::new();
    // Entries may share a file only if the vendor image has the same
    // bytes for them.
    let mut conflict = None;
    let mut take = |file: &str, origin: Option<(String, &[u8])>| match origin {
        Some((origin, bytes)) => match found.iter().find(|f: &&Extracted| f.file == file) {
            Some(prior) if prior.bytes == bytes => {}
            Some(prior) => {
                conflict.get_or_insert(format!(
                    "{file}: the vendor's {} and {origin} differ",
                    prior.origin
                ));
            }
            None => found.push(Extracted {
                file: file.into(),
                bytes: bytes.to_vec(),
                origin,
            }),
        },
        None => missing.push(file.into()),
    };
    let board_entries = match &board.apcb {
        BoardApcb::Entries { entries } => entries.as_slice(),
        _ => &[],
    };
    if soc.ish {
        if soc.bios_origin.is_none() {
            return Err("ISH extraction requires an explicit bios-origin".into());
        }
        let (slots, count) = flash
            .ish_slots_checked()
            .map_err(|e| format!("vendor ISH slots: {e:?}"))?;
        let find_slot = |which: IshSlot| {
            slots[..count]
                .iter()
                .flatten()
                .find(|s| s.kind == which.psp_kind())
                .ok_or_else(|| format!("vendor ISH image is missing {}", which.label()))
        };
        let slot_a = find_slot(IshSlot::A)?;
        let slot_b = find_slot(IshSlot::B)?;
        let (psp_a, bios_a) = flash
            .ish_slot_directories(slot_a)
            .map_err(|e| format!("vendor ISH slot A directories: {e:?}"))?;
        let (psp_b, bios_b) = flash
            .ish_slot_directories(slot_b)
            .map_err(|e| format!("vendor ISH slot B directories: {e:?}"))?;
        let bios_a = bios_a.ok_or("vendor ISH slot A has no BIOS directory")?;
        let bios_b = bios_b.ok_or("vendor ISH slot B has no BIOS directory")?;
        for f in &soc.psp {
            let Content::File(file) = f.content()? else {
                continue;
            };
            if IshSlot::matches(f.slot, IshSlot::A) {
                take(file, dir_file(psp_a, f, "PSP slot A"));
            }
            if IshSlot::matches(f.slot, IshSlot::B) {
                take(file, dir_file(psp_b, f, "PSP slot B"));
            }
        }
        for f in soc.bios.iter().chain(board_entries) {
            let Content::File(file) = f.content()? else {
                continue;
            };
            if IshSlot::matches(f.slot, IshSlot::A) {
                take(file, dir_file(bios_a, f, "BIOS slot A"));
            }
            if IshSlot::matches(f.slot, IshSlot::B) {
                take(file, dir_file(bios_b, f, "BIOS slot B"));
            }
        }
        if let BoardApcb::Files { instances } = board.apcb {
            for instance in 0..instances {
                let apcb = DirectoryFile {
                    kind: bios::APCB,
                    instance,
                    ..Default::default()
                };
                take(
                    &apcb_file(board_name, instance),
                    dir_file(bios_a, &apcb, "BIOS slot A"),
                );
                take(
                    &apcb_file(board_name, instance),
                    dir_file(bios_b, &apcb, "BIOS slot B"),
                );
            }
        }
        for m in &soc.microcode {
            let patch = find_microcode_in_dirs(rom, &[bios_a, bios_b], m.equivalence_id)?;
            take(&m.file, patch);
        }
    } else {
        let psp = flash
            .psp_directories()
            .map_err(|e| format!("vendor PSP directory: {e:?}"))?;
        let bios_dirs = flash
            .bios_directories_checked(slot)
            .map_err(|e| format!("vendor BIOS directory: {e:?}"))?;
        for f in &soc.psp {
            let Content::File(file) = f.content()? else {
                continue;
            };
            take(file, level_file(&psp, f, "PSP")?);
        }
        for f in soc.bios.iter().chain(board_entries) {
            let Content::File(file) = f.content()? else {
                continue;
            };
            take(file, level_file(&bios_dirs, f, "BIOS")?);
        }
        if let BoardApcb::Files { instances } = board.apcb {
            for instance in 0..instances {
                let apcb = DirectoryFile {
                    kind: bios::APCB,
                    instance,
                    ..Default::default()
                };
                take(
                    &apcb_file(board_name, instance),
                    level_file(&bios_dirs, &apcb, "BIOS")?,
                );
            }
        }
        let dirs: Vec<_> = bios_dirs
            .level2
            .into_iter()
            .chain(std::iter::once(bios_dirs.level1))
            .collect();
        for m in &soc.microcode {
            let patch = find_microcode_in_dirs(rom, &dirs, m.equivalence_id)?;
            take(&m.file, patch);
        }
    }
    for v in &soc.vbios {
        take(&v.file, find_vbios(rom, v.vendor, v.device)?);
    }
    if let Some(conflict) = conflict {
        return Err(conflict);
    }
    Ok((found, missing))
}

fn dir_file<'a, E: fstart_amdfw_format::directory::Entry>(
    dir: fstart_amdfw_format::directory::Directory<'a, E>,
    f: &DirectoryFile,
    name: &str,
) -> Option<(String, &'a [u8])> {
    let entry = dir.entries.iter().find(|e| {
        e.kind() == f.kind && e.instance() == f.instance && e.subprogram() == f.subprogram
    })?;
    Some((
        format!("{name} directory {:#x}", dir.offset),
        dir.data(entry).ok()?,
    ))
}

/// The file of entry `f` in the levels its table puts it in: level 2
/// first for both, as the PSP looks it up.
fn level_file<'a, E: fstart_amdfw_format::directory::Entry>(
    levels: &fstart_amdfw_format::directory::Levels<'a, E>,
    f: &DirectoryFile,
    name: &str,
) -> Result<Option<(String, &'a [u8])>, String> {
    let directories: Vec<_> = match Level::from_table(f.level)? {
        Level::One => vec![levels.level1],
        Level::Two => levels.level2.into_iter().collect(),
        Level::Both => levels.level2.into_iter().chain([levels.level1]).collect(),
    };
    Ok(directories.into_iter().find_map(|dir| {
        let entry = dir.entries.iter().find(|e| {
            e.kind() == f.kind && e.instance() == f.instance && e.subprogram() == f.subprogram
        })?;
        Some((
            format!("{name} directory {:#x}", dir.offset),
            dir.data(entry).ok()?,
        ))
    }))
}

/// The newest patch for `equivalence_id`: from the BIOS directory if it has
/// one, else wherever in the image one is (e.g. a file of another firmware
/// file system).
fn find_microcode_in_dirs<'a>(
    rom: &'a [u8],
    bios_dirs: &[fstart_amdfw_format::directory::Directory<
        'a,
        fstart_amdfw_format::directory::BiosEntry,
    >],
    equivalence_id: u16,
) -> Result<Option<(String, &'a [u8])>, String> {
    let size = microcode::patch_size(equivalence_id)
        .ok_or_else(|| format!("no patch format for CPUs {equivalence_id:#06x}"))?;
    let patch_id = |p: &[u8]| u32::from_le_bytes([p[4], p[5], p[6], p[7]]);
    let plausible = |p: &[u8]| {
        p.len() == size
            && microcode::equivalence_id(p) == Some(equivalence_id)
            && patch_id(p) != 0
            // No chipset restriction (northbridge/southbridge ID and revision).
            && p[0x10..0x18].iter().all(|&b| b == 0)
            && p[0x1a..0x1c].iter().all(|&b| b == 0)
    };
    let in_directory = bios_dirs
        .iter()
        .find(|d| d.entries.iter().any(|e| e.kind == bios::MICROCODE))
        .into_iter()
        .flat_map(|dir| {
            dir.entries
                .iter()
                .filter(|e| e.kind == bios::MICROCODE)
                .filter_map(move |e| {
                    Some((
                        format!("BIOS directory {:#x}", dir.offset),
                        dir.data(e).ok()?,
                    ))
                })
        })
        .filter(|(_, p)| plausible(p));
    // Files of other firmware file systems need not be aligned.
    let id = equivalence_id.to_le_bytes();
    let scanned = (0..rom.len().saturating_sub(size))
        .filter(|&at| rom[at + 0x18..at + 0x1a] == id)
        .map(|at| (format!("flash offset {at:#x}"), &rom[at..at + size]))
        .filter(|(_, p)| plausible(p));
    let candidates: Vec<_> = in_directory.chain(scanned).collect();
    let newest = candidates.iter().map(|(_, p)| patch_id(p)).max();
    let mut newest: Vec<_> = candidates
        .into_iter()
        .filter(|(_, p)| Some(patch_id(p)) == newest)
        .collect();
    newest.dedup_by(|a, b| a.1 == b.1);
    match newest.len() {
        0 => Ok(None),
        1 => Ok(newest.pop()),
        _ => Err(format!(
            "different patches {:#x} for CPUs {equivalence_id:#06x}",
            newest.first().map_or(0, |(_, p)| patch_id(p))
        )),
    }
}

/// The legacy x86 option ROM for PCI `vendor:device` anywhere in the image
/// (header, PCI data structure and checksum checked).
fn find_vbios(rom: &[u8], vendor: u16, device: u16) -> Result<Option<(String, &[u8])>, String> {
    let le16 = |b: &[u8], at: usize| b.get(at..at + 2).map(|b| u16::from_le_bytes([b[0], b[1]]));
    let option_rom = |at: usize| -> Option<&[u8]> {
        let image = rom.get(at..)?;
        if image.get(..2)? != [0x55, 0xaa] {
            return None;
        }
        let size = usize::from(*image.get(2)?) * 512;
        let bytes = image.get(..size).filter(|_| size != 0)?;
        let pcir = usize::from(le16(bytes, 0x18)?);
        (bytes.get(pcir..pcir + 4)? == b"PCIR"
            && le16(bytes, pcir + 4)? == vendor
            && le16(bytes, pcir + 6)? == device
            && *bytes.get(pcir + 0x14)? == 0
            && bytes.iter().fold(0u8, |sum, &b| sum.wrapping_add(b)) == 0)
            .then_some(bytes)
    };
    let mut found: Vec<_> = (0..rom.len().saturating_sub(1))
        .filter(|&at| rom[at] == 0x55 && rom[at + 1] == 0xaa)
        .filter_map(|at| option_rom(at).map(|bytes| (at, bytes)))
        .collect();
    found.dedup_by(|a, b| a.1 == b.1);
    let mut distinct = found.clone();
    distinct.sort_by(|a, b| a.1.cmp(b.1));
    distinct.dedup_by(|a, b| a.1 == b.1);
    match distinct.len() {
        0 => Ok(None),
        1 => Ok(Some((
            format!("flash offset {:#x}", found[0].0),
            found[0].1,
        ))),
        _ => Err(format!(
            "different video BIOSes for {vendor:04x}:{device:04x} at {}",
            found
                .iter()
                .map(|(at, _)| format!("{at:#x}"))
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// The blob tables of a board's AMD firmware, from its and its platform's
/// `fstart` manifest tables: one SoC table and the board's per SoC the
/// board has (each generation of an SP3 board).
pub fn tables(
    board_table: &serde_json::Value,
    platform_table: &serde_json::Value,
) -> Result<Vec<(SocBlobs, BoardBlobs)>, String> {
    let mut tables = Vec::new();
    for board in board_tables(&board_table["amd"])? {
        let soc: SocBlobs = serde_json::from_value(platform_table["amd"][&board.soc].clone())
            .map_err(|e| format!("platform [package.metadata.fstart.amd.{}]: {e}", board.soc))?;
        tables.push((soc, board));
    }
    Ok(tables)
}

/// The blob directory's files of every pair of `tables` ([`tables`]) from
/// the donor image `rom` of board `board_name`. A donor may carry only
/// some of a board's SoCs (an SP3 vendor build of one generation): those
/// it lacks are absent, and the image packs the others. Missing files of a
/// SoC it has are an error unless `allow_missing`.
pub fn extract_donor(
    tables: &[(SocBlobs, BoardBlobs)],
    board_name: &str,
    rom: &[u8],
    allow_missing: bool,
) -> Result<Donated, String> {
    let mut donated = Donated {
        board: board_name.into(),
        ..Default::default()
    };
    for (soc, board) in tables {
        let Some(donor) = donor_image(rom, soc) else {
            donated.absent.push(board.soc.clone());
            continue;
        };
        let (found, missing) = extract(soc, board, board_name, donor)?;
        if !missing.is_empty() && !allow_missing {
            return Err(format!(
                "the vendor image's {} firmware does not have: {}",
                board.soc,
                missing.join(", ")
            ));
        }
        donated.found.extend(found);
        donated.missing.extend(missing);
    }
    if donated.found.is_empty() && !allow_missing {
        return Err("the vendor image has firmware for none of the board's SoCs".into());
    }
    Ok(donated)
}

/// Whether the flash image `rom` has firmware of any SoC of `tables`
/// ([`donor_image`]) whose PSP and BIOS directories [`extract`] can read:
/// an EFS pointing at a corrupt directory is none.
pub fn has_firmware(tables: &[(SocBlobs, BoardBlobs)], rom: &[u8]) -> bool {
    tables.iter().any(|(soc, _)| {
        donor_image(rom, soc).is_some_and(|image| {
            let flash = soc.flash(image);
            if soc.ish {
                flash.ish_slots_checked().is_ok_and(|(slots, count)| {
                    count > 0
                        && slots[..count].iter().flatten().any(|s| {
                            s.bios_l2_offset.is_some() && flash.ish_slot_directories(s).is_ok()
                        })
                })
            } else {
                flash.psp_directories().is_ok()
                    && flash.bios_directories_checked(soc.bios_directory).is_ok()
            }
        })
    })
}

/// The files a donor image gave for a board ([`extract_donor`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Donated {
    /// The board's name, as its table states it.
    pub board: String,
    /// The member of the vendor ZIP the image is, if the donor is one
    /// ([`crate::donor::image`]).
    pub member: Option<String>,
    pub found: Vec<Extracted>,
    /// Files of a SoC the donor has firmware of, but not these.
    pub missing: Vec<String>,
    /// The SoCs the donor has no firmware of at all.
    pub absent: Vec<String>,
}

#[cfg(test)]
#[path = "blobs_tests.rs"]
mod tests;
