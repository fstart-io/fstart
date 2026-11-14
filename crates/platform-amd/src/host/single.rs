// SPDX-License-Identifier: MIT OR Apache-2.0
//! One 16 MiB flash image with the PSP firmware of one SoC: from a vendor
//! image (`--image-config`) or packed from the SoC's and the board's blob
//! tables (`--blobs`). The stage replaces the reset image; the FFS is the
//! layout's window.
extern crate std;
use super::{Host, bounded};
use crate::HostOptions;
use fstart_amdfw_format::efs::BiosDirectorySlot;
use fstart_core::board::{AmdMicrocodeConfig, AmdVendorMicrocode, MicrocodeConfig};
use fstart_image_build::{
    amd::{
        assembly::{AmdAssembly, FfsWindow, Firmware, PspOptions},
        blobs,
    },
    build_plan::{FfsAssembly, FileBinding, ImageOperation, ImageStep},
    plan::BuildSelection,
};
use std::{boxed::Box, format, string::String, vec::Vec};

/// Where the PSP firmware of a full image comes from.
#[derive(Clone, Copy)]
pub enum Source {
    /// A vendor flash image: its first `prefix` bytes (EFS, directories,
    /// signed blobs) stay apart from the `psp` fields, the stage replaces
    /// the reset image.
    Vendor { prefix: u32, psp: PspOptions },
    /// PSP firmware packed from the files of the SoC's and the board's
    /// blob tables in the `--blobs` directory ([`blobs`]). With
    /// `reset_offset`, a whole native image: the PSP partition, the reset
    /// image at that flash offset and the layout's FFS.
    Packed {
        reset_offset: Option<u32>,
        psp: PspOptions,
    },
}

/// The image of one SoC.
pub struct Image {
    /// The SoC's blob table (`[package.metadata.fstart.amd.<soc>]`).
    pub soc: &'static str,
    /// Where the PSP finds the BIOS directory.
    pub bios_directory: BiosDirectorySlot,
    /// Where the PSP writes the APOB.
    pub apob_destination: u64,
    pub source: Source,
}

impl Image {
    /// The image steps: the FFS `ffs` builds with the microcode it
    /// carries, then the flash with the PSP firmware and the reset image.
    /// Without a vendor image or blobs, just the FFS.
    pub(super) fn images(
        &self,
        host: &Host<'_>,
        selection: &BuildSelection,
        options: &HostOptions,
        ffs: impl FnOnce(Option<MicrocodeConfig>, bool) -> Option<FfsAssembly>,
    ) -> Result<Vec<ImageStep>, String> {
        let layout = &host.layout;
        let packed = self.packed(host, selection, options)?;
        let microcode = self.microcode(options, packed.as_ref())?;
        let mut ffs = ffs(microcode, true);
        // The video BIOSes of the SoC's blob table go into the FFS.
        if let (Some(ffs), Some(packed)) = (&mut ffs, &packed) {
            ffs.file_assets = packed.files.assets.clone();
        }
        let flash_size = layout.flash.size as u32;
        let (firmware, psp) = match (self.source, &options.image_config, packed) {
            (Source::Vendor { prefix, psp }, Some(path), _) => (
                Firmware::Vendor {
                    image: FileBinding {
                        environment: "VENDOR_FLASH".into(),
                        path: path.clone(),
                        capacity: u64::from(flash_size),
                    },
                    prefix_size: prefix,
                },
                psp,
            ),
            (Source::Packed { reset_offset, psp }, _, Some(packed)) => (
                Firmware::Packed {
                    files: packed.files,
                    reset_offset,
                },
                psp,
            ),
            _ => return Ok(ffs.map(ImageStep::from).into_iter().collect()),
        };
        let assembly = AmdAssembly {
            firmware,
            ffs: ffs.as_ref().zip(layout.ffs).map(|(_, window)| FfsWindow {
                step: ImageStep::FFS.into(),
                offset: (window.base - layout.flash.base) as u32,
                size: window.size as u32,
            }),
            stage_unit: super::STAGE_UNIT.into(),
            flash_size,
            bios_directory: self.bios_directory,
            reset_size: layout.image.size as u32,
            reset_destination: layout.image.base,
            apob_destination: self.apob_destination,
            // Entry 0x6B is only for a PSP verstage.
            shared_memory: None,
            psp,
        };
        assembly.validate()?;
        // Check the vendor image against the layout the stage is linked for
        // before building.
        if let Firmware::Vendor { image, .. } = &assembly.firmware {
            let path = &image.path;
            let vendor = std::fs::read(path).map_err(|e| format!("vendor image {path}: {e}"))?;
            assembly
                .check_vendor(&vendor)
                .map_err(|e| format!("vendor image {path}: {e}"))?;
        }
        let mut steps: Vec<ImageStep> = ffs.map(ImageStep::from).into_iter().collect();
        steps.push(ImageStep::new(
            "flash",
            ImageOperation::AmdEfs(Box::new(assembly)),
        ));
        Ok(steps)
    }

    /// The microcode patches the FFS carries for the CPU driver: the BIOS
    /// directory microcode of a vendor image, or the microcode files of the
    /// SoC's blob table. None without an image configuration or blobs.
    fn microcode(
        &self,
        options: &HostOptions,
        packed: Option<&blobs::Resolved>,
    ) -> Result<Option<MicrocodeConfig>, String> {
        let config = match (self.source, &options.image_config, packed) {
            (Source::Vendor { .. }, Some(path), _) => {
                let image =
                    std::fs::canonicalize(path).map_err(|e| format!("vendor image {path}: {e}"))?;
                AmdMicrocodeConfig {
                    files: heapless::Vec::new(),
                    vendor_images: heapless::Vec::from_slice(&[AmdVendorMicrocode {
                        image: bounded(&image.to_string_lossy())?,
                        bios_directory: self.bios_directory,
                        origin: 0,
                    }])
                    .map_err(|_| "too many vendor images")?,
                }
            }
            (Source::Packed { .. }, _, Some(packed)) if !packed.microcode.is_empty() => {
                let mut files = heapless::Vec::new();
                for file in &packed.microcode {
                    files
                        .push(bounded(file)?)
                        .map_err(|_| "too many microcode files")?;
                }
                AmdMicrocodeConfig {
                    files,
                    vendor_images: heapless::Vec::new(),
                }
            }
            _ => return Ok(None),
        };
        Ok(Some(MicrocodeConfig::Amd(config)))
    }

    /// The files of a packed image: the SoC's blob table
    /// (`[package.metadata.fstart.amd.<soc>]` of this crate) and the board's
    /// (`[package.metadata.fstart.amd]`) in the selected blob directory. None
    /// without a blob directory.
    fn packed(
        &self,
        host: &Host<'_>,
        selection: &BuildSelection,
        options: &HostOptions,
    ) -> Result<Option<blobs::Resolved>, String> {
        let (Source::Packed { .. }, Some(dir)) = (self.source, &options.blobs) else {
            return Ok(None);
        };
        if options.image_config.is_some() {
            return Err(format!(
                "{} images are packed from --blobs, not --image-config",
                host.name
            ));
        }
        let tables = &selection.manifests;
        let board: blobs::BoardBlobs = serde_json::from_value(tables.board["amd"].clone())
            .map_err(|e| format!("board [package.metadata.fstart.amd]: {e}"))?;
        if board.apob_destination.is_some() {
            return Err(format!("{} places the APOB itself", host.name));
        }
        if board.soc != self.soc {
            return Err(format!(
                "the board's AMD firmware is for {}, not {}",
                board.soc, self.soc
            ));
        }
        let soc: blobs::SocBlobs = serde_json::from_value(tables.platform["amd"][self.soc].clone())
            .map_err(|e| format!("platform [package.metadata.fstart.amd.{}]: {e}", self.soc))?;
        if soc.bios_directory != self.bios_directory {
            return Err("the SoC's blob table names another BIOS directory".into());
        }
        let name = tables.board["board"]
            .as_str()
            .ok_or("the board table has no board name")?;
        let dir = std::fs::canonicalize(dir).map_err(|e| format!("blob directory {dir}: {e}"))?;
        blobs::resolve(&soc, &board, name, &dir, host.layout.flash.size as u32).map(Some)
    }
}
