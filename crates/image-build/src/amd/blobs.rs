// SPDX-License-Identifier: MIT OR Apache-2.0
//! The AMD firmware an image is packed from, as two Cargo.toml tables:
//!
//! - the SoC's ([`SocBlobs`], `[package.metadata.fstart.amd.<soc>]` of the
//!   platform crate): the PSP partition's geometry and settings, and which
//!   file of a blob directory each PSP and BIOS directory entry, microcode
//!   patch and video BIOS is;
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
    assembly::{ApcbFiles, FileBlob, PicassoFiles},
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
    /// Flash offset of the EFS, the start of the PSP partition.
    pub efs_offset: u32,
    /// End of the PSP partition.
    pub partition_end: u32,
    /// The PSP's SPI read mode and speed (EFS).
    pub spi_read_mode: u8,
    pub spi_speed: u8,
    #[serde(default)]
    pub micron: bool,
    /// PSP directory entry 0x0b.
    pub soft_fuse: u64,
    pub psp: Vec<DirectoryFile>,
    pub bios: Vec<DirectoryFile>,
    /// Patches the FFS carries for MP init.
    #[serde(default)]
    pub microcode: Vec<MicrocodeFile>,
    /// Video BIOSes the FFS carries, by the PCI device they are for.
    #[serde(default)]
    pub vbios: Vec<VbiosFile>,
}

/// The file of a PSP or BIOS directory entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct DirectoryFile {
    #[serde(rename = "type")]
    pub kind: u8,
    #[serde(default)]
    pub subprogram: u8,
    #[serde(default)]
    pub instance: u8,
    pub file: String,
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
    /// The SoC table of the platform crate.
    pub soc: String,
    /// Pack fstart's PSP application ([`super::psp_app`]).
    #[serde(default)]
    pub psp_app: bool,
    pub apcb: BoardApcb,
}

/// Where a board's APCB instances come from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum BoardApcb {
    /// `instances` files, one per RAM ID, as the board's vendor ROM has
    /// them: `<board>/apcb/<n>.bin` in the blob directory ([`extract`]
    /// writes them).
    Files { instances: u8 },
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
    pub files: PicassoFiles,
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
    let directory = |files: &[DirectoryFile]| {
        files
            .iter()
            .map(|f| {
                Ok(FileBlob {
                    key: Key::new(f.kind, f.subprogram, f.instance),
                    path: path(&f.file)?,
                })
            })
            .collect::<Result<Vec<_>, String>>()
    };
    let mut bios = directory(&soc.bios)?;
    let apcb = match &board.apcb {
        BoardApcb::Files { instances } => {
            for instance in 0..*instances {
                let file = path(&apcb_file(board_name, instance))?;
                for kind in [bios::APCB, bios::APCB_BACKUP] {
                    bios.push(FileBlob {
                        key: Key::new(kind, 0, instance),
                        path: file.clone(),
                    });
                }
            }
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
        files: PicassoFiles {
            flash_size,
            efs_offset: soc.efs_offset,
            partition_end: soc.partition_end,
            spi_read_mode: soc.spi_read_mode,
            spi_speed: soc.spi_speed,
            micron: soc.micron,
            soft_fuse: soc.soft_fuse,
            psp: directory(&soc.psp)?,
            bios,
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
    let flash = Flash::new(rom).with_profile(Profile::for_slot(slot));
    let psp = flash
        .psp_directories()
        .map_err(|e| format!("vendor PSP directory: {e:?}"))?;
    let bios_dirs = flash
        .bios_directories_checked(slot)
        .map_err(|e| format!("vendor BIOS directory: {e:?}"))?;
    let mut found = Vec::new();
    let mut missing = Vec::new();
    let mut take = |file: &str, origin: Option<(String, &[u8])>| match origin {
        Some((origin, bytes)) => found.push(Extracted {
            file: file.into(),
            bytes: bytes.to_vec(),
            origin,
        }),
        None => missing.push(file.into()),
    };
    for f in &soc.psp {
        let entry = psp
            .find(f.kind, Some(f.instance), Some(f.subprogram))
            .and_then(|(dir, e)| Some((dir.offset, dir.data(e).ok()?)));
        take(
            &f.file,
            entry.map(|(dir, bytes)| (format!("PSP directory {dir:#x}"), bytes)),
        );
    }
    let directory_file = |kind, instance, subprogram| {
        bios_dirs
            .find(kind, Some(instance), Some(subprogram))
            .and_then(|(dir, e)| {
                Some((
                    format!("BIOS directory {:#x}", dir.offset),
                    dir.data(e).ok()?,
                ))
            })
    };
    for f in &soc.bios {
        take(&f.file, directory_file(f.kind, f.instance, f.subprogram));
    }
    if let BoardApcb::Files { instances } = board.apcb {
        for instance in 0..instances {
            take(
                &apcb_file(board_name, instance),
                directory_file(bios::APCB, instance, 0),
            );
        }
    }
    for m in &soc.microcode {
        let patch = find_microcode(rom, &bios_dirs, m.equivalence_id)?;
        take(&m.file, patch);
    }
    for v in &soc.vbios {
        take(&v.file, find_vbios(rom, v.vendor, v.device)?);
    }
    Ok((found, missing))
}

/// The newest patch for `equivalence_id`: from the BIOS directory if it has
/// one, else wherever in the image one is (e.g. a file of another firmware
/// file system).
fn find_microcode<'a>(
    rom: &'a [u8],
    bios_dirs: &fstart_amdfw_format::directory::Levels<
        'a,
        fstart_amdfw_format::directory::BiosEntry,
    >,
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
        .all(bios::MICROCODE)
        .filter_map(|(dir, e)| {
            Some((
                format!("BIOS directory {:#x}", dir.offset),
                dir.data(e).ok()?,
            ))
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

#[cfg(test)]
#[path = "blobs_tests.rs"]
mod tests;
