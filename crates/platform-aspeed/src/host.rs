//! ASPEED build plans.
//!
//! AST2400, AST1250 and AST2500: the bootblock executes SPI flash in place
//! from the reset vector,
//! with its data, stack and heap in the on-chip SRAM. Once DRAM is up it
//! loads the main stage into DRAM, which boots the payload. The FFS is
//! packed behind the bootblock inside the 16 MiB the boot-time 3-byte
//! address mode reaches; the composite image is padded to the whole flash
//! part.
extern crate std;
use crate::facts::{AspeedBoardFacts, AspeedEmulator, AspeedSoc, BoardFacts};
use crate::layout::{FIRMWARE_WINDOW_SIZE, FLASH_BASE, Layout};
use fstart_core::layout::RegionKind as Kind;
use fstart_core::*;
use fstart_image_build::{
    build_plan::{
        BootImageFormat, BootstrapRole, BuildPlan, CargoTarget, CompilationUnit,
        FfsAssembly as Assembly, ImageStep, InputFile, UnitOutput,
    },
    layout::EncodedLayout,
    linker::{Xip, resolved_ram_stage},
    plan::{BuildSelection, HostPlan, NoOptions, PlanError, ResolvedPlan, Span},
    run::{RunArtifact, RunMedia, RunRecipe},
};
use std::{
    collections::BTreeMap,
    format,
    string::{String, ToString},
    vec,
    vec::Vec,
};

/// On-chip SRAM below the boot timestamp table.
const fn stage_sram(layout: &Layout) -> Span {
    Span {
        base: layout.sram_base,
        size: layout.timestamps() - layout.sram_base,
    }
}
/// The bootblock authenticates the FFS root (Ed25519) on this stack.
pub const STACK_SIZE: u64 = 0x4000;
pub const HEAP_SIZE: u64 = 0x1000;
pub const MAIN_STACK_SIZE: u32 = 0x1_0000;
pub const MAIN_HEAP_SIZE: u32 = 0x4_0000;

pub struct Plan<B>(core::marker::PhantomData<B>);
impl<B: AspeedBoardFacts> HostPlan<B> for Plan<B> {
    type Options = NoOptions;

    fn resolve(selection: BuildSelection, _: NoOptions) -> Result<ResolvedPlan, PlanError> {
        resolve(B::FACTS, selection)
    }
}

impl<B: AspeedBoardFacts> fstart_image_build::acpi::HostAcpi<B> for Plan<B> {
    fn acpi_baselines(_base: u64) -> Result<fstart_image_build::acpi::AcpiOutcome, String> {
        Ok(fstart_image_build::acpi::AcpiOutcome::NotApplicable {
            reason: "BMC platform".into(),
        })
    }
}

pub fn resolve(facts: BoardFacts, selection: BuildSelection) -> Result<BuildPlan, PlanError> {
    selection.reject_debug()?;
    let payload = selection.payload.unwrap_or_else(|| "halt".into());
    let elf = match payload.as_str() {
        "halt" => false,
        "elf" => true,
        _ => {
            return Err(PlanError::unsupported_payload(
                "the ASPEED main stage boots halt or a bare ELF",
            ));
        }
    };
    let bundle = match facts.soc {
        AspeedSoc::Ast2400 | AspeedSoc::Ast1250 => "ast2400",
        AspeedSoc::Ast2500 => "ast2500",
        AspeedSoc::Ast2600 => return Err("the AST2600 is not supported yet".into()),
    };
    let soc = facts.soc.layout();
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
    let writable = stage_sram(soc);
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
        boot_image_format: BootImageFormat::AspeedSpi,
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
            load_addr: soc.main_stage_base,
            stack_size: MAIN_STACK_SIZE,
            heap_size: Some(MAIN_HEAP_SIZE),
            runs_from: RunsFrom::Ram,
            compression: fstart_core::ffs::Compression::None,
            data_addr: None,
        },
    ]));
    let main = main_stage(bundle, soc, &stages)?;
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
            main_dram(soc),
        ],
        flash: None,
        bootstrap: vec![("main".into(), BootstrapRole::Mainstage)],
        stages,
        security: default_security_config(),
        payload: elf.then(|| fstart_core::elf_payload(fstart_core::ffs::Compression::None)),
        microcode: None,
        data_assets: vec![],
        file_assets: vec![],
        smm: None,
        full_flash_image: true,
        boot_image_format: BootImageFormat::AspeedSpi,
        boot_hart_id: 0,
        build: BoardBuildPolicy {
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
    let bootblock_bundle = format!("bundle-{bundle}-bootblock");
    let main_bundle = format!("bundle-{bundle}-main");
    let plan = BuildPlan {
        options: Default::default(),
        payload,
        units: vec![
            unit(
                "bootblock",
                &[bootblock_bundle.as_str()],
                fstart_image_build::linker::resolved_xip(&layout)?,
                UnitOutput::Executable {
                    expectations: layout.elf_expectations()?,
                    load_address: image.base,
                    flat_capacity: image.size,
                },
            ),
            unit(
                "main",
                &[main_bundle.as_str(), "payload-elf"][..1 + usize::from(elf)],
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
        run: facts
            .emulator
            .map(|emulator| run(emulator, &facts))
            .transpose()?,
        inputs,
    };
    plan.order()?;
    Ok(plan)
}

/// DRAM up to the end of the main stage: what every accepted DRAM
/// configuration provides.
fn main_dram(soc: &Layout) -> MemoryRegion {
    MemoryRegion {
        name: hstr("dram"),
        base: soc.dram_base,
        size: soc.main_stage_base + soc.main_stage_size - soc.dram_base,
        kind: RegionKind::Ram,
    }
}

/// Link the main stage into its DRAM window, with the handoff page below
/// it reserved.
fn main_stage(
    name: &str,
    soc: &Layout,
    stages: &StageLayout,
) -> Result<fstart_image_build::linker::ResolvedRamStage, String> {
    let config = BoardConfig {
        name: hstr(name),
        platform: Platform::Armv5te,
        memory: MemoryMap {
            regions: hvec([main_dram(soc)]),
            flash_layout: None,
            car: None,
        },
        stages: stages.clone(),
        security: default_security_config(),
        payload: None,
        microcode: None,
        data_assets: Default::default(),
        file_assets: Default::default(),
        full_flash_image: true,
        boot_hart_id: 0,
        build: BoardBuildPolicy::default(),
        acpi: None,
        smbios: None,
        smm: None,
    };
    let window = Span {
        base: soc.main_stage_base,
        size: soc.main_stage_size,
    };
    let handoff = Span {
        base: soc.handoff_addr,
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
    resolved_ram_stage(&config, "main", &descriptor, BootImageFormat::AspeedSpi)
}

/// How `emulator` runs the image.
fn run(emulator: AspeedEmulator, facts: &BoardFacts) -> Result<RunRecipe, String> {
    // The machines execute the composite SPI image in place, with a flash
    // part of the board's CE0 size as their FMC model.
    let machine = match emulator {
        AspeedEmulator::PalmettoBmc => "palmetto-bmc",
        AspeedEmulator::Ast2500Evb => "ast2500-evb",
        AspeedEmulator::Ast2600Evb => return Err("no run recipe for ast2600-evb yet".into()),
    };
    let model = match facts.flash_size {
        0x200_0000 => "n25q256a",
        0x400_0000 => "mt25ql512ab",
        size => {
            return Err(format!(
                "no QEMU {machine} flash model for a {size:#x}-byte CE0 flash"
            ));
        }
    };
    Ok(RunRecipe {
        qemu: "qemu-system-arm".into(),
        machine: format!("{machine},execute-in-place=on,fmc-model={model}"),
        cpu: None,
        accel_kvm: false,
        memory: None,
        smp: None,
        nographic: true,
        media: vec![RunMedia::Mtd {
            artifact: RunArtifact::Step(ImageStep::FFS.into()),
        }],
        devices: vec![],
        args: vec!["-no-reboot".into()],
        disk: None,
        tpm: None,
        tests: vec![],
    })
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
            "--cfg".into(),
            "sha1_backend=\"soft\"".into(),
            "--cfg".into(),
            "sha2_backend=\"soft\"".into(),
            "--cfg".into(),
            "sha2_backend_soft=\"compact\"".into(),
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
        emulator: Some(AspeedEmulator::Ast2500Evb),
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
        let soc = crate::layout::AST2500;
        assert_eq!(
            (load_address, flat_capacity),
            (soc.main_stage_base, soc.main_stage_size)
        );
        assert!(
            soc.handoff_addr + fstart_core::handoff::HANDOFF_MAX_SIZE as u64 <= soc.main_stage_base
        );
    }

    #[test]
    fn elf_payloads_come_from_the_kernel_input() {
        let selection = BuildSelection {
            payload: Some("elf".into()),
            ..BuildSelection::default()
        };
        let plan = resolve(MJ11, selection).unwrap();
        assert_eq!(
            plan.units[1].features,
            ["bundle-ast2500-main", "payload-elf"]
        );
        assert_eq!(plan.inputs[0].name, "kernel");
        let linux = BuildSelection {
            payload: Some("linux".into()),
            ..BuildSelection::default()
        };
        assert!(resolve(MJ11, linux).is_err());
    }

    #[test]
    fn the_ast2600_fails_the_plan() {
        let facts = BoardFacts {
            soc: AspeedSoc::Ast2600,
            emulator: None,
            ..MJ11
        };
        assert!(resolve(facts, BuildSelection::default()).is_err());
    }

    #[test]
    fn ast2400_generation_stages_use_its_dram_and_sram() {
        for soc in [AspeedSoc::Ast2400, AspeedSoc::Ast1250] {
            let facts = BoardFacts {
                soc,
                flash_size: 32 * 1024 * 1024,
                emulator: Some(AspeedEmulator::PalmettoBmc),
            };
            let plan = resolve(facts, BuildSelection::default()).unwrap();
            let [bootblock, main] = &plan.units[..] else {
                panic!("bootblock and main")
            };
            assert_eq!(bootblock.features, ["bundle-ast2400-bootblock"]);
            assert_eq!(main.features, ["bundle-ast2400-main"]);
            let script = bootblock.linker_script.as_deref().unwrap();
            assert!(script.contains("RAM (rw) : ORIGIN = 0x1e720000"));
            let UnitOutput::Executable { load_address, .. } = main.output else {
                panic!("main is an executable")
            };
            assert_eq!(load_address, 0x4200_0000);
            let run = plan.run.unwrap();
            assert_eq!(
                run.machine,
                "palmetto-bmc,execute-in-place=on,fmc-model=n25q256a"
            );
        }
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
