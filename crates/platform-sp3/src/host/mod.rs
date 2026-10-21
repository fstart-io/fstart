//! Host build plan and the SP3 flash image (FIRMWARE_IMAGE §7).
//!
//! fbuild's plan runner calls [`Plan::emit`] with the board's `Board` type
//! (which implements [`Sp3BoardFacts`]) and the JSON [`BuildSelection`];
//! the platform answers with the concrete [`BuildPlan`]: one firmware unit,
//! `reset`, built with every generation the board supports, linked at the
//! common reset image address ([`layout::RESET`]) with
//! `linker/reset.ld.in` and `fstart_entry = "amd-psp-reset"`, and an
//! [`Assembly::Sp3Flash`] that `fbuild assemble` composes into the vendor
//! ROM with [`fstart_image_build::sp3_flash`]: one vendor EFS per half,
//! a reset image copy per composed half, one FFS in the lower half
//! (`--image-config` composes into another vendor ROM, e.g. the target
//! board's flash backup).
//!
//! The FFS holds the payload and a copy of the reset unit: the FFS builder
//! finalizes the unit's trust policy only in FFS-resident files, and the
//! composer copies it, the FFS locator and the flash anchor (FFS flash
//! offset and size) into the PSP-copied `0x62` image.

use std::boxed::Box;
use std::collections::BTreeMap;
use std::string::{String, ToString};
use std::{format, vec};

use fstart_core::layout::{Region, RegionKind};
use fstart_core::{
    BoardBuildPolicy, FirmwareImageConfig, FirmwareImagePolicy, MemoryRegion, MonolithicConfig,
    PayloadConfig, SocImageFormat, StageBuildConfig, StageLayout, hstr,
};
use fstart_driver_zen::soc::Generation;
use fstart_image_build::build_plan::{
    Assembly, BuildPlan, CargoTarget, CompilationUnit, CompilerCfgSchema, FfsAssembly, InputFile,
    UnitOutput,
};
use fstart_image_build::elf::{Architecture, Descriptor, Expectations};
use fstart_image_build::layout::EncodedLayout;
use fstart_image_build::plan::{BuildSelection, Span as PlanSpan};
use fstart_image_build::sp3_flash::{FlashPlan, ResetSlot, Sp3FlashAssembly, Survey};

use crate::layout::{
    self, DATA, EARLY_REGION, FFS_WINDOW, HEAP, PAGE_TABLES, RESET, ROM3, STACK, STORE,
    Sp3BoardFacts, Span,
};

/// The firmware unit (and FFS stage) name of the reset image.
pub const RESET_UNIT: &str = "reset";
/// Platform feature that bundles the reset-image stage runtime.
pub const RESET_FEATURE: &str = "bundle-reset";
const LINKER_TEMPLATE: &str = include_str!("../../linker/reset.ld.in");

/// Conventional platform export consumed by the generic fbuild runner.
pub struct Plan<B>(core::marker::PhantomData<B>);

impl<B: Sp3BoardFacts> Plan<B> {
    pub fn emit(selection_json: &str) {
        let selection: BuildSelection =
            serde_json::from_str(selection_json).expect("invalid build selection");
        let plan = resolve::<B>(&selection).expect("invalid SP3 board plan");
        std::println!(
            "{}",
            serde_json::to_string(&plan).expect("serialize SP3 plan")
        );
    }
}

/// The platform feature of a generation.
pub const fn feature(generation: Generation) -> &'static str {
    match generation {
        Generation::Naples => "naples",
        Generation::Rome => "rome",
        Generation::Milan => "milan",
    }
}

fn plan_span(span: Span) -> PlanSpan {
    PlanSpan {
        base: span.base,
        size: span.size,
    }
}

fn region(kind: RegionKind, span: Span) -> Region {
    Region {
        kind,
        base: span.base,
        size: span.size,
    }
}

/// Runtime layout descriptor of the reset image (read by
/// `fstart_stage::layout::current()`): image, writable/stack/heap, the
/// flash (ROM3) and the FFS window within it, the store, and the page
/// tables as an exclusion.
pub fn descriptor() -> Result<EncodedLayout, String> {
    let writable = Span::new(DATA.base, STACK.end() - DATA.base);
    EncodedLayout::encode(
        0,
        &[
            region(RegionKind::Image, RESET.dram()),
            region(RegionKind::Writable, writable),
            region(RegionKind::Stack, STACK),
            region(RegionKind::Heap, HEAP),
            region(RegionKind::Flash, ROM3),
            region(RegionKind::Firmware, FFS_WINDOW),
            region(RegionKind::FirmwareStore, STORE),
            region(RegionKind::Reserved, PAGE_TABLES),
        ],
    )
    .map_err(|e| format!("SP3 layout descriptor: {e:?}"))
}

/// The reset image linker script (one for every generation).
pub fn linker_script() -> Result<String, String> {
    let reset = RESET;
    let hex = |v: u64| format!("{v:#x}");
    let section = fstart_image_build::linker::layout_section(&descriptor()?, "IMAGE");
    let values = [
        ("@IMAGE_BASE@", hex(reset.destination)),
        ("@IMAGE_SIZE@", hex(u64::from(reset.size))),
        ("@IMAGE_END@", hex(reset.dram().end())),
        ("@PT_BASE@", hex(PAGE_TABLES.base)),
        ("@PT_SIZE@", hex(PAGE_TABLES.size)),
        ("@DATA_BASE@", hex(DATA.base)),
        ("@DATA_SIZE@", hex(DATA.size)),
        ("@HEAP_BASE@", hex(HEAP.base)),
        ("@HEAP_SIZE@", hex(HEAP.size)),
        ("@STACK_BASE@", hex(STACK.base)),
        ("@STACK_SIZE@", hex(STACK.size)),
        ("@LAYOUT_SECTION@", section),
    ];
    let script = values
        .iter()
        .fold(LINKER_TEMPLATE.to_string(), |s, (key, value)| {
            s.replace(key, value)
        });
    if script.contains('@') {
        return Err("unresolved placeholder in the SP3 linker script".into());
    }
    Ok(script)
}

/// What the linked reset image must look like: one ELF64 x86-64, stored
/// exactly as the common reset image, running there and in the early
/// region, entered at the reset vector.
pub fn expectations() -> Result<Expectations, String> {
    let reset = RESET;
    let image = plan_span(reset.dram());
    let expectations = Expectations {
        architecture: Architecture::X86_64,
        elf64: true,
        little_endian: true,
        stored: vec![image],
        runtime: vec![image, plan_span(EARLY_REGION)],
        identity_mapping: true,
        entry: Some(reset.reset_vector()),
        copy: None,
        descriptor: Some(Descriptor {
            section: ".fstart.layout".into(),
            start_symbol: "_fstart_layout_start".into(),
            end_symbol: "_fstart_layout_end".into(),
            bytes: descriptor()?.as_bytes().to_vec(),
            reservation: image,
        }),
        stored_flags: None,
        symbols: BTreeMap::from([
            ("_sp3_reset_vector".into(), reset.reset_vector()),
            ("_sp3_entry16".into(), reset.dram().end() - 0x1000),
            ("_sp3_page_tables".into(), PAGE_TABLES.base),
        ]),
    };
    expectations.validate()?;
    Ok(expectations)
}

/// Stage environment of the reset unit: an initial image (no predecessor)
/// that does everything, so `fstart-stage` links the FFS locator patch site
/// (`FSTART_ANCHOR`) the composer fills.
pub const RESET_ENVIRONMENT: &str = "monolithic";

fn cfg_schema() -> CompilerCfgSchema {
    CompilerCfgSchema {
        entries: vec![crate::entry::ENTRY.into()],
        environments: vec![RESET_ENVIRONMENT.into()],
        payloads: vec!["halt".into(), "linux".into(), "crabefi".into()],
    }
}

/// The payload a build selects (`--payload`, default halt).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Payload {
    Halt,
    /// Direct x86 Linux (bzImage from `--kernel`, stored in the FFS).
    Linux,
    /// CrabEFI linked into the reset unit.
    Uefi,
}

impl Payload {
    pub fn select(selection: &BuildSelection) -> Result<Self, String> {
        let payload = match selection.payload.as_deref() {
            None | Some("halt") => Self::Halt,
            Some("linux") => Self::Linux,
            Some("uefi") => Self::Uefi,
            Some(other) => {
                return Err(format!(
                    "SP3 supports the halt, linux and uefi payloads, not {other}"
                ));
            }
        };
        if selection.has_x86_linux_overrides() && payload != Self::Linux {
            return Err("x86 Linux options require '--payload linux'".into());
        }
        Ok(payload)
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::Halt => "halt",
            Self::Linux => "linux",
            Self::Uefi => "uefi",
        }
    }

    /// `fstart_payload` cfg value of the reset unit.
    pub const fn cfg(self) -> &'static str {
        match self {
            Self::Halt => "halt",
            Self::Linux => "linux",
            Self::Uefi => "crabefi",
        }
    }

    /// Platform features the reset unit needs for this payload.
    pub const fn features(self) -> &'static [&'static str] {
        match self {
            Self::Halt => &[],
            Self::Linux => &["payload-linux"],
            Self::Uefi => &["payload-uefi"],
        }
    }

    fn config(self, selection: &BuildSelection) -> Result<Option<PayloadConfig>, String> {
        use fstart_core::payload_manifest::{
            X86_LINUX_DEFAULT_KERNEL_LOAD_ADDR, X86_LINUX_DEFAULT_ZERO_PAGE_ADDR,
        };
        Ok(match self {
            Self::Halt => None,
            Self::Uefi => Some(fstart_core::x86_uefi_payload()),
            Self::Linux => Some(fstart_core::x86_linux_payload(
                selection
                    .x86_linux_kernel_load_addr
                    .unwrap_or(X86_LINUX_DEFAULT_KERNEL_LOAD_ADDR),
                X86_LINUX_DEFAULT_ZERO_PAGE_ADDR,
                selection.x86_linux_bootargs.as_deref().unwrap_or_default(),
                selection.x86_linux_print_mtrrs,
            )?),
        })
    }
}

/// How the build is composed into the board's vendor ROM: every `0x62`
/// entry of a supported generation names the reset image at the top of
/// the lower half (above the early region), the FFS starts at
/// [`layout::FFS_OFFSET`] or above.
pub fn flash_plan<B: Sp3BoardFacts>() -> FlashPlan {
    FlashPlan {
        generations: B::GENERATIONS.to_vec(),
        reset: ResetSlot {
            flash_offset: RESET.flash_offset,
            size: RESET.size,
            destination: RESET.destination,
        },
        ffs_offset: layout::FFS_OFFSET,
        dram_floor: EARLY_REGION.end(),
    }
}

/// The assembler's view of the reset unit and its FFS: the FFS lives in
/// the ROM3 firmware window ([`FFS_WINDOW`], an upper bound; the composer
/// enforces the vendor ROM's real one), the unit is monolithic and stored
/// in the FFS (see the module docs) besides its `0x62` copy.
fn ffs_assembly(payload: Payload, config: Option<PayloadConfig>) -> FfsAssembly {
    let (reset, ffs) = (RESET, FFS_WINDOW);
    let region = |name: &str, span: Span, kind| MemoryRegion {
        name: hstr(name),
        base: span.base,
        size: span.size,
        kind,
    };
    FfsAssembly {
        platform: fstart_core::Platform::X86_64,
        memory: vec![
            region("flash", ROM3, fstart_core::memory::RegionKind::Rom),
            region("early", EARLY_REGION, fstart_core::memory::RegionKind::Ram),
        ],
        flash: None,
        bootstrap: vec![],
        stages: StageLayout::Monolithic(MonolithicConfig {
            build: StageBuildConfig {
                firmware_image: Some(FirmwareImageConfig::default()),
                verify_firmware: true,
                payload: payload != Payload::Halt,
                pci: true,
                acpi: true,
                ..Default::default()
            },
            load_addr: reset.destination,
            stack_size: STACK.size as u32,
            heap_size: Some(HEAP.size as u32),
            data_addr: Some(DATA.base),
            page_table_addr: Some((PAGE_TABLES.base, PAGE_TABLES.size)),
            page_size: fstart_core::stage::PageSize::Size1GiB,
            external: false,
        }),
        security: fstart_core::dev_security_config("keys/dev-signing.pub"),
        payload: config,
        microcode: None,
        data_assets: vec![],
        full_flash_image: false,
        soc_image_format: SocImageFormat::None,
        boot_hart_id: 0,
        build: BoardBuildPolicy {
            firmware_image: FirmwareImagePolicy::memory_mapped(ffs.base, ffs.size),
            ..Default::default()
        },
    }
}

/// Resolve the board's plan: one build for every generation it supports.
pub fn resolve<B: Sp3BoardFacts>(selection: &BuildSelection) -> Result<BuildPlan, String> {
    if B::GENERATIONS.is_empty() {
        return Err("the board lists no SP3 generation".into());
    }
    // The board crate enables the generation features it lists; the host
    // runner builds the platform with them, so this checks the board.
    if let Some(g) = B::GENERATIONS.iter().find(|&&g| !layout::compiled(g)) {
        return Err(format!(
            "board lists {} but does not enable fstart-platform-sp3/{}",
            g.name(),
            feature(*g)
        ));
    }
    let payload = Payload::select(selection)?;
    if !selection.compiler_files.is_empty() {
        return Err("SP3 takes no compiler file inputs".into());
    }
    if B::MAX_CPUS == 0 {
        return Err("CPU population must be nonzero".into());
    }
    let reset = RESET;
    let unit = CompilationUnit {
        name: RESET_UNIT.into(),
        cargo_target: CargoTarget::BoardBinary,
        target: "x86_64-unknown-none".into(),
        entry: crate::entry::ENTRY.into(),
        cfg_schema: cfg_schema(),
        environment: RESET_ENVIRONMENT.into(),
        payload: payload.cfg().into(),
        features: core::iter::once(RESET_FEATURE)
            .chain(B::GENERATIONS.iter().map(|&g| feature(g)))
            .chain(payload.features().iter().copied())
            .map(String::from)
            .collect(),
        build_std: Some("core,alloc".into()),
        release_only: false,
        // DRAM below 4 GiB: small code model, static relocations.
        rustflags: [
            "-Zub-checks=no",
            "-Crelocation-model=static",
            "-Ccode-model=small",
            "-Clink-arg=-no-pie",
        ]
        .map(String::from)
        .to_vec(),
        linker_script: Some(linker_script()?),
        environment_values: BTreeMap::from([(
            "FSTART_MP_MAX_CPUS".into(),
            B::MAX_CPUS.to_string(),
        )]),
        bindings: vec![],
        file_bindings: vec![],
        output: UnitOutput::Executable {
            expectations: expectations()?,
            load_address: reset.destination,
            flat_capacity: u64::from(reset.size),
        },
    };
    // The kernel is the one file a caller supplies (`--kernel`); the
    // assembler enforces what actually fits next to the reset unit copy.
    let inputs = match payload {
        Payload::Linux => vec![InputFile {
            name: "kernel".into(),
            default: None,
            capacity: FFS_WINDOW.size,
        }],
        Payload::Halt | Payload::Uefi => vec![],
    };
    let plan = BuildPlan {
        payload: payload.name().into(),
        units: vec![unit],
        stages: vec![RESET_UNIT.into()],
        assembly: Assembly::Sp3Flash(Box::new(Sp3FlashAssembly {
            ffs: ffs_assembly(payload, payload.config(selection)?),
            // `--image-config` replaces the board's vendor ROM (same
            // workspace-relative or absolute path rules).
            vendor_image: selection
                .image_config
                .clone()
                .unwrap_or_else(|| B::VENDOR_IMAGE.into()),
            reset_unit: RESET_UNIT.into(),
            compose: flash_plan::<B>(),
        })),
        inputs,
    };
    plan.order()?;
    Ok(plan)
}

/// Check the board's vendor ROM against the build without composing: the
/// halves it would compose and keep and the FFS window. `root` is the workspace root.
pub fn check_vendor<B: Sp3BoardFacts>(root: &std::path::Path) -> Result<Survey, String> {
    let path = root.join(B::VENDOR_IMAGE);
    let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(flash_plan::<B>().survey(&bytes)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fstart_image_build::sp3_flash::{FlashHalf, HalfAction};

    struct S8030;
    impl Sp3BoardFacts for S8030 {
        const GENERATIONS: &'static [Generation] = &[Generation::Rome, Generation::Milan];
        const VENDOR_IMAGE: &'static str =
            "../sp3/firmware/tyan-s8030gm2ne/S8030GM2NE_BIOS_V4.04/8030V404.ROM";
        const MAX_CPUS: u16 = 256;
    }

    fn sp3(plan: &BuildPlan) -> &Sp3FlashAssembly {
        let Assembly::Sp3Flash(image) = &plan.assembly else {
            panic!("not an SP3 flash assembly")
        };
        image
    }

    #[test]
    fn plan_has_one_reset_unit_for_every_board_generation() {
        let plan = resolve::<S8030>(&BuildSelection::default()).unwrap();
        assert_eq!(plan.stages, [RESET_UNIT]);
        let unit = plan.unit(RESET_UNIT).unwrap();
        assert_eq!(unit.entry, "amd-psp-reset");
        assert_eq!(unit.environment, RESET_ENVIRONMENT);
        assert_eq!(unit.features, [RESET_FEATURE, "rome", "milan"]);
        assert_eq!(unit.environment_values["FSTART_MP_MAX_CPUS"], "256");
        let UnitOutput::Executable {
            expectations,
            load_address,
            flat_capacity,
        } = &unit.output
        else {
            panic!("not executable")
        };
        assert_eq!((*load_address, *flat_capacity), (0x75de_0000, 0x22_0000));
        assert_eq!(expectations.entry, Some(0x75ff_fff0));
        let script = unit.linker_script.as_deref().unwrap();
        for needle in [
            "ORIGIN = 0x75de0000, LENGTH = 0x220000",
            ".sp3.entry16 (0x76000000 - 0x1000)",
            "ORIGIN = 0x75000000, LENGTH = 0x101000",
            "_fstart_layout_start = .;",
            "QUAD(0x400000)",
            "the common 0x220000 image",
        ] {
            assert!(script.contains(needle), "{needle}");
        }
        struct Nothing;
        impl Sp3BoardFacts for Nothing {
            const GENERATIONS: &'static [Generation] = &[];
            const VENDOR_IMAGE: &'static str = S8030::VENDOR_IMAGE;
            const MAX_CPUS: u16 = 1;
        }
        assert!(resolve::<Nothing>(&BuildSelection::default()).is_err());
    }

    #[test]
    fn plan_composes_every_supported_half_with_the_ffs_in_the_common_window() {
        let plan = resolve::<S8030>(&BuildSelection::default()).unwrap();
        let image = sp3(&plan);
        assert_eq!(image.vendor_image, S8030::VENDOR_IMAGE);
        assert_eq!(image.reset_unit, RESET_UNIT);
        assert_eq!(
            image.compose,
            FlashPlan {
                generations: vec![Generation::Rome, Generation::Milan],
                reset: ResetSlot {
                    flash_offset: 0xde_0000,
                    size: 0x22_0000,
                    destination: 0x75de_0000
                },
                ffs_offset: 0x60_0000,
                dram_floor: 0x75c0_0000,
            }
        );
        let config = plan.assembly.config("tyan-s8030").unwrap();
        assert_eq!(
            config.build.firmware_image,
            FirmwareImagePolicy::memory_mapped(0xfd_0060_0000, 0x7e_0000)
        );
        let StageLayout::Monolithic(stage) = &config.stages else {
            panic!("not monolithic")
        };
        assert!(!stage.external && stage.load_addr == 0x75de_0000);
        assert!(config.payload.is_none() && plan.inputs.is_empty());

        // Payloads, the vendor ROM override and what is refused.
        let select = |payload: &str| BuildSelection {
            payload: Some(payload.into()),
            ..Default::default()
        };
        let linux = resolve::<S8030>(&BuildSelection {
            x86_linux_bootargs: Some("console=ttyS0,115200".into()),
            ..select("linux")
        })
        .unwrap();
        let unit = linux.unit(RESET_UNIT).unwrap();
        assert_eq!(
            (
                unit.payload.as_str(),
                unit.features.last().unwrap().as_str()
            ),
            ("linux", "payload-linux")
        );
        assert_eq!(linux.inputs[0].name, "kernel");
        let payload = sp3(&linux).ffs.payload.as_ref().unwrap();
        assert_eq!(payload.bootargs.as_deref(), Some("console=ttyS0,115200"));
        let uefi = resolve::<S8030>(&select("uefi")).unwrap();
        assert_eq!(uefi.unit(RESET_UNIT).unwrap().payload, "crabefi");
        assert!(resolve::<S8030>(&select("coreboot")).is_err());
        assert!(
            resolve::<S8030>(&BuildSelection {
                x86_linux_print_mtrrs: true,
                ..Default::default()
            })
            .is_err()
        );
        let custom = BuildSelection {
            image_config: Some("/tmp/dump.rom".into()),
            ..Default::default()
        };
        let plan = resolve::<S8030>(&custom).unwrap();
        assert_eq!(sp3(&plan).vendor_image, "/tmp/dump.rom");
        plan.validate_image_selection(&custom).unwrap();
        // The plan round-trips through fbuild's JSON transport.
        let json = serde_json::to_string(&plan).unwrap();
        let back: BuildPlan = serde_json::from_str(&json).unwrap();
        back.order().unwrap();
        assert_eq!(sp3(&back).compose, sp3(&plan).compose);
    }

    #[test]
    fn descriptor_decodes_with_the_layout_regions() {
        let bytes = descriptor().unwrap();
        let layout = fstart_core::layout::Layout::parse(bytes.as_bytes()).unwrap();
        let find = |kind| layout.regions().find(|r| r.kind == kind).unwrap();
        assert_eq!(find(RegionKind::Image).base, 0x75de_0000);
        assert_eq!(find(RegionKind::Firmware).base, 0xfd_0060_0000);
        assert_eq!(find(RegionKind::Firmware).end(), Some(0xfd_00de_0000));
        assert_eq!(find(RegionKind::Flash).base, 0xfd_0000_0000);
        assert_eq!(find(RegionKind::Stack).end(), Some(STACK.end()));
    }

    #[test]
    fn vendor_check_against_the_s8030_rom() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        if !root.join(S8030::VENDOR_IMAGE).exists() {
            std::eprintln!("skipping: S8030 v4.04 ROM not present");
            return;
        }
        let survey = check_vendor::<S8030>(&root).unwrap();
        assert_eq!(survey.ffs, 0x60_0000..0xde_0000);
        let composed: std::vec::Vec<_> = survey
            .halves
            .iter()
            .map(|(half, action)| match action {
                HalfAction::Compose(layout) => (*half, layout.generation),
                HalfAction::Keep(g) => panic!("{half:?} kept ({g:?})"),
            })
            .collect();
        assert_eq!(
            composed,
            [
                (FlashHalf::Lower, Generation::Rome),
                (FlashHalf::Upper, Generation::Milan)
            ]
        );
        struct Naples;
        impl Sp3BoardFacts for Naples {
            const GENERATIONS: &'static [Generation] = &[Generation::Naples];
            const VENDOR_IMAGE: &'static str = S8030::VENDOR_IMAGE;
            const MAX_CPUS: u16 = 1;
        }
        assert!(check_vendor::<Naples>(&root).is_err());
    }
}
