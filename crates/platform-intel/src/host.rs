//! Intel family policy, evaluated on the host from the selected board's Rust facts.
extern crate std;
use crate::facts::{BoardFacts, IntelBoardFacts, IntelPlatform, IntelPlatformConfig};
use fstart_image_build::{
    intel_plan::{IntelReservations, IntelStage, StageReservation},
    plan::{BuildSelection, HostPlan, IntelPlan, IntelStagePlan, ResolvedPlan, Span},
};

use std::{format, string::ToString, vec, vec::Vec};

/// Conventional platform export consumed by the generic fbuild runner.
pub struct Plan<B>(core::marker::PhantomData<B>);
impl<B: IntelBoardFacts> HostPlan<B> for Plan<B> {
    fn resolve(selection: BuildSelection) -> Result<ResolvedPlan, std::string::String> {
        compilation_plan(resolve::<B>(selection)?)
    }
}

pub fn compilation_plan(
    plan: IntelPlan,
) -> Result<fstart_image_build::build_plan::BuildPlan, std::string::String> {
    use fstart_image_build::build_plan::{
        ArtifactBinding, BuildPlan, CargoTarget, CompilationUnit, InputFile, RunRecipe, UnitOutput,
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
    // Long-mode SMM images are copied without absolute-address fixups;
    // their data references must be RIP-relative. The 32-bit loader rebases
    // its audited absolute references instead. PLT-style call relocations
    // resolve locally at link time, avoiding GOT-indirect calls in either mode.
    let smm_relocation = if plan.platform == fstart_core::Platform::X86 {
        "static"
    } else {
        "pie"
    };
    let mut units = vec![CompilationUnit {
        name: "smm".into(),
        cargo_target: CargoTarget::BoardLibrary,
        target: plan.target(),
        features: plan.smm_features.clone(),
        // rustc ships no library for the 32-bit JSON target.
        build_std: (plan.platform == fstart_core::Platform::X86).then(|| "core,alloc".into()),
        release_only: true,
        linker_script: None,
        rustflags: flags(&format!(
            "-Cpanic=abort -Copt-level=s -Crelocation-model={smm_relocation} -Zplt=yes -Cno-redzone=yes -Clinker-plugin-lto=no -Cembed-bitcode=no -Zfunction-sections=yes",
        )),
        environment_values: mp_capacity(),
        bindings: vec![],
        file_bindings: vec![],
        // The board's SMM handler module, outside every stage `main`.
        board_features: vec!["smm".into()],
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
            name: row.role.name().into(), cargo_target: CargoTarget::BoardBinary, target: plan.target(),
            features: row.features.clone(), build_std: Some("core,alloc".into()), release_only: false,
            rustflags: flags(&format!("-Zub-checks=no -Crelocation-model={relocation_model} -Ccode-model=small -Clink-arg=-no-pie --cfg curve25519_dalek_backend=\"serial\" --cfg sha2_backend=\"soft\" --cfg sha2_backend_soft=\"compact\"")),
            linker_script: Some(fstart_image_build::linker::resolved_intel(&plan.reservations, row.role, true, plan.platform)?),
            environment_values: mp_capacity(),
            bindings,
            file_bindings: vec![],
            board_features: if ram { vec!["acpi".into()] } else { vec![] },
            output: UnitOutput::Executable {
                expectations: plan.reservations.elf_expectations(row.role, plan.platform)?, load_address: reservation.image.base,
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
        .filter(|payload| {
            matches!(
                payload.kind,
                fstart_core::PayloadKind::LinuxBoot | fstart_core::PayloadKind::Coreboot
            )
        })
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
        platform: plan.platform,
        images: vec![plan.assembly()?.into()],
        run: Some(RunRecipe::q35(
            "bootblock",
            plan.reservations.flash.size,
            plan.reservations.firmware.base - plan.reservations.flash.base,
            plan.platform == fstart_core::Platform::X86,
        )),
        inputs,
    };
    resolved.order()?;
    Ok(resolved)
}

/// The `fbuild --debug` facilities an Intel image compiles in.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Debug {
    /// The chipset's EHCI debug port console ([`crate::debug_console`]).
    ehci: bool,
    /// The SPI flash log next to the board's console
    /// ([`crate::debug_console`]).
    flash_console: bool,
}

impl Debug {
    fn select(
        ehci_debug: bool,
        options: &[std::string::String],
    ) -> Result<Self, std::string::String> {
        let mut debug = Self::default();
        for option in options {
            match option.as_str() {
                "ehci" if ehci_debug => debug.ehci = true,
                "ehci" => return Err("the board reserves no EHCI debug port".into()),
                "flash-console" => debug.flash_console = true,
                other => return Err(format!("Intel platforms have no debug option '{other}'")),
            }
        }
        Ok(debug)
    }

    /// Platform features every stage needs for the selection.
    fn features(self) -> impl Iterator<Item = std::string::String> {
        [
            (self.ehci, "ehci-console"),
            (self.flash_console, "flash-console"),
        ]
        .into_iter()
        .filter_map(|(selected, feature)| selected.then_some(feature.into()))
    }
}

pub fn reservations<P: IntelPlatform>(
    facts: BoardFacts,
    flash_console: bool,
) -> Result<IntelReservations, std::string::String> {
    let (flash, firmware) =
        fstart_image_build::plan::FlashTransport::from(facts.flash).windows()?;
    if flash.size != u64::from(facts.flash_size) {
        return Err("layout differs from physical flash capacity".into());
    }
    let span = |base, size| Span { base, size };
    let car = span(P::CAR_BASE, P::CAR_SIZE);
    let cache_prefix = u64::from(facts.mutable_prefix(flash_console));
    if firmware.size <= cache_prefix + 4096 {
        return Err("mutable flash regions leave no reset image capacity".into());
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
            stack: P::BOOTBLOCK_STACK,
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
    resolve_for::<B::Platform>(
        B::FACTS,
        B::CONFIG.max_cpus(),
        B::CONFIG.ehci_debug().is_some(),
        selection,
    )
}

fn resolve_for<P: IntelPlatform>(
    facts: BoardFacts,
    max_cpus: u16,
    ehci_debug: bool,
    selection: BuildSelection,
) -> Result<IntelPlan, std::string::String> {
    let debug = Debug::select(ehci_debug, &selection.debug)?;
    selection.reject_unknown_options(fstart_image_build::plan::options::X86_LINUX)?;
    let x86_linux = selection.x86_linux()?;
    let requested_payload = selection.payload.clone().unwrap_or_else(|| "halt".into());
    if x86_linux.any() && requested_payload != "linux" {
        return Err("x86 Linux options require '--payload linux'".into());
    }
    let x86_linux_bootargs = x86_linux.bootargs.clone().unwrap_or_default();
    if !matches!(
        requested_payload.as_str(),
        "halt" | "linux" | "uefi" | "uefi-ui" | "uefi-basic" | "coreboot"
    ) {
        return Err("Intel supports halt, direct Linux, UEFI and coreboot payloads".into());
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
        features.extend(debug.features());
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
        } else if selected_payload == "coreboot" {
            features.push("payload-coreboot".into());
        }
        IntelStagePlan {
            role,
            features,
            compression: match role {
                IntelStage::Bootblock => fstart_core::Compression::None,
                IntelStage::Postcar => crate::compression::POSTCAR,
                IntelStage::Ramstage => crate::compression::RAMSTAGE,
            },
        }
    });
    if max_cpus == 0 {
        return Err("CPU population must be nonzero".into());
    }
    let reservations = reservations::<P>(facts, debug.flash_console)?;
    let payload_config = match payload.as_str() {
        "uefi" => Some(fstart_core::x86_uefi_payload(crate::compression::PAYLOAD)),
        "coreboot" => Some(fstart_core::x86_coreboot_payload(
            crate::compression::PAYLOAD,
        )),
        // Empty bootargs default to no command line: there is deliberately no
        // implicit serial-console policy, so direct Linux stays silent unless
        // the caller passes `--linux-bootargs` explicitly.
        "linux" => Some(
            fstart_core::x86_linux_payload(
                x86_linux
                    .kernel_load_addr
                    .unwrap_or(fstart_core::payload_manifest::X86_LINUX_DEFAULT_KERNEL_LOAD_ADDR),
                fstart_core::payload_manifest::X86_LINUX_DEFAULT_ZERO_PAGE_ADDR,
                &x86_linux_bootargs,
                x86_linux.print_mtrrs,
                crate::compression::PAYLOAD,
            )
            .map_err(ToString::to_string)?,
        ),
        _ => None,
    };
    let mut mutable_regions = fstart_core::board::MutableFlashRegions::new();
    if facts.memory_cache {
        mutable_regions
            .push(fstart_core::board::MutableFlashRegion {
                name: fstart_core::hstr("mrc-cache"),
                size: crate::memory_cache::EXTENT_SIZE,
                alignment: crate::memory_cache::BANK_SIZE,
            })
            .expect("MRC prefix fits the region transport");
    }
    if debug.flash_console {
        mutable_regions
            .push(fstart_core::board::MutableFlashRegion {
                name: fstart_core::hstr("console"),
                size: crate::facts::FLASH_CONSOLE_SIZE,
                alignment: 0x10000,
            })
            .expect("console prefix fits the region transport");
    }
    let plan = IntelPlan {
        mutable_regions,
        reservations,
        platform: if facts.protected_mode {
            fstart_core::Platform::X86
        } else {
            fstart_core::Platform::X86_64
        },
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
        security: fstart_core::default_security_config(),
        smm: fstart_core::SmmConfig {
            entry_points: Some(max_cpus),
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
        resolve_for::<Gm965>(facts, 2, false, selection)
    }

    #[test]
    fn mutable_regions_come_from_the_runtime_owners_and_survive_transport() {
        let mut plan = resolve(
            FACTS.with_memory_cache(),
            BuildSelection {
                debug: vec!["flash-console".into()],
                ..Default::default()
            },
        )
        .unwrap();
        let regions = &plan.mutable_regions;
        assert_eq!(regions.len(), 2);
        assert_eq!(regions[0].name, "mrc-cache");
        assert_eq!(regions[0].size, crate::memory_cache::EXTENT_SIZE);
        assert_eq!(regions[0].alignment, crate::memory_cache::BANK_SIZE);
        assert_eq!(regions[1].name, "console");
        assert_eq!(regions[1].size, crate::facts::FLASH_CONSOLE_SIZE);
        let config = plan.assembly().unwrap().config("test").unwrap();
        assert_eq!(config.build.mutable_regions[0].size, regions[0].size);
        assert_eq!(config.build.mutable_regions[1].size, regions[1].size);
        plan.mutable_regions[0].alignment = 3;
        assert!(plan.validate().is_err());
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
        assert!(resolve_for::<I945>(FACTS, 0, false, BuildSelection::default()).is_err());
    }
    #[test]
    fn platform_selects_target_payload_stage_bundles_and_smm() {
        let default = resolve(FACTS, BuildSelection::default()).unwrap();
        assert_eq!(default.target(), "x86_64-unknown-none");
        assert_eq!(default.payload, "halt");
        assert_eq!(
            default.stages.map(|stage| stage.compression),
            [
                fstart_core::Compression::None,
                crate::compression::POSTCAR,
                crate::compression::RAMSTAGE
            ]
        );
        let uefi = resolve(
            FACTS,
            BuildSelection {
                payload: Some("uefi".into()),
                ..Default::default()
            },
        )
        .unwrap();
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
                options: [
                    ("x86-linux-kernel-load-addr".into(), "0x2000000".into()),
                    ("x86-linux-bootargs".into(), "console=ttyS0".into()),
                    ("x86-linux-print-mtrrs".into(), "true".into()),
                ]
                .into(),
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
        let smm = plan.unit("smm").unwrap();
        assert!(smm.rustflags.contains(&"-Crelocation-model=pie".into()));
        assert!(smm.rustflags.contains(&"-Zplt=yes".into()));
        let protected = compilation_plan(
            resolve_for::<I945>(
                FACTS.with_protected_mode(),
                2,
                false,
                BuildSelection::default(),
            )
            .unwrap(),
        )
        .unwrap();
        let smm32 = protected.unit("smm").unwrap();
        assert!(
            smm32
                .rustflags
                .contains(&"-Crelocation-model=static".into())
        );
        assert!(smm32.rustflags.contains(&"-Zplt=yes".into()));
        for name in ["bootblock", "postcar", "ramstage"] {
            let unit = plan.unit(name).unwrap();
            // Board ACPI, and the heap it needs, belong to the ramstage only.
            let board_features: &[&str] = if name == "ramstage" { &["acpi"] } else { &[] };
            assert_eq!(unit.board_features, board_features);
            let script = unit.linker_script.as_ref().unwrap();
            assert_eq!(script.contains("_FSTART_HEAP ="), name == "ramstage");
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
            options: [("x86-linux-bootargs".into(), "console=ttyS0".into())].into(),
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
        let atom = resolve_for::<I945>(FACTS, 2, false, BuildSelection::default()).unwrap();
        let mobile = resolve_for::<MobileI945>(FACTS, 2, false, BuildSelection::default()).unwrap();
        let gm = resolve_for::<MobileGm965>(FACTS, 2, false, BuildSelection::default()).unwrap();
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

    #[cfg(feature = "stage")]
    #[test]
    fn i945_runtime_cpu_driver_follows_the_package() {
        use crate::IntelEarlyPlatform;
        use fstart_arch::x86::cpu::intel::{
            core2_cpu::Core2CpuDriver, pineview::PineviewCpuDriver,
        };
        // Type-check both runtime CPU drivers without executing MSR/I/O operations.
        fn cpu<P: IntelEarlyPlatform<Cpu = C>, C>() {}
        cpu::<crate::i945::I945Ich7<SocketM>, Core2CpuDriver>();
        cpu::<I945, PineviewCpuDriver>();
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
        let gm = reservations::<Gm965>(FACTS, false).unwrap();
        let facts = BoardFacts::new(
            fstart_core::FlashLayout::X86Legacy(fstart_core::X86LegacyFlashLayout {
                size: 0x1000000,
            }),
            0x1000000,
        );
        let plan = resolve_for::<Pineview>(facts, 4, false, BuildSelection::default()).unwrap();
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
        let gm = reservations::<Gm965>(FACTS, false).unwrap();
        let facts = BoardFacts::new(
            fstart_core::FlashLayout::X86Legacy(fstart_core::X86LegacyFlashLayout {
                size: 0x80000,
            }),
            0x80000,
        );
        let plan = resolve_for::<I945>(facts, 2, false, BuildSelection::default()).unwrap();
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
