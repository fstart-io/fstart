//! Host build plan and the SP3 flash image (FIRMWARE_IMAGE §7).
//!
//! fbuild's plan runner calls [`Plan::emit`] with the board's `Board` type
//! (which implements [`Sp3BoardFacts`]) and the JSON [`BuildSelection`];
//! the platform answers with the concrete [`BuildPlan`]: one firmware unit,
//! `reset`, linked at the generation's `0x62` destination with
//! `linker/reset.ld.in` and `fstart_entry = "amd-psp-reset"`.
//!
//! Composition of the flash half and the 32 MiB dual image lives in
//! [`image`]. Until image-build has an SP3 assembly variant the plan is
//! compile-only (`Assembly::Unassembled`) and [`image_plan`] carries what the
//! assembler needs.

pub mod image;

use std::collections::BTreeMap;
use std::string::{String, ToString};
use std::{format, vec};

use fstart_core::layout::{Region, RegionKind};
use fstart_driver_zen::soc::Generation;
use fstart_image_build::build_plan::{
    Assembly, BuildPlan, CargoTarget, CompilationUnit, CompilerCfgSchema, UnitOutput,
};
use fstart_image_build::elf::{Architecture, Descriptor, Expectations};
use fstart_image_build::layout::EncodedLayout;
use fstart_image_build::plan::{BuildSelection, Span as PlanSpan};

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

fn cfg_schema() -> CompilerCfgSchema {
    CompilerCfgSchema {
        entries: vec![crate::entry::ENTRY.into()],
        environments: vec!["ram".into()],
        payloads: vec!["halt".into()],
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
    if selection.payload.as_deref().is_some_and(|p| p != "halt")
        || selection.has_x86_linux_overrides()
    {
        // Payloads come from the FFS, which needs the SP3 assembly.
        return Err(
            "SP3 builds support only the halt payload until image assembly is wired".into(),
        );
    }
    if selection.image_config.is_some() || !selection.compiler_files.is_empty() {
        return Err("SP3 takes its vendor image from the board facts".into());
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
        environment: "ram".into(),
        payload: "halt".into(),
        features: vec![RESET_FEATURE.into()],
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
    let plan = BuildPlan {
        payload: "halt".into(),
        units: vec![unit],
        stages: vec![RESET_UNIT.into()],
        assembly: Assembly::Unassembled {
            platform: fstart_core::Platform::X86_64,
        },
        inputs: vec![],
    };
    plan.order()?;
    Ok(plan)
}

/// What composing this board's flash half needs (for the image assembler).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImagePlan {
    /// Workspace-relative vendor ROM.
    pub vendor_image: String,
    pub half: FlashHalf,
    /// Unit whose flat output is the `0x62` image.
    pub reset_unit: &'static str,
    pub compose: image::HalfPlan,
}

pub fn image_plan<B: Sp3BoardFacts>() -> ImagePlan {
    ImagePlan {
        vendor_image: B::VENDOR_IMAGE.into(),
        half: B::FLASH_HALF,
        reset_unit: RESET_UNIT,
        compose: image::HalfPlan {
            generation: B::GENERATION,
            reset: layout::reset_geometry(B::GENERATION),
            ffs_offset: layout::ffs_offset(B::GENERATION) as usize,
            rewrite_reset_geometry: B::REWRITE_RESET_GEOMETRY,
        },
    }
}

/// Check the board's vendor ROM against the build: generation of the
/// selected half, `0x62` geometry (or the rewrite opt-in) and room for the
/// FFS above the vendor prefix. `root` is the workspace root.
pub fn check_vendor<B: Sp3BoardFacts>(
    root: &std::path::Path,
) -> Result<image::VendorLayout, String> {
    let plan = image_plan::<B>();
    let path = root.join(&plan.vendor_image);
    let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let half = image::select_half(&bytes, plan.half).map_err(|e| e.to_string())?;
    let vendor = image::VendorLayout::parse(half).map_err(|e| e.to_string())?;
    let compose = plan.compose;
    if vendor.generation != compose.generation {
        return Err(format!(
            "{} half of {} is {}, the variant is {}",
            if plan.half == FlashHalf::Lower {
                "lower"
            } else {
                "upper"
            },
            plan.vendor_image,
            vendor.generation.name(),
            compose.generation.name()
        ));
    }
    if vendor.reset != compose.reset && !compose.rewrite_reset_geometry {
        return Err(format!(
            "vendor 0x62 geometry {:x?} differs from the build's {:x?}",
            vendor.reset, compose.reset
        ));
    }
    if vendor.prefix_end > compose.ffs_offset {
        return Err(format!(
            "vendor prefix ends at {:#x}, above the FFS at {:#x}",
            vendor.prefix_end, compose.ffs_offset
        ));
    }
    Ok(vendor)
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

        for payload in ["linux", "uefi"] {
            let selection = BuildSelection {
                payload: Some(payload.into()),
                ..Default::default()
            };
            assert!(resolve::<Rome>(&selection).is_err());
        }
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
