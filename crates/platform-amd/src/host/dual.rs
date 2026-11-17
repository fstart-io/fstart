// SPDX-License-Identifier: MIT OR Apache-2.0
//! The SP3 flash (FIRMWARE_IMAGE §7): a 16 MiB half per generation, each
//! with its own EFS, PSP firmware and a copy of the reset image, and one
//! FFS in the lower half. `fbuild assemble` packs each half from the blobs
//! of its generation (`--blobs`, or the blobs `--donor` extracts from a
//! vendor image) with [`fstart_image_build::amd::halved_flash`]: one half
//! per generation the blobs have. A vendor image only donates blobs.
//!
//! The FFS holds the payload and a copy of the stage unit: the FFS builder
//! finalizes the unit's trust policy only in FFS-resident files, and the
//! composer copies it, the FFS locator and the flash anchor (FFS flash
//! offset and size) into the PSP-copied `0x62` image.
extern crate std;
use super::{Host, bounded};
use crate::HostOptions;
use fstart_amdfw_format::efs::BiosDirectorySlot;
use fstart_core::board::{AmdMicrocodeConfig, MicrocodeConfig};
use fstart_image_build::amd::blobs;
use fstart_image_build::amd::halved_flash::{AmdHalvedFlashAssembly, FlashPlan, PackedHalf};
use fstart_image_build::build_plan::{FfsAssembly, ImageOperation, ImageStep};
use fstart_image_build::plan::BuildSelection;
use std::{boxed::Box, format, vec, vec::Vec};

/// A generation the flash carries: its blob table
/// (`[package.metadata.fstart.amd.<name>]`) and its BIOS directory.
#[derive(Debug, Clone, Copy)]
pub struct Half {
    pub name: &'static str,
    pub bios_directory: BiosDirectorySlot,
}

/// The SP3 flash of a board.
pub struct Image {
    /// The generations the board lists, in boot ROM order.
    pub halves: Vec<Half>,
    /// How the build's halves are laid out.
    pub plan: FlashPlan,
}

impl Image {
    /// The composed flash: the FFS (with a copy of the stage unit) in the
    /// lower half and the halves packed from the blob directory. Without
    /// blobs, just the FFS.
    pub(super) fn images(
        &self,
        host: &Host,
        selection: &BuildSelection,
        options: &HostOptions,
        ffs: impl FnOnce(Option<MicrocodeConfig>, bool) -> Option<FfsAssembly>,
    ) -> Result<Vec<ImageStep>, String> {
        if self.halves.is_empty() {
            return Err(format!("{} lists no SP3 generation", host.name));
        }
        // The PSP firmware is packed from the blob directory: `--blobs`,
        // or the blobs fbuild extracts from a `--donor` image.
        let Some(dir) = &options.blobs else {
            let ffs = ffs(None, false).ok_or("the SP3 layout has an FFS")?;
            return Ok(vec![ffs.into()]);
        };
        let (packed, microcode) = self.packed(selection, dir)?;
        let ffs = ffs(Some(microcode), false).ok_or("the SP3 layout has an FFS")?;
        Ok(vec![
            ffs.into(),
            ImageStep::new(
                "flash",
                ImageOperation::AmdHalvedFlash(Box::new(AmdHalvedFlashAssembly {
                    ffs: ImageStep::FFS.into(),
                    packed,
                    stage_unit: super::STAGE_UNIT.into(),
                    compose: self.plan.clone(),
                })),
            ),
        ])
    }

    /// The halves packed from the blob directory `dir`: one per generation
    /// the board lists, from the platform's
    /// `[package.metadata.fstart.amd.<generation>]` table and the board's;
    /// and the microcode files of all of them for the FFS. The image step
    /// packs the halves whose firmware the blobs have
    /// ([`fstart_image_build::amd::halved_flash::keep_present`]).
    fn packed(
        &self,
        selection: &BuildSelection,
        dir: &str,
    ) -> Result<(Vec<PackedHalf>, MicrocodeConfig), String> {
        // Absolute, but not read: a bundle's plan names the directory its
        // assembly fills from the donor (`fstart_image_build::bundle`).
        let dir = std::path::absolute(dir).map_err(|e| format!("blob directory {dir}: {e}"))?;
        let tables = &selection.manifests;
        let boards = blobs::board_tables(&tables.board["amd"])?;
        let name = tables.board["board"]
            .as_str()
            .ok_or("the board table has no board name")?;
        let mut halves = vec![];
        let mut files = heapless::Vec::new();
        for half in &self.halves {
            let board = boards
                .iter()
                .find(|board| board.soc == half.name)
                .ok_or_else(|| format!("the board has no {} firmware table", half.name))?;
            let soc: blobs::SocBlobs =
                serde_json::from_value(tables.platform["amd"][half.name].clone()).map_err(|e| {
                    format!("platform [package.metadata.fstart.amd.{}]: {e}", half.name)
                })?;
            if soc.bios_directory != half.bios_directory {
                return Err(format!(
                    "the {} table names another BIOS directory",
                    half.name
                ));
            }
            let resolved = blobs::resolve(
                &soc,
                board,
                name,
                &dir,
                fstart_amdfw_format::HALF_SIZE as u32,
            )?;
            for file in &resolved.microcode {
                files
                    .push(bounded(file)?)
                    .map_err(|_| "too many microcode files")?;
            }
            halves.push(PackedHalf {
                generation: half.bios_directory,
                microcode: resolved.microcode,
                files: resolved.files,
                apob_destination: board
                    .apob_destination
                    .or(soc.apob_destination)
                    .ok_or_else(|| format!("the {} table has no APOB destination", half.name))?,
            });
        }
        if halves.len() > 2 {
            return Err("an SP3 flash has two halves, not one per generation".into());
        }
        Ok((halves, MicrocodeConfig::Amd(AmdMicrocodeConfig { files })))
    }
}
