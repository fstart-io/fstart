//! ASPEED build plans.
//!
//! AST2500: one stage executing SPI flash in place from the reset vector,
//! with its data, stack and heap in the on-chip SRAM. The FFS is packed
//! behind it inside the 16 MiB the boot-time 3-byte address mode reaches;
//! the composite image is padded to the whole flash part.
extern crate std;
use crate::facts::{AspeedBoardFacts, AspeedSoc, BoardFacts};
use fstart_core::layout::RegionKind as Kind;
use fstart_core::*;
use fstart_image_build::{
    build_plan::{BuildPlan, CargoTarget, CompilationUnit, FfsAssembly as Assembly, UnitOutput},
    layout::EncodedLayout,
    linker::Xip,
    plan::{BuildSelection, Span},
};
use std::{
    collections::BTreeMap,
    string::{String, ToString},
    vec,
};

/// CE0 is mapped at the reset vector.
pub const FLASH_BASE: u64 = 0;
/// What 3-byte SPI addressing reaches after reset.
pub const FIRMWARE_WINDOW_SIZE: u64 = 16 * 1024 * 1024;
/// AST2500 on-chip SRAM: 36 KiB at 0x1E72_0000.
pub const AST2500_SRAM: Span = Span {
    base: 0x1E72_0000,
    size: 0x9000,
};
pub const STACK_SIZE: u64 = 0x1000;
pub const HEAP_SIZE: u64 = 0x1000;

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
    if payload != "halt" {
        return Err("the AST2500 bootblock has no payload stage yet; use halt".into());
    }
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
        ],
        flash: None,
        bootstrap: vec![],
        stages: StageLayout::Monolithic(MonolithicConfig {
            external: false,
            build: StageBuildConfig::default(),
            load_addr: image.base,
            data_addr: Some(writable.base),
            stack_size: stack.size as u32,
            heap_size: Some(heap.size as u32),
        }),
        security: default_security_config(),
        payload: None,
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
    let plan = BuildPlan {
        payload,
        units: vec![CompilationUnit {
            name: "bootblock".into(),
            cargo_target: CargoTarget::BoardBinary,
            target: Platform::Armv5te.target_triple().into(),
            features: vec!["bundle-ast2500-bootblock".into()],
            build_std: Some("core,alloc".into()),
            rustflags: vec![
                "-Zub-checks=no".into(),
                "--cfg=portable_atomic_unsafe_assume_single_core".into(),
            ],
            release_only: false,
            linker_script: Some(fstart_image_build::linker::resolved_xip(&layout)?),
            environment_values: BTreeMap::new(),
            bindings: vec![],
            file_bindings: vec![],
            board_features: vec![],
            output: UnitOutput::Executable {
                expectations: layout.elf_expectations()?,
                load_address: image.base,
                flat_capacity: image.size,
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

    const MJ11: BoardFacts = BoardFacts {
        soc: AspeedSoc::Ast2500,
        flash_size: 64 * 1024 * 1024,
        qemu_machine: Some(QemuMachine::Ast2500Evb),
    };

    #[test]
    fn ast2500_bootblock_executes_in_place_with_the_head() {
        let plan = resolve(MJ11, BuildSelection::default()).unwrap();
        let [unit] = &plan.units[..] else {
            panic!("one stage")
        };
        assert_eq!(unit.target, "armv5te-none-eabi");
        let script = unit.linker_script.as_deref().unwrap();
        assert!(script.contains("KEEP(*(.head.aspeed))"));
        assert!(script.contains("ROM (rx) : ORIGIN = 0x0, LENGTH = 0x1000000"));
        assert!(script.contains("RAM (rw) : ORIGIN = 0x1e720000"));
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
