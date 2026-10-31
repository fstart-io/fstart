//! The FU740 build plan: one stage running from the L2 LIM, the FFS on the
//! SPI flash, and the payload (Linux or CrabEFI with OpenSBI) in DRAM.
//! Addresses come from the board's [`Fu740Config`], the one the firmware
//! reads.

extern crate std;

use fstart_core::layout::RegionKind as Kind;
use fstart_core::*;
use fstart_image_build::build_plan::{
    BuildPlan, CargoTarget, CompilationUnit, CompilerCfgSchema, FfsAssembly as Assembly, InputFile,
    UnitOutput,
};
use fstart_image_build::elf::{Architecture, Descriptor, Expectations};
use fstart_image_build::layout::EncodedLayout;
use fstart_image_build::plan::{BuildSelection, Span};
use std::{
    collections::BTreeMap,
    string::{String, ToString},
    vec,
    vec::Vec,
};

use crate::{FU740_DRAM_BASE, Fu740Board, Fu740Config};

/// The L2 loosely-integrated memory the stage runs from.
const LIM: Span = span(0x0800_0000, 0x0020_0000);
const STACK_SIZE: u64 = 0x4000;
const HEAP_SIZE: u64 = 0x4000;
const KERNEL_FILE: &str = "Image-riscv64";
const KERNEL_CAPACITY: u64 = 0x0b00_0000;
const OPENSBI_FILE: &str = "fw_dynamic.bin";
const OPENSBI_CAPACITY: u64 = 0x0100_0000;
const DTB_CAPACITY: u64 = 0x0001_0000;

const fn span(base: u64, size: u64) -> Span {
    Span { base, size }
}

/// The plan runner's entry: `Plan::<Board>::emit(selection)`.
pub struct Plan<B>(core::marker::PhantomData<B>);

impl<B: Fu740Board> Plan<B> {
    pub fn emit(selection_json: &str) {
        let selection = serde_json::from_str(selection_json).expect("invalid build selection");
        let plan =
            resolve(B::CONFIG, B::DEVICE_TREE, selection).expect("invalid FU740 image policy");
        std::println!(
            "{}",
            serde_json::to_string(&plan).expect("serialize FU740 plan")
        );
    }
}

/// The plan for `config` and the payload `selection` names.
pub fn resolve(
    config: &Fu740Config,
    device_tree: &'static str,
    selection: BuildSelection,
) -> Result<BuildPlan, String> {
    let payload = selection.payload.unwrap_or_else(|| "linux".into());
    if !matches!(payload.as_str(), "halt" | "linux" | "uefi") {
        return Err("FU740 supports halt, Linux and UEFI".into());
    }
    let linux = payload == "linux";
    let keep_firmware = payload != "halt";
    let firmware_image = span(config.firmware_base, config.firmware_size);
    let (stack, heap) = stack_heap(LIM, STACK_SIZE, HEAP_SIZE)?;

    let mut regions = vec![
        LIM.region(Kind::Writable),
        stack.region(Kind::Stack),
        heap.region(Kind::Heap),
        LIM.region(Kind::Image),
        firmware_image.region(Kind::Flash),
        firmware_image.region(Kind::Firmware),
    ];
    if linux {
        regions.push(span(config.kernel_addr, KERNEL_CAPACITY).region(Kind::Payload));
    }
    if keep_firmware {
        regions.push(span(config.firmware_addr, OPENSBI_CAPACITY).region(Kind::PayloadFirmware));
    }
    if linux {
        regions.push(span(config.dtb_addr, DTB_CAPACITY).region(Kind::DeviceTree));
    }
    let descriptor = EncodedLayout::encode(0, &regions).map_err(|e| e.to_string())?;
    let boot_hart_id = u32::try_from(config.boot_hart_id).map_err(|e| e.to_string())?;
    let linker_script = lim_linker_script(LIM, heap, stack, boot_hart_id, &descriptor)?;
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
    expectations.validate().map_err(|e| e.to_string())?;

    let opensbi = || FirmwareConfig {
        kind: FirmwareKind::OpenSbi,
        file: hstr(OPENSBI_FILE),
        load_addr: config.firmware_addr,
    };
    let assembly_payload = match payload.as_str() {
        "linux" => Some(PayloadConfig {
            kind: PayloadKind::LinuxBoot,
            kernel_file: Some(hstr(KERNEL_FILE)),
            kernel_load_addr: Some(config.kernel_addr),
            x86_zero_page_addr: None,
            fdt: FdtSource::Override(hstr(device_tree)),
            dtb_addr: Some(config.dtb_addr),
            src_dtb_addr: None,
            bootargs: Some(hstr(config.bootargs)),
            print_x86_mtrrs: false,
            compression: Compression::Lz4,
            firmware: Some(opensbi()),
            fit_file: None,
            fit_config: None,
            fit_parse: None,
        }),
        "uefi" => Some(PayloadConfig {
            kind: PayloadKind::UefiPayload,
            kernel_file: None,
            kernel_load_addr: None,
            x86_zero_page_addr: None,
            fdt: FdtSource::Platform,
            dtb_addr: None,
            src_dtb_addr: None,
            bootargs: None,
            print_x86_mtrrs: false,
            compression: Compression::Lz4,
            firmware: Some(opensbi()),
            fit_file: None,
            fit_config: None,
            fit_parse: None,
        }),
        _ => None,
    };
    let assembly = Assembly {
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
                base: firmware_image.base,
                size: firmware_image.size,
                kind: RegionKind::Rom,
            },
            MemoryRegion {
                name: hstr("dram"),
                base: FU740_DRAM_BASE,
                size: config.ddr.dram_size,
                kind: RegionKind::Ram,
            },
        ],
        flash: None,
        bootstrap: vec![],
        stages: StageLayout::Monolithic(MonolithicConfig {
            external: false,
            build: StageBuildConfig {
                firmware_image: Some(FirmwareImageConfig {
                    temp_ram_buffer: None,
                }),
                verify_firmware: true,
                payload: true,
                fdt: true,
                pci: false,
                ..Default::default()
            },
            load_addr: LIM.base,
            data_addr: None,
            stack_size: stack.size as u32,
            heap_size: Some(heap.size as u32),
        }),
        security: fstart_core::default_security_config(),
        payload: assembly_payload,
        microcode: None,
        data_assets: vec![],
        file_assets: vec![],
        full_flash_image: false,
        soc_image_format: SocImageFormat::None,
        boot_hart_id,
        build: BoardBuildPolicy {
            qemu_machine: None,
            firmware_image: FirmwareImagePolicy::memory_mapped(
                firmware_image.base,
                firmware_image.size,
            ),
            flash_image: None,
            ..Default::default()
        },
    };
    let mut inputs = Vec::new();
    if linux {
        inputs.push(InputFile {
            name: "kernel".into(),
            default: Some(KERNEL_FILE.into()),
            capacity: KERNEL_CAPACITY,
        });
    }
    if keep_firmware {
        inputs.push(InputFile {
            name: "firmware".into(),
            default: Some(OPENSBI_FILE.into()),
            capacity: OPENSBI_CAPACITY,
        });
    }
    let mut features = vec!["bundle-fu740".into()];
    match payload.as_str() {
        "linux" => features.push("linux".into()),
        "uefi" => features.push("crabefi".into()),
        _ => {}
    }
    let unit = CompilationUnit {
        name: "stage".into(),
        cargo_target: CargoTarget::BoardBinary,
        target: "riscv64gc-unknown-none-elf".into(),
        cfg_schema: CompilerCfgSchema {
            payloads: ["halt", "linux", "crabefi"].map(Into::into).to_vec(),
        },
        payload: if payload == "uefi" {
            "crabefi".into()
        } else {
            payload.clone()
        },
        features,
        build_std: Some("core,alloc".into()),
        rustflags: vec!["-Zub-checks=no".into()],
        release_only: false,
        linker_script: Some(linker_script),
        environment_values: BTreeMap::new(),
        bindings: vec![],
        file_bindings: vec![],
        board_features: vec![],
        output: UnitOutput::Executable {
            expectations,
            load_address: LIM.base,
            flat_capacity: LIM.size,
        },
    };
    let plan = BuildPlan {
        payload,
        units: vec![unit],
        stages: vec!["stage".into()],
        assembly: assembly.into(),
        inputs,
    };
    plan.order()?;
    Ok(plan)
}

/// Heap and stack at the top of `writable`, stack last.
fn stack_heap(writable: Span, stack_size: u64, heap_size: u64) -> Result<(Span, Span), String> {
    let stack = span(
        writable
            .end()?
            .checked_sub(stack_size)
            .ok_or("stack capacity")?,
        stack_size,
    );
    let heap = span(
        stack.base.checked_sub(heap_size).ok_or("heap capacity")?,
        heap_size,
    );
    if heap.base <= writable.base {
        return Err("heap/stack exceed writable reservation".into());
    }
    Ok((stack, heap))
}

/// The stage, its heap and stack all in LIM, so the early entry never
/// depends on DRAM; the SPI firmware window stays a separate storage
/// reservation.
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
    writeln!(
        out,
        "OUTPUT_ARCH(riscv)\nENTRY(_start)\n_boot_hart_id = {boot_hart_id};\nMEMORY {{\n \
         RAM (rwx) : ORIGIN = {:#x}, LENGTH = {:#x}\n \
         HEAP (rw) : ORIGIN = {:#x}, LENGTH = {:#x}\n \
         STACK (rw) : ORIGIN = {:#x}, LENGTH = {:#x}\n}}",
        lim.base, data_len, heap.base, heap.size, stack.base, stack.size
    )
    .map_err(|e| e.to_string())?;
    out.push_str("\nSECTIONS\n{\n");
    out.push_str("    .text : {\n        _text_start = .;\n        KEEP(*(.text.entry))\n        *(.text .text.* .ltext .ltext.*)\n        KEEP(*(.vectors .vectors.*))\n        _text_end = .;\n    } > RAM\n");
    out.push_str(&fstart_image_build::linker::layout_section(
        descriptor, "RAM",
    ));
    writeln!(
        out,
        "    .fstart.anchor : ALIGN(8) {{\n        *(.fstart.anchor)\n        . = ALIGN(8);\n        _FSTART_HEAP_SIZE = .;\n        QUAD({:#x})\n    }} > RAM\n",
        heap.size
    )
    .map_err(|e| e.to_string())?;
    out.push_str("    .rodata : ALIGN(8) {\n        _rodata_start = .;\n        KEEP(*(.fstart.bootstrap_pin))\n        *(.rodata .rodata.* .lrodata .ldata.*)\n    } > RAM\n");
    out.push_str("    .data : ALIGN(8) {\n        _data_start = .;\n        *(.data .data.* .ldata .ldata.*)\n        _data_end = .;\n    } > RAM\n    _data_load = LOADADDR(.data);\n");
    out.push_str("    .bss (NOLOAD) : ALIGN(8) {\n        _bss_start = .;\n        *(.bss .bss.* .lbss .lbss.*)\n        *(COMMON)\n        _bss_end = .;\n    } > RAM\n");
    writeln!(
        out,
        "    .fstart.heap (NOLOAD) : ALIGN(16) {{\n        _FSTART_HEAP = .;\n        . = . + {:#x};\n    }} > HEAP\n",
        heap.size
    )
    .map_err(|e| e.to_string())?;
    writeln!(
        out,
        "    .stack (NOLOAD) : ALIGN(16) {{\n        _stack_bottom = .;\n        . = . + {:#x};\n        . = ALIGN(16);\n        _stack_top = .;\n        _writable_end = .;\n    }} > STACK\n",
        stack.size
    )
    .map_err(|e| e.to_string())?;
    out.push_str("    _binary_end = _data_end;\n");
    out.push_str(
        "    ASSERT(_bss_end <= ORIGIN(HEAP), \"stage data/BSS exceed resolved capacity\")\n}\n",
    );
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Fu740DdrConfig, Fu740PrciConfig};

    static CTL: [u32; 265] = [0; 265];
    static PHY: [u32; 1215] = [0; 1215];
    static CONFIG: Fu740Config = Fu740Config {
        prci: Fu740PrciConfig::new(26_000_000),
        ddr: Fu740DdrConfig::new(&CTL, &PHY, 0x4_0000_0000),
        firmware_base: 0x2000_0000,
        firmware_size: 0x0200_0000,
        dtb_addr: 0x8f00_0000,
        kernel_addr: 0x8400_0000,
        firmware_addr: 0x8300_0000,
        boot_hart_id: 1,
        bootargs: "console=ttySIF0",
    }
    .build();

    fn payload(name: &str) -> Option<PayloadConfig> {
        let selection = BuildSelection {
            payload: Some(name.into()),
            ..Default::default()
        };
        let plan = resolve(&CONFIG, "board.dtb", selection).unwrap();
        plan.assembly.ffs().unwrap().payload.clone()
    }

    #[test]
    fn payloads_take_the_board_device_tree_and_opensbi() {
        assert!(payload("halt").is_none());
        let linux = payload("linux").unwrap();
        assert!(matches!(linux.fdt, FdtSource::Override(name) if name.as_str() == "board.dtb"));
        assert_eq!(linux.kernel_load_addr, Some(0x8400_0000));
        assert!(payload("uefi").unwrap().firmware.is_some());
        let coreboot = BuildSelection {
            payload: Some("coreboot".into()),
            ..Default::default()
        };
        assert!(resolve(&CONFIG, "board.dtb", coreboot).is_err());
    }
}
