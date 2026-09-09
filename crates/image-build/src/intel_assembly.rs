//! Intel format projection, never a source of compiler policy.
use crate::{intel_plan::IntelStage, plan::IntelPlan};
use fstart_core::board::{IntelMicrocodeConfig, MicrocodeConfig};
use fstart_core::*;

impl IntelPlan {
    /// Compatibility projection for image assembly only; never a compiler plan.
    pub fn assembly(&self) -> Result<crate::build_plan::Assembly, String> {
        let mut stages = hvec([]);
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
        let mut microcode = hvec([]);
        for path in &self.microcode {
            microcode
                .push(
                    path.as_str()
                        .try_into()
                        .map_err(|_| "microcode path too long")?,
                )
                .map_err(|_| "too many microcode files")?;
        }
        Ok(crate::build_plan::Assembly {
            platform: Platform::X86_64,
            memory: vec![MemoryRegion {
                name: hstr("bootstrap-ram"),
                base: self.reservations.bootstrap_ram.base,
                size: self.reservations.bootstrap_ram.size,
                kind: RegionKind::Ram,
            }],
            flash: Some(self.flash.clone()),
            bootstrap: vec![
                ("postcar".into(), crate::build_plan::BootstrapRole::Postcar),
                (
                    "ramstage".into(),
                    crate::build_plan::BootstrapRole::Mainstage,
                ),
            ],
            stages: StageLayout::MultiStage(stages),
            security: self.security.clone(),
            payload: (self.payload == "uefi").then(x86_uefi_payload),
            microcode: Some(MicrocodeConfig::Intel(IntelMicrocodeConfig {
                files: microcode,
                early: true,
                mp: true,
            })),
            full_flash_image: true,
            soc_image_format: SocImageFormat::None,
            boot_hart_id: 0,
            build: BoardBuildPolicy {
                firmware_image: FirmwareImagePolicy::memory_mapped(
                    self.reservations.firmware.base,
                    self.reservations.firmware.size,
                ),
                ..Default::default()
            },
        })
    }
}
