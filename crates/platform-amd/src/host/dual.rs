// SPDX-License-Identifier: MIT OR Apache-2.0
//! The SP3 flash (FIRMWARE_IMAGE §7): a 16 MiB half per generation, each
//! with its own EFS, PSP firmware and a copy of the reset image, and one
//! FFS in the lower half. `fbuild assemble` composes the build into the
//! board's vendor ROM (or `--image-config`, e.g. the target board's flash
//! backup) or packs the halves from blobs with
//! [`fstart_image_build::amd::halved_flash`].
//!
//! The FFS holds the payload and a copy of the stage unit: the FFS builder
//! finalizes the unit's trust policy only in FFS-resident files, and the
//! composer copies it, the FFS locator and the flash anchor (FFS flash
//! offset and size) into the PSP-copied `0x62` image.
extern crate std;
use super::{Host, bounded};
use crate::HostOptions;
use fstart_amdfw_format::efs::BiosDirectorySlot;
use fstart_core::board::{AmdMicrocodeConfig, AmdVendorMicrocode, MicrocodeConfig};
use fstart_image_build::amd::blobs;
use fstart_image_build::amd::halved_flash::{
    AmdHalvedFlashAssembly, FlashPlan, PackedHalf, Survey,
};
use fstart_image_build::build_plan::{FfsAssembly, ImageOperation, ImageStep};
use fstart_image_build::plan::BuildSelection;
use std::string::String;
use std::{boxed::Box, format, vec, vec::Vec};

/// A generation the flash carries: its blob table
/// (`[package.metadata.fstart.amd.<name>]`) and its BIOS directory.
#[derive(Debug, Clone, Copy)]
pub struct Half {
    pub name: &'static str,
    pub bios_directory: BiosDirectorySlot,
}

/// The SP3 flash of a board.
pub struct Image<'a> {
    /// The generations the board lists, in boot ROM order.
    pub halves: &'a [Half],
    /// Workspace-relative vendor ROM: the 32 MiB dual image, one 16 MiB
    /// half, or a full readout with the plan's mirrors.
    pub vendor_image: &'static str,
    /// How the build is composed into the vendor ROM.
    pub plan: FlashPlan,
}

impl Image<'_> {
    /// The composed flash: the FFS (with a copy of the stage unit) in the
    /// lower half and the halves from the vendor ROM or the blob directory.
    pub(super) fn images(
        &self,
        host: &Host<'_>,
        selection: &BuildSelection,
        options: &HostOptions,
        ffs: impl FnOnce(Option<MicrocodeConfig>, bool) -> Option<FfsAssembly>,
    ) -> Result<Vec<ImageStep>, String> {
        if self.halves.is_empty() {
            return Err(format!("{} lists no SP3 generation", host.name));
        }
        // The PSP firmware: packed from the blob directory (`--blobs`,
        // `--donor`), or the board's vendor ROM, which `--image-config`
        // replaces (same workspace-relative or absolute path rules).
        let (vendor_image, packed, microcode) = match (&options.blobs, &options.image_config) {
            (Some(_), Some(_)) => {
                return Err(
                    "SP3 images are packed from --blobs or composed into --image-config".into(),
                );
            }
            (Some(dir), None) => {
                let (halves, microcode) = self.packed(selection, dir)?;
                (String::new(), halves, microcode)
            }
            (None, image) => {
                let vendor_image = image.clone().unwrap_or_else(|| self.vendor_image.into());
                let microcode = self.microcode(&vendor_image)?;
                (vendor_image, vec![], microcode)
            }
        };
        let ffs = ffs(Some(microcode), false).ok_or("the SP3 layout has an FFS")?;
        Ok(vec![
            ffs.into(),
            ImageStep::new(
                "flash",
                ImageOperation::AmdHalvedFlash(Box::new(AmdHalvedFlashAssembly {
                    ffs: ImageStep::FFS.into(),
                    vendor_image,
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
    /// and the microcode files of all of them for the FFS.
    fn packed(
        &self,
        selection: &BuildSelection,
        dir: &str,
    ) -> Result<(Vec<PackedHalf>, MicrocodeConfig), String> {
        let dir = std::fs::canonicalize(dir).map_err(|e| format!("blob directory {dir}: {e}"))?;
        let tables = &selection.manifests;
        let boards = blobs::board_tables(&tables.board["amd"])?;
        let name = tables.board["board"]
            .as_str()
            .ok_or("the board table has no board name")?;
        let mut halves = vec![];
        let mut files = heapless::Vec::new();
        for half in self.halves {
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
        Ok((
            halves,
            MicrocodeConfig::Amd(AmdMicrocodeConfig {
                files,
                vendor_images: heapless::Vec::new(),
            }),
        ))
    }

    /// The microcode the FFS carries for MP init: the BIOS directory
    /// patches of every generation the board supports, from either half of
    /// the vendor ROM (the composer decides which half boots which
    /// generation).
    fn microcode(&self, vendor_image: &str) -> Result<MicrocodeConfig, String> {
        let mut vendor_images = heapless::Vec::new();
        for half in self.halves {
            for origin in [0, fstart_amdfw_format::HALF_SIZE as u32] {
                vendor_images
                    .push(AmdVendorMicrocode {
                        image: vendor_image
                            .try_into()
                            .map_err(|_| format!("vendor image path {vendor_image} is too long"))?,
                        bios_directory: half.bios_directory,
                        origin,
                    })
                    .map_err(|_| "too many microcode sources")?;
            }
        }
        Ok(MicrocodeConfig::Amd(AmdMicrocodeConfig {
            files: heapless::Vec::new(),
            vendor_images,
        }))
    }

    /// Check the board's vendor ROM against the build without composing:
    /// the halves it would compose and keep and the FFS window. `root` is
    /// the workspace root.
    pub fn check_vendor(&self, root: &std::path::Path) -> Result<Survey, String> {
        let path = root.join(self.vendor_image);
        let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(self.plan.survey(&bytes)?)
    }
}
