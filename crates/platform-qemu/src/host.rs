//! AArch64 virt's concrete host plan. Hardware flow and reset copier are unchanged.
extern crate std;
use crate::facts::{Aarch64BoardFacts, Aarch64ImageFacts};
use fstart_core::*;
use fstart_image_build::{
    build_plan::{Assembly, BuildPlan, CargoTarget, CompilationUnit, InputFile, UnitOutput},
    layout::EncodedLayout,
    linker::Xip,
    plan::{BuildSelection, Span},
};
use std::{
    collections::BTreeMap,
    string::{String, ToString},
    vec,
    vec::Vec,
};

fn compiler_cfg_schema() -> fstart_image_build::build_plan::CompilerCfgSchema {
    let strings = |values: &[&str]| values.iter().map(|v| (*v).into()).collect();
    fstart_image_build::build_plan::CompilerCfgSchema {
        entries: strings(&["riscv64", "armv7", "aarch64-relocate", "x86_64"]),
        environments: strings(&["monolithic", "car", "postcar", "ram", "smm"]),
        payloads: strings(&["halt", "linux", "crabefi"]),
    }
}

pub struct Plan<B>(core::marker::PhantomData<B>);
impl<B: Aarch64BoardFacts> Plan<B> {
    pub fn emit(selection_json: &str) {
        let selection = serde_json::from_str(selection_json).expect("invalid build selection");
        let plan = resolve(B::IMAGE, selection).expect("invalid AArch64 image facts");
        std::println!(
            "{}",
            serde_json::to_string(&plan).expect("serialize AArch64 plan")
        );
    }
}

pub fn resolve(facts: Aarch64ImageFacts, selection: BuildSelection) -> Result<BuildPlan, String> {
    use fstart_core::layout::RegionKind as Kind;
    let facts = Aarch64ImageFacts::new(facts.flash_capacity);
    let payload = selection.payload.unwrap_or_else(|| "linux".into());
    if !matches!(payload.as_str(), "halt" | "linux" | "uefi") {
        return Err("AArch64 supports halt, Linux and UEFI".into());
    }
    let span = |base, size| Span { base, size };
    let flash = span(0, facts.flash_capacity);
    let image = span(0, 0x0400_0000);
    let firmware = span(0x0400_0000, 0x0400_0000);
    let execution = span(0x4040_0000, 0x0040_0000);
    let writable = span(0x4080_0000, 0x0080_0000);
    let stack = span(0x40d0_0000, 0x0030_0000);
    let heap = span(0x40c0_0000, 0x0010_0000);
    let kernel = (payload == "linux").then_some(span(0x4100_0000, 0x0400_0000));
    let runtime = (payload != "halt").then_some(span(0x0e09_0000, 0x0020_0000));
    let dtb = (payload != "halt").then_some(span(0x4010_0000, 0x0001_0000));
    let mut regions = vec![
        image.region(Kind::Image),
        writable.region(Kind::Writable),
        stack.region(Kind::Stack),
        heap.region(Kind::Heap),
        flash.region(Kind::Flash),
        firmware.region(Kind::Firmware),
        execution.region(Kind::Execution),
    ];
    for (kind, range) in [
        (Kind::Payload, kernel),
        (Kind::PayloadFirmware, runtime),
        (Kind::DeviceTree, dtb),
    ] {
        if let Some(range) = range {
            regions.push(range.region(kind));
        }
    }
    let layout = Xip {
        platform: Platform::Aarch64,
        boot_hart_id: 0,
        image,
        execution: Some(execution),
        writable,
        heap,
        stack,
        descriptor: EncodedLayout::encode(0, &regions).map_err(|e| e.to_string())?,
    };
    let mut features = vec!["bundle-aarch64".into()];
    if payload != "halt" {
        features.push(if payload == "linux" {
            "linux".into()
        } else {
            "crabefi".into()
        });
    }
    let security = crate::qemu_virt_security_config("keys/dev-signing.pub");
    let assembly = Assembly {
        platform: Platform::Aarch64,
        memory: vec![
            MemoryRegion {
                name: hstr("flash"),
                base: flash.base,
                size: flash.size,
                kind: RegionKind::Rom,
            },
            MemoryRegion {
                name: hstr("stage-reservation"),
                base: writable.base,
                size: writable.size,
                kind: RegionKind::Ram,
            },
        ],
        ifd: None,
        bootstrap: vec![],
        stages: StageLayout::Monolithic(MonolithicConfig {
            build: StageBuildConfig {
                firmware_image: Some(FirmwareImageConfig {
                    temp_ram_buffer: None,
                }),
                verify_firmware: true,
                payload: true,
                fdt: true,
                pci: true,
                ..Default::default()
            },
            load_addr: execution.base,
            data_addr: Some(writable.base),
            stack_size: stack.size as u32,
            heap_size: Some(heap.size as u32),
            page_table_addr: None,
            page_size: Default::default(),
        }),
        security,
        payload: runtime.map(|runtime| PayloadConfig {
            kind: if payload == "linux" {
                PayloadKind::LinuxBoot
            } else {
                PayloadKind::UefiPayload
            },
            kernel_file: kernel.map(|_| hstr("Image")),
            kernel_load_addr: kernel.map(|r| r.base),
            fdt: FdtSource::Platform,
            dtb_addr: dtb.map(|r| r.base),
            src_dtb_addr: None,
            bootargs: None,
            print_x86_mtrrs: false,
            compression: Compression::Lz4,
            firmware: Some(FirmwareConfig {
                kind: FirmwareKind::ArmTrustedFirmware,
                file: hstr("bl31.bin"),
                load_addr: runtime.base,
            }),
            fit_file: None,
            fit_config: None,
            fit_parse: None,
        }),
        microcode: None,
        full_flash_image: true,
        soc_image_format: SocImageFormat::None,
        boot_hart_id: 0,
        build: BoardBuildPolicy {
            firmware_image: FirmwareImagePolicy::memory_mapped(firmware.base, firmware.size),
            flash_image: Some(FirmwareImagePolicy::memory_mapped(flash.base, flash.size)),
            ..Default::default()
        },
    };
    let inputs = [
        ("kernel", "Image", kernel),
        ("firmware", "bl31.bin", runtime),
    ]
    .into_iter()
    .filter_map(|(name, file, range)| {
        range.map(|r| InputFile {
            name: name.into(),
            default: Some(file.into()),
            capacity: r.size,
        })
    })
    .collect::<Vec<_>>();
    let plan = BuildPlan {
        payload: payload.clone(),
        units: vec![CompilationUnit {
            name: "stage".into(),
            cargo_target: CargoTarget::BoardBinary,
            target: "aarch64-unknown-none".into(),
            entry: "aarch64-relocate".into(),
            cfg_schema: compiler_cfg_schema(),
            environment: "monolithic".into(),
            payload: if payload == "uefi" {
                "crabefi".into()
            } else {
                payload
            },
            features,
            build_std: Some("core,alloc".into()),
            rustflags: vec!["-Zub-checks=no".into()],
            release_only: false,
            linker_script: Some(fstart_image_build::linker::resolved_xip(&layout)?),
            environment_values: BTreeMap::new(),
            bindings: vec![],
            output: UnitOutput::Executable {
                expectations: layout.elf_expectations()?,
                load_address: execution.base,
                flat_capacity: image.size,
                flat_exact_size: false,
            },
        }],
        stages: vec!["stage".into()],
        assembly,
        inputs,
    };
    plan.order()?;
    Ok(plan)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn relocated_single_unit_has_distinct_storage_execution_and_writable_ranges() {
        let plan = resolve(
            Aarch64ImageFacts::new(0x0800_0000),
            BuildSelection {
                payload: Some("halt".into()),
            },
        )
        .unwrap();
        assert_eq!(plan.order().unwrap().len(), 1);
        let UnitOutput::Executable { expectations, .. } = &plan.units[0].output else {
            panic!("not executable")
        };
        assert_eq!(expectations.stored[0].base, 0);
        assert_eq!(
            expectations.copy.as_ref().unwrap().execution.base,
            0x4040_0000
        );
        assert_eq!(expectations.runtime[1].base, 0x4080_0000);
        assert!(plan.units[0].bindings.is_empty());
        assert!(plan.inputs.is_empty());
    }
}
