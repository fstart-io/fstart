//! One resolved image with the fixed Intel CAR/postcar/ramstage compiler units.

#[cfg(test)]
#[path = "resolved_intel_tests.rs"]
mod tests;

use crate::{
    board_manifest::BoardManifest,
    intel_layout::{IntelReservations, IntelStage},
    payload::PayloadChoice,
    profile_source::ProfileSource,
};
use fstart_core::board::{IntelMicrocodeConfig, MicrocodeConfig};
use fstart_core::*;
use fstart_core::{
    ConstVec, IntelIfdFlashLayout, IntelIfdRegion, IntelIfdRegionConfig, SecurityConfig, SmmConfig,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::{
    collections::BTreeMap,
    path::{Component, Path},
};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct Profile {
    family: String,
    target: String,
    reservations: IntelReservations,
    ifd: Vec<IfdRegion>,
    bootblock_features: Vec<String>,
    postcar_features: Vec<String>,
    ramstage_features: Vec<String>,
    default_payload: String,
    /// Paths are workspace-relative, not relative to whichever board selected us.
    microcode: Vec<String>,
    security: SecurityConfig,
    max_cpus: u16,
    smm: SmmConfig,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct IfdRegion {
    kind: IfdKind,
    offset: u32,
    size: u32,
}
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum IfdKind {
    Descriptor,
    Gbe,
    Me,
    Bios,
}
impl IfdKind {
    fn core(self) -> IntelIfdRegion {
        match self {
            Self::Descriptor => IntelIfdRegion::Descriptor,
            Self::Gbe => IntelIfdRegion::Gbe,
            Self::Me => IntelIfdRegion::Me,
            Self::Bios => IntelIfdRegion::Bios,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ResolvedIntelStage {
    pub role: IntelStage,
    pub features: Vec<String>,
    pub payload: String,
}

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
    pub origins: BTreeMap<String, String>,
}

impl ResolvedIntel {
    pub(crate) fn from_profile_source(
        root: &Path,
        board: &BoardManifest,
        choice: Option<PayloadChoice>,
        loaded: ProfileSource,
    ) -> Result<Self, String> {
        let p: Profile =
            serde_json::from_value(loaded.profile).map_err(|e| format!("Intel profile: {e}"))?;
        if p.family != "intel-car" || p.target != "x86_64-unknown-none" {
            return Err("unsupported Intel family/target".into());
        }
        if board.platform.as_deref().is_some_and(|p| p != "x86_64")
            || board.target.as_deref().is_some_and(|t| t != p.target)
        {
            return Err("board platform/target conflicts with Intel profile".into());
        }
        if board.layout != Default::default() {
            return Err("monolithic layout overrides cannot select Intel stage budgets".into());
        }
        if !board.features.is_empty() {
            return Err("Intel metadata builds do not use board feature forwarding".into());
        }
        p.reservations.validate()?;
        let ifd = resolve_ifd(&p.ifd, &p.reservations)?;
        let payload = choice
            .map(|c| c.as_str().to_owned())
            .unwrap_or(p.default_payload);
        if !matches!(payload.as_str(), "halt" | "uefi") {
            return Err(
                "Intel profile supports halt and UEFI, not direct Linux/DTB loading".into(),
            );
        }
        if p.max_cpus == 0
            || p.smm.entry_points.is_some_and(|n| n < p.max_cpus)
            || p.smm.stack_size == 0
        {
            return Err("SMM entries/stacks must cover the MP CPU ceiling".into());
        }
        let stage = |role, mut features: Vec<String>| -> Result<ResolvedIntelStage, String> {
            features.push("stage".into());
            let selected_payload = if matches!(role, IntelStage::Ramstage) {
                payload.as_str()
            } else {
                "halt"
            };
            if selected_payload == "uefi" {
                features.push("fstart-stage/crabefi".into());
            }
            features.extend(board.variant_features.iter().cloned());
            features.sort();
            features.dedup();
            crate::resolved::validate_features(&features, &loaded.package, &loaded.metadata)?;
            Ok(ResolvedIntelStage {
                role,
                features,
                payload: selected_payload.into(),
            })
        };
        let stages = [
            stage(IntelStage::Bootblock, p.bootblock_features)?,
            stage(IntelStage::Postcar, p.postcar_features)?,
            stage(IntelStage::Ramstage, p.ramstage_features)?,
        ];
        for required in [
            "fstart-stage/acpi",
            "fstart-platform-intel/acpi",
            "fstart-platform-intel/mp",
            "fstart-platform-intel/smbios",
        ] {
            if !stages[2].features.iter().any(|f| f == required) {
                return Err(format!(
                    "normal Intel ramstage profile must select {required}"
                ));
            }
        }
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
            payload,
            reservations: p.reservations,
            stages,
            ifd,
            microcode,
            security: p.security,
            max_cpus: p.max_cpus,
            smm: p.smm,
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

fn resolve_ifd(
    regions: &[IfdRegion],
    layout: &IntelReservations,
) -> Result<IntelIfdFlashLayout, String> {
    if regions.is_empty() || regions.len() > 8 {
        return Err("IFD needs 1..8 regions".into());
    }
    let mut kinds = Vec::new();
    for (index, region) in regions.iter().enumerate() {
        let end = region
            .offset
            .checked_add(region.size)
            .ok_or("IFD region overflows")?;
        if region.size == 0
            || region.offset % 4096 != 0
            || region.size % 4096 != 0
            || u64::from(end) > layout.flash.size
            || kinds.contains(&region.kind.core())
        {
            return Err("invalid, duplicate or unaligned IFD region".into());
        }
        kinds.push(region.kind.core());
        for other in &regions[..index] {
            if region.offset < other.offset + other.size && other.offset < end {
                return Err("IFD regions overlap".into());
            }
        }
    }
    if !kinds.contains(&IntelIfdRegion::Descriptor) || !kinds.contains(&IntelIfdRegion::Bios) {
        return Err("IFD requires descriptor and BIOS regions".into());
    }
    let convert = |r: &IfdRegion| IntelIfdRegionConfig {
        kind: r.kind.core(),
        offset: r.offset,
        size: r.size,
    };
    let all = regions[1..]
        .iter()
        .fold(ConstVec::new(convert(&regions[0])), |v, r| {
            v.push(convert(r))
        });
    let ifd = IntelIfdFlashLayout::new(all);
    let bios = ifd.bios_region().ok_or("missing BIOS")?;
    if ifd.base() != layout.flash.base
        || u64::from(ifd.size()) != layout.flash.size
        || ifd.bios_base() != Some(layout.firmware.base)
        || u64::from(bios.size) != layout.firmware.size
    {
        return Err("IFD mapping differs from resolved flash/BIOS windows".into());
    }
    Ok(ifd)
}
