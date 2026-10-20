//! Host build plan and the SP3 flash image (FIRMWARE_IMAGE §7).
//!
//! fbuild's plan runner calls [`Plan::emit`] with the board's `Board` type
//! (which implements [`Sp3BoardFacts`]) and the JSON [`BuildSelection`];
//! the platform answers with the concrete [`BuildPlan`]: one firmware unit,
//! `reset`, linked at the generation's `0x62` destination with
//! `linker/reset.ld.in` and `fstart_entry = "amd-psp-reset"`, and an
//! [`Assembly::Sp3Flash`] that `fbuild assemble` composes into the vendor
//! ROM's half (and the 32 MiB dual image) with
//! [`fstart_image_build::sp3_flash`].
//!
//! The FFS holds the payload and a copy of the reset unit: the FFS builder
//! finalizes the unit's trust policy only in FFS-resident files, and the
//! composer copies it (and the FFS locator) into the PSP-copied `0x62`
//! image.

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
use fstart_image_build::sp3_flash::{self, HalfPlan, ResetSlot, Sp3FlashAssembly, VendorLayout};

use crate::layout::{
    self, DATA, EARLY_REGION, FLASH_MMIO, FlashHalf, HEAP, PAGE_TABLES, STACK, STORE,
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

/// The generation selected by the platform's features, when exactly one is
/// enabled (firmware builds; host tests may enable all three).
pub fn compiled_generation() -> Option<Generation> {
    match (
        cfg!(feature = "naples"),
        cfg!(feature = "rome"),
        cfg!(feature = "milan"),
    ) {
        (true, false, false) => Some(Generation::Naples),
        (false, true, false) => Some(Generation::Rome),
        (false, false, true) => Some(Generation::Milan),
        _ => None,
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
/// flash window and the FFS within it, the store, and the page tables as
/// an exclusion.
pub fn descriptor(generation: Generation) -> Result<EncodedLayout, String> {
    let reset = layout::reset_geometry(generation);
    reset.check().map_err(|e| format!("{e:?}"))?;
    let writable = Span::new(DATA.base, STACK.end() - DATA.base);
    EncodedLayout::encode(
        0,
        &[
            region(RegionKind::Image, reset.dram()),
            region(RegionKind::Writable, writable),
            region(RegionKind::Stack, STACK),
            region(RegionKind::Heap, HEAP),
            region(RegionKind::Flash, FLASH_MMIO),
            region(RegionKind::Firmware, layout::ffs_window(generation)),
            region(RegionKind::FirmwareStore, STORE),
            region(RegionKind::Reserved, PAGE_TABLES),
        ],
    )
    .map_err(|e| format!("SP3 layout descriptor: {e:?}"))
}

/// The reset image linker script for `generation`.
pub fn linker_script(generation: Generation) -> Result<String, String> {
    let reset = layout::reset_geometry(generation);
    let hex = |v: u64| format!("{v:#x}");
    let section = fstart_image_build::linker::layout_section(&descriptor(generation)?, "IMAGE");
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
/// exactly in the `0x62` slot, running there and in the early region,
/// entered at the reset vector.
pub fn expectations(generation: Generation) -> Result<Expectations, String> {
    let reset = layout::reset_geometry(generation);
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
            bytes: descriptor(generation)?.as_bytes().to_vec(),
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

/// The image-build view of a flash half.
pub const fn flash_half(half: FlashHalf) -> sp3_flash::FlashHalf {
    match half {
        FlashHalf::Lower => sp3_flash::FlashHalf::Lower,
        FlashHalf::Upper => sp3_flash::FlashHalf::Upper,
    }
}

/// How the board's half is composed: the generation's `0x62` slot and FFS
/// offset, and the board's rewrite opt-in.
pub fn half_plan<B: Sp3BoardFacts>() -> HalfPlan {
    let reset = layout::reset_geometry(B::GENERATION);
    HalfPlan {
        generation: B::GENERATION,
        reset: ResetSlot {
            flash_offset: reset.flash_offset,
            size: reset.size,
            destination: reset.destination,
        },
        ffs_offset: layout::ffs_offset(B::GENERATION),
        rewrite_reset_geometry: B::REWRITE_RESET_GEOMETRY,
    }
}

/// The assembler's view of the reset unit and its FFS: the FFS fills the
/// firmware window of the booted half, the unit is monolithic and stored
/// in the FFS (see the module docs) besides its `0x62` slot.
fn ffs_assembly(
    generation: Generation,
    payload: Payload,
    config: Option<PayloadConfig>,
) -> FfsAssembly {
    let reset = layout::reset_geometry(generation);
    let ffs = layout::ffs_window(generation);
    let region = |name: &str, span: Span, kind| MemoryRegion {
        name: hstr(name),
        base: span.base,
        size: span.size,
        kind,
    };
    FfsAssembly {
        platform: fstart_core::Platform::X86_64,
        memory: vec![
            region("flash", FLASH_MMIO, fstart_core::memory::RegionKind::Rom),
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

/// Resolve the board's plan for one generation build.
pub fn resolve<B: Sp3BoardFacts>(selection: &BuildSelection) -> Result<BuildPlan, String> {
    let generation = B::GENERATION;
    if let Some(compiled) = compiled_generation()
        && compiled != generation
    {
        return Err(format!(
            "board variant is {} but the platform is built for {}",
            generation.name(),
            compiled.name()
        ));
    }
    let payload = Payload::select(selection)?;
    if !selection.compiler_files.is_empty() {
        return Err("SP3 takes no compiler file inputs".into());
    }
    if B::MAX_CPUS == 0 {
        return Err("CPU population must be nonzero".into());
    }
    let reset = layout::reset_geometry(generation);
    let unit = CompilationUnit {
        name: RESET_UNIT.into(),
        cargo_target: CargoTarget::BoardBinary,
        target: "x86_64-unknown-none".into(),
        entry: crate::entry::ENTRY.into(),
        cfg_schema: cfg_schema(),
        environment: RESET_ENVIRONMENT.into(),
        payload: payload.cfg().into(),
        features: core::iter::once(RESET_FEATURE)
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
        linker_script: Some(linker_script(generation)?),
        environment_values: BTreeMap::from([(
            "FSTART_MP_MAX_CPUS".into(),
            B::MAX_CPUS.to_string(),
        )]),
        bindings: vec![],
        file_bindings: vec![],
        output: UnitOutput::Executable {
            expectations: expectations(generation)?,
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
            capacity: layout::ffs_window(generation).size,
        }],
        Payload::Halt | Payload::Uefi => vec![],
    };
    let plan = BuildPlan {
        payload: payload.name().into(),
        units: vec![unit],
        stages: vec![RESET_UNIT.into()],
        assembly: Assembly::Sp3Flash(Box::new(Sp3FlashAssembly {
            ffs: ffs_assembly(generation, payload, payload.config(selection)?),
            // `--image-config` replaces the board's vendor ROM (same
            // workspace-relative or absolute path rules).
            vendor_image: selection
                .image_config
                .clone()
                .unwrap_or_else(|| B::VENDOR_IMAGE.into()),
            half: flash_half(B::FLASH_HALF),
            reset_unit: RESET_UNIT.into(),
            compose: half_plan::<B>(),
        })),
        inputs,
    };
    plan.order()?;
    Ok(plan)
}

/// Check the board's vendor ROM against the build: generation of the
/// selected half, `0x62` geometry (or the rewrite opt-in) and room for the
/// FFS above the vendor prefix. `root` is the workspace root.
pub fn check_vendor<B: Sp3BoardFacts>(root: &std::path::Path) -> Result<VendorLayout, String> {
    let path = root.join(B::VENDOR_IMAGE);
    let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let half = sp3_flash::select_half(&bytes, flash_half(B::FLASH_HALF))?;
    Ok(half_plan::<B>().check_vendor(half)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Rome;
    impl Sp3BoardFacts for Rome {
        const GENERATION: Generation = Generation::Rome;
        const VENDOR_IMAGE: &'static str =
            "../sp3/firmware/tyan-s8030gm2ne/S8030GM2NE_BIOS_V4.04/8030V404.ROM";
        const FLASH_HALF: FlashHalf = FlashHalf::Lower;
        const MAX_CPUS: u16 = 128;
    }
    struct Milan;
    impl Sp3BoardFacts for Milan {
        const GENERATION: Generation = Generation::Milan;
        const VENDOR_IMAGE: &'static str = Rome::VENDOR_IMAGE;
        const FLASH_HALF: FlashHalf = FlashHalf::Upper;
        const MAX_CPUS: u16 = 256;
    }

    fn sp3(plan: &BuildPlan) -> &Sp3FlashAssembly {
        let Assembly::Sp3Flash(image) = &plan.assembly else {
            panic!("not an SP3 flash assembly")
        };
        image
    }

    #[test]
    fn plan_has_one_reset_unit_linked_at_the_0x62_slot() {
        let milan = resolve::<Milan>(&BuildSelection::default());
        if compiled_generation().is_some_and(|g| g != Generation::Milan) {
            // A Milan variant cannot use a platform built for another generation.
            assert!(milan.is_err());
            return;
        }
        let plan = milan.unwrap();
        assert_eq!(plan.stages, [RESET_UNIT]);
        let unit = plan.unit(RESET_UNIT).unwrap();
        assert_eq!(unit.entry, "amd-psp-reset");
        assert_eq!(unit.environment, RESET_ENVIRONMENT);
        assert_eq!(unit.features, [RESET_FEATURE]);
        assert_eq!(unit.environment_values["FSTART_MP_MAX_CPUS"], "256");
        let UnitOutput::Executable {
            expectations,
            load_address,
            flat_capacity,
        } = &unit.output
        else {
            panic!("not executable")
        };
        assert_eq!((*load_address, *flat_capacity), (0x75ce_0000, 0x32_0000));
        assert_eq!(expectations.entry, Some(0x75ff_fff0));
        let script = unit.linker_script.as_deref().unwrap();
        for needle in [
            "ORIGIN = 0x75ce0000, LENGTH = 0x320000",
            ".sp3.entry16 (0x76000000 - 0x1000)",
            "ORIGIN = 0x75000000, LENGTH = 0x101000",
            "_fstart_layout_start = .;",
            "QUAD(0x400000)",
        ] {
            assert!(script.contains(needle), "{needle}");
        }
    }

    #[test]
    fn plan_assembles_the_vendor_half_with_the_ffs_in_the_firmware_window() {
        let Ok(plan) = resolve::<Rome>(&BuildSelection::default()) else {
            assert!(compiled_generation().is_some_and(|g| g != Generation::Rome));
            return;
        };
        let image = sp3(&plan);
        assert_eq!(image.vendor_image, Rome::VENDOR_IMAGE);
        assert_eq!(image.half, sp3_flash::FlashHalf::Lower);
        assert_eq!(image.reset_unit, RESET_UNIT);
        assert_eq!(
            image.compose.reset,
            ResetSlot {
                flash_offset: 0xdb_0000,
                size: 0x25_0000,
                destination: 0x75db_0000
            }
        );
        assert_eq!(image.compose.ffs_offset, 0x60_0000);
        assert!(!image.compose.rewrite_reset_geometry);
        let config = plan.assembly.config("tyan-s8030-rome").unwrap();
        assert_eq!(
            config.build.firmware_image,
            FirmwareImagePolicy::memory_mapped(0xff60_0000, 0x7b_0000)
        );
        let StageLayout::Monolithic(stage) = &config.stages else {
            panic!("not monolithic")
        };
        assert!(!stage.external && stage.load_addr == 0x75db_0000);
        assert!(config.payload.is_none() && plan.inputs.is_empty());

        // Payloads, the vendor ROM override and what is refused.
        let select = |payload: &str| BuildSelection {
            payload: Some(payload.into()),
            ..Default::default()
        };
        let linux = resolve::<Rome>(&BuildSelection {
            x86_linux_bootargs: Some("console=ttyS0,115200".into()),
            ..select("linux")
        })
        .unwrap();
        let unit = linux.unit(RESET_UNIT).unwrap();
        assert_eq!(
            (unit.payload.as_str(), &unit.features[1][..]),
            ("linux", "payload-linux")
        );
        assert_eq!(linux.inputs[0].name, "kernel");
        let payload = sp3(&linux).ffs.payload.as_ref().unwrap();
        assert_eq!(payload.bootargs.as_deref(), Some("console=ttyS0,115200"));
        let uefi = resolve::<Rome>(&select("uefi")).unwrap();
        assert_eq!(uefi.unit(RESET_UNIT).unwrap().payload, "crabefi");
        assert!(resolve::<Rome>(&select("coreboot")).is_err());
        assert!(
            resolve::<Rome>(&BuildSelection {
                x86_linux_print_mtrrs: true,
                ..Default::default()
            })
            .is_err()
        );
        let custom = BuildSelection {
            image_config: Some("/tmp/dump.rom".into()),
            ..Default::default()
        };
        let plan = resolve::<Rome>(&custom).unwrap();
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
        let bytes = descriptor(Generation::Rome).unwrap();
        let layout = fstart_core::layout::Layout::parse(bytes.as_bytes()).unwrap();
        let find = |kind| layout.regions().find(|r| r.kind == kind).unwrap();
        assert_eq!(find(RegionKind::Image).base, 0x75db_0000);
        assert_eq!(find(RegionKind::Firmware).base, 0xff60_0000);
        assert_eq!(find(RegionKind::Firmware).end(), Some(0xffdb_0000));
        assert_eq!(find(RegionKind::Stack).end(), Some(STACK.end()));
    }

    #[test]
    fn vendor_check_against_the_s8030_rom() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        if !root.join(Rome::VENDOR_IMAGE).exists() {
            std::eprintln!("skipping: S8030 v4.04 ROM not present");
            return;
        }
        assert_eq!(
            check_vendor::<Rome>(&root).unwrap().generation,
            Generation::Rome
        );
        assert_eq!(
            check_vendor::<Milan>(&root).unwrap().generation,
            Generation::Milan
        );
        struct Swapped;
        impl Sp3BoardFacts for Swapped {
            const GENERATION: Generation = Generation::Milan;
            const VENDOR_IMAGE: &'static str = Rome::VENDOR_IMAGE;
            const FLASH_HALF: FlashHalf = FlashHalf::Lower;
            const MAX_CPUS: u16 = 1;
        }
        assert!(check_vendor::<Swapped>(&root).is_err());
    }
}
