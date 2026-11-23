//! Intel family policy, evaluated on the host from the selected board's Rust facts.
extern crate std;
use crate::facts::{BoardFacts, IntelBoardFacts, IntelPlatform, IntelPlatformConfig};
use fstart_image_build::{
    intel_plan::{IntelReservations, IntelStage, StageReservation},
    plan::{
        BuildSelection, HostPlan, IntelPlan, IntelStagePlan, PlanError, ResolvedPlan, Span,
        X86LinuxOptions,
    },
};

use std::{format, string::ToString, vec, vec::Vec};

/// Intel platforms accept authenticated file assets as well as Linux policy.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Options {
    #[serde(rename = "file-assets")]
    pub file_assets: Option<std::string::String>,
    #[serde(flatten)]
    pub linux: X86LinuxOptions,
}

/// Conventional platform export consumed by the generic fbuild runner.
pub struct Plan<B>(core::marker::PhantomData<B>);
impl<B: IntelBoardFacts> HostPlan<B> for Plan<B> {
    type Options = Options;

    fn resolve(selection: BuildSelection, options: Options) -> Result<ResolvedPlan, PlanError> {
        // Explicit image configuration carries private authenticated data (such
        // as a discrete GPU ROM), never silently discovered board-local files.
        let assets = options
            .file_assets
            .as_deref()
            .map(|path| {
                std::fs::read(path)
                    .map_err(|error| format!("Intel image config: {error}"))
                    .and_then(|bytes| image_assets(&bytes))
            })
            .transpose()?
            .unwrap_or_default();
        let log_level = selection.log_level.clone();
        let mut plan = compilation_plan(resolve::<B>(selection, options.linux)?)?;
        plan.options = self::options::<B>(log_level.as_deref())?;
        let stage_names = plan.stages.clone();
        let Some(fstart_image_build::build_plan::ImageStep {
            operation: fstart_image_build::build_plan::ImageOperation::Ffs(image),
            ..
        }) = plan.images.first_mut()
        else {
            return Err("Intel FFS image missing".into());
        };
        for asset in assets {
            if image
                .data_assets
                .iter()
                .chain(&stage_names)
                .any(|name| name == asset.name.as_str())
                || matches!(
                    asset.name.as_str(),
                    "cpu_microcode_blob.bin" | "payload" | "linux-manifest"
                )
            {
                return Err(format!("duplicate Intel data asset: {}", asset.name).into());
            }
            image.file_assets.push(asset);
        }
        Ok(plan)
    }
}

/// Parse the explicit Intel image configuration. Paths are workspace-relative,
/// as for every FFS file asset. Reject unknown keys and duplicate FFS names.
fn image_assets(bytes: &[u8]) -> Result<Vec<fstart_core::board::FileAsset>, std::string::String> {
    let mut config: std::collections::BTreeMap<
        std::string::String,
        Vec<fstart_core::board::FileAsset>,
    > = serde_json::from_slice(bytes).map_err(|error| format!("Intel image config: {error}"))?;
    let assets = config.remove("file_assets").unwrap_or_default();
    if !config.is_empty() {
        return Err("unknown Intel image configuration field".into());
    }
    if assets.len() > 8 {
        return Err("too many Intel file assets".into());
    }
    for (index, asset) in assets.iter().enumerate() {
        if asset.name.is_empty()
            || asset.path.is_empty()
            || assets[..index]
                .iter()
                .any(|previous| previous.name == asset.name)
        {
            return Err("empty or duplicate Intel file asset".into());
        }
    }
    Ok(assets)
}

/// The options a build of `B` at `log_level` offers, kept in CMOS: the
/// console level it starts at, the chipset's, the board's, in the order
/// the ramstage resolves them.
pub fn options<B: IntelBoardFacts>(
    log_level: Option<&str>,
) -> Result<fstart_image_build::options::Options, std::string::String> {
    use fstart_image_build::options::{OptionStore, collect};
    let level = fstart_log::start_level(log_level.map(fstart_log::parse_verbosity));
    let window = crate::facts::OPTION_WINDOW;
    let store = OptionStore::Cmos {
        first: window.first,
        last: window.last,
    };
    collect(Some(store), |each| {
        fstart_options::describe(&fstart_log::option::KNOBS, &level, each);
        B::CONFIG.describe_options(each);
        for set in B::OPTIONS {
            set.describe(each);
        }
    })
}

pub fn compilation_plan(
    plan: IntelPlan,
) -> Result<fstart_image_build::build_plan::BuildPlan, std::string::String> {
    use fstart_image_build::build_plan::{
        ArtifactBinding, BuildPlan, CargoTarget, CompilationUnit, InputFile, SmmRelocation,
        SmmUnit, UnitOutput,
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
    // its audited absolute references instead.
    let mut units = vec![
        SmmUnit {
            name: "smm".into(),
            platform: plan.platform,
            features: plan.smm_features.clone(),
            relocation: if plan.platform == fstart_core::Platform::X86 {
                SmmRelocation::Static
            } else {
                SmmRelocation::Pie
            },
            environment_values: mp_capacity(),
            image: fstart_image_build::smm_image::ImageOptions {
                entry_count: plan.smm.entry_points.unwrap_or(plan.max_cpus),
                stack_size: plan.smm.stack_size,
                coreboot_module_args: plan.smm.coreboot.module_args,
                coreboot_header: plan.smm.coreboot.emit_header,
            },
        }
        .into(),
    ];
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
        options: Default::default(),
        update: None,
        payload: plan.payload.clone(),
        units,
        stages: plan.stages.iter().map(|r| r.role.name().into()).collect(),
        platform: plan.platform,
        images: vec![plan.assembly()?.into()],
        // QEMU emulates none of these chipsets: q35 is not an i945, and
        // its own boards (fstart-platform-qemu) carry its run recipe.
        run: None,
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
                "ehci" => {
                    return Err(
                        "no enabled EHCI debug controller, or its BAR overlaps a chipset decode"
                            .into(),
                    );
                }
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
    x86_linux: X86LinuxOptions,
) -> Result<IntelPlan, PlanError> {
    resolve_for::<B::Platform>(
        B::FACTS,
        B::CONFIG.max_cpus(),
        B::CONFIG.ehci_debug().is_some(),
        selection,
        x86_linux,
    )
}

fn resolve_for<P: IntelPlatform>(
    facts: BoardFacts,
    max_cpus: u16,
    ehci_debug: bool,
    selection: BuildSelection,
    x86_linux: X86LinuxOptions,
) -> Result<IntelPlan, PlanError> {
    let debug = Debug::select(ehci_debug, &selection.debug)?;
    let requested_payload = selection.payload.clone().unwrap_or_else(|| "halt".into());
    if x86_linux.any() && requested_payload != "linux" {
        return Err("x86 Linux options require '--payload linux'".into());
    }
    let x86_linux_bootargs = x86_linux.bootargs.clone().unwrap_or_default();
    if !matches!(
        requested_payload.as_str(),
        "halt" | "linux" | "uefi" | "uefi-ui" | "uefi-basic" | "coreboot"
    ) {
        return Err(PlanError::unsupported_payload(
            "Intel supports halt, direct Linux, UEFI and coreboot payloads",
        ));
    }
    let payload = if requested_payload.starts_with("uefi") {
        "uefi".to_string()
    } else {
        requested_payload.clone()
    };
    let mut reservations = reservations::<P>(facts, debug.flash_console)?;
    let bios_size = reservations.firmware.size;
    if payload == "uefi" && !facts.protected_mode && bios_size <= 0x8_0000 {
        return Err(PlanError::unsupported_payload(format!(
            "UEFI payloads do not fit in the {bios_size:#x}-byte BIOS window"
        )));
    }
    // On 1–1.5 MiB BIOS windows, the LZ4-compressed UEFI ramstage can
    // overlap the reset image. Use denser storage without changing the
    // payload or the fixed flash layout. Only postcar needs the decoder.
    let dense = payload == "uefi" && !facts.protected_mode && bios_size <= 0x18_0000;
    if dense {
        let postcar = &mut reservations.postcar;
        postcar.stack = 0x1_0000;
        // 1088 KiB for the decoder's bounded allocations, rounded up for
        // heap metadata. Leave another 64 KiB for BSS and alignment.
        postcar.heap = 0x12_0000;
        postcar.writable.size = postcar.heap + postcar.stack + 0x1_0000;
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
        let mut features = if dense && role == IntelStage::Postcar {
            vec!["postcar-lzma".into()]
        } else {
            vec![bundle.into()]
        };
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
                IntelStage::Ramstage if dense => fstart_core::Compression::Lzma,
                IntelStage::Ramstage => crate::compression::RAMSTAGE,
            },
        }
    });
    if max_cpus == 0 {
        return Err("CPU population must be nonzero".into());
    }
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

    /// Through the selection's options, as the plan runner parses them.
    fn resolve(
        facts: BoardFacts,
        selection: BuildSelection,
    ) -> Result<IntelPlan, std::string::String> {
        let options = selection.options()?;
        resolve_for::<Gm965>(facts, 2, false, selection, options).map_err(|e| e.to_string())
    }

    #[test]
    fn explicit_image_assets_are_bounded_and_unambiguous() {
        let mut selection = BuildSelection::default();
        selection.set_option("file-assets", "private/t61.json");
        assert_eq!(
            selection
                .options::<Options>()
                .unwrap()
                .file_assets
                .as_deref(),
            Some("private/t61.json")
        );
        let assets = image_assets(
            br#"{"file_assets":[{"name":"pci10de,0429.rom","path":"private/nvs140m.rom"}]}"#,
        )
        .unwrap();
        assert_eq!(assets.len(), 1);
        assert_eq!(assets[0].name.as_str(), "pci10de,0429.rom");
        assert!(image_assets(br#"{}"#).unwrap().is_empty());
        for bad in [
            br#"{"unknown":[]}"#.as_slice(),
            br#"{"file_assets":[{"name":"","path":"a"}]}"#,
            br#"{"file_assets":[{"name":"a","path":""}]}"#,
            br#"{"file_assets":[{"name":"a","path":"one"},{"name":"a","path":"two"}]}"#,
        ] {
            assert!(image_assets(bad).is_err());
        }
    }

    #[test]
    fn file_assets_reach_ffs_without_shadowing_firmware_files() {
        struct Board;
        impl IntelBoardFacts for Board {
            type Platform = Gm965;
            const CONFIG: &'static crate::gm965::Gm965Ich8Platform =
                &crate::gm965::Gm965Ich8Config::new().max_cpus(2).build();
            const FACTS: BoardFacts = FACTS.with_data_assets(&["data.vbt"]);
        }
        let path =
            std::env::temp_dir().join(format!("fstart-intel-assets-{}.json", std::process::id()));
        for name in [
            "pci10de,0429.rom",
            "data.vbt",
            "ramstage",
            "cpu_microcode_blob.bin",
        ] {
            std::fs::write(
                &path,
                serde_json::to_vec(&serde_json::json!({
                    "file_assets": [{"name": name, "path": "private/video.rom"}]
                }))
                .unwrap(),
            )
            .unwrap();
            let mut selection = BuildSelection::default();
            selection.set_option("file-assets", path.to_str().unwrap());
            let options = selection.options::<Options>().unwrap();
            let plan = Plan::<Board>::resolve(selection, options);
            if name == "pci10de,0429.rom" {
                let plan = plan.unwrap();
                let image = plan.ffs_assembly().unwrap();
                assert_eq!(image.file_assets.len(), 1);
                assert_eq!(image.file_assets[0].name.as_str(), name);
                assert_eq!(image.data_assets, ["data.vbt"]);
            } else {
                assert!(plan.is_err(), "{name}");
            }
        }
        std::fs::remove_file(path).unwrap();
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
        let plan = super::resolve::<Board>(BuildSelection::default(), Default::default()).unwrap();
        assert_eq!(plan.max_cpus, 4);
        assert_eq!(plan.smm.entry_points, Some(4));
        assert_eq!(
            plan.reservations.bootblock.writable.base,
            <Socket441 as LegacyCpu>::CAR_BASE
        );
        assert_eq!(plan.microcode.len(), 2);
        assert!(
            resolve_for::<I945>(
                FACTS,
                0,
                false,
                BuildSelection::default(),
                Default::default()
            )
            .is_err()
        );
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
        assert_eq!(uefi.stages[1].features, ["postcar-lzma"]);
        assert_eq!(uefi.stages[2].compression, fstart_core::Compression::Lzma);
        assert_ne!(uefi.reservations.postcar.heap, 0);
        assert_eq!(default.reservations.postcar.heap, 0);
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
                Default::default(),
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
    fn uefi_uses_bios_capacity_not_chip_size() {
        use fstart_core::{FlashLayout, X86LegacyFlashLayout};
        let select = |size, payload: &str| {
            let facts =
                BoardFacts::new(FlashLayout::X86Legacy(X86LegacyFlashLayout { size }), size);
            resolve(
                facts,
                BuildSelection {
                    payload: Some(payload.into()),
                    ..Default::default()
                },
            )
        };
        for payload in ["uefi", "uefi-basic", "uefi-ui"] {
            let error = select(0x8_0000, payload).unwrap_err();
            assert!(error.contains("0x80000-byte BIOS window"), "{error}");
        }
        let dense = select(0x10_0000, "uefi-ui").unwrap();
        assert_eq!(dense.stages[2].compression, fstart_core::Compression::Lzma);
        let roomy = select(0x20_0000, "uefi-ui").unwrap();
        assert_eq!(roomy.stages[2].compression, crate::compression::RAMSTAGE);
        assert_eq!(roomy.reservations.postcar.heap, 0);
        assert!(select(0x8_0000, "halt").is_ok());
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
    fn physical_ifd_capacity_is_independent_of_bootblock_rom_mtrr() {
        use fstart_image_build::{intel_plan::IntelStage, linker::resolved_intel};
        for (capacity, cache_base, cache_mask) in [
            (3 << 20, 0xffc0_0000u32, 0xffc0_0000u32),
            (32 << 20, 0xff00_0000, 0xff00_0000),
        ] {
            let flash = IntelIfdFlashLayout::with_geometry(
                fstart_core::memory::IfdVersion::V2,
                capacity,
                ConstVec::new(DESCRIPTOR).push(DESCRIPTOR).push(Region {
                    kind: Kind::Bios,
                    offset: capacity - (2 << 20),
                    size: 2 << 20,
                }),
            );
            let plan = resolve(
                BoardFacts::new(fstart_core::FlashLayout::IntelIfd(flash), capacity),
                BuildSelection::default(),
            )
            .unwrap();
            assert_eq!(plan.reservations.flash.size, u64::from(capacity));
            assert_eq!(plan.reservations.firmware.size, 2 << 20);
            let script = resolved_intel(
                &plan.reservations,
                IntelStage::Bootblock,
                false,
                plan.platform,
            )
            .unwrap();
            assert!(script.contains(&format!(
                "_rom_mtrr_base = {cache_base:#x}; _rom_mtrr_mask = {cache_mask:#x};"
            )));
        }
    }

    #[test]
    fn legacy_cpu_packages_are_independent_of_the_chipset() {
        type MobileI945 = crate::i945::I945Ich7<SocketM>;
        type MobileGm965 = crate::gm965::Gm965Ich8<SocketM>;
        let atom = resolve_for::<I945>(
            FACTS,
            2,
            false,
            BuildSelection::default(),
            Default::default(),
        )
        .unwrap();
        let mobile = resolve_for::<MobileI945>(
            FACTS,
            2,
            false,
            BuildSelection::default(),
            Default::default(),
        )
        .unwrap();
        let gm = resolve_for::<MobileGm965>(
            FACTS,
            2,
            false,
            BuildSelection::default(),
            Default::default(),
        )
        .unwrap();
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
        let plan = resolve_for::<Pineview>(
            facts,
            4,
            false,
            BuildSelection::default(),
            Default::default(),
        )
        .unwrap();
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
        let plan = resolve_for::<I945>(
            facts,
            2,
            false,
            BuildSelection::default(),
            Default::default(),
        )
        .unwrap();
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
