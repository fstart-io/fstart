//! ASPEED build plans.
//!
//! AST2500: the bootblock executes SPI flash in place from the reset vector,
//! with its data, stack and heap in the on-chip SRAM. Once DRAM is up it
//! loads the main stage into DRAM, which boots the payload. The FFS is
//! packed behind the bootblock inside the 16 MiB the boot-time 3-byte
//! address mode reaches; the composite image is padded to the whole flash
//! part.
extern crate std;
use crate::facts::{AspeedBoardFacts, AspeedSoc, BoardFacts};
use crate::layout::*;
use fstart_core::layout::RegionKind as Kind;
use fstart_core::*;
use fstart_image_build::{
    build_plan::{
        BootstrapRole, BuildPlan, CargoTarget, CompilationUnit, FfsAssembly as Assembly, InputFile,
        UnitOutput,
    },
    layout::EncodedLayout,
    linker::{Xip, resolved_ram_stage},
    plan::{BuildSelection, Span},
};
use std::{
    collections::BTreeMap,
    string::{String, ToString},
    vec,
    vec::Vec,
};

/// AST2500 on-chip SRAM below the boot timestamp table.
pub const AST2500_SRAM: Span = Span {
    base: SRAM_BASE,
    size: TIMESTAMPS - SRAM_BASE,
};
/// The bootblock authenticates the FFS root (Ed25519) on this stack.
pub const STACK_SIZE: u64 = 0x4000;
pub const HEAP_SIZE: u64 = 0x1000;
pub const MAIN_STACK_SIZE: u32 = 0x1_0000;
pub const MAIN_HEAP_SIZE: u32 = 0x4_0000;

pub struct Plan<B>(core::marker::PhantomData<B>);
impl<B: AspeedBoardFacts> Plan<B> {
    pub fn emit(selection_json: &str) {
        let selection = serde_json::from_str(selection_json).expect("invalid build selection");
        let plan = resolve(B::FACTS, selection).expect("invalid ASPEED board plan");
        std::println!(
            "{}",
            serde_json::to_string(&plan).expect("serialize ASPEED plan")
        );
    }
}

pub fn resolve(facts: BoardFacts, selection: BuildSelection) -> Result<BuildPlan, String> {
    selection.reject_debug()?;
    if selection.has_x86_linux_overrides() {
        return Err("x86 Linux options are supported only by Intel platforms".into());
    }
    let payload = selection.payload.unwrap_or_else(|| "halt".into());
    let elf = match payload.as_str() {
        "halt" => false,
        "elf" => true,
        _ => return Err("the AST2500 main stage boots halt or a bare ELF".into()),
    };
    let AspeedSoc::Ast2500 = facts.soc;
    if facts.flash_size < FIRMWARE_WINDOW_SIZE {
        return Err("the boot flash must cover the 16 MiB firmware window".into());
    }
    let flash = Span {
        base: FLASH_BASE,
        size: facts.flash_size,
    };
    let image = Span {
        base: FLASH_BASE,
        size: FIRMWARE_WINDOW_SIZE,
    };
    let writable = AST2500_SRAM;
    let stack = Span {
        base: writable.end()? - STACK_SIZE,
        size: STACK_SIZE,
    };
    let heap = Span {
        base: stack.base - HEAP_SIZE,
        size: HEAP_SIZE,
    };
    let layout = Xip {
        platform: Platform::Armv5te,
        soc_image_format: SocImageFormat::AspeedSpi,
        boot_hart_id: 0,
        image,
        execution: None,
        writable,
        heap,
        stack,
        descriptor: EncodedLayout::encode(
            0,
            &[
                image.region(Kind::Image),
                writable.region(Kind::Writable),
                stack.region(Kind::Stack),
                heap.region(Kind::Heap),
                flash.region(Kind::Flash),
                image.region(Kind::Firmware),
            ],
        )
        .map_err(|e| e.to_string())?,
    };
    let stages = StageLayout::MultiStage(hvec([
        StageConfig {
            name: hstr("bootblock"),
            build: StageBuildConfig {
                load_next_stage: Some(hstr("main")),
                ..StageBuildConfig::default()
            },
            load_addr: image.base,
            stack_size: stack.size as u32,
            heap_size: Some(heap.size as u32),
            runs_from: RunsFrom::Rom,
            compression: fstart_core::ffs::Compression::None,
            data_addr: Some(writable.base),
        },
        StageConfig {
            name: hstr("main"),
            build: StageBuildConfig {
                verify_firmware: true,
                payload: true,
                ..StageBuildConfig::default()
            },
            load_addr: MAIN_STAGE_BASE,
            stack_size: MAIN_STACK_SIZE,
            heap_size: Some(MAIN_HEAP_SIZE),
            runs_from: RunsFrom::Ram,
            compression: fstart_core::ffs::Compression::None,
            data_addr: None,
        },
    ]));
    let main = main_stage(&stages)?;
    let assembly = Assembly {
        platform: Platform::Armv5te,
        memory: vec![
            MemoryRegion {
                name: hstr("flash"),
                base: flash.base,
                size: flash.size,
                kind: RegionKind::Rom,
            },
            MemoryRegion {
                name: hstr("sram"),
                base: writable.base,
                size: writable.size,
                kind: RegionKind::Ram,
            },
            main_dram(),
        ],
        flash: None,
        bootstrap: vec![("main".into(), BootstrapRole::Mainstage)],
        stages,
        security: default_security_config(),
        payload: elf.then(|| fstart_core::elf_payload(fstart_core::ffs::Compression::None)),
        microcode: None,
        data_assets: vec![],
        file_assets: vec![],
        full_flash_image: true,
        soc_image_format: SocImageFormat::AspeedSpi,
        boot_hart_id: 0,
        build: BoardBuildPolicy {
            qemu_machine: facts.qemu_machine,
            firmware_image: FirmwareImagePolicy::memory_mapped(image.base, image.size),
            flash_image: Some(FirmwareImagePolicy::memory_mapped(flash.base, flash.size)),
            ..Default::default()
        },
    };
    // The payload ELF; the assembler enforces what actually fits.
    let inputs: Vec<InputFile> = if elf {
        vec![InputFile {
            name: "kernel".into(),
            default: None,
            capacity: FIRMWARE_WINDOW_SIZE,
        }]
    } else {
        vec![]
    };
    let plan = BuildPlan {
        payload,
        units: vec![
            unit(
                "bootblock",
                &["bundle-ast2500-bootblock"],
                fstart_image_build::linker::resolved_xip(&layout)?,
                UnitOutput::Executable {
                    expectations: layout.elf_expectations()?,
                    load_address: image.base,
                    flat_capacity: image.size,
                },
            ),
            unit(
                "main",
                &["bundle-ast2500-main", "elf"][..1 + usize::from(elf)],
                main.linker_script,
                UnitOutput::Executable {
                    expectations: main.expectations,
                    load_address: main.memory.base,
                    flat_capacity: main.memory.size,
                },
            ),
        ],
        stages: vec!["bootblock".into(), "main".into()],
        platform: assembly.platform,
        images: vec![assembly.into()],
        inputs,
    };
    plan.order()?;
    Ok(plan)
}

/// DRAM up to the end of the main stage: what every accepted DRAM
/// configuration provides.
fn main_dram() -> MemoryRegion {
    MemoryRegion {
        name: hstr("dram"),
        base: DRAM_BASE,
        size: MAIN_STAGE_BASE + MAIN_STAGE_SIZE - DRAM_BASE,
        kind: RegionKind::Ram,
    }
}

/// Link the main stage into its DRAM window, with the handoff page below
/// it reserved.
fn main_stage(
    stages: &StageLayout,
) -> Result<fstart_image_build::linker::ResolvedRamStage, String> {
    let config = BoardConfig {
        name: hstr("ast2500"),
        platform: Platform::Armv5te,
        memory: MemoryMap {
            regions: hvec([main_dram()]),
            flash_layout: None,
            car: None,
        },
        stages: stages.clone(),
        security: default_security_config(),
        payload: None,
        microcode: None,
        data_assets: Default::default(),
        file_assets: Default::default(),
        soc_image_format: SocImageFormat::AspeedSpi,
        full_flash_image: true,
        boot_hart_id: 0,
        build: BoardBuildPolicy::default(),
        acpi: None,
        smbios: None,
        smm: None,
    };
    let window = Span {
        base: MAIN_STAGE_BASE,
        size: MAIN_STAGE_SIZE,
    };
    let handoff = Span {
        base: HANDOFF_ADDR,
        size: fstart_core::handoff::HANDOFF_MAX_SIZE as u64,
    };
    let descriptor = EncodedLayout::encode(
        1,
        &[
            window.region(Kind::Image),
            window.region(Kind::Writable),
            handoff.region(Kind::Reserved),
        ],
    )
    .map_err(|e| e.to_string())?;
    resolved_ram_stage(&config, "main", &descriptor)
}

fn unit(
    name: &str,
    features: &[&str],
    linker_script: String,
    output: UnitOutput,
) -> CompilationUnit {
    CompilationUnit {
        name: name.into(),
        cargo_target: CargoTarget::BoardBinary,
        target: Platform::Armv5te.target_triple().into(),
        features: features.iter().map(|f| (*f).into()).collect(),
        build_std: Some("core,alloc".into()),
        rustflags: vec![
            "-Zub-checks=no".into(),
            "--cfg=portable_atomic_unsafe_assume_single_core".into(),
        ],
        release_only: false,
        linker_script: Some(linker_script),
        environment_values: BTreeMap::new(),
        bindings: vec![],
        file_bindings: vec![],
        board_features: vec![],
        output,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MJ11: BoardFacts = BoardFacts {
        soc: AspeedSoc::Ast2500,
        flash_size: 64 * 1024 * 1024,
        qemu_machine: Some(QemuMachine::Ast2500Evb),
    };

    #[test]
    fn ast2500_bootblock_executes_in_place_and_loads_main_into_dram() {
        let plan = resolve(MJ11, BuildSelection::default()).unwrap();
        let [bootblock, main] = &plan.units[..] else {
            panic!("bootblock and main")
        };
        assert_eq!(bootblock.target, "armv5te-none-eabi");
        let script = bootblock.linker_script.as_deref().unwrap();
        assert!(script.contains("KEEP(*(.head.aspeed))"));
        assert!(script.contains("ROM (rx) : ORIGIN = 0x0, LENGTH = 0x1000000"));
        assert!(script.contains("RAM (rw) : ORIGIN = 0x1e720000"));
        assert_eq!(main.features, ["bundle-ast2500-main"]);
        let UnitOutput::Executable {
            load_address,
            flat_capacity,
            ..
        } = main.output
        else {
            panic!("main is an executable")
        };
        assert_eq!(
            (load_address, flat_capacity),
            (MAIN_STAGE_BASE, MAIN_STAGE_SIZE)
        );
        assert!(HANDOFF_ADDR + fstart_core::handoff::HANDOFF_MAX_SIZE as u64 <= MAIN_STAGE_BASE);
    }

    #[test]
    fn elf_payloads_come_from_the_kernel_input() {
        let selection = BuildSelection {
            payload: Some("elf".into()),
            ..BuildSelection::default()
        };
        let plan = resolve(MJ11, selection).unwrap();
        assert_eq!(plan.units[1].features, ["bundle-ast2500-main", "elf"]);
        assert_eq!(plan.inputs[0].name, "kernel");
        let linux = BuildSelection {
            payload: Some("linux".into()),
            ..BuildSelection::default()
        };
        assert!(resolve(MJ11, linux).is_err());
    }

    #[test]
    fn rejects_flash_smaller_than_the_firmware_window() {
        let small = BoardFacts {
            flash_size: 8 * 1024 * 1024,
            ..MJ11
        };
        assert!(resolve(small, BuildSelection::default()).is_err());
    }
}
