// SPDX-License-Identifier: MIT OR Apache-2.0
//! One AMD image compositor: fixed-prefix donors, selected vendor banks and
//! mirrors, or a natively packed PSP partition. Platforms supply geometry and
//! the reset-stage ABI; parsing, placement, directory edits and readback are
//! shared, independent of silicon/board identity.
use super::{
    Blob, FlashRegion, Key, PicassoImage,
    banks::{BankLayout, PackedHalf, ResetSlot, Survey},
    levels::{self, Level, TwoLevelImage},
    stage::{Patched, StageLocator, patch_reset_image},
};
use crate::build_plan::{FfsAssembly, FileBinding};
use fstart_amdfw_format::{
    Flash, Profile,
    anchor::FlashAnchor,
    directory::{BiosEntry, Entry, bios, modify_entries},
    efs::{BiosDirectorySlot, Efs},
};
use serde::{Deserialize, Serialize};
use zerocopy::FromBytes;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileBlob {
    pub key: Key,
    pub path: String,
    /// The directory levels of a two-level partition it is in.
    #[serde(default)]
    pub level: Level,
}

/// A directory entry without a file: erased flash the firmware writes, or a
/// value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FixedEntry {
    /// A PSP directory entry, else a BIOS directory entry.
    pub psp: bool,
    pub key: Key,
    #[serde(default)]
    pub level: Level,
    pub content: Fixed,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Fixed {
    /// This many bytes of erased flash.
    Erased(u32),
    Value(u64),
}

/// Explicit packaging inputs, not runtime silicon policy or vendor authentication.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PspFiles {
    pub flash_size: u32,
    pub efs_offset: u32,
    pub partition_end: u32,
    /// The PSP's SPI read mode and speed (EFS); erased if absent.
    pub spi_read_mode: Option<u8>,
    pub spi_speed: Option<u8>,
    pub micron: bool,
    /// PSP entry 0x0b of a single-level partition. Two-level partitions
    /// have it among their `fixed` entries, in the levels they want it.
    pub soft_fuse: Option<u64>,
    /// `$PSP` + `$PL2` and `$BHD` + `$BL2` ([`TwoLevelImage`]) instead of
    /// Picasso's single level.
    #[serde(default)]
    pub two_level: bool,
    pub psp: Vec<FileBlob>,
    pub bios: Vec<FileBlob>,
    #[serde(default)]
    pub fixed: Vec<FixedEntry>,
    pub apob_nv: Option<FlashRegion>,
    pub psp_nv: Option<FlashRegion>,
    #[serde(default)]
    pub apcb: Option<ApcbFiles>,
    #[serde(default)]
    pub assets: Vec<fstart_core::board::FileAsset>,
    #[serde(default)]
    pub psp_app: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ApcbFiles {
    pub template: String,
    pub gpio: [[u8; 3]; 4],
    pub spds: Vec<String>,
    #[serde(default)]
    pub second_channel: bool,
    #[serde(default)]
    pub strip_manufacturer: bool,
}
impl ApcbFiles {
    fn instance(&self, template: &[u8], spd: &[u8]) -> Result<Vec<u8>, String> {
        let spd: [u8; 512] = match spd.try_into() {
            Ok(binary) => binary,
            Err(_) => super::apcb::parse_spd_hex(
                std::str::from_utf8(spd).map_err(|_| "SPD is neither 512 bytes nor hex text")?,
            )?,
        };
        super::apcb::inject(
            template,
            self.gpio,
            [
                [Some(&spd), None],
                [self.second_channel.then_some(&spd), None],
            ],
            self.strip_manufacturer,
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FixedLayout {
    pub flash_size: u32,
    pub bios_directory: BiosDirectorySlot,
    pub prefix_size: u32,
    pub reset: ResetSlot,
    pub apob_destination: u64,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum VendorLayout {
    Fixed(FixedLayout),
    Banks(BankLayout),
}

/// Reset/DRAM geometry for a packed partition. Its flash offset can be allocated
/// by the packer, unlike a donor's existing or explicitly relocated reset slot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackedReset {
    pub size: u32,
    pub destination: u64,
    pub apob_destination: u64,
    pub shared_memory: Option<(u64, u32)>,
    pub bios_directory: BiosDirectorySlot,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum Firmware {
    Vendor {
        image: FileBinding,
        layout: VendorLayout,
    },
    PackedBanks {
        halves: Vec<PackedHalf>,
        layout: BankLayout,
    },
    Packed {
        files: PspFiles,
        reset_offset: Option<u32>,
        reset: PackedReset,
    },
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FfsWindow {
    pub assembly: FfsAssembly,
    pub offset: u32,
    pub size: u32,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AmdAssembly {
    pub firmware: Firmware,
    pub ffs: Option<FfsWindow>,
    pub reset_unit: String,
    #[serde(default)]
    pub psp: PspOptions,
    #[serde(default)]
    pub locator: StageLocator,
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PspOptions {
    pub compress_reset: bool,
    pub spi: Option<(u8, u8)>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct BiosDirectoryFacts {
    pub efs: usize,
    pub reset_offset: u32,
    pub reset_size: u32,
    pub reset_destination: u64,
    /// None when the boot directory has no APOB entry (e.g. an L2 directory).
    pub apob_destination: Option<u64>,
}
#[derive(Debug, Clone, Serialize)]
pub struct BankFacts {
    pub origin: usize,
    pub profile: BiosDirectorySlot,
    pub bios: BiosDirectoryFacts,
}
#[derive(Debug)]
pub struct AmdImage {
    pub offset: u32,
    pub bytes: Vec<u8>,
    pub banks: Vec<BankFacts>,
    pub ffs_offset: Option<u32>,
    pub patched: Patched,
    whole_flash: bool,
    boot_size: usize,
}
impl AmdImage {
    pub fn is_whole_flash(&self) -> bool {
        self.whole_flash
    }
    /// A mirrored full readout also offers the actual PSP boot window as a
    /// separate bank image. Consumers need not know any bank-size constants.
    pub fn boot_bank(&self) -> Option<&[u8]> {
        (self.whole_flash && self.bytes.len() != self.boot_size)
            .then(|| &self.bytes[..self.boot_size])
    }
}

fn amd_compress(data: &[u8]) -> Vec<u8> {
    let stream = miniz_oxide::deflate::compress_to_vec_zlib(data, 9);
    let mut out = vec![0; 256];
    out[0x14..0x18].copy_from_slice(&(stream.len() as u32).to_le_bytes());
    out.extend(stream);
    out
}
pub(super) fn amd_decompress(file: &[u8], limit: usize) -> Option<Vec<u8>> {
    let size = u32::read_from_bytes(file.get(0x14..0x18)?).ok()? as usize;
    miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(
        file.get(256..256usize.checked_add(size)?)?,
        limit,
    )
    .ok()
}

/// Read all BIOS levels consistently and prove every retained dependency fits
/// in the bank's prefix. Fixed-layout plans additionally require their APOB.
pub fn read_bios_directory(
    image: &[u8],
    kept: u32,
    slot: BiosDirectorySlot,
) -> Result<BiosDirectoryFacts, String> {
    let flash = Flash::new(image).with_profile(Profile::for_slot(slot));
    let (efs, _) = flash.efs_checked().map_err(|e| format!("EFS: {e:?}"))?;
    let levels = flash
        .bios_directories_checked(slot)
        .map_err(|e| format!("BIOS directory: {e:?}"))?;
    let boot = levels.boot();
    let mut resets = boot.entries.iter().filter(|e| e.kind == bios::RESET_IMAGE);
    let reset = resets.next().ok_or("missing BIOS reset entry")?;
    if resets.next().is_some() {
        return Err("ambiguous BIOS reset entry".into());
    }
    if !reset.reset() || !reset.copy() {
        return Err("reset image must have reset and copy flags".into());
    }
    let reset_range = boot
        .range(reset)
        .map_err(|e| format!("reset image: {e:?}"))?;
    for directory in levels.iter() {
        for entry in directory
            .entries
            .iter()
            .filter(|e| e.kind == bios::RESET_IMAGE)
        {
            if directory
                .range(entry)
                .map_err(|e| format!("reset image: {e:?}"))?
                != reset_range
                || entry.flags != reset.flags
                || entry.destination != reset.destination
                || entry.subprogram != reset.subprogram
                || entry.region != reset.region
            {
                return Err("the BIOS directory levels disagree on the reset image".into());
            }
        }
    }
    flash
        .validate_prefix(slot, kept as usize, Some(reset_range.clone()))
        .map_err(|e| format!("firmware dependency would not survive composition: {e:?}"))?;
    let mut apobs = boot.entries.iter().filter(|e| e.kind == bios::APOB);
    let apob_destination = apobs.next().map(|e| e.destination.get());
    if apobs.next().is_some() {
        return Err("ambiguous BIOS APOB entry".into());
    }
    Ok(BiosDirectoryFacts {
        efs,
        reset_offset: u32::try_from(reset_range.start).map_err(|_| "reset source out of range")?,
        reset_size: reset.size.get(),
        reset_destination: reset.destination.get(),
        apob_destination,
    })
}

/// Normalized composition work, not a second image-format implementation.
/// Fixed-prefix and bank-selected donors both become a list of these banks.
#[derive(Clone, Copy)]
struct Bank {
    origin: usize,
    size: usize,
    prefix: u32,
    profile: BiosDirectorySlot,
    reset: ResetSlot,
    apob: Option<u64>,
    relocate: bool,
}
struct VendorPreparation {
    banks: Vec<Bank>,
    boot_size: usize,
    ffs: Option<std::ops::Range<u32>>,
    survey: Option<Survey>,
}

impl AmdAssembly {
    pub fn reset_geometry(&self) -> (u32, u64) {
        match &self.firmware {
            Firmware::Vendor { layout, .. } => {
                let reset = match layout {
                    VendorLayout::Fixed(f) => f.reset,
                    VendorLayout::Banks(b) => b.reset,
                };
                (reset.size, reset.destination)
            }
            Firmware::Packed { reset, .. } => (reset.size, reset.destination),
            Firmware::PackedBanks { layout, .. } => (layout.reset.size, layout.reset.destination),
        }
    }
    pub fn validate(&self) -> Result<(), String> {
        let (size, destination) = self.reset_geometry();
        if self.reset_unit.is_empty()
            || size == 0
            || destination == 0
            || destination
                .checked_add(u64::from(size))
                .is_none_or(|end| end > 0x1_0000_0000 || end & 0xffff != 0)
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
        let end = |offset: u32, size: u32| offset.checked_add(size).ok_or("layout overflows");
        match &self.firmware {
            Firmware::Vendor { image, layout } => {
                if image.path.is_empty() || image.path.contains('\0') || image.capacity == 0 {
                    return Err("invalid AMD vendor image binding".into());
                }
                match layout {
                    VendorLayout::Fixed(f) => {
                        if !(0x40000..=0x1000000).contains(&f.flash_size)
                            || !f.flash_size.is_power_of_two()
                            || f.reset.size > f.flash_size
                            || end(f.reset.flash_offset, size)? != f.flash_size
                            || f.prefix_size == 0
                            || f.prefix_size > f.reset.flash_offset
                            || f.apob_destination == 0
                            || image.capacity != u64::from(f.flash_size)
                        {
                            return Err("invalid fixed AMD vendor geometry".into());
                        }
                        if let Some(ffs) = &self.ffs
                            && (ffs.offset < f.prefix_size
                                || ffs.size == 0
                                || end(ffs.offset, ffs.size)? > f.reset.flash_offset)
                        {
                            return Err(
                                "vendor flash layout is not prefix < FFS < top-aligned reset image"
                                    .into(),
                            );
                        }
                    }
                    VendorLayout::Banks(b) => {
                        b.check()?;
                        if let Some(ffs) = &self.ffs
                            && (ffs.offset != b.ffs_offset
                                || ffs.size != b.reset.flash_offset - b.ffs_offset)
                        {
                            return Err("bank FFS reservation differs from its layout".into());
                        }
                    }
                }
            }
            Firmware::PackedBanks { halves, layout } => {
                layout.check()?;
                if halves.is_empty() || halves.len() > 2 {
                    return Err("a banked flash has one or two halves".into());
                }
                if let Some(ffs) = &self.ffs
                    && (ffs.offset != layout.ffs_offset
                        || ffs.size != layout.reset.flash_offset - layout.ffs_offset)
                {
                    return Err("bank FFS reservation differs from its layout".into());
                }
                for half in halves {
                    let files = &half.files;
                    if !layout.generations.contains(&half.generation)
                        || !files.two_level
                        || files.apcb.is_some()
                        || files.psp_app
                        || files.flash_size as usize != super::banks::HALF_SIZE
                        || files.efs_offset != 0x20000
                        || files.partition_end <= files.efs_offset
                        || files.partition_end > layout.ffs_offset
                        || files.psp.len() > 64
                        || files.bios.len() > 64
                        || half.apob_destination == 0
                    {
                        return Err("invalid packed bank geometry or firmware source".into());
                    }
                    if files
                        .bindings()
                        .iter()
                        .any(|f| f.path.is_empty() || f.path.contains('\0'))
                    {
                        return Err("invalid packed bank file path".into());
                    }
                }
            }
            Firmware::Packed {
                files,
                reset_offset,
                reset,
            } => {
                if self.psp.spi.is_some() {
                    return Err("a packed image sets the PSP's SPI in its configuration".into());
                }
                if files.flash_size < size
                    || files.flash_size > 0x1000000
                    || !files.flash_size.is_power_of_two()
                    || reset.apob_destination == 0
                    || files.efs_offset >= files.partition_end
                    || files.partition_end > files.flash_size
                    || files.psp.len() > 64
                    || files.bios.len() > 64
                {
                    return Err("invalid Picasso PSP image geometry/budget".into());
                }
                let partition = files.efs_offset..files.partition_end;
                let outside = |at, size| {
                    end(at, size).map(|end| {
                        end <= files.flash_size && (end <= partition.start || at >= partition.end)
                    })
                };
                match (reset_offset, &self.ffs) {
                    (Some(at), ffs) => {
                        let reset_end = end(*at, size)?;
                        if !outside(*at, size)?
                            || ffs.as_ref().is_some_and(|ffs| {
                                ffs.size == 0
                                    || !outside(ffs.offset, ffs.size).unwrap_or(false)
                                    || (ffs.offset < reset_end && ffs.offset + ffs.size > *at)
                            })
                        {
                            return Err("native AMD flash windows overlap".into());
                        }
                    }
                    (None, Some(_)) => {
                        return Err("an FFS needs a native image's reset window".into());
                    }
                    (None, None) if self.psp.compress_reset => {
                        return Err("only a native image's reset image is compressed".into());
                    }
                    (None, None) => {}
                }
                if files.two_level {
                    // The packer checks the entries.
                    if reset_offset.is_none() || files.apcb.is_some() || files.psp_app {
                        return Err(
                            "a two-level partition is a native image with board APCB files".into(),
                        );
                    }
                    // Picasso's settings; two-level partitions list these
                    // as entries.
                    if files.micron
                        || files.soft_fuse.is_some()
                        || files.apob_nv.is_some()
                        || files.psp_nv.is_some()
                    {
                        return Err("a two-level partition has its NV areas and soft fuse as \
                                    entries, and no Micron setting"
                            .into());
                    }
                } else {
                    if files.spi_read_mode.is_none_or(|mode| mode > 7)
                        || files.spi_speed.is_none_or(|speed| speed > 3)
                        || files.soft_fuse.is_none()
                        || !files.fixed.is_empty()
                        || reset.bios_directory != PicassoImage::BIOS_DIRECTORY
                    {
                        return Err("invalid Picasso PSP image settings".into());
                    }
                    for (bios, blobs) in [(false, &files.psp), (true, &files.bios)] {
                        for file in blobs {
                            super::validate_key(file.key, bios)?;
                        }
                    }
                }
                let mut keys = std::collections::BTreeSet::new();
                for (bios, blobs) in [(false, &files.psp), (true, &files.bios)] {
                    for file in blobs {
                        if file.path.is_empty()
                            || file.path.contains('\0')
                            || !keys.insert((bios, file.key, file.level))
                        {
                            return Err("invalid or duplicate AMD file declaration".into());
                        }
                    }
                }
            }
        }
        Ok(())
    }
    pub fn bindings(&self) -> Result<Vec<FileBinding>, String> {
        self.validate()?;
        Ok(match &self.firmware {
            Firmware::Vendor { image, .. } => vec![image.clone()],
            Firmware::Packed { files, .. } => files.bindings(),
            Firmware::PackedBanks { halves, .. } => halves
                .iter()
                .enumerate()
                .flat_map(|(i, half)| {
                    half.files.bindings().into_iter().map(move |mut f| {
                        f.environment = format!("BANK_{i}_{}", f.environment);
                        f
                    })
                })
                .collect(),
        })
    }
    pub fn check_vendor(&self, vendor: &[u8]) -> Result<(), String> {
        self.validate()?;
        self.prepare_vendor(vendor).map(|_| ())
    }
    fn prepare_vendor(&self, vendor: &[u8]) -> Result<VendorPreparation, String> {
        if let Firmware::Vendor { image, .. } = &self.firmware
            && vendor.len() as u64 > image.capacity
        {
            return Err("vendor image exceeds its input capacity".into());
        }
        let (banks, boot_size, ffs, survey) = match &self.firmware {
            Firmware::Vendor {
                layout: VendorLayout::Fixed(f),
                ..
            } => {
                if vendor.len() != f.flash_size as usize {
                    return Err("vendor image does not match the flash size".into());
                }
                let bank = Bank {
                    origin: 0,
                    size: vendor.len(),
                    prefix: f.prefix_size,
                    profile: f.bios_directory,
                    reset: f.reset,
                    apob: Some(f.apob_destination),
                    relocate: false,
                };
                check_facts(
                    &read_bios_directory(vendor, bank.prefix, bank.profile)?,
                    &bank,
                )?;
                (
                    vec![bank],
                    vendor.len(),
                    self.ffs.as_ref().map(|w| w.offset..w.offset + w.size),
                    None,
                )
            }
            Firmware::Vendor {
                layout: VendorLayout::Banks(b),
                ..
            }
            | Firmware::PackedBanks { layout: b, .. } => {
                let window = b.boot_window(vendor)?;
                let survey = b.survey(vendor)?;
                let banks = survey
                    .composed()
                    .map(|(half, layout)| Bank {
                        origin: half.offset(),
                        size: super::banks::HALF_SIZE,
                        prefix: layout.prefix_end as u32,
                        profile: layout.generation,
                        reset: b.reset,
                        apob: None,
                        relocate: true,
                    })
                    .collect();
                (
                    banks,
                    window.len(),
                    self.ffs.as_ref().map(|_| survey.ffs.clone()),
                    Some(survey),
                )
            }
            _ => return Err("not a banked or vendor image assembly".into()),
        };
        // Validate every dependency before clearing any donor bytes, for either
        // topology. The eventual reset may be relocated, but old dependencies
        // must already fit in the retained prefix.
        for bank in &banks {
            read_bios_directory(
                &vendor[bank.origin..bank.origin + bank.size],
                bank.prefix,
                bank.profile,
            )?;
        }
        Ok(VendorPreparation {
            banks,
            boot_size,
            ffs,
            survey,
        })
    }

    pub fn assemble(
        &self,
        sources: &[&[u8]],
        reset: &[u8],
        ffs: Option<&[u8]>,
    ) -> Result<AmdImage, String> {
        self.validate()?;
        let (reset_size, reset_destination) = self.reset_geometry();
        if reset.len() != reset_size as usize {
            return Err("reset image differs from its linked size".into());
        }
        if sources.len() != self.bindings()?.len() {
            return Err("AMD image source count mismatch".into());
        }
        let packed = if let Firmware::PackedBanks { halves, layout } = &self.firmware {
            let bindings = self.bindings()?;
            let files: std::collections::BTreeMap<_, _> = bindings
                .iter()
                .zip(sources)
                .map(|(f, bytes)| (f.path.as_str(), *bytes))
                .collect();
            Some(super::banks::packed_rom(halves, layout, &|path| {
                files
                    .get(path)
                    .map(|b| b.to_vec())
                    .ok_or_else(|| format!("missing bank file {path}"))
            })?)
        } else {
            None
        };
        let vendor = match &self.firmware {
            Firmware::Vendor { .. } => Some(sources[0]),
            Firmware::PackedBanks { .. } => packed.as_deref(),
            Firmware::Packed { .. } => None,
        };
        let prepared = vendor.map(|v| self.prepare_vendor(v)).transpose()?;
        let ffs_range = prepared
            .as_ref()
            .and_then(|p| p.ffs.clone())
            .or_else(|| self.ffs.as_ref().map(|w| w.offset..w.offset + w.size));
        match (ffs_range.as_ref(), ffs) {
            (Some(window), Some(ffs)) if ffs.len() <= window.len() => {}
            (None, None) => {}
            _ => return Err("FFS is missing, unexpected or exceeds its AMD flash window".into()),
        }
        let boot_size = match &self.firmware {
            Firmware::Vendor { .. } | Firmware::PackedBanks { .. } => {
                prepared.as_ref().unwrap().boot_size
            }
            Firmware::Packed { files, .. } => files.flash_size as usize,
        };
        if matches!(
            &self.firmware,
            Firmware::Vendor {
                layout: VendorLayout::Banks(_),
                ..
            } | Firmware::PackedBanks { .. }
        ) {
            let vector = &reset[reset.len() - 16..];
            if vector.iter().all(|&b| b == 0) || vector.iter().all(|&b| b == 0xff) {
                return Err("no reset vector in the last 16 bytes".into());
            }
        }
        let mut stage = reset.to_vec();
        let patched = if let Some(ffs) = ffs {
            patch_reset_image(
                &mut stage,
                ffs,
                FlashAnchor::new(
                    boot_size as u32,
                    ffs_range.as_ref().unwrap().start,
                    ffs.len() as u32,
                ),
                self.locator,
            )?
        } else {
            Patched::default()
        };
        let mut stored = if self.psp.compress_reset {
            amd_compress(&stage)
        } else {
            stage.clone()
        };
        if stored.len() > stage.len() {
            return Err("the compressed reset image is larger than the stage".into());
        }
        stored.resize(stage.len(), 0xff);

        let (offset, mut bytes, banks, whole_flash) = match &self.firmware {
            Firmware::Vendor { .. } | Firmware::PackedBanks { .. } => {
                let prepared = prepared.as_ref().unwrap();
                let mut image = vendor.unwrap()[..prepared.boot_size].to_vec();
                // This is the one vendor write loop for fixed and banked donors.
                // Banks not selected by the layout are never touched.
                for bank in &prepared.banks {
                    let bytes = &mut image[bank.origin..bank.origin + bank.size];
                    let donor = Flash::new(bytes).with_profile(Profile::for_slot(bank.profile));
                    let (efs_at, _) = donor.efs_checked().map_err(|e| format!("EFS: {e:?}"))?;
                    let tables: Vec<_> = donor
                        .bios_directories_checked(bank.profile)
                        .map_err(|e| format!("BIOS directory: {e:?}"))?
                        .iter()
                        .map(|dir| dir.offset)
                        .collect();
                    bytes[bank.prefix as usize..].fill(0xff);
                    if let Some((mode, speed)) = self.psp.spi {
                        Efs::mut_from_prefix(&mut bytes[efs_at..])
                            .map_err(|_| "no EFS")?
                            .0
                            .set_spi(bank.profile, mode, speed);
                    }
                    for at in tables {
                        modify_entries::<BiosEntry>(bytes, at, |entry| {
                            if entry.kind != bios::RESET_IMAGE {
                                return;
                            }
                            if bank.relocate && entry.has_data() {
                                const MODE: u64 = 3 << 62;
                                let source = entry.source.get();
                                let physical = (fstart_amdfw_format::ROM2_BASE..1 << 32)
                                    .contains(&(source & !MODE));
                                entry.source.set(
                                    source & MODE
                                        | (u64::from(bank.reset.flash_offset)
                                            + if physical {
                                                fstart_amdfw_format::ROM2_BASE
                                            } else {
                                                0
                                            }),
                                );
                                entry.size.set(bank.reset.size);
                                entry.destination.set(bank.reset.destination);
                                entry.flags |= BiosEntry::RESET | BiosEntry::COPY;
                            }
                            if self.psp.compress_reset {
                                entry.flags |= BiosEntry::COMPRESSED;
                            } else {
                                entry.flags &= !BiosEntry::COMPRESSED;
                            }
                        })
                        .ok_or("BIOS directory changed during composition")?;
                    }
                    let start = bank.reset.flash_offset as usize;
                    bytes[start..start + stored.len()].copy_from_slice(&stored);
                }
                (0, image, prepared.banks.clone(), true)
            }
            Firmware::Packed {
                files,
                reset_offset,
                reset,
            } => {
                let packed = self.pack(files, sources, stage.clone(), *reset_offset, reset)?;
                let mut image = if reset_offset.is_some() {
                    vec![0xff; files.flash_size as usize]
                } else {
                    packed.bytes.clone()
                };
                let offset = if let Some(at) = reset_offset {
                    image[packed.offset as usize..][..packed.bytes.len()]
                        .copy_from_slice(&packed.bytes);
                    image[*at as usize..][..stored.len()].copy_from_slice(&stored);
                    0
                } else {
                    packed.offset
                };
                let mut view = vec![0xff; files.flash_size as usize];
                view[offset as usize..][..image.len()].copy_from_slice(&image);
                let facts = read_bios_directory(&view, files.flash_size, reset.bios_directory)?;
                let bank = Bank {
                    origin: 0,
                    size: files.flash_size as usize,
                    prefix: files.flash_size,
                    profile: reset.bios_directory,
                    reset: ResetSlot {
                        flash_offset: reset_offset.unwrap_or(facts.reset_offset),
                        size: reset_size,
                        destination: reset_destination,
                    },
                    apob: Some(reset.apob_destination),
                    relocate: false,
                };
                (offset, image, vec![bank], reset_offset.is_some())
            }
        };
        if let (Some(window), Some(ffs)) = (&ffs_range, ffs) {
            bytes[window.start as usize..][..ffs.len()].copy_from_slice(ffs);
        }
        let mut readback = vec![0xff; boot_size];
        readback[offset as usize..][..bytes.len()].copy_from_slice(&bytes);
        let mut facts = Vec::new();
        // One readback verifier for every selected/generated bank: directory
        // checksums, dependency closure, reset geometry/flags and exact stage.
        for bank in &banks {
            let view = &readback[bank.origin..bank.origin + bank.size];
            let found = read_bios_directory(view, bank.prefix, bank.profile)?;
            check_facts(&found, bank)?;
            let levels = Flash::new(view)
                .with_profile(Profile::for_slot(bank.profile))
                .bios_directories_checked(bank.profile)
                .map_err(|e| format!("BIOS directory: {e:?}"))?;
            if levels
                .iter()
                .flat_map(|d| d.entries.iter())
                .filter(|e| e.kind == bios::RESET_IMAGE)
                .any(|e| e.compressed() != self.psp.compress_reset)
            {
                return Err(
                    "reset directory compression flag differs from its stored bytes".into(),
                );
            }
            let start = found.reset_offset as usize;
            let region = view
                .get(start..start + stage.len())
                .ok_or("reset readback outside its bank")?;
            let loaded = if self.psp.compress_reset {
                amd_decompress(region, stage.len())
            } else {
                Some(region.to_vec())
            };
            if loaded.as_deref() != Some(stage.as_slice()) {
                return Err("the PSP would not load the composed stage".into());
            }
            facts.push(BankFacts {
                origin: bank.origin,
                profile: bank.profile,
                bios: found,
            });
        }
        if let (
            Firmware::Vendor {
                layout: VendorLayout::Banks(layout),
                ..
            }
            | Firmware::PackedBanks { layout, .. },
            Some(prepared),
        ) = (&self.firmware, prepared.as_ref())
        {
            layout.verify_boot_order(&bytes, prepared.survey.as_ref().unwrap())?;
            bytes = layout.expand(vendor.unwrap(), bytes);
        }
        Ok(AmdImage {
            offset,
            bytes,
            banks: facts,
            patched,
            ffs_offset: ffs_range.map(|r| r.start),
            whole_flash,
            boot_size,
        })
    }

    fn pack(
        &self,
        files: &PspFiles,
        sources: &[&[u8]],
        reset: Vec<u8>,
        reset_offset: Option<u32>,
        geometry: &PackedReset,
    ) -> Result<super::PackedImage, String> {
        if files.two_level {
            return self.pack_two_level(files, sources, reset.len(), reset_offset, geometry);
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
        let split = files.psp.len();
        let apcb_at = split + files.bios.len();
        let mut psp: Vec<Blob> = blobs(&files.psp, &sources[..split]);
        if files.psp_app {
            psp.push(Blob {
                key: Key::new(super::psp_app::PSP_ENTRY, 0, 0),
                bytes: super::psp_app::image(),
            });
        }
        let mut bios: Vec<Blob> = blobs(&files.bios, &sources[split..apcb_at]);
        bios.retain(|blob: &Blob| blob.key.kind != bios::MICROCODE);
        if let Some(apcb) = &files.apcb {
            for (instance, spd) in sources[apcb_at + 1..].iter().enumerate() {
                let instance = u8::try_from(instance).map_err(|_| "too many APCB instances")?;
                let bytes = apcb.instance(sources[apcb_at], spd)?;
                bios.push(Blob {
                    key: Key::new(bios::APCB_BACKUP, 0, instance),
                    bytes: bytes.clone(),
                });
                bios.push(Blob {
                    key: Key::new(bios::APCB, 0, instance),
                    bytes,
                });
            }
        }
        PicassoImage {
            flash_size: files.flash_size,
            efs_offset: files.efs_offset,
            partition_end: files.partition_end,
            spi_read_mode: files.spi_read_mode.ok_or("no SPI read mode")?,
            spi_speed: files.spi_speed.ok_or("no SPI speed")?,
            micron: files.micron,
            soft_fuse: files.soft_fuse.ok_or("no soft fuse")?,
            psp,
            bios,
            reset_image: reset,
            reset_slot: reset_offset.map(|offset| (offset, self.psp.compress_reset)),
            reset_destination: geometry.destination,
            apob_destination: geometry.apob_destination,
            shared_memory: geometry.shared_memory,
            apob_nv: files.apob_nv,
            psp_nv: files.psp_nv,
        }
        .pack()
    }

    /// The two-level partition of `files`, with the reset image of
    /// `reset_size` bytes at `reset_offset`.
    fn pack_two_level(
        &self,
        files: &PspFiles,
        sources: &[&[u8]],
        reset_size: usize,
        reset_offset: Option<u32>,
        geometry: &PackedReset,
    ) -> Result<super::PackedImage, String> {
        let reset = levels::Reset {
            offset: reset_offset.ok_or("a two-level partition needs a reset window")?,
            size: u32::try_from(reset_size).map_err(|_| "reset image too large")?,
            destination: geometry.destination,
            compressed: self.psp.compress_reset,
        };
        let mut sources = sources.iter();
        files
            .two_level(
                geometry.bios_directory,
                |_| Ok(sources.next().ok_or("missing AMD file")?.to_vec()),
                reset,
                geometry.apob_destination,
            )?
            .pack()
    }
}

impl PspFiles {
    fn bindings(&self) -> Vec<FileBinding> {
        let capacity = u64::from(self.partition_end - self.efs_offset);
        let blobs = [("PSP", &self.psp), ("BIOS", &self.bios)]
            .into_iter()
            .flat_map(|(namespace, blobs)| {
                blobs.iter().map(move |file| FileBinding {
                    environment: format!(
                        "{namespace}_{:02X}_{:02X}_{:02X}{}",
                        file.key.kind,
                        file.key.subprogram,
                        file.key.instance,
                        match file.level {
                            Level::Both => "",
                            Level::One => "_L1",
                            Level::Two => "_L2",
                        }
                    ),
                    path: file.path.clone(),
                    capacity,
                })
            });
        let apcb = self.apcb.iter().flat_map(|apcb| {
            core::iter::once(("APCB_TEMPLATE".to_string(), &apcb.template)).chain(
                apcb.spds
                    .iter()
                    .enumerate()
                    .map(|(i, spd)| (format!("APCB_SPD_{i:02X}"), spd)),
            )
        });
        blobs
            .chain(apcb.map(|(environment, path)| FileBinding {
                environment,
                path: path.clone(),
                capacity: 0x10000,
            }))
            .collect()
    }
    /// The two-level partition of these files for the PSP of `slot`, each
    /// file's bytes from `read` in [`AmdAssembly::bindings`] order. The
    /// microcode goes into the FFS, not the BIOS directory.
    pub fn two_level(
        &self,
        slot: fstart_amdfw_format::efs::BiosDirectorySlot,
        mut read: impl FnMut(&FileBlob) -> Result<Vec<u8>, String>,
        reset: levels::Reset,
        apob_destination: u64,
    ) -> Result<TwoLevelImage, String> {
        if !self.two_level || self.apcb.is_some() || self.psp_app {
            return Err("not a two-level partition of files".into());
        }
        let mut entries = |declared: &[FileBlob]| {
            let mut entries = Vec::new();
            for file in declared {
                let bytes = read(file)?;
                if file.key.kind != bios::MICROCODE {
                    entries.push(levels::Entry {
                        key: file.key,
                        level: file.level,
                        content: levels::Content::File(bytes),
                    });
                }
            }
            Ok::<_, String>(entries)
        };
        let mut psp = entries(&self.psp)?;
        let mut bios = entries(&self.bios)?;
        for fixed in &self.fixed {
            let entry = levels::Entry {
                key: fixed.key,
                level: fixed.level,
                content: match fixed.content {
                    Fixed::Erased(size) => levels::Content::Erased(size),
                    Fixed::Value(value) => levels::Content::Value(value),
                },
            };
            if fixed.psp { &mut psp } else { &mut bios }.push(entry);
        }
        Ok(TwoLevelImage {
            slot,
            flash_size: self.flash_size,
            efs_offset: self.efs_offset,
            partition_end: self.partition_end,
            spi: self.spi_read_mode.zip(self.spi_speed),
            psp,
            bios,
            reset,
            apob_destination,
        })
    }
}
fn check_facts(facts: &BiosDirectoryFacts, bank: &Bank) -> Result<(), String> {
    if (
        facts.reset_offset,
        facts.reset_size,
        facts.reset_destination,
    ) != (
        bank.reset.flash_offset,
        bank.reset.size,
        bank.reset.destination,
    ) || bank
        .apob
        .is_some_and(|expected| facts.apob_destination != Some(expected))
    {
        return Err(format!(
            "BIOS directory {facts:?} differs from planned reset {:?}",
            bank.reset
        ));
    }
    Ok(())
}
