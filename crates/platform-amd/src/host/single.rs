// SPDX-License-Identifier: MIT OR Apache-2.0
//! One 16 MiB flash image with the PSP firmware of one SoC, packed from the
//! SoC's and the board's blob tables (`--blobs`, or the blobs `--donor`
//! extracts from a vendor image). The stage is the reset image; the FFS is
//! the layout's window.
extern crate std;
use super::{Host, bounded};
use crate::HostOptions;
use fstart_amdfw_format::efs::BiosDirectorySlot;
use fstart_core::board::{AmdMicrocodeConfig, MicrocodeConfig};
use fstart_image_build::{
    amd::{
        assembly::{AmdAssembly, FfsWindow, Firmware, PspOptions},
        blobs,
    },
    build_plan::{FfsAssembly, ImageOperation, ImageStep},
    plan::BuildSelection,
};
use std::{boxed::Box, format, string::String, vec::Vec};

/// How the PSP firmware packed from the files of the SoC's and the board's
/// blob tables ([`blobs`]) makes the image. With `reset_offset`, a whole
/// native image: the PSP partition, the reset image at that flash offset
/// and the layout's FFS.
#[derive(Clone, Copy)]
pub struct Source {
    pub reset_offset: Option<u32>,
    pub psp: PspOptions,
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
    /// Without blobs, just the FFS.
    pub(super) fn images(
        &self,
        host: &Host,
        selection: &BuildSelection,
        options: &HostOptions,
        ffs: impl FnOnce(Option<MicrocodeConfig>, bool) -> Option<FfsAssembly>,
    ) -> Result<Vec<ImageStep>, String> {
        let layout = &host.layout;
        let packed = self.packed(host, selection, options)?;
        let microcode = packed.as_ref().map(microcode).transpose()?.flatten();
        let mut ffs = ffs(microcode, true);
        // The video BIOSes of the SoC's blob table go into the FFS.
        if let (Some(ffs), Some(packed)) = (&mut ffs, &packed) {
            ffs.file_assets = packed.files.assets.clone();
        }
        let flash_size = layout.flash.size as u32;
        let Some(packed) = packed else {
            return Ok(ffs.map(ImageStep::from).into_iter().collect());
        };
        let firmware = Firmware::Packed {
            files: packed.files,
            reset_offset: self.source.reset_offset,
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
            psp: self.source.psp,
        };
        assembly.validate()?;
        let mut steps: Vec<ImageStep> = ffs.map(ImageStep::from).into_iter().collect();
        steps.push(ImageStep::new(
            "flash",
            ImageOperation::AmdEfs(Box::new(assembly)),
        ));
        Ok(steps)
    }

    /// The files of a packed image: the SoC's blob table
    /// (`[package.metadata.fstart.amd.<soc>]` of this crate) and the board's
    /// (`[package.metadata.fstart.amd]`) in the selected blob directory. None
    /// without a blob directory.
    fn packed(
        &self,
        host: &Host,
        selection: &BuildSelection,
        options: &HostOptions,
    ) -> Result<Option<blobs::Resolved>, String> {
        let Some(dir) = &options.blobs else {
            return Ok(None);
        };
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

/// The microcode patches the FFS carries for the CPU driver: the microcode
/// files of the SoC's blob table, if it names any.
fn microcode(packed: &blobs::Resolved) -> Result<Option<MicrocodeConfig>, String> {
    if packed.microcode.is_empty() {
        return Ok(None);
    }
    let mut files = heapless::Vec::new();
    for file in &packed.microcode {
        files
            .push(bounded(file)?)
            .map_err(|_| "too many microcode files")?;
    }
    Ok(Some(MicrocodeConfig::Amd(AmdMicrocodeConfig { files })))
}
