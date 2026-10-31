//! FU740 heapless LIM bootstrap and DRAM runtime, independent of emulators.
extern crate std;
use crate::{Fu740BoardFacts, Fu740ImageFacts};
use fstart_core::*;
use fstart_image_build::{
    build_plan::{
        BuildPlan, CargoTarget, CompilationUnit, CompilerCfgSchema, FfsAssembly, InputFile,
        UnitOutput,
    },
    elf::{Architecture, Descriptor, Expectations},
    layout::EncodedLayout,
    plan::{BuildSelection, Span},
};
use std::{
    collections::BTreeMap,
    string::{String, ToString},
    vec,
};

// Way zero is always cache: only ways 1..15 back the LIM aperture.
const LIM: Span = Span {
    base: 0x0800_0000,
    size: 0x001e_0000,
};
const STACK_SIZE: u64 = 0x4000;
const HEAP_SIZE: u64 = 0x4000;
const MAIN: Span = Span {
    base: 0x8100_0000,
    size: 0x0200_0000,
};
const HANDOFF: Span = Span {
    base: 0x8000_0000,
    size: 0x1000,
};

pub struct Plan<B>(core::marker::PhantomData<B>);
impl<B: Fu740BoardFacts> Plan<B> {
    pub fn emit(selection: &str) {
        let selection = serde_json::from_str(selection).expect("invalid build selection");
        let plan = resolve(B::IMAGE, selection).expect("invalid FU740 image policy");
        std::println!(
            "{}",
            serde_json::to_string(&plan).expect("serialize FU740 plan")
        );
    }
}

pub fn resolve(facts: Fu740ImageFacts, selection: BuildSelection) -> Result<BuildPlan, String> {
    use fstart_core::layout::RegionKind as Kind;
    selection.validate_compiler_files()?;
    if !selection.compiler_files.is_empty() || selection.has_x86_linux_overrides() {
        return Err("FU740 accepts no compiler files or x86 Linux options".into());
    }
    let payload = selection.payload.unwrap_or_else(|| "linux".into());
    if !matches!(payload.as_str(), "halt" | "linux" | "uefi") {
        return Err("FU740 supports halt, linux and uefi".into());
    }
    let active = payload != "halt";
    let linux = payload == "linux";
    let flash = Span {
        base: facts.flash_base,
        size: facts.flash_size,
    };
    let dram = Span {
        base: facts.dram_base,
        size: facts.dram_size,
    };
    flash.end()?;
    dram.end()?;
    let kernel = Span {
        base: facts.kernel_addr,
        size: facts
            .dtb_addr
            .checked_sub(facts.kernel_addr)
            .ok_or("kernel overlaps DTB")?,
    };
    let runtime = Span {
        base: facts.runtime_addr,
        size: facts
            .kernel_addr
            .checked_sub(facts.runtime_addr)
            .ok_or("runtime overlaps kernel")?,
    };
    let dtb = Span {
        base: facts.dtb_addr,
        size: 0x10000,
    };
    for range in [kernel, runtime, dtb] {
        range.end()?;
        if !dram.contains(range.base, range.size) {
            return Err("payload reservation outside DRAM".into());
        }
    }
    let stack = Span {
        base: LIM.end()? - STACK_SIZE,
        size: STACK_SIZE,
    };
    let heap = Span {
        base: stack.base - HEAP_SIZE,
        size: HEAP_SIZE,
    };
    let mut regions = vec![
        LIM.region(Kind::Writable),
        stack.region(Kind::Stack),
        heap.region(Kind::Heap),
        LIM.region(Kind::Image),
        flash.region(Kind::Flash),
        flash.region(Kind::Firmware),
    ];
    if linux {
        regions.push(kernel.region(Kind::Payload));
        regions.push(dtb.region(Kind::DeviceTree));
    }
    if active {
        regions.push(runtime.region(Kind::PayloadFirmware));
    }
    let descriptor = EncodedLayout::encode(0, &regions).map_err(|e| e.to_string())?;
    let linker_script = lim_linker_script(LIM, heap, stack, facts.boot_hart_id, &descriptor)?;
    let expectations = Expectations {
        architecture: Architecture::Riscv64,
        elf64: true,
        little_endian: true,
        stored: vec![LIM],
        runtime: vec![LIM],
        identity_mapping: true,
        entry: Some(LIM.base),
        copy: None,
        descriptor: Some(Descriptor {
            section: ".fstart.layout".into(),
            start_symbol: "_fstart_layout_start".into(),
            end_symbol: "_fstart_layout_end".into(),
            bytes: descriptor.as_bytes().to_vec(),
            reservation: LIM,
        }),
        stored_flags: None,
        symbols: BTreeMap::from([
            ("_stack_bottom".into(), stack.base),
            ("_stack_top".into(), stack.end()?),
            ("_FSTART_HEAP".into(), heap.base),
        ]),
    };
    expectations.validate()?;
    let mut features = vec!["bundle-fu740".into()];
    if active {
        features.push(if linux {
            "linux".into()
        } else {
            "crabefi".into()
        });
    }
    let mut inputs = vec![];
    if linux {
        inputs.push(InputFile {
            name: "kernel".into(),
            default: Some("Image-riscv64".into()),
            capacity: kernel.size,
        });
        inputs.push(InputFile {
            name: "dtb".into(),
            default: Some(facts.dtb_file.into()),
            capacity: dtb.size,
        });
    }
    if active {
        inputs.push(InputFile {
            name: "firmware".into(),
            default: Some("fw_dynamic.bin".into()),
            capacity: runtime.size,
        });
    }
    let assembly = FfsAssembly {
        platform: Platform::Riscv64,
        memory: vec![
            MemoryRegion {
                name: hstr("lim"),
                base: LIM.base,
                size: LIM.size,
                kind: RegionKind::Ram,
            },
            MemoryRegion {
                name: hstr("spi-xip"),
                base: flash.base,
                size: flash.size,
                kind: RegionKind::Rom,
            },
            MemoryRegion {
                name: hstr("dram"),
                base: dram.base,
                size: dram.size,
                kind: RegionKind::Ram,
            },
        ],
        flash: None,
        bootstrap: vec![(
            "main".into(),
            fstart_image_build::build_plan::BootstrapRole::Mainstage,
        )],
        stages: StageLayout::MultiStage(hvec([
            StageConfig {
                name: hstr("bootblock"),
                build: StageBuildConfig {
                    load_next_stage: Some(hstr("main")),
                    ..Default::default()
                },
                load_addr: LIM.base,
                stack_size: STACK_SIZE as u32,
                heap_size: None,
                runs_from: RunsFrom::Ram,
                compression: Compression::None,
                data_addr: None,
                page_table_addr: None,
                page_size: Default::default(),
            },
            StageConfig {
                name: hstr("main"),
                build: StageBuildConfig {
                    verify_firmware: true,
                    payload: true,
                    fdt: true,
                    ..Default::default()
                },
                load_addr: MAIN.base,
                stack_size: 0x10000,
                heap_size: Some(0x100000),
                runs_from: RunsFrom::Ram,
                compression: Compression::None,
                data_addr: None,
                page_table_addr: None,
                page_size: Default::default(),
            },
        ])),
        security: default_security_config(),
        payload: active.then(|| PayloadConfig {
            kind: if linux {
                PayloadKind::LinuxBoot
            } else {
                PayloadKind::UefiPayload
            },
            kernel_file: linux.then(|| hstr("Image-riscv64")),
            kernel_load_addr: linux.then_some(kernel.base),
            x86_zero_page_addr: None,
            fdt: if linux {
                FdtSource::Override(hstr(facts.dtb_file))
            } else {
                FdtSource::Platform
            },
            dtb_addr: linux.then_some(dtb.base),
            src_dtb_addr: None,
            bootargs: linux.then(|| hstr(facts.bootargs)),
            print_x86_mtrrs: false,
            compression: Compression::Lz4,
            firmware: Some(FirmwareConfig {
                kind: FirmwareKind::OpenSbi,
                file: hstr("fw_dynamic.bin"),
                load_addr: runtime.base,
            }),
            fit_file: None,
            fit_config: None,
            fit_parse: None,
        }),
        microcode: None,
        data_assets: vec![],
        file_assets: vec![],
        full_flash_image: false,
        soc_image_format: SocImageFormat::None,
        boot_hart_id: facts.boot_hart_id,
        build: BoardBuildPolicy {
            firmware_image: FirmwareImagePolicy::memory_mapped(flash.base, flash.size),
            ..Default::default()
        },
    };
    let bootblock = CompilationUnit {
        name: "bootblock".into(),
        cargo_target: CargoTarget::BoardBinary,
        target: "riscv64gc-unknown-none-elf".into(),
        entry: "riscv64".into(),
        cfg_schema: CompilerCfgSchema {
            entries: vec!["riscv64".into()],
            environments: vec!["car".into(), "ram".into()],
            payloads: vec!["halt".into(), "linux".into(), "crabefi".into()],
        },
        environment: "car".into(),
        payload: "halt".into(),
        features: vec!["bundle-bootblock".into()],
        build_std: Some("core,alloc".into()),
        rustflags: vec!["-Zub-checks=no".into()],
        release_only: false,
        linker_script: Some(linker_script),
        environment_values: BTreeMap::new(),
        bindings: vec![],
        file_bindings: vec![],
        output: UnitOutput::Executable {
            expectations,
            load_address: LIM.base,
            flat_capacity: LIM.size,
        },
    };
    let main_stack = Span {
        base: MAIN.end()? - 0x10000,
        size: 0x10000,
    };
    let main_heap = Span {
        base: main_stack.base - 0x100000,
        size: 0x100000,
    };
    if !dram.contains(MAIN.base, MAIN.size) {
        return Err("DRAM does not cover mainstage".into());
    }
    let mut main_regions = regions;
    main_regions.retain(|r| {
        !matches!(
            r.kind,
            Kind::Image | Kind::Writable | Kind::Stack | Kind::Heap
        )
    });
    main_regions.extend([
        MAIN.region(Kind::Image),
        MAIN.region(Kind::Writable),
        main_stack.region(Kind::Stack),
        main_heap.region(Kind::Heap),
        HANDOFF.region(Kind::Reserved),
    ]);
    let main_descriptor = EncodedLayout::encode(1, &main_regions).map_err(|e| e.to_string())?;
    let mut main = bootblock.clone();
    main.name = "main".into();
    main.environment = "ram".into();
    main.payload = if payload == "uefi" {
        "crabefi".into()
    } else {
        payload.clone()
    };
    main.features = features;
    main.linker_script = Some(lim_linker_script(
        MAIN,
        main_heap,
        main_stack,
        facts.boot_hart_id,
        &main_descriptor,
    )?);
    let UnitOutput::Executable {
        expectations: main_expectations,
        load_address,
        flat_capacity,
    } = &mut main.output
    else {
        unreachable!()
    };
    main_expectations.stored = vec![MAIN];
    main_expectations.runtime = vec![MAIN];
    main_expectations.entry = Some(MAIN.base);
    main_expectations.descriptor = Some(Descriptor {
        section: ".fstart.layout".into(),
        start_symbol: "_fstart_layout_start".into(),
        end_symbol: "_fstart_layout_end".into(),
        bytes: main_descriptor.as_bytes().to_vec(),
        reservation: MAIN,
    });
    main_expectations.symbols = BTreeMap::from([
        ("_stack_bottom".into(), main_stack.base),
        ("_stack_top".into(), main_stack.end()?),
        ("_FSTART_HEAP".into(), main_heap.base),
    ]);
    *load_address = MAIN.base;
    *flat_capacity = MAIN.size;
    let plan = BuildPlan {
        payload,
        inputs,
        stages: vec!["bootblock".into(), "main".into()],
        assembly: assembly.into(),
        units: vec![bootblock, main],
    };
    plan.order()?;
    Ok(plan)
}

/// LIM-resident stage. Heap/stack sit at the top of LIM so reset never
/// depends on initialized DRAM. This linker implementation moved from QEMU.
fn lim_linker_script(
    lim: Span,
    heap: Span,
    stack: Span,
    boot_hart_id: u32,
    descriptor: &EncodedLayout,
) -> Result<String, String> {
    use std::fmt::Write as _;
    for range in [lim, heap, stack] {
        range.end()?;
        if range.base % 16 != 0 || range.size % 16 != 0 {
            return Err("unaligned LIM reservation".into());
        }
    }
    if !lim.contains(heap.base, heap.size)
        || !lim.contains(stack.base, stack.size)
        || heap.end()? != stack.base
        || stack.end()? != lim.end()?
    {
        return Err("LIM heap and stack reservations overlap or are inconsistent".into());
    }
    let data_len = heap.base - lim.base;
    let mut out = String::new();
    writeln!(out, "OUTPUT_ARCH(riscv)\nENTRY(_start)\n_boot_hart_id = {boot_hart_id};\nMEMORY {{\n RAM (rwx) : ORIGIN = {:#x}, LENGTH = {:#x}\n HEAP (rw) : ORIGIN = {:#x}, LENGTH = {:#x}\n STACK (rw) : ORIGIN = {:#x}, LENGTH = {:#x}\n}}", lim.base, data_len, heap.base, heap.size, stack.base, stack.size).map_err(|e| e.to_string())?;
    out.push_str("\nSECTIONS\n{\n");
    out.push_str("    .text : {\n        _text_start = .;\n        KEEP(*(.text.entry))\n        *(.text .text.* .ltext .ltext.*)\n        KEEP(*(.vectors .vectors.*))\n        _text_end = .;\n    } > RAM\n");
    out.push_str(&fstart_image_build::linker::layout_section(
        descriptor, "RAM",
    ));
    writeln!(out, "    .fstart.anchor : ALIGN(8) {{\n        *(.fstart.anchor)\n        . = ALIGN(8);\n        _FSTART_HEAP_SIZE = .;\n        QUAD({:#x})\n    }} > RAM\n", heap.size).map_err(|e| e.to_string())?;
    out.push_str("    .rodata : ALIGN(8) {\n        _rodata_start = .;\n        KEEP(*(.fstart.bootstrap_pin))\n        *(.rodata .rodata.* .lrodata .ldata.*)\n    } > RAM\n");
    out.push_str("    .data : ALIGN(8) {\n        _data_start = .;\n        *(.data .data.* .ldata .ldata.*)\n        _data_end = .;\n    } > RAM\n    _data_load = LOADADDR(.data);\n");
    out.push_str("    .bss (NOLOAD) : ALIGN(8) {\n        _bss_start = .;\n        *(.bss .bss.* .lbss .lbss.*)\n        *(COMMON)\n        _bss_end = .;\n    } > RAM\n");
    writeln!(out, "    .fstart.heap (NOLOAD) : ALIGN(16) {{\n        _FSTART_HEAP = .;\n        . = . + {:#x};\n    }} > HEAP\n", heap.size).map_err(|e| e.to_string())?;
    writeln!(out, "    .stack (NOLOAD) : ALIGN(16) {{\n        _stack_bottom = .;\n        . = . + {:#x};\n        . = ALIGN(16);\n        _stack_top = .;\n        _writable_end = .;\n    }} > STACK\n", stack.size).map_err(|e| e.to_string())?;
    out.push_str("    _binary_end = _data_end;\n");
    out.push_str(
        "    ASSERT(_bss_end <= ORIGIN(HEAP), \"stage data/BSS exceed resolved capacity\")\n}\n",
    );
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    const UNMATCHED: Fu740ImageFacts = Fu740ImageFacts {
        flash_base: 0x2000_0000,
        flash_size: 0x0200_0000,
        dram_base: 0x8000_0000,
        dram_size: 0x4_0000_0000,
        kernel_addr: 0x8400_0000,
        runtime_addr: 0x8300_0000,
        dtb_addr: 0x8f00_0000,
        dtb_file: "unmatched.dtb",
        bootargs: "console=ttySIF0 earlycon=sbi",
        boot_hart_id: 1,
    };
    #[test]
    fn fu740_uses_usable_lim_then_dram_and_declares_every_payload_file() {
        for payload in ["halt", "linux", "uefi"] {
            let plan = resolve(
                UNMATCHED,
                BuildSelection {
                    payload: Some(payload.into()),
                    ..Default::default()
                },
            )
            .unwrap();
            let UnitOutput::Executable {
                expectations,
                load_address,
                flat_capacity,
            } = &plan.units[0].output
            else {
                panic!("not executable")
            };
            assert_eq!((*load_address, *flat_capacity), (LIM.base, LIM.size));
            assert_eq!(expectations.entry, Some(LIM.base));
            assert_eq!(expectations.symbols["_stack_top"], 0x081e_0000);
            assert!(expectations.symbols["_FSTART_HEAP"] < 0x081e_0000);
            assert_eq!(plan.stages, ["bootblock", "main"]);
            assert_eq!(plan.units[0].environment, "car");
            assert_eq!(plan.units[0].payload, "halt");
            assert_eq!(plan.units[0].features, ["bundle-bootblock"]);
            assert_eq!(plan.units[1].environment, "ram");
            let UnitOutput::Executable {
                expectations: main,
                load_address: main_load,
                ..
            } = &plan.units[1].output
            else {
                panic!("missing mainstage")
            };
            assert_eq!(*main_load, MAIN.base);
            assert!(main.symbols["_FSTART_HEAP"] >= MAIN.base);
            assert_eq!(main.symbols["_stack_top"], MAIN.end().unwrap());
            assert_eq!(plan.assembly.bootstrap().len(), 1);
            assert!(plan.assembly.ffs().unwrap().build.qemu_machine.is_none());
            assert_eq!(
                plan.inputs.len(),
                match payload {
                    "halt" => 0,
                    "linux" => 3,
                    _ => 1,
                }
            );
            if payload == "linux" {
                assert_eq!(
                    plan.inputs
                        .iter()
                        .find(|i| i.name == "dtb")
                        .unwrap()
                        .capacity,
                    0x10000
                );
            }
        }
    }
    #[test]
    fn board_destinations_drive_assembly_and_validation() {
        let mut facts = UNMATCHED;
        facts.kernel_addr += 0x1000;
        let plan = resolve(facts, BuildSelection::default()).unwrap();
        assert_eq!(
            plan.assembly
                .ffs()
                .unwrap()
                .payload
                .as_ref()
                .unwrap()
                .kernel_load_addr,
            Some(facts.kernel_addr)
        );
        facts.dtb_addr = facts.kernel_addr;
        assert!(resolve(facts, BuildSelection::default()).is_err());
    }
}
