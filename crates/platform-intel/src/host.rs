//! Intel family policy, evaluated on the host from the selected board's Rust facts.
extern crate std;
use crate::facts::{BoardFacts, IntelBoardFacts, IntelPlatform, IntelPlatformConfig};
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
        let plan = resolve::<B>(selection).expect("invalid Intel board plan");
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
        ArtifactBinding, BuildPlan, CargoTarget, CompilationUnit, InputFile, UnitOutput,
    };
    plan.validate()?;
    let flags = |value: &str| {
        value
            .split_whitespace()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
    };
    let mp_capacity = || {
        std::collections::BTreeMap::from([("FSTART_MP_MAX_CPUS".into(), plan.max_cpus.to_string())])
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
            // Static-small emits direct relative calls; the retained-relocation
            // audit rejects any load-base-dependent cross-section reference.
            "-Cpanic=abort -Copt-level=s -Crelocation-model=static -Cno-redzone=yes -Clinker-plugin-lto=no -Cembed-bitcode=no -Zfunction-sections=yes",
        ),
        environment_values: mp_capacity(),
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
        // RIP-relative XIP references span ROM and CAR without the large
        // model's absolute calls. Link a fixed executable, not a dynamic PIE.
        // DRAM stages also fit the small model, using static references.
        // These legacy CPUs lack SHA/AVX2. Select SHA-2 0.11's compact scalar
        // backend, matching 0.10's force-soft-compact workspace feature.
        let relocation_model = if boot { "pic" } else { "static" };
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
            rustflags: flags(&format!("-Zub-checks=no -Crelocation-model={relocation_model} -Ccode-model=small -Clink-arg=-no-pie --cfg curve25519_dalek_backend=\"serial\" --cfg sha2_backend=\"soft\" --cfg sha2_backend_soft=\"compact\"")),
            linker_script: Some(fstart_image_build::linker::resolved_intel(&plan.reservations, row.role, true)?),
            environment_values: mp_capacity(),
            bindings,
            output: UnitOutput::Executable {
                expectations: plan.reservations.elf_expectations(row.role)?, load_address: reservation.image.base,
                flat_capacity: if boot { reservation.image.size } else { reservation.load_window()?.size },
            },
        });
    }
    // A kernel payload is the one assembler input a caller may supply instead
    // of a board file. The capacity is the whole flash window: the assembler
    // enforces what actually fits, this only rejects absurd files early.
    let inputs = plan
        .payload_config
        .as_ref()
        .filter(|payload| payload.kind == fstart_core::PayloadKind::LinuxBoot)
        .map(|payload| InputFile {
            name: "kernel".into(),
            default: payload.kernel_file.as_ref().map(ToString::to_string),
            capacity: plan.reservations.firmware.size,
        })
        .into_iter()
        .collect::<Vec<_>>();
    let resolved = BuildPlan {
        payload: plan.payload.clone(),
        units,
        stages: plan.stages.iter().map(|r| r.role.name().into()).collect(),
        assembly: plan.assembly()?,
        inputs,
    };
    resolved.order()?;
    Ok(resolved)
}

pub fn reservations<P: IntelPlatform>(
    facts: BoardFacts,
) -> Result<IntelReservations, std::string::String> {
    let (flash, firmware) =
        fstart_image_build::plan::FlashTransport::from(facts.flash).windows()?;
    if flash.size != u64::from(facts.flash_size) {
        return Err("layout differs from physical flash capacity".into());
    }
    let span = |base, size| Span { base, size };
    let car = span(P::CAR_BASE, P::CAR_SIZE);
    let cache_prefix = if facts.memory_cache { 0x20000 } else { 0 };
    if firmware.size <= cache_prefix + 4096 {
        return Err("mutable cache leaves no reset image capacity".into());
    }
    let reservations = IntelReservations {
        flash,
        firmware,
        bootstrap_ram: span(0x100000, 0x3ff00000),
        bootblock: StageReservation {
            // This is the legal XIP address window, not a reserved media slot.
            // The linker places the actual initialized image at its upper end.
            image: span(firmware.base + cache_prefix, firmware.size - cache_prefix),
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
        // Firmware store: S3 stage caches (about 1 MiB for a payload-enabled
        // release ramstage), the training record and ACPI/SMBIOS tables. Only
        // the used part stays reserved.
        store: span(0x5000000, 0x400000),
    };
    reservations.validate()?;
    Ok(reservations)
}

pub fn resolve<B: IntelBoardFacts>(
    selection: BuildSelection,
) -> Result<IntelPlan, std::string::String> {
    resolve_for::<B::Platform>(B::FACTS, B::CONFIG.max_cpus(), selection)
}

fn resolve_for<P: IntelPlatform>(
    facts: BoardFacts,
    max_cpus: u16,
    selection: BuildSelection,
) -> Result<IntelPlan, std::string::String> {
    let has_x86_linux_overrides = selection.has_x86_linux_overrides();
    let requested_payload = selection.payload.clone().unwrap_or_else(|| "halt".into());
    if has_x86_linux_overrides && requested_payload != "linux" {
        return Err("x86 Linux options require '--payload linux'".into());
    }
    let x86_linux_bootargs = selection.x86_linux_bootargs.unwrap_or_default();
    if !matches!(
        requested_payload.as_str(),
        "halt" | "linux" | "uefi" | "uefi-ui" | "uefi-basic"
    ) {
        return Err("Intel supports halt, direct Linux, and UEFI payloads".into());
    }
    let payload = if requested_payload.starts_with("uefi") {
        "uefi".to_string()
    } else {
        requested_payload.clone()
    };
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
        if facts.memory_cache && role != IntelStage::Postcar {
            features.push("memory-cache".into());
        }
        if selected_payload == "uefi" {
            features.push(
                match requested_payload.as_str() {
                    "uefi" => "payload-uefi",
                    "uefi-ui" => "payload-uefi-ui",
                    "uefi-basic" => "payload-uefi-basic",
                    _ => unreachable!(),
                }
                .into(),
            );
        } else if selected_payload == "linux" {
            features.push("payload-linux".into());
        }
        IntelStagePlan {
            role,
            features,
            payload: selected_payload.into(),
        }
    });
    if max_cpus == 0 {
        return Err("CPU population must be nonzero".into());
    }
    let reservations = reservations::<P>(facts)?;
    let payload_config = match payload.as_str() {
        "uefi" => Some(fstart_core::x86_uefi_payload()),
        // Empty bootargs default to no command line: there is deliberately no
        // implicit serial-console policy, so direct Linux stays silent unless
        // the caller passes `--linux-bootargs` explicitly.
        "linux" => Some(
            fstart_core::x86_linux_payload(
                selection
                    .x86_linux_kernel_load_addr
                    .unwrap_or(fstart_core::payload_manifest::X86_LINUX_DEFAULT_KERNEL_LOAD_ADDR),
                fstart_core::payload_manifest::X86_LINUX_DEFAULT_ZERO_PAGE_ADDR,
                &x86_linux_bootargs,
                selection.x86_linux_print_mtrrs,
            )
            .map_err(ToString::to_string)?,
        ),
        _ => None,
    };
    let plan = IntelPlan {
        memory_cache: facts.memory_cache,
        reservations,
        target: "x86_64-unknown-none".into(),
        payload,
        stages,
        smm_features: vec!["bundle-smm".into()],
        flash: facts.flash.into(),
        max_cpus,
        microcode: P::MICROCODE_SIGNATURES
            .iter()
            .map(|name| format!("intel-microcode/intel-ucode/{name}"))
            .collect::<Vec<_>>(),
        data_assets: facts
            .data_assets
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
        payload_config,
        security: fstart_core::dev_security_config("keys/dev-signing.pub"),
        smm: fstart_core::SmmConfig {
            entry_points: Some(max_cpus),
            stack_size: 0x400,
            ..Default::default()
        },
    };
    plan.validate()?;
    Ok(plan)
}

#[cfg(all(
    test,
    feature = "gm965-ich8",
    feature = "i945-ich7",
    feature = "pineview-ich7"
))]
mod tests {
    use super::*;
    use crate::legacy_cpu::{Fcbga559, LegacyCpu, Socket441, SocketM};
    use fstart_core::{
        ConstVec, IntelIfdFlashLayout, IntelIfdRegion as Kind, IntelIfdRegionConfig as Region,
    };

    type Gm965 = crate::gm965::Gm965Ich8<SocketM<0xfef0_0000, 0x80000>>;
    type I945 = crate::i945::I945Ich7<Socket441>;
    type Pineview = crate::pineview::PineviewIch7<Fcbga559>;

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
    const FACTS: BoardFacts = BoardFacts::new(fstart_core::FlashLayout::IntelIfd(FLASH), 0x400000);

    fn resolve(
        facts: BoardFacts,
        selection: BuildSelection,
    ) -> Result<IntelPlan, std::string::String> {
        resolve_for::<Gm965>(facts, 2, selection)
    }

    #[test]
    fn board_binding_drives_host_identity_and_population() {
        struct Board;
        static CONFIG: crate::i945::I945Ich7Platform =
            crate::i945::I945Ich7Config::new().max_cpus(4).build();
        impl IntelBoardFacts for Board {
            type Platform = I945;
            const CONFIG: &'static crate::i945::I945Ich7Platform = &CONFIG;
            const FACTS: BoardFacts = FACTS;
        }
        let plan = super::resolve::<Board>(BuildSelection::default()).unwrap();
        assert_eq!(plan.max_cpus, 4);
        assert_eq!(plan.smm.entry_points, Some(4));
        assert_eq!(
            plan.reservations.bootblock.writable.base,
            <Socket441 as LegacyCpu>::CAR_BASE
        );
        assert_eq!(plan.microcode.len(), 2);
        assert!(resolve_for::<I945>(FACTS, 0, BuildSelection::default()).is_err());
    }
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
                ..Default::default()
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
        let units = compilation_plan(uefi).unwrap().units;
        assert!(units.iter().all(|unit| {
            unit.environment_values
                .get("FSTART_MP_MAX_CPUS")
                .map(|value| value.as_str())
                == Some("2")
        }));
        let linux = resolve(
            FACTS,
            BuildSelection {
                payload: Some("linux".into()),
                x86_linux_kernel_load_addr: Some(0x0200_0000),
                x86_linux_bootargs: Some("console=ttyS0".into()),
                x86_linux_print_mtrrs: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(
            linux.stages[2].features,
            ["bundle-ramstage", "payload-linux"]
        );
        let payload = linux.payload_config.as_ref().unwrap();
        assert_eq!(payload.kernel_load_addr, Some(0x0200_0000));
        assert_eq!(payload.x86_zero_page_addr, Some(0x0009_0000));
        assert_eq!(payload.bootargs.as_deref(), Some("console=ttyS0"));
        assert!(payload.print_x86_mtrrs);
    }

    #[test]
    fn stage_code_models_match_xip_and_dram_placement() {
        let plan = compilation_plan(resolve(FACTS, BuildSelection::default()).unwrap()).unwrap();
        for name in ["bootblock", "postcar", "ramstage"] {
            let unit = plan.unit(name).unwrap();
            assert!(unit.rustflags.contains(&"-Ccode-model=small".into()));
            assert!(unit.rustflags.contains(&"-Clink-arg=-no-pie".into()));
            let relocation = if name == "bootblock" { "pic" } else { "static" };
            assert!(
                unit.rustflags
                    .contains(&format!("-Crelocation-model={relocation}"))
            );
            assert!(
                unit.linker_script
                    .as_ref()
                    .unwrap()
                    .contains("SIZEOF(.got)")
                    || name != "bootblock"
            );
        }
    }

    #[test]
    fn x86_linux_options_require_the_linux_payload() {
        let selection = BuildSelection {
            x86_linux_bootargs: Some("console=ttyS0".into()),
            ..Default::default()
        };
        assert_eq!(
            resolve(FACTS, selection).unwrap_err(),
            "x86 Linux options require '--payload linux'"
        );
    }

    #[test]
    fn uefi_cli_profile_selects_capabilities_without_changing_hardware_geometry() {
        let select = |payload: &str| {
            resolve(
                FACTS,
                BuildSelection {
                    payload: Some(payload.into()),
                    ..Default::default()
                },
            )
            .unwrap()
        };
        let full = select("uefi");
        let ui = select("uefi-ui");
        let basic = select("uefi-basic");
        assert_eq!(full.stages[2].features, ["bundle-ramstage", "payload-uefi"]);
        assert_eq!(
            ui.stages[2].features,
            ["bundle-ramstage", "payload-uefi-ui"]
        );
        assert_eq!(
            basic.stages[2].features,
            ["bundle-ramstage", "payload-uefi-basic"]
        );
        assert_eq!(full.reservations.flash, ui.reservations.flash);
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
            BoardFacts::new(fstart_core::FlashLayout::IntelIfd(LARGER_BIOS), 0x400000),
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
    fn legacy_cpu_packages_are_independent_of_the_chipset() {
        type MobileI945 = crate::i945::I945Ich7<SocketM>;
        type MobileGm965 = crate::gm965::Gm965Ich8<SocketM>;
        let atom = resolve_for::<I945>(FACTS, 2, BuildSelection::default()).unwrap();
        let mobile = resolve_for::<MobileI945>(FACTS, 2, BuildSelection::default()).unwrap();
        let gm = resolve_for::<MobileGm965>(FACTS, 2, BuildSelection::default()).unwrap();
        assert!(atom.microcode.iter().all(|file| file.contains("06-1c-")));
        assert!(mobile.microcode.iter().any(|file| file.contains("06-0e-")));
        assert!(mobile.microcode.iter().any(|file| file.contains("06-0f-")));
        assert!(!mobile.microcode.iter().any(|file| file.contains("06-1c-")));
        assert_eq!(mobile.microcode, gm.microcode);
        assert_eq!(
            mobile.reservations.bootblock.writable,
            gm.reservations.bootblock.writable
        );
        assert_eq!(mobile.reservations.bootblock.writable.size, 0x8000);

        // X61's larger existing window is explicit CPU/CAR policy, not GM965 identity.
        let x61 = resolve(FACTS, BuildSelection::default()).unwrap();
        assert_eq!(x61.microcode, gm.microcode);
        assert_eq!(x61.reservations.bootblock.writable.base, 0xfef0_0000);
        assert_eq!(x61.reservations.bootblock.writable.size, 0x80000);
        assert_eq!(
            x61.reservations.ramstage.image,
            gm.reservations.ramstage.image
        );
        assert_eq!(
            x61.reservations.ramstage.writable,
            gm.reservations.ramstage.writable
        );
    }

    #[cfg(all(feature = "stage", feature = "mp"))]
    #[test]
    fn i945_runtime_cpu_driver_follows_the_package() {
        use crate::IntelEarlyPlatform;
        use fstart_arch::x86::cpu::intel::{
            core2_cpu::Core2CpuDriver, pineview::PineviewCpuDriver,
        };
        // Type-check both runtime factories without executing MSR/I/O operations.
        let _: fn(Option<&'static [u8]>) -> Core2CpuDriver =
            <crate::i945::I945Ich7<SocketM> as IntelEarlyPlatform>::cpu_driver;
        let _: fn(Option<&'static [u8]>) -> PineviewCpuDriver =
            <I945 as IntelEarlyPlatform>::cpu_driver;
    }

    #[test]
    fn i945_payload_ecam_matches_the_config_used_by_pciexbar_setup() {
        use fstart_driver_intel::IntelEcamConfig;
        let mut config = crate::i945::I945Ich7Config::new()
            .variant(crate::i945::I945Variant::DesktopGc)
            .build()
            .northbridge;
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
        let gm = reservations::<Gm965>(FACTS).unwrap();
        let facts = BoardFacts::new(
            fstart_core::FlashLayout::X86Legacy(fstart_core::X86LegacyFlashLayout {
                size: 0x1000000,
            }),
            0x1000000,
        );
        let plan = resolve_for::<Pineview>(facts, 4, BuildSelection::default()).unwrap();
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
            <Fcbga559 as LegacyCpu>::CAR_BASE
        );
        assert_eq!(
            pineview.bootblock.writable.size,
            <Fcbga559 as LegacyCpu>::CAR_SIZE
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
        let gm = reservations::<Gm965>(FACTS).unwrap();
        let facts = BoardFacts::new(
            fstart_core::FlashLayout::X86Legacy(fstart_core::X86LegacyFlashLayout {
                size: 0x80000,
            }),
            0x80000,
        );
        let plan = resolve_for::<I945>(facts, 2, BuildSelection::default()).unwrap();
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
        assert_eq!(
            i945.bootblock.writable.base,
            <Socket441 as LegacyCpu>::CAR_BASE
        );
        assert_eq!(
            i945.bootblock.writable.size,
            <Socket441 as LegacyCpu>::CAR_SIZE
        );
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
