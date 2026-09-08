//! Concrete QEMU virt plans. Machine policy composes one shared image projection.
//! Hardware flows and the AArch64 reset copier are unchanged.
extern crate std;
use crate::facts::{VirtBoardFacts, VirtMachine};
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
        entries: strings(&["riscv64", "armv7", "aarch64-relocate"]),
        environments: strings(&["monolithic"]),
        payloads: strings(&["halt", "linux", "crabefi"]),
    }
}

pub struct Plan<B>(core::marker::PhantomData<B>);
impl<B: VirtBoardFacts> Plan<B> {
    pub fn emit(selection_json: &str) {
        let selection = serde_json::from_str(selection_json).expect("invalid build selection");
        let plan = resolve(B::MACHINE, selection).expect("invalid QEMU virt image policy");
        std::println!(
            "{}",
            serde_json::to_string(&plan).expect("serialize QEMU virt plan")
        );
    }
}

/// Platform-owned presets, not a board-authored lifecycle or geometry schema.
/// All supported virt machines split their fixed flash window equally between
/// initialized stage storage and the firmware filesystem. Relocation changes
/// execution, not that storage convention.
struct MachinePolicy {
    platform: Platform,
    target: &'static str,
    entry: &'static str,
    bundle: &'static str,
    flash: Span,
    execution: Option<Span>,
    writable: Span,
    stack_size: u64,
    heap_size: u64,
    kernel: Span,
    kernel_file: &'static str,
    runtime: Option<(Span, FirmwareKind, &'static str)>,
    dtb: Span,
}

const fn span(base: u64, size: u64) -> Span {
    Span { base, size }
}

impl VirtMachine {
    fn policy(self, payload: &str) -> Result<MachinePolicy, String> {
        if !matches!(payload, "halt" | "linux" | "uefi") {
            return Err("QEMU virt supports halt, Linux and (where available) UEFI".into());
        }
        Ok(match self {
            Self::Riscv64 => MachinePolicy {
                platform: Platform::Riscv64,
                target: "riscv64gc-unknown-none-elf",
                entry: "riscv64",
                bundle: "bundle-riscv64",
                flash: span(0x2000_0000, 0x0200_0000),
                execution: None,
                writable: span(0x8100_0000, 0x0040_0000),
                stack_size: 0x0010_0000,
                heap_size: 0x0004_0000,
                kernel: span(0x8200_0000, 0x0400_0000),
                kernel_file: "Image",
                runtime: Some((
                    span(0x8010_0000, 0x0020_0000),
                    FirmwareKind::OpenSbi,
                    "fw_dynamic.bin",
                )),
                dtb: span(
                    if payload == "uefi" {
                        0x80f0_0000
                    } else {
                        0x87f0_0000
                    },
                    0x0001_0000,
                ),
            },
            Self::Armv7 => {
                if payload == "uefi" {
                    return Err("ARMv7 virt does not support UEFI".into());
                }
                MachinePolicy {
                    platform: Platform::Armv7,
                    target: "armv7a-none-eabi",
                    entry: "armv7",
                    bundle: "bundle-armv7",
                    flash: span(0, 0x0800_0000),
                    execution: None,
                    writable: span(0x4020_0000, 0x0010_0000),
                    stack_size: 0x0004_0000,
                    heap_size: 0x0004_0000,
                    kernel: span(0x4100_0000, 0x0400_0000),
                    kernel_file: "zImage",
                    runtime: None,
                    dtb: span(0x40f0_0000, 0x0001_0000),
                }
            }
            Self::Aarch64 => MachinePolicy {
                platform: Platform::Aarch64,
                target: "aarch64-unknown-none",
                entry: "aarch64-relocate",
                bundle: "bundle-aarch64",
                flash: span(0, 0x0800_0000),
                execution: Some(span(0x4040_0000, 0x0040_0000)),
                writable: span(0x4080_0000, 0x0080_0000),
                stack_size: 0x0030_0000,
                heap_size: 0x0010_0000,
                kernel: span(0x4100_0000, 0x0400_0000),
                kernel_file: "Image",
                runtime: Some((
                    span(0x0e09_0000, 0x0020_0000),
                    FirmwareKind::ArmTrustedFirmware,
                    "bl31.bin",
                )),
                dtb: span(0x4010_0000, 0x0001_0000),
            },
        })
    }
}

pub fn resolve(machine: VirtMachine, selection: BuildSelection) -> Result<BuildPlan, String> {
    use fstart_core::layout::RegionKind as Kind;
    let payload = selection.payload.unwrap_or_else(|| "linux".into());
    let policy = machine.policy(&payload)?;
    let flash = policy.flash;
    let image = span(flash.base, flash.size / 2);
    let firmware = span(image.end()?, flash.size / 2);
    let execution = policy.execution;
    let code = execution.unwrap_or(image);
    let writable = policy.writable;
    let stack = span(
        writable
            .end()?
            .checked_sub(policy.stack_size)
            .ok_or("stack capacity")?,
        policy.stack_size,
    );
    let heap = span(
        stack
            .base
            .checked_sub(policy.heap_size)
            .ok_or("heap capacity")?,
        policy.heap_size,
    );
    let kernel = (payload == "linux").then_some(policy.kernel);
    let runtime = policy.runtime.filter(|_| payload != "halt");
    let dtb = (payload != "halt").then_some(policy.dtb);
    let mut regions = vec![
        image.region(Kind::Image),
        writable.region(Kind::Writable),
        stack.region(Kind::Stack),
        heap.region(Kind::Heap),
        flash.region(Kind::Flash),
        firmware.region(Kind::Firmware),
    ];
    for (kind, range) in [
        (Kind::Execution, execution),
        (Kind::Payload, kernel),
        (Kind::PayloadFirmware, runtime.map(|r| r.0)),
        (Kind::DeviceTree, dtb),
    ] {
        if let Some(range) = range {
            regions.push(range.region(kind));
        }
    }
    let layout = Xip {
        platform: policy.platform,
        boot_hart_id: 0,
        image,
        execution,
        writable,
        heap,
        stack,
        descriptor: EncodedLayout::encode(0, &regions).map_err(|e| e.to_string())?,
    };
    let mut features = vec![policy.bundle.into()];
    if payload != "halt" {
        features.push(if payload == "linux" {
            "linux".into()
        } else {
            "crabefi".into()
        });
    }
    let security = crate::qemu_virt_security_config("keys/dev-signing.pub");
    let assembly = Assembly {
        platform: policy.platform,
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
            load_addr: code.base,
            data_addr: Some(writable.base),
            stack_size: stack.size as u32,
            heap_size: Some(heap.size as u32),
            page_table_addr: None,
            page_size: Default::default(),
        }),
        security,
        payload: (payload != "halt").then(|| PayloadConfig {
            kind: if payload == "linux" {
                PayloadKind::LinuxBoot
            } else {
                PayloadKind::UefiPayload
            },
            kernel_file: kernel.map(|_| hstr(policy.kernel_file)),
            kernel_load_addr: kernel.map(|r| r.base),
            fdt: FdtSource::Platform,
            dtb_addr: dtb.map(|r| r.base),
            src_dtb_addr: None,
            bootargs: None,
            print_x86_mtrrs: false,
            compression: Compression::Lz4,
            firmware: runtime.map(|(range, kind, file)| FirmwareConfig {
                kind,
                file: hstr(file),
                load_addr: range.base,
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
        ("kernel", policy.kernel_file, kernel),
        (
            "firmware",
            runtime.map_or("", |r| r.2),
            runtime.map(|r| r.0),
        ),
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
            target: policy.target.into(),
            entry: policy.entry.into(),
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
                load_address: code.base,
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
    fn xip_presets_keep_payload_ranges_and_direct_arm_linux_contract() {
        use fstart_core::layout::{Layout, RegionKind as Kind};
        for (machine, flash, writable, stack, heap, dtb) in [
            (
                VirtMachine::Riscv64,
                0x2000_0000,
                0x8100_0000,
                0x0010_0000,
                0x0004_0000,
                0x87f0_0000,
            ),
            (
                VirtMachine::Armv7,
                0,
                0x4020_0000,
                0x0004_0000,
                0x0004_0000,
                0x40f0_0000,
            ),
        ] {
            for payload in ["halt", "linux", "uefi"] {
                let resolved = resolve(
                    machine,
                    BuildSelection {
                        payload: Some(payload.into()),
                    },
                );
                if matches!(machine, VirtMachine::Armv7) && payload == "uefi" {
                    assert!(resolved.unwrap_err().contains("does not support UEFI"));
                    continue;
                }
                let plan = resolved.unwrap();
                let unit = &plan.units[0];
                let UnitOutput::Executable {
                    expectations,
                    load_address,
                    ..
                } = &unit.output
                else {
                    panic!("not executable")
                };
                assert!(expectations.copy.is_none());
                assert_eq!(*load_address, flash);
                assert_eq!(expectations.runtime[1].base, writable);
                let layout = Layout::parse(&expectations.descriptor.bytes).unwrap();
                assert_eq!(layout.region(Kind::Stack).unwrap().size, stack);
                assert_eq!(layout.region(Kind::Heap).unwrap().size, heap);
                assert_eq!(
                    unit.features[0],
                    if matches!(machine, VirtMachine::Armv7) {
                        "bundle-armv7"
                    } else {
                        "bundle-riscv64"
                    }
                );
                if payload == "halt" {
                    assert!(plan.inputs.is_empty());
                    assert!(plan.assembly.payload.is_none());
                    assert!(layout.region(Kind::DeviceTree).is_none());
                    continue;
                }
                assert_eq!(
                    layout.region(Kind::DeviceTree).unwrap().base,
                    if payload == "uefi" { 0x80f0_0000 } else { dtb }
                );
                assert_eq!(layout.region(Kind::DeviceTree).unwrap().size, 0x10000);
                assert_eq!(layout.region(Kind::Payload).is_some(), payload == "linux");
                let external_runtime = matches!(machine, VirtMachine::Riscv64);
                assert_eq!(
                    layout.region(Kind::PayloadFirmware).is_some(),
                    external_runtime
                );
                assert_eq!(
                    plan.inputs.iter().any(|i| i.name == "firmware"),
                    external_runtime
                );
                assert_eq!(
                    plan.assembly.payload.unwrap().firmware.is_some(),
                    external_runtime
                );
            }
        }
    }

    #[test]
    fn relocated_single_unit_has_distinct_storage_execution_and_writable_ranges() {
        let plan = resolve(
            VirtMachine::Aarch64,
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
