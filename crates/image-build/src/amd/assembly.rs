// SPDX-License-Identifier: MIT OR Apache-2.0
//! The AMD flash image of a PSP-booted stage: the PSP firmware (EFS, PSP
//! and BIOS directories, signed blobs), the stage as the BIOS reset image
//! (BIOS directory entry 0x62) the PSP copies to DRAM, and the FFS with the
//! payload, if the image has one. The PSP firmware either comes from a vendor
//! flash image, whose prefix stays byte for byte except for the [`PspOptions`]
//! fields, or is packed from blobs
//! ([`super::PicassoImage`]). Paths are explicit build inputs, never
//! discovered.
use super::{Blob, FlashRegion, Key, PicassoImage};
use crate::build_plan::{FfsAssembly, FileBinding};
use fstart_amdfw::{
    Flash,
    directory::{BiosEntry, bios, modify_entries},
    efs::{BiosDirectorySlot, Efs},
};
use serde::{Deserialize, Serialize};
use zerocopy::{FromBytes, IntoBytes};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileBlob {
    pub key: Key,
    /// Workspace-relative or absolute, exactly as supplied by the build owner.
    pub path: String,
}
/// Explicit packaging inputs, not runtime silicon policy or vendor authentication.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PicassoFiles {
    pub flash_size: u32,
    pub efs_offset: u32,
    pub partition_end: u32,
    pub spi_read_mode: u8,
    pub spi_speed: u8,
    pub micron: bool,
    pub soft_fuse: u64,
    pub psp: Vec<FileBlob>,
    pub bios: Vec<FileBlob>,
    pub apob_nv: Option<FlashRegion>,
    pub psp_nv: Option<FlashRegion>,
}

/// Where the PSP firmware comes from.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Firmware {
    /// A vendor flash image: its first `prefix_size` bytes stay.
    Vendor {
        image: FileBinding,
        prefix_size: u32,
    },
    /// Blobs packed into a PSP partition. `configuration` is the JSON of
    /// `files`, bound into the image's provenance too.
    Packed {
        configuration: FileBinding,
        files: PicassoFiles,
    },
}
impl Firmware {
    /// The `--image-config` input.
    pub fn configuration(&self) -> &FileBinding {
        match self {
            Self::Vendor { image, .. } => image,
            Self::Packed { configuration, .. } => configuration,
        }
    }
}

/// The FFS and its flash window.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FfsWindow {
    pub assembly: FfsAssembly,
    pub offset: u32,
    pub size: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AmdAssembly {
    pub firmware: Firmware,
    pub ffs: Option<FfsWindow>,
    /// Unit whose flat binary is the reset image.
    pub reset_unit: String,
    pub flash_size: u32,
    /// Where the SoC's PSP finds the BIOS directory.
    pub bios_directory: BiosDirectorySlot,
    /// The reset image's size and the DRAM it is copied to.
    pub reset_size: u32,
    pub reset_destination: u64,
    /// The PSP output block destination (BIOS directory entry 0x61).
    pub apob_destination: u64,
    /// The PSP's shared DRAM (entry 0x6b), packed images only.
    pub shared_memory: Option<(u64, u32)>,
    /// How the PSP loads the reset image and reads the flash.
    #[serde(default)]
    pub psp: PspOptions,
}

/// Faster PSP boot for vendor images. Both change the vendor prefix: the
/// compressed flag of every BIOS directory level's reset image entry (and
/// its checksum), and the EFS SPI fields.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PspOptions {
    /// zlib-compress the reset image, which the PSP inflates to DRAM:
    /// it then reads the compressed stage instead of the whole region.
    pub compress_reset: bool,
    /// The SPI read mode and speed of the SoC's PSP, encoded as in the FCH
    /// SPI controller (e.g. 7, 1: Fast Read at 33 MHz).
    pub spi: Option<(u8, u8)>,
}

/// The PSP's compressed image: a 256-byte header with the compressed size at
/// 0x14, then the zlib stream.
fn amd_compress(data: &[u8]) -> Vec<u8> {
    let stream = miniz_oxide::deflate::compress_to_vec_zlib(data, 9);
    let mut out = vec![0; 256];
    out[0x14..0x18].copy_from_slice(&(stream.len() as u32).to_le_bytes());
    out.extend(stream);
    out
}

/// The data of an `amdcompress` file, inflated to at most `limit` bytes.
pub(super) fn amd_decompress(file: &[u8], limit: usize) -> Option<Vec<u8>> {
    let size = u32::read_from_bytes(file.get(0x14..0x18)?).ok()? as usize;
    miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(file.get(256..256 + size)?, limit).ok()
}

/// A composed image.
#[derive(Debug)]
pub struct AmdImage {
    /// The flash offset of `bytes`: 0 for a whole flash image, the EFS for
    /// a packed PSP partition, which is NOT a flash image by itself.
    pub offset: u32,
    pub bytes: Vec<u8>,
    /// The BIOS directory the PSP boots from in the result.
    pub facts: BiosDirectoryFacts,
}
impl AmdImage {
    pub fn is_whole_flash(&self, flash_size: u32) -> bool {
        self.offset == 0 && self.bytes.len() == flash_size as usize
    }
}

/// BIOS directory entries the composition relies on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BiosDirectoryFacts {
    /// The flash offset of the EFS.
    pub efs: usize,
    pub reset_offset: u32,
    pub reset_size: u32,
    pub reset_destination: u64,
    pub apob_destination: u64,
}

/// Read the reset image (type 0x62) and APOB (type 0x61) entries of the
/// BIOS directory the PSP of `slot` boots from in `image`: the EFS's, or the
/// level 2 it points to. The EFS, the directories and every file but the
/// reset image must lie within `kept`, the bytes the composition keeps, and
/// every level must agree on the reset image.
pub fn read_bios_directory(
    image: &[u8],
    kept: u32,
    slot: BiosDirectorySlot,
) -> Result<BiosDirectoryFacts, String> {
    let flash = Flash::new(image);
    let kept = u64::from(kept);
    let within = |offset: usize, size: usize| offset as u64 + size as u64 <= kept;
    let (efs, _) = flash
        .efs()
        .filter(|&(offset, _)| within(offset, size_of::<Efs>()))
        .ok_or("no EFS in the kept firmware")?;
    let levels = flash
        .bios_directories(slot)
        .ok_or("no valid BIOS directory for the SoC in the image")?;
    let boot = levels.boot();
    let find = |kind| boot.entries.iter().find(|e| e.kind == kind);
    let reset = find(bios::RESET_IMAGE).ok_or("BIOS directory has no reset image (type 0x62)")?;
    let apob = find(bios::APOB).ok_or("BIOS directory has no APOB entry (type 0x61)")?;
    for directory in levels.iter() {
        if !within(directory.offset, directory.table().len()) {
            return Err("BIOS directory is outside the kept firmware".into());
        }
        for entry in directory.entries {
            let (kind, size, source) = (entry.kind, entry.size.get(), entry.source.get());
            // Sources are either flash offsets or memory-mapped flash
            // addresses; 0 and erased ones have no contents.
            let offset = flash.offset(source).map_or(u64::MAX, |o| o as u64);
            if kind == bios::RESET_IMAGE {
                if entry.as_bytes() != reset.as_bytes() {
                    return Err("the BIOS directory levels disagree on the reset image".into());
                }
            } else if size != 0
                && source != 0
                && source != u64::MAX
                && offset
                    .checked_add(u64::from(size))
                    .is_none_or(|end| end > kept)
            {
                return Err(format!(
                    "BIOS directory entry {kind:#x} at {offset:#x} is outside the kept firmware"
                ));
            }
        }
    }
    let reset_offset = flash
        .offset(reset.source.get())
        .and_then(|o| u32::try_from(o).ok())
        .ok_or("reset image source out of range")?;
    Ok(BiosDirectoryFacts {
        efs,
        reset_offset,
        reset_size: reset.size.get(),
        reset_destination: reset.destination.get(),
        apob_destination: apob.destination.get(),
    })
}

impl AmdAssembly {
    /// The vendor image keeps the reset image at the top of the flash.
    fn vendor_reset_offset(&self) -> u32 {
        self.flash_size - self.reset_size
    }

    pub fn validate(&self) -> Result<(), String> {
        let end = |offset: u32, size: u32| offset.checked_add(size).ok_or("layout overflows");
        if self.reset_unit.is_empty()
            || self.reset_size == 0
            || self.reset_size > self.flash_size
            || self.reset_destination == 0
            || self.apob_destination == 0
            || self
                .reset_destination
                .checked_add(u64::from(self.reset_size))
                .is_none_or(|end| end > 0x1_0000_0000 || end & 0xffff != 0)
            || !(0x40000..=0x1000000).contains(&self.flash_size)
            || !self.flash_size.is_power_of_two()
        {
            return Err("the reset image must end on a 64-KiB DRAM boundary below 4 GiB".into());
        }
        if self
            .psp
            .spi
            .is_some_and(|(mode, speed)| mode > 7 || mode == 1 || speed > 5)
        {
            return Err("invalid PSP SPI read mode or speed".into());
        }
        match (&self.firmware, &self.ffs) {
            (Firmware::Vendor { image, prefix_size }, ffs) => {
                let ffs_end = match ffs {
                    Some(ffs) if ffs.offset < *prefix_size => {
                        return Err("FFS in the vendor prefix".into());
                    }
                    Some(ffs) => end(ffs.offset, ffs.size)?,
                    None => *prefix_size,
                };
                if *prefix_size == 0
                    || ffs_end > self.vendor_reset_offset()
                    || image.capacity != u64::from(self.flash_size)
                    || self.shared_memory.is_some()
                {
                    return Err(
                        "vendor flash layout is not prefix < FFS < top-aligned reset image".into(),
                    );
                }
            }
            (Firmware::Packed { .. }, _) if self.psp != PspOptions::default() => {
                return Err("PSP options are for vendor images; packed ones set the SPI in their configuration".into());
            }
            (
                Firmware::Packed {
                    configuration,
                    files,
                },
                None,
            ) => {
                if configuration.environment != "IMAGE_CONFIG"
                    || configuration.capacity != 65536
                    || configuration.path.is_empty()
                    || configuration.path.contains('\0')
                    || files.flash_size != self.flash_size
                    || files.efs_offset >= files.partition_end
                    || files.partition_end > files.flash_size
                    || files.spi_read_mode > 7
                    || files.spi_speed > 3
                    || files.psp.len() > 64
                    || files.bios.len() > 64
                    || self.bios_directory != PicassoImage::BIOS_DIRECTORY
                {
                    return Err("invalid Picasso PSP image operation geometry/budget".into());
                }
                let mut keys = std::collections::BTreeSet::new();
                for (bios, blobs) in [(false, &files.psp), (true, &files.bios)] {
                    for file in blobs {
                        super::validate_key(file.key, bios)?;
                        if file.path.is_empty()
                            || file.path.contains('\0')
                            || !keys.insert((bios, file.key))
                        {
                            return Err("invalid or duplicate Picasso file declaration".into());
                        }
                    }
                }
            }
            (Firmware::Packed { .. }, Some(_)) => {
                return Err("packed PSP partitions have no FFS".into());
            }
        }
        Ok(())
    }

    /// The image inputs: the vendor image, or the configuration and then the
    /// PSP and BIOS blobs in their order. Their namespaces are image-only,
    /// never a compiler environment.
    pub fn bindings(&self) -> Result<Vec<FileBinding>, String> {
        self.validate()?;
        Ok(match &self.firmware {
            Firmware::Vendor { image, .. } => vec![image.clone()],
            Firmware::Packed {
                configuration,
                files,
            } => {
                let capacity = u64::from(files.partition_end - files.efs_offset);
                let blobs = [("PSP", &files.psp), ("BIOS", &files.bios)]
                    .into_iter()
                    .flat_map(|(namespace, blobs)| {
                        blobs.iter().map(move |file| FileBinding {
                            environment: format!(
                                "{namespace}_{:02X}_{:02X}_{:02X}",
                                file.key.kind, file.key.subprogram, file.key.instance
                            ),
                            path: file.path.clone(),
                            capacity,
                        })
                    });
                core::iter::once(configuration.clone())
                    .chain(blobs)
                    .collect()
            }
        })
    }

    /// Check a vendor image against the layout: its BIOS directory must
    /// describe this reset image and APOB, within the prefix.
    pub fn check_vendor(&self, vendor: &[u8]) -> Result<BiosDirectoryFacts, String> {
        let Firmware::Vendor { prefix_size, .. } = &self.firmware else {
            return Err("not a vendor image assembly".into());
        };
        if vendor.len() != self.flash_size as usize {
            return Err(format!(
                "vendor image is {:#x} bytes, the flash {:#x}",
                vendor.len(),
                self.flash_size
            ));
        }
        let facts = read_bios_directory(vendor, *prefix_size, self.bios_directory)?;
        self.check_facts(&facts, self.vendor_reset_offset())?;
        Ok(facts)
    }

    fn check_facts(&self, facts: &BiosDirectoryFacts, reset_offset: u32) -> Result<(), String> {
        let found = (
            facts.reset_offset,
            facts.reset_size,
            facts.reset_destination,
            facts.apob_destination,
        );
        let planned = (
            reset_offset,
            self.reset_size,
            self.reset_destination,
            self.apob_destination,
        );
        if found != planned {
            return Err(format!(
                "the BIOS directory {facts:?} doesn't match the reset image at {reset_offset:#x}, \
                 {:#x} bytes to {:#x}, and the APOB at {:#x}",
                self.reset_size, self.reset_destination, self.apob_destination
            ));
        }
        Ok(())
    }

    /// Compose the image. `sources` follows [`Self::bindings`]; `reset` is
    /// the stage's flat binary of exactly `reset_size` bytes; `ffs` the
    /// assembled FFS if the image has one, whose locator and verification
    /// policy go into the stage's placeholders.
    pub fn assemble(
        &self,
        sources: &[&[u8]],
        reset: &[u8],
        ffs: Option<&[u8]>,
    ) -> Result<AmdImage, String> {
        self.validate()?;
        if reset.len() != self.reset_size as usize {
            return Err(format!(
                "reset image is {:#x} bytes, the BIOS directory entry {:#x}",
                reset.len(),
                self.reset_size
            ));
        }
        if sources.len() != self.bindings()?.len() {
            return Err("AMD image source count mismatch".into());
        }
        let stage = match (&self.ffs, ffs) {
            (Some(window), Some(ffs)) if ffs.len() <= window.size as usize => {
                patch_stage(reset, ffs)?
            }
            (Some(window), Some(ffs)) => {
                return Err(format!(
                    "FFS ({:#x} bytes) exceeds its flash window ({:#x})",
                    ffs.len(),
                    window.size
                ));
            }
            (None, None) => reset.to_vec(),
            _ => return Err("the FFS doesn't match the image's".into()),
        };
        // The EFS, the BIOS directory and the reset image's flash offset
        // the PSP must find.
        let (offset, bytes, kept, efs, reset_offset) = match &self.firmware {
            Firmware::Vendor { prefix_size, .. } => {
                let vendor = sources[0];
                let facts = self.check_vendor(vendor)?;
                let mut image = vec![0xff; self.flash_size as usize];
                let prefix = *prefix_size as usize;
                image[..prefix].copy_from_slice(&vendor[..prefix]);
                if let (Some(window), Some(ffs)) = (&self.ffs, ffs) {
                    let offset = window.offset as usize;
                    image[offset..offset + ffs.len()].copy_from_slice(ffs);
                }
                let reset = self.apply_psp_options(&mut image, &facts, &stage)?;
                image[facts.reset_offset as usize..][..reset.len()].copy_from_slice(&reset);
                (0, image, *prefix_size, facts.efs, Some(facts.reset_offset))
            }
            Firmware::Packed { files, .. } => {
                let packed = self.pack(files, sources, stage.clone())?;
                let efs = files.efs_offset as usize;
                (packed.offset, packed.bytes, files.flash_size, efs, None)
            }
        };
        // Read it back as the PSP does: from the kept firmware, not through
        // an EFS in fstart's bytes.
        let mut flash = vec![0xff; self.flash_size as usize];
        flash[offset as usize..offset as usize + bytes.len()].copy_from_slice(&bytes);
        let facts = read_bios_directory(&flash, kept, self.bios_directory)?;
        self.check_facts(&facts, reset_offset.unwrap_or(facts.reset_offset))?;
        let start = facts.reset_offset as usize;
        let region = flash.get(start..start + stage.len()).unwrap_or_default();
        let found = if self.psp.compress_reset {
            amd_decompress(region, stage.len())
        } else {
            Some(region.to_vec())
        };
        if facts.efs != efs || found.as_deref() != Some(&stage[..]) {
            return Err("the PSP wouldn't find the stage in the composed image".into());
        }
        Ok(AmdImage {
            offset,
            bytes,
            facts,
        })
    }

    /// Set the EFS SPI fields and the compressed flag of the reset image in
    /// every BIOS directory level of the PSP's slot in `image`, and return
    /// the reset region's bytes: the stage, or its `amdcompress` file padded
    /// with erased flash.
    fn apply_psp_options(
        &self,
        image: &mut [u8],
        facts: &BiosDirectoryFacts,
        stage: &[u8],
    ) -> Result<Vec<u8>, String> {
        if let Some((mode, speed)) = self.psp.spi {
            Efs::mut_from_prefix(&mut image[facts.efs..])
                .map_err(|_| "no EFS")?
                .0
                .set_spi(self.bios_directory, mode, speed);
        }
        if !self.psp.compress_reset {
            return Ok(stage.to_vec());
        }
        let levels: Vec<usize> = Flash::new(image)
            .bios_directories(self.bios_directory)
            .ok_or("no BIOS directory")?
            .iter()
            .map(|directory| directory.offset)
            .collect();
        for offset in levels {
            modify_entries::<BiosEntry>(image, offset, |entry| {
                if entry.kind == bios::RESET_IMAGE {
                    entry.flags |= BiosEntry::COMPRESSED;
                }
            })
            .ok_or("BIOS directory changed under the PSP options")?;
        }
        let mut reset = amd_compress(stage);
        if reset.len() > stage.len() {
            return Err("the compressed reset image is larger than the stage".into());
        }
        reset.resize(stage.len(), 0xff);
        Ok(reset)
    }

    /// Pack the PSP partition. The configuration witness must be the planned
    /// one; this does not authenticate the blobs.
    fn pack(
        &self,
        files: &PicassoFiles,
        sources: &[&[u8]],
        reset: Vec<u8>,
    ) -> Result<super::PackedImage, String> {
        if sources[0].len() > 65536
            || serde_json::from_slice::<PicassoFiles>(sources[0]).map_err(|e| e.to_string())?
                != *files
        {
            return Err("PSP image configuration changed after host resolution".into());
        }
        let total = sources[1..]
            .iter()
            .try_fold(0usize, |sum, bytes| sum.checked_add(bytes.len()))
            .ok_or("PSP input size overflow")?;
        if total > (files.partition_end - files.efs_offset) as usize {
            return Err("PSP blob inputs exceed partition budget".into());
        }
        let blobs = |declared: &[FileBlob], bytes: &[&[u8]]| {
            declared
                .iter()
                .zip(bytes)
                .map(|(file, bytes)| Blob {
                    key: file.key,
                    bytes: bytes.to_vec(),
                })
                .collect()
        };
        let split = 1 + files.psp.len();
        PicassoImage {
            flash_size: files.flash_size,
            efs_offset: files.efs_offset,
            partition_end: files.partition_end,
            spi_read_mode: files.spi_read_mode,
            spi_speed: files.spi_speed,
            micron: files.micron,
            soft_fuse: files.soft_fuse,
            psp: blobs(&files.psp, &sources[1..split]),
            bios: blobs(&files.bios, &sources[split..]),
            reset_image: reset,
            reset_destination: self.reset_destination,
            apob_destination: self.apob_destination,
            shared_memory: self.shared_memory,
            apob_nv: files.apob_nv,
            psp_nv: files.psp_nv,
        }
        .pack()
    }
}

/// The stage with the FFS's locator and verification policy in the places
/// of its placeholders.
fn patch_stage(reset: &[u8], ffs: &[u8]) -> Result<Vec<u8>, String> {
    use fstart_core::ffs::{
        ANCHOR_SIZE, AnchorBlock,
        trust::{TRUST_SIZE, TrustBlock},
    };
    let mut stage = reset.to_vec();
    let anchor = fstart_ffs::FfsReader::new(ffs)
        .scan_for_anchor()
        .map_err(|e| format!("FFS anchor not found: {e:?}"))?;
    let mut placeholder = [0; ANCHOR_SIZE];
    AnchorBlock::placeholder().write_to(&mut placeholder);
    let site = find_unique(&stage, &placeholder, 1).ok_or("no unique anchor site in the stage")?;
    stage[site..site + ANCHOR_SIZE].copy_from_slice(&ffs[anchor..anchor + ANCHOR_SIZE]);

    let mut trust_placeholder = [0; TRUST_SIZE];
    TrustBlock::placeholder().write_to(&mut trust_placeholder);
    let trust = (0..ffs.len().saturating_sub(TRUST_SIZE - 1))
        .step_by(8)
        .filter(|&o| ffs[o..o + TRUST_SIZE] != trust_placeholder)
        .find_map(|o| TrustBlock::parse(&ffs[o..o + TRUST_SIZE]).filter(|b| b.key_count > 0))
        .ok_or("no finalized trust block in the FFS")?;
    let site =
        find_unique(&stage, &trust_placeholder, 8).ok_or("no unique trust site in the stage")?;
    trust.write_to(&mut stage[site..site + TRUST_SIZE]);
    Ok(stage)
}

fn find_unique(haystack: &[u8], needle: &[u8], step: usize) -> Option<usize> {
    let mut sites = (0..haystack.len().saturating_sub(needle.len() - 1))
        .step_by(step)
        .filter(|&o| &haystack[o..o + needle.len()] == needle);
    let first = sites.next()?;
    sites.next().is_none().then_some(first)
}
