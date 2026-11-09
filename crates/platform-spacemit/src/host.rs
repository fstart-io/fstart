//! SpacemiT build plans.
//!
//! K3: one stage in on-chip SRAM, wrapped in the boot ROM's FSBL container.
//! The ROM loads the container to `0xC080_0000` and enters the stage behind
//! its 4 KiB head; the stage owns the SRAM up to its top, where the ROM kept
//! its own data while loading. Halt is the only payload until DRAM is
//! trained.
extern crate std;
use crate::facts::{BoardFacts, SpacemitBoardFacts, SpacemitSoc};
use fstart_core::layout::RegionKind as LayoutKind;
use fstart_core::{
    BoardBuildPolicy, BoardConfig, Compression, MemoryMap, MemoryRegion, Platform, RegionKind,
    RunsFrom, SocImageFormat, StageBuildConfig, StageConfig, StageLayout, default_security_config,
    hstr, hvec,
};
use fstart_image_build::{
    build_plan::{BuildPlan, CargoTarget, CompilationUnit, FfsAssembly as Assembly, UnitOutput},
    image::spacemit::{FSBL_CODE_OFFSET, K3_FSBL_LOAD_ADDR},
    layout::EncodedLayout,
    linker::resolved_ram_stage,
    plan::{BuildSelection, Span},
};
use std::{
    collections::BTreeMap,
    string::{String, ToString},
    vec,
};

/// K3 on-chip SRAM the first stage owns: from its entry behind the FSBL
/// head to the top of the 512 KiB.
pub const K3_STAGE_SRAM: Span = Span {
    base: K3_FSBL_LOAD_ADDR + FSBL_CODE_OFFSET as u64,
    size: 0xC088_0000 - (K3_FSBL_LOAD_ADDR + FSBL_CODE_OFFSET as u64),
};
pub const BOOTBLOCK_STACK_SIZE: u32 = 0x4000;

/// Conventional platform export consumed by the generic fbuild runner.
pub struct Plan<B>(core::marker::PhantomData<B>);
impl<B: SpacemitBoardFacts> Plan<B> {
    pub fn emit(selection_json: &str) {
        let selection = serde_json::from_str(selection_json).expect("invalid build selection");
        let plan = resolve(B::FACTS, selection).expect("invalid SpacemiT board plan");
        std::println!(
            "{}",
            serde_json::to_string(&plan).expect("serialize SpacemiT plan")
        );
    }
}

fn sram_region(sram: Span) -> MemoryRegion {
    MemoryRegion {
        name: hstr("sram"),
        base: sram.base,
        size: sram.size,
        kind: RegionKind::Ram,
    }
}

fn bootblock_stage(sram: Span) -> StageConfig {
    StageConfig {
        name: hstr("bootblock"),
        build: StageBuildConfig::default(),
        load_addr: sram.base,
        stack_size: BOOTBLOCK_STACK_SIZE,
        heap_size: None,
        runs_from: RunsFrom::Ram,
        compression: Compression::None,
        data_addr: None,
    }
}

fn board_config(sram: Span) -> BoardConfig {
    BoardConfig {
        name: hstr("spacemit-k3"),
        platform: Platform::Riscv64,
        memory: MemoryMap {
            regions: hvec([sram_region(sram)]),
            flash_layout: None,
            car: None,
        },
        stages: StageLayout::MultiStage(hvec([bootblock_stage(sram)])),
        security: default_security_config(),
        payload: None,
        microcode: None,
        data_assets: Default::default(),
        file_assets: Default::default(),
        soc_image_format: SocImageFormat::SpacemitK3Fsbl,
        full_flash_image: false,
        build: BoardBuildPolicy::default(),
        acpi: None,
        smbios: None,
        smm: None,
        boot_hart_id: 0,
    }
}

pub fn resolve(facts: BoardFacts, selection: BuildSelection) -> Result<BuildPlan, String> {
    selection.reject_debug()?;
    if selection.has_x86_linux_overrides() {
        return Err("x86 Linux options are supported only by Intel platforms".into());
    }
    let payload = selection.payload.unwrap_or_else(|| "halt".into());
    if payload != "halt" {
        return Err("the K3 bootblock has no DRAM stage yet; use halt".into());
    }
    let SpacemitSoc::K3 = facts.soc;
    let sram = K3_STAGE_SRAM;
    let config = board_config(sram);
    let descriptor = EncodedLayout::encode(
        0,
        &[
            sram.region(LayoutKind::Image),
            sram.region(LayoutKind::Writable),
        ],
    )
    .map_err(|e| e.to_string())?;
    let bootblock = resolved_ram_stage(&config, "bootblock", &descriptor)?;

    let assembly = Assembly {
        platform: Platform::Riscv64,
        memory: vec![sram_region(sram)],
        flash: None,
        bootstrap: vec![],
        stages: StageLayout::MultiStage(hvec([bootblock_stage(sram)])),
        security: default_security_config(),
        payload: None,
        microcode: None,
        data_assets: vec![],
        file_assets: vec![],
        full_flash_image: false,
        soc_image_format: SocImageFormat::SpacemitK3Fsbl,
        boot_hart_id: 0,
        build: BoardBuildPolicy::default(),
    };
    let plan = BuildPlan {
        payload,
        units: vec![CompilationUnit {
            name: "bootblock".into(),
            cargo_target: CargoTarget::BoardBinary,
            target: Platform::Riscv64.target_triple().into(),
            features: vec!["bundle-k3-bootblock".into()],
            build_std: Some("core,alloc".into()),
            rustflags: vec!["-Zub-checks=no".into()],
            release_only: false,
            linker_script: Some(bootblock.linker_script),
            environment_values: BTreeMap::new(),
            bindings: vec![],
            file_bindings: vec![],
            board_features: vec![],
            output: UnitOutput::Executable {
                expectations: bootblock.expectations,
                load_address: bootblock.memory.base,
                flat_capacity: bootblock.memory.size,
            },
        }],
        stages: vec!["bootblock".into()],
        assembly: assembly.into(),
        inputs: vec![],
    };
    plan.order()?;
    Ok(plan)
}

#[cfg(test)]
mod tests {
    use super::*;

    const KIT: BoardFacts = BoardFacts {
        soc: SpacemitSoc::K3,
    };

    #[test]
    fn k3_bootblock_runs_from_sram_behind_the_fsbl_head() {
        let plan = resolve(KIT, BuildSelection::default()).unwrap();
        let [unit] = &plan.units[..] else {
            panic!("one stage")
        };
        assert_eq!(unit.target, "riscv64gc-unknown-none-elf");
        assert_eq!(unit.features, ["bundle-k3-bootblock"]);
        let UnitOutput::Executable {
            load_address,
            flat_capacity,
            ..
        } = &unit.output
        else {
            panic!("bootblock is not executable")
        };
        assert_eq!(
            (*load_address, *flat_capacity),
            (0xC080_1000, 0xC088_0000 - 0xC080_1000)
        );
        let assembly = plan.assembly.ffs().unwrap();
        assert_eq!(assembly.soc_image_format, SocImageFormat::SpacemitK3Fsbl);
        assert!(assembly.payload.is_none());
    }

    #[test]
    fn rejects_payloads_without_a_dram_stage() {
        let linux = BuildSelection {
            payload: Some("linux".into()),
            ..Default::default()
        };
        assert!(resolve(KIT, linux).unwrap_err().contains("halt"));
    }
}
