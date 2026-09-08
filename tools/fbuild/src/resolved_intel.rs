//! One resolved image with the fixed Intel CAR/postcar/ramstage compiler units.

use crate::{
    board_manifest::BoardManifest,
    intel_layout::{IntelReservations, IntelStage},
    profile_source::ProfileSource,
};
use fstart_core::board::{IntelMicrocodeConfig, MicrocodeConfig};
use fstart_core::*;
use fstart_core::{IntelIfdFlashLayout, SecurityConfig, SmmConfig};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::{
    collections::BTreeMap,
    path::{Component, Path},
};

pub use fstart_image_build::plan::IntelStagePlan as ResolvedIntelStage;

#[derive(Debug, Clone, Serialize)]
pub struct ResolvedIntel {
    pub target: String,
    pub payload: String,
    pub reservations: IntelReservations,
    pub stages: [ResolvedIntelStage; 3],
    pub ifd: IntelIfdFlashLayout,
    /// Normalized board-relative assembler inputs, derived from workspace paths.
    pub microcode: Vec<String>,
    pub security: SecurityConfig,
    pub max_cpus: u16,
    pub smm: SmmConfig,
    pub smm_features: Vec<String>,
    pub origins: BTreeMap<String, String>,
}

impl ResolvedIntel {
    pub(crate) fn from_plan(
        root: &Path,
        board: &BoardManifest,
        loaded: ProfileSource,
        p: fstart_image_build::plan::IntelPlan,
    ) -> Result<Self, String> {
        if board.platform.as_deref().is_some_and(|p| p != "x86_64")
            || board.target.as_deref().is_some_and(|t| t != p.target)
        {
            return Err("board platform/target conflicts with Intel plan".into());
        }
        if board.layout != Default::default() {
            return Err("monolithic layout overrides cannot select Intel stage budgets".into());
        }
        if board.features != board.variant_features {
            return Err("Intel metadata builds do not use board feature forwarding".into());
        }
        let ifd = p.validate()?;
        let dependency = &board
            .build_profile
            .as_ref()
            .ok_or("missing platform reference")?
            .dependency;
        let qualify = |features: Vec<String>| -> Result<Vec<String>, String> {
            let mut features = features
                .into_iter()
                .map(|f| format!("{dependency}/{f}"))
                .chain(board.variant_features.iter().cloned())
                .collect::<Vec<_>>();
            features.sort();
            features.dedup();
            crate::resolved::validate_features(&features, &loaded.package, &loaded.metadata)?;
            Ok(features)
        };
        let [a, b, c] = p
            .stages
            .map(|mut stage| -> Result<ResolvedIntelStage, String> {
                stage.features = qualify(stage.features)?;
                Ok(stage)
            });
        let stages = [a?, b?, c?];
        let smm_features = qualify(p.smm_features)?;
        if p.microcode.is_empty() || p.microcode.len() > 16 {
            return Err("Intel profile needs 1..16 microcode inputs".into());
        }
        let board_relative = board
            .dir
            .strip_prefix(root)
            .map_err(|_| "board is outside workspace")?;
        let prefix = "../".repeat(board_relative.components().count());
        let microcode = p
            .microcode
            .iter()
            .map(|file| {
                if !Path::new(file)
                    .components()
                    .all(|c| matches!(c, Component::Normal(_)))
                {
                    return Err("microcode input must be a workspace-relative file".to_owned());
                }
                let path = format!("{prefix}{file}");
                if path.len() > 128 {
                    return Err("microcode assembler path exceeds capacity".to_owned());
                }
                Ok(path)
            })
            .collect::<Result<_, _>>()?;
        Ok(Self {
            target: p.target,
            payload: p.payload,
            reservations: p.reservations,
            stages,
            ifd,
            microcode,
            security: p.security,
            max_cpus: p.max_cpus,
            smm: p.smm,
            smm_features,
            origins: BTreeMap::from([
                ("profile".into(), loaded.source),
                (
                    "profile-manifest".into(),
                    loaded.platform_manifest.display().to_string(),
                ),
                ("microcode-path-base".into(), "workspace".into()),
            ]),
        })
    }
}

impl ResolvedIntel {
    pub fn json(&self) -> Result<String, String> {
        serde_json::to_string_pretty(self).map_err(|e| e.to_string())
    }

    pub fn artifact_dir(&self, root: &Path, board: &str, release: bool) -> Result<PathBuf, String> {
        let mut hash = Sha256::new();
        hash.update(self.json()?);
        for stage in &self.stages {
            hash.update(crate::linker::resolved_intel(
                &self.reservations,
                stage.role,
                true,
            )?);
        }
        hash.update(crate::toolchain::rustflags_for_triple(&self.target));
        Ok(root
            .join("target/fstart-build")
            .join(board)
            .join(if release { "release" } else { "debug" })
            .join(format!("{:x}", hash.finalize())))
    }

    /// Compatibility projection for image assembly only; never a compiler plan.
    pub fn assembler_config(&self, board: &str) -> Result<BoardConfig, String> {
        let text =
            |s: &str| heapless::String::try_from(s).map_err(|_| "board name too long".to_owned());
        let mut stages = heapless::Vec::new();
        for row in &self.stages {
            let reservation = self.reservations.stage(row.role);
            let ram = row.role == IntelStage::Ramstage;
            let boot = row.role == IntelStage::Bootblock;
            stages
                .push(StageConfig {
                    name: hstr(row.role.name()),
                    build: StageBuildConfig {
                        firmware_image: (boot || ram).then_some(FirmwareImageConfig {
                            temp_ram_buffer: ram.then_some(TempRamBuffer {
                                base: self.reservations.scratch.base,
                                size: self.reservations.scratch.size,
                            }),
                        }),
                        verify_firmware: boot || ram,
                        load_next_stage: match row.role {
                            IntelStage::Bootblock => Some(hstr("postcar")),
                            IntelStage::Postcar => Some(hstr("ramstage")),
                            IntelStage::Ramstage => None,
                        },
                        payload: ram,
                        pci: ram,
                        acpi: ram,
                        smbios: ram,
                        mp: ram.then_some(MpBuildConfig {
                            max_cpus: self.max_cpus,
                            smm: true,
                        }),
                        ..Default::default()
                    },
                    load_addr: reservation.image.base,
                    data_addr: Some(reservation.writable.base),
                    stack_size: reservation
                        .stack
                        .try_into()
                        .map_err(|_| "stack too large")?,
                    heap_size: (reservation.heap != 0)
                        .then(|| u32::try_from(reservation.heap))
                        .transpose()
                        .map_err(|_| "heap too large")?,
                    runs_from: if boot { RunsFrom::Rom } else { RunsFrom::Ram },
                    compression: if ram {
                        Compression::Lz4
                    } else {
                        Compression::None
                    },
                    page_table_addr: None,
                    page_size: Default::default(),
                })
                .map_err(|_| "too many stages")?;
        }
        let microcode = self
            .microcode
            .iter()
            .map(|path| {
                heapless::String::try_from(path.as_str()).map_err(|_| "microcode path too long")
            })
            .collect::<Result<heapless::Vec<_, 16>, _>>()?;
        Ok(BoardConfig {
            name: text(board)?,
            platform: Platform::X86_64,
            memory: MemoryMap {
                regions: hvec([MemoryRegion {
                    name: hstr("bootstrap-ram"),
                    base: self.reservations.bootstrap_ram.base,
                    size: self.reservations.bootstrap_ram.size,
                    kind: RegionKind::Ram,
                }]),
                flash_layout: Some(FlashLayout::IntelIfd(self.ifd)),
                car: Some(CarConfig {
                    base: self.reservations.bootblock.writable.base,
                    size: self.reservations.bootblock.writable.size,
                }),
            },
            stages: StageLayout::MultiStage(stages),
            security: self.security.clone(),
            payload: (self.payload == "uefi").then(x86_uefi_payload),
            microcode: Some(MicrocodeConfig::Intel(IntelMicrocodeConfig {
                files: microcode,
                early: true,
                mp: true,
            })),
            soc_image_format: SocImageFormat::None,
            full_flash_image: true,
            build: BoardBuildPolicy {
                firmware_image: FirmwareImagePolicy::memory_mapped(
                    self.reservations.firmware.base,
                    self.reservations.firmware.size,
                ),
                ..Default::default()
            },
            acpi: Some(AcpiConfig {
                print_hex: true,
                platform: AcpiPlatform::X86,
            }),
            smbios: None,
            smm: Some(self.smm),
            boot_hart_id: 0,
        })
    }
}
