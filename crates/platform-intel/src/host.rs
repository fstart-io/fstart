//! Intel family policy, evaluated on the host from the selected board's Rust facts.
extern crate std;
use crate::facts::{BoardFacts, Chipset, IntelBoardFacts};
use fstart_image_build::{
    intel_plan::{IntelReservations, IntelStage, StageReservation},
    plan::{BuildSelection, IntelPlan, IntelStagePlan, Span},
};
use std::{format, string::ToString, vec, vec::Vec};

fn compiler_cfg_schema() -> fstart_image_build::build_plan::CompilerCfgSchema {
    let strings = |values: &[&str]| values.iter().map(|v| (*v).into()).collect();
    fstart_image_build::build_plan::CompilerCfgSchema {
        entries: strings(&["riscv64", "armv7", "aarch64-relocate", "x86_64"]),
        environments: strings(&["monolithic", "car", "postcar", "ram", "smm"]),
        payloads: strings(&["halt", "linux", "crabefi"]),
    }
}

/// Conventional platform export consumed by the generic fbuild runner.
pub struct Plan<B>(core::marker::PhantomData<B>);
impl<B: IntelBoardFacts> Plan<B> {
    pub fn emit(selection_json: &str) {
        let selection: BuildSelection =
            serde_json::from_str(selection_json).expect("invalid build selection");
        let plan = resolve(B::FACTS, selection).expect("invalid Intel board plan");
        std::println!(
            "{}",
            serde_json::to_string(&compilation_plan(plan).expect("concrete Intel plan"))
                .expect("serialize Intel plan")
        );
    }
}

pub fn compilation_plan(
    plan: IntelPlan,
) -> Result<fstart_image_build::build_plan::BuildPlan, std::string::String> {
    use fstart_image_build::build_plan::{
        ArtifactBinding, BuildPlan, CargoTarget, CompilationUnit, UnitOutput,
    };
    use std::collections::BTreeMap;
    plan.validate()?;
    let flags = |value: &str| {
        value
            .split_whitespace()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
    };
    let mut units = vec![CompilationUnit {
        name: "smm".into(),
        cargo_target: CargoTarget::BoardLibrary,
        target: plan.target.clone(),
        entry: "x86_64".into(),
        cfg_schema: compiler_cfg_schema(),
        environment: "smm".into(),
        payload: "halt".into(),
        features: plan.smm_features.clone(),
        build_std: None,
        release_only: true,
        linker_script: None,
        rustflags: flags(
            "-Cpanic=abort -Copt-level=s -Crelocation-model=pic -Cno-redzone=yes -Clinker-plugin-lto=no -Cembed-bitcode=no -Zfunction-sections=yes",
        ),
        environment_values: BTreeMap::new(),
        bindings: vec![],
        output: UnitOutput::SmmImage {
            entry_count: plan.smm.entry_points.unwrap_or(plan.max_cpus),
            stack_size: plan.smm.stack_size,
            coreboot_module_args: plan.smm.coreboot.module_args,
            coreboot_header: plan.smm.coreboot.emit_header,
        },
    }];
    for row in &plan.stages {
        let ram = row.role == IntelStage::Ramstage;
        let boot = row.role == IntelStage::Bootblock;
        let reservation = plan.reservations.stage(row.role);
        let mut bindings = vec![];
        if ram {
            bindings.push(ArtifactBinding {
                producer: "smm".into(),
                artifact: "image".into(),
                environment: "FSTART_SMM_IMAGE".into(),
            });
            if plan.smm.coreboot.emit_header {
                bindings.push(ArtifactBinding {
                    producer: "smm".into(),
                    artifact: "header".into(),
                    environment: "FSTART_SMM_COREBOOT_HEADER".into(),
                });
            }
        }
        units.push(CompilationUnit {
            name: row.role.name().into(), cargo_target: CargoTarget::BoardBinary, target: plan.target.clone(), entry: "x86_64".into(),
            cfg_schema: compiler_cfg_schema(),
            environment: row.role.environment().into(), payload: if row.payload == "uefi" { "crabefi".into() } else { row.payload.clone() },
            features: row.features.clone(), build_std: Some("core,alloc".into()), release_only: false,
            rustflags: flags("-Zub-checks=no -Crelocation-model=static -Ccode-model=large --cfg curve25519_dalek_backend=\"serial\""),
            linker_script: Some(fstart_image_build::linker::resolved_intel(&plan.reservations, row.role, true)?),
            environment_values: if ram { BTreeMap::from([("FSTART_INTEL_MAX_CPUS".into(), plan.max_cpus.to_string())]) } else { BTreeMap::new() },
            bindings,
            output: UnitOutput::Executable {
                expectations: plan.reservations.elf_expectations(row.role)?, load_address: reservation.image.base,
                flat_capacity: if boot { reservation.image.size } else { reservation.load_window()?.size },
            },
        });
    }
    let resolved = BuildPlan {
        payload: plan.payload.clone(),
        units,
        stages: plan.stages.iter().map(|r| r.role.name().into()).collect(),
        assembly: plan.assembly()?,
        inputs: vec![],
    };
    resolved.order()?;
    Ok(resolved)
}

pub fn reservations(facts: BoardFacts) -> Result<IntelReservations, std::string::String> {
    let (flash, firmware) =
        fstart_image_build::plan::FlashTransport::from(facts.flash).windows()?;
    if flash.size != u64::from(facts.flash_size) {
        return Err("layout differs from physical flash capacity".into());
    }
    let span = |base, size| Span { base, size };
    let car = match facts.chipset {
        Chipset::Gm965Ich8 => span(0xfef00000, 0x80000),
        Chipset::I945Ich7 => span(crate::i945::I945_CAR_BASE, crate::i945::I945_CAR_SIZE),
        Chipset::PineviewIch7 => span(
            crate::pineview::PINEVIEW_CAR_BASE,
            crate::pineview::PINEVIEW_CAR_SIZE,
        ),
    };
    let reservations = IntelReservations {
        flash,
        firmware,
        bootstrap_ram: span(0x100000, 0x3ff00000),
        bootblock: StageReservation {
            // This is the legal XIP address window, not a reserved media slot.
            // The linker places the actual initialized image at its upper end.
            image: firmware,
            writable: car,
            stack: 0x2000,
            heap: 0,
        },
        postcar: StageReservation {
            image: span(0x1000000, 0x10000),
            writable: span(0x1010000, 0x10000),
            stack: 0x2000,
            heap: 0,
        },
        ramstage: StageReservation {
            image: span(0x4000000, 0x400000),
            writable: span(0x4400000, 0xc00000),
            stack: 0x400000,
            heap: 0x200000,
        },
        low_memory: span(0, 0x100000),
        scratch: span(0x2000000, 0x1000000),
    };
    reservations.validate()?;
    Ok(reservations)
}

pub fn resolve(
    facts: BoardFacts,
    selection: BuildSelection,
) -> Result<IntelPlan, std::string::String> {
    let payload = selection.payload.unwrap_or_else(|| "halt".into());
    if !matches!(payload.as_str(), "halt" | "uefi") {
        return Err("Intel supports halt and UEFI, not direct Linux/DTB loading".into());
    }
    let stages = [
        (IntelStage::Bootblock, "bundle-bootblock"),
        (IntelStage::Postcar, "bundle-postcar"),
        (IntelStage::Ramstage, "bundle-ramstage"),
    ]
    .map(|(role, bundle)| {
        let selected_payload = if role == IntelStage::Ramstage {
            payload.as_str()
        } else {
            "halt"
        };
        let mut features = vec![bundle.into()];
        if selected_payload == "uefi" {
            features.push(
                match facts.uefi_build_profile {
                    fstart_core::board::UefiBuildProfile::Full => "payload-uefi",
                    fstart_core::board::UefiBuildProfile::Basic => "payload-uefi-basic",
                }
                .into(),
            );
        }
        IntelStagePlan {
            role,
            features,
            payload: selected_payload.into(),
        }
    });
    let reservations = reservations(facts)?;
    let signatures: &[&str] = match facts.chipset {
        Chipset::Gm965Ich8 => &[
            "06-0f-02", "06-0f-06", "06-0f-07", "06-0f-0a", "06-0f-0b", "06-0f-0d", "06-16-01",
        ],
        Chipset::I945Ich7 => &["06-1c-02", "06-1c-0a"],
        Chipset::PineviewIch7 => &["06-1c-02", "06-1c-0a"],
    };
    let plan = IntelPlan {
        reservations,
        target: "x86_64-unknown-none".into(),
        payload,
        stages,
        smm_features: vec!["bundle-smm".into()],
        flash: facts.flash.into(),
        max_cpus: facts.max_cpus,
        microcode: signatures
            .iter()
            .map(|name| format!("intel-microcode/intel-ucode/{name}"))
            .collect::<Vec<_>>(),
        security: fstart_core::dev_security_config("keys/dev-signing.pub"),
        smm: fstart_core::SmmConfig {
            entry_points: Some(facts.max_cpus),
            stack_size: 0x400,
            ..Default::default()
        },
    };
    plan.validate()?;
    Ok(plan)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fstart_core::{
        ConstVec, IntelIfdFlashLayout, IntelIfdRegion as Kind, IntelIfdRegionConfig as Region,
    };
    const DESCRIPTOR: Region = Region {
        kind: Kind::Descriptor,
        offset: 0,
        size: 0x1000,
    };
    const FLASH: IntelIfdFlashLayout =
        IntelIfdFlashLayout::new(ConstVec::new(DESCRIPTOR).push(DESCRIPTOR).push(Region {
            kind: Kind::Bios,
            offset: 0x280000,
            size: 0x180000,
        }));
    const FACTS: BoardFacts = BoardFacts::new(
        fstart_core::FlashLayout::IntelIfd(FLASH),
        0x400000,
        2,
        Chipset::Gm965Ich8,
    );
    #[test]
    fn platform_selects_target_payload_stage_bundles_and_smm() {
        let default = resolve(FACTS, BuildSelection::default()).unwrap();
        assert_eq!(default.target, "x86_64-unknown-none");
        assert_eq!(default.payload, "halt");
        assert!(default.stages.iter().all(|stage| stage.payload == "halt"));
        let uefi = resolve(
            FACTS,
            BuildSelection {
                payload: Some("uefi".into()),
            },
        )
        .unwrap();
        assert_eq!(
            uefi.stages
                .iter()
                .map(|stage| stage.payload.as_str())
                .collect::<Vec<_>>(),
            ["halt", "halt", "uefi"]
        );
        assert_eq!(uefi.stages[0].features, ["bundle-bootblock"]);
        assert_eq!(uefi.stages[1].features, ["bundle-postcar"]);
        assert_eq!(uefi.stages[2].features, ["bundle-ramstage", "payload-uefi"]);
        assert_eq!(uefi.smm_features, ["bundle-smm"]);
        assert!(
            resolve(
                FACTS,
                BuildSelection {
                    payload: Some("linux".into())
                }
            )
            .is_err()
        );
    }

    #[test]
    fn uefi_build_policy_selects_capabilities_without_changing_hardware_geometry() {
        use fstart_core::board::UefiBuildProfile;
        let select = |facts| {
            resolve(
                facts,
                BuildSelection {
                    payload: Some("uefi".into()),
                },
            )
            .unwrap()
        };
        let full = select(FACTS);
        let basic = select(FACTS.with_uefi_build_profile(UefiBuildProfile::Basic));
        assert_eq!(full.stages[2].features, ["bundle-ramstage", "payload-uefi"]);
        assert_eq!(
            basic.stages[2].features,
            ["bundle-ramstage", "payload-uefi-basic"]
        );
        assert_eq!(full.reservations.flash, basic.reservations.flash);
        assert_eq!(
            full.reservations.ramstage.image,
            basic.reservations.ramstage.image
        );
        assert_eq!(
            full.reservations.ramstage.writable,
            basic.reservations.ramstage.writable
        );
    }

    #[test]
    fn bios_identity_and_capacity_follow_typed_ifd() {
        let plan = resolve(FACTS, BuildSelection::default()).unwrap();
        assert_eq!(plan.reservations.firmware.base, 0xffe80000);
        assert_eq!(plan.reservations.firmware.size, 0x180000);
        assert_eq!(
            plan.reservations.bootblock.image,
            plan.reservations.firmware
        );
        const LARGER_BIOS: IntelIfdFlashLayout =
            IntelIfdFlashLayout::new(ConstVec::new(DESCRIPTOR).push(DESCRIPTOR).push(Region {
                kind: Kind::Bios,
                offset: 0x200000,
                size: 0x200000,
            }));
        let changed = resolve(
            BoardFacts::new(
                fstart_core::FlashLayout::IntelIfd(LARGER_BIOS),
                0x400000,
                2,
                Chipset::Gm965Ich8,
            ),
            BuildSelection::default(),
        )
        .unwrap();
        assert_eq!(changed.reservations.firmware.base, 0xffe00000);
        assert_eq!(
            changed.reservations.bootblock.image,
            changed.reservations.firmware
        );
    }
    #[test]
    fn i945_payload_ecam_matches_the_config_used_by_pciexbar_setup() {
        use fstart_driver_intel::IntelEcamConfig;
        let mut config = crate::i945::I945Ich7Config::new()
            .variant(crate::i945::I945Variant::DesktopGc)
            .build()
            .northbridge_config();
        assert_eq!(config.ecam_base(), crate::i945::I945_ECAM_BASE);
        assert_eq!(config.ecam_buses, 64);
        config.ecam_base = 0xe0000000;
        assert_eq!(config.ecam_base(), 0xe0000000);
    }

    #[test]
    fn legacy_transport_rejects_invalid_capacity_and_reservation_mismatch() {
        use fstart_image_build::plan::FlashTransport;
        for size in [0, 0x80001] {
            assert!(
                FlashTransport::X86Legacy(fstart_core::X86LegacyFlashLayout { size })
                    .decode()
                    .is_err()
            );
        }
        let mut plan = resolve(FACTS, BuildSelection::default()).unwrap();
        plan.flash = FlashTransport::X86Legacy(fstart_core::X86LegacyFlashLayout { size: 0x80000 });
        assert!(plan.validate().is_err());
        assert!(compilation_plan(plan).is_err());
    }

    #[test]
    fn pineview_pairing_reuses_family_reservations_with_real_car_delta() {
        let gm = reservations(FACTS).unwrap();
        let facts = BoardFacts::new(
            fstart_core::FlashLayout::X86Legacy(fstart_core::X86LegacyFlashLayout {
                size: 0x1000000,
            }),
            0x1000000,
            4,
            Chipset::PineviewIch7,
        );
        let plan = resolve(facts, BuildSelection::default()).unwrap();
        let pineview = &plan.reservations;
        assert_eq!(
            pineview.flash,
            Span {
                base: 0xff000000,
                size: 0x1000000
            }
        );
        assert_eq!(pineview.firmware, pineview.flash);
        assert_eq!(
            plan.microcode,
            [
                "intel-microcode/intel-ucode/06-1c-02",
                "intel-microcode/intel-ucode/06-1c-0a"
            ]
        );
        assert_eq!(
            pineview.bootblock.writable.base,
            crate::pineview::PINEVIEW_CAR_BASE
        );
        assert_eq!(
            pineview.bootblock.writable.size,
            crate::pineview::PINEVIEW_CAR_SIZE
        );
        for role in [
            fstart_image_build::intel_plan::IntelStage::Postcar,
            fstart_image_build::intel_plan::IntelStage::Ramstage,
        ] {
            let (gm, pineview) = (gm.stage(role), pineview.stage(role));
            assert_eq!(
                (gm.image, gm.writable, gm.stack, gm.heap),
                (
                    pineview.image,
                    pineview.writable,
                    pineview.stack,
                    pineview.heap
                )
            );
        }
        assert_eq!(pineview.bootblock.image, pineview.firmware);
    }

    #[test]
    fn i945_pairing_reuses_family_reservations_with_real_car_delta() {
        let gm = reservations(FACTS).unwrap();
        let facts = BoardFacts::new(
            fstart_core::FlashLayout::X86Legacy(fstart_core::X86LegacyFlashLayout {
                size: 0x80000,
            }),
            0x80000,
            2,
            Chipset::I945Ich7,
        );
        let plan = resolve(facts, BuildSelection::default()).unwrap();
        let i945 = &plan.reservations;
        assert_eq!(
            i945.flash,
            Span {
                base: 0xfff80000,
                size: 0x80000
            }
        );
        assert_eq!(i945.firmware, i945.flash);
        assert_eq!(
            plan.microcode,
            [
                "intel-microcode/intel-ucode/06-1c-02",
                "intel-microcode/intel-ucode/06-1c-0a"
            ]
        );
        assert!(matches!(
            plan.assembly()
                .unwrap()
                .config("test")
                .unwrap()
                .memory
                .flash_layout,
            Some(fstart_core::FlashLayout::X86Legacy(
                fstart_core::X86LegacyFlashLayout { size: 0x80000 }
            ))
        ));
        assert_eq!(i945.bootblock.writable.base, crate::i945::I945_CAR_BASE);
        assert_eq!(i945.bootblock.writable.size, crate::i945::I945_CAR_SIZE);
        for role in [
            fstart_image_build::intel_plan::IntelStage::Postcar,
            fstart_image_build::intel_plan::IntelStage::Ramstage,
        ] {
            let (gm, i945) = (gm.stage(role), i945.stage(role));
            assert_eq!(
                (gm.image, gm.writable, gm.stack, gm.heap),
                (i945.image, i945.writable, i945.stack, i945.heap)
            );
        }
        assert_eq!(i945.bootblock.image, i945.firmware);
    }
}
