//! Concrete QEMU virt plans. Machine policy composes one shared image projection.
//! Hardware flows and the AArch64 reset copier are unchanged.
extern crate std;
use crate::facts::{VirtBoardFacts, VirtMachine};
use fstart_core::*;
use fstart_image_build::{
    build_plan::{
        ArtifactBinding, Assembly, BuildPlan, CargoTarget, CompilationUnit, InputFile, UnitOutput,
    },
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
        environments: strings(&["monolithic", "smm"]),
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
/// ARM retains two physical 64-MiB banks. RISC-V has one physical 32-MiB
/// bank: the initial stage and subsequent files are packed together at actual
/// size, not separated by an artificial half-bank reservation.
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
            Self::Q35 | Self::Sbsa | Self::SifiveU | Self::Unmatched => {
                return Err("QEMU machine is not a virt preset".into());
            }
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
    match machine {
        VirtMachine::Riscv64 | VirtMachine::Armv7 | VirtMachine::Aarch64 => {
            resolve_virt(machine, selection)
        }
        VirtMachine::Q35 | VirtMachine::Sbsa | VirtMachine::SifiveU | VirtMachine::Unmatched => {
            resolve_qemu(machine, selection)
        }
    }
}

fn resolve_virt(machine: VirtMachine, selection: BuildSelection) -> Result<BuildPlan, String> {
    use fstart_core::layout::RegionKind as Kind;
    let payload = selection.payload.unwrap_or_else(|| "linux".into());
    let policy = machine.policy(&payload)?;
    let flash = policy.flash;
    let (image, firmware) = if matches!(machine, VirtMachine::Riscv64) {
        (flash, flash)
    } else {
        let image = span(flash.base, flash.size / 2);
        (image, span(image.end()?, flash.size / 2))
    };
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
        flash: None,
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
            },
        }],
        stages: vec!["stage".into()],
        assembly,
        inputs,
    };
    plan.order()?;
    Ok(plan)
}

/// Link geometry for one non-virt QEMU machine. The sbsa and sifive-u flows
/// reuse the shared XIP/relocation projection; q35 needs x86 reset-vector
/// placement and unmatched executes from LIM, so those two emit focused
/// platform-owned linker text instead of a second lifecycle model.
#[derive(Debug, Clone, Copy)]
enum QemuLink {
    Xip {
        image: Span,
        execution: Option<Span>,
        writable: Span,
    },
    X86 {
        flash: Span,
        ram: Span,
    },
    Lim {
        lim: Span,
    },
}

/// Payload files for the machines that load a kernel plus OpenSBI firmware.
/// q35 carries no payload files: its UEFI selection mirrors the legacy empty
/// config, and halt selects nothing.
#[derive(Debug, Clone, Copy)]
struct QemuLinuxPayload {
    kernel_file: &'static str,
    kernel_addr: u64,
    kernel_capacity: u64,
    firmware_file: &'static str,
    firmware_addr: u64,
    firmware_capacity: u64,
    dtb_addr: u64,
    dtb_override: Option<&'static str>,
    bootargs: &'static str,
}

/// Platform-owned policy row for one non-virt QEMU machine. Every address,
/// size, compression and payload value is copied verbatim from the retired
/// legacy builders; the linker mechanics follow the shared virt model.
struct QemuPolicy {
    platform: Platform,
    target: &'static str,
    entry: &'static str,
    bundle: &'static str,
    boot_hart_id: u32,
    default_payload: &'static str,
    uefi: bool,
    linux: Option<QemuLinuxPayload>,
    link: QemuLink,
    stack_size: u64,
    heap_size: u64,
    memory: Vec<MemoryRegion>,
    firmware_image: Span,
    flash_image: Option<Span>,
    full_flash_image: bool,
    qemu_machine: Option<QemuMachine>,
    dev_security: bool,
    pci: bool,
    fdt: bool,
    page_table: Option<Span>,
}

impl VirtMachine {
    fn qemu_policy(self) -> Result<QemuPolicy, String> {
        let linux_payload = |dtb_override| QemuLinuxPayload {
            kernel_file: "Image-riscv64",
            kernel_addr: 0x8400_0000,
            kernel_capacity: 0x0b00_0000,
            firmware_file: "fw_dynamic.bin",
            firmware_addr: 0x8300_0000,
            firmware_capacity: 0x0100_0000,
            dtb_addr: 0x8f00_0000,
            dtb_override,
            bootargs: "console=ttySIF0 earlycon=sbi",
        };
        Ok(match self {
            Self::Q35 => QemuPolicy {
                platform: Platform::X86_64,
                target: "x86_64-unknown-none",
                entry: "x86_64",
                bundle: "bundle-q35",
                boot_hart_id: 0,
                default_payload: "halt",
                uefi: true,
                linux: None,
                link: QemuLink::X86 {
                    flash: span(0xff00_0000, 0x0100_0000),
                    // Top 16 MiB of low RAM is reserved for TSEG: Q35 TSEG
                    // always sits at the top of installed RAM and is hidden
                    // from non-SMM access once SMM locks it, so firmware
                    // statics (stack/heap/BSS, carved top-down from `ram`)
                    // must live below it. 16 MiB covers every ESMRAMC size
                    // encoding plus the usual extended-TSEG settings; larger
                    // decoded TSEGs are rejected loudly at runtime (see
                    // `init_mp_smm`), never silently overlapped.
                    ram: span(0x0100_0000, 0x3e00_0000),
                },
                stack_size: 0x0040_0000,
                heap_size: 0x0020_0000,
                memory: vec![
                    MemoryRegion {
                        name: hstr("flash"),
                        base: 0xff00_0000,
                        size: 0x0100_0000,
                        kind: RegionKind::Rom,
                    },
                    MemoryRegion {
                        name: hstr("workram"),
                        base: 0x0010_0000,
                        size: 0x3ff0_0000,
                        kind: RegionKind::Ram,
                    },
                ],
                firmware_image: span(0xff10_0000, 0x00ef_f000),
                flash_image: None,
                full_flash_image: false,
                qemu_machine: None,
                dev_security: true,
                pci: true,
                fdt: false,
                page_table: Some(span(0x1000, 0x4000)),
            },
            Self::Sbsa => QemuPolicy {
                platform: Platform::Aarch64,
                target: "aarch64-unknown-none",
                entry: "aarch64-relocate",
                bundle: "bundle-sbsa",
                boot_hart_id: 0,
                default_payload: "halt",
                uefi: false,
                linux: None,
                link: QemuLink::Xip {
                    image: span(0x1000_0000, 0x1000_0000),
                    execution: Some(span(0x100_0010_0000, 0x0040_0000)),
                    writable: span(0x100_0050_0000, 0x0080_0000),
                },
                stack_size: 0x04_0000,
                heap_size: 0x04_0000,
                memory: vec![
                    MemoryRegion {
                        name: hstr("flash"),
                        base: 0x1000_0000,
                        size: 0x1000_0000,
                        kind: RegionKind::Rom,
                    },
                    MemoryRegion {
                        name: hstr("ram"),
                        base: 0x100_0000_0000,
                        size: 0x4000_0000,
                        kind: RegionKind::Ram,
                    },
                ],
                firmware_image: span(0x1010_0000, 0x0ff0_0000),
                flash_image: Some(span(0x1000_0000, 0x1000_0000)),
                full_flash_image: true,
                qemu_machine: Some(QemuMachine::SbsaRef),
                dev_security: false,
                pci: true,
                fdt: false,
                page_table: None,
            },
            Self::SifiveU => QemuPolicy {
                platform: Platform::Riscv64,
                target: "riscv64gc-unknown-none-elf",
                entry: "riscv64",
                bundle: "bundle-sifive-u",
                boot_hart_id: 1,
                default_payload: "linux",
                uefi: true,
                linux: Some(linux_payload(None)),
                link: QemuLink::Xip {
                    image: span(0x8000_0000, 0x0100_0000),
                    execution: None,
                    // Compact reservation like the RISC-V virt preset: QEMU
                    // places its DTB near the top of RAM, so the linked
                    // running span must not cover the upper DRAM slack.
                    writable: span(0x8100_0000, 0x0040_0000),
                },
                stack_size: 0x04_0000,
                heap_size: 0x04_0000,
                memory: vec![MemoryRegion {
                    name: hstr("dram"),
                    base: 0x8000_0000,
                    size: 0x4000_0000,
                    kind: RegionKind::Ram,
                }],
                firmware_image: span(0x8000_0000, 0x0100_0000),
                flash_image: None,
                full_flash_image: false,
                qemu_machine: Some(QemuMachine::SifiveU),
                dev_security: false,
                pci: false,
                fdt: true,
                page_table: None,
            },
            Self::Unmatched => QemuPolicy {
                platform: Platform::Riscv64,
                target: "riscv64gc-unknown-none-elf",
                entry: "riscv64",
                bundle: "bundle-unmatched",
                boot_hart_id: 1,
                default_payload: "linux",
                uefi: true,
                linux: Some(linux_payload(Some("unmatched.dtb"))),
                link: QemuLink::Lim {
                    lim: span(0x0800_0000, 0x0020_0000),
                },
                stack_size: 0x4000,
                heap_size: 0x4000,
                memory: vec![
                    MemoryRegion {
                        name: hstr("lim"),
                        base: 0x0800_0000,
                        size: 0x0020_0000,
                        kind: RegionKind::Ram,
                    },
                    MemoryRegion {
                        name: hstr("spi-xip"),
                        base: 0x2000_0000,
                        size: 0x0200_0000,
                        kind: RegionKind::Rom,
                    },
                    MemoryRegion {
                        name: hstr("dram"),
                        base: 0x8000_0000,
                        size: 0x4_0000_0000,
                        kind: RegionKind::Ram,
                    },
                ],
                firmware_image: span(0x2000_0000, 0x0200_0000),
                flash_image: None,
                full_flash_image: false,
                qemu_machine: None,
                dev_security: true,
                pci: false,
                fdt: true,
                page_table: None,
            },
            Self::Riscv64 | Self::Armv7 | Self::Aarch64 => {
                return Err("virt machine has no QEMU policy row".into());
            }
        })
    }
}

/// Fixed heap/stack placement at the top of the writable reservation, shared
/// by every non-virt QEMU machine like the existing virt projection.
fn qemu_stack_heap(
    writable: Span,
    stack_size: u64,
    heap_size: u64,
) -> Result<(Span, Span), String> {
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

/// x86_64 top-aligned XIP bootblock with reset vector, plus the linked
/// descriptor the common ELF validation requires. Section order and symbols
/// follow the retired BoardConfig x86 layout; only heap/stack placement and
/// the descriptor are shared virt mechanics.
fn qemu_q35_linker_script(
    flash: Span,
    ram: Span,
    heap: Span,
    stack: Span,
    boot_hart_id: u32,
    descriptor: &EncodedLayout,
) -> Result<String, String> {
    use std::fmt::Write as _;
    for range in [flash, ram, heap, stack] {
        range.end()?;
        if range.base % 16 != 0 || range.size % 16 != 0 {
            return Err("unaligned q35 reservation".into());
        }
    }
    if !ram.contains(heap.base, heap.size)
        || !ram.contains(stack.base, stack.size)
        || heap.end()? != stack.base
        || stack.end()? != ram.end()?
    {
        return Err("q35 heap and stack reservations overlap or are inconsistent".into());
    }
    let data_len = heap.base - ram.base;
    let mut out = String::new();
    writeln!(
        out,
        "OUTPUT_ARCH(i386:x86-64)\nENTRY(_start)\n_boot_hart_id = {boot_hart_id};\nMEMORY {{\n \
         ROM (rx) : ORIGIN = {:#x}, LENGTH = {:#x}\n \
         RAM (rwx) : ORIGIN = {:#x}, LENGTH = {:#x}\n \
         HEAP (rw) : ORIGIN = {:#x}, LENGTH = {:#x}\n \
         STACK (rw) : ORIGIN = {:#x}, LENGTH = {:#x}\n}}",
        flash.base, flash.size, ram.base, data_len, heap.base, heap.size, stack.base, stack.size
    )
    .map_err(|e| e.to_string())?;
    let bootblock_top = flash.end()? - 0x1000;
    writeln!(out, "SECTIONS\n{{\n    /* x86 bootblock: place the C environment at the top of ROM. */\n    _bootblock_top = {bootblock_top:#x};")
        .map_err(|e| e.to_string())?;
    out.push_str("    _bootblock_program_size = SIZEOF(.text) + SIZEOF(.fstart.layout) + SIZEOF(.fstart.anchor) + SIZEOF(.rodata) + SIZEOF(.data);\n");
    out.push_str(
        "    _bootblock_base = ((_bootblock_top - _bootblock_program_size) & ~0xfff) - 0x1000;\n",
    );
    out.push_str("    .text _bootblock_base : {\n        _bootblock = .;\n        _text_start = .;\n        KEEP(*(.text.entry))\n        *(.text .text.* .ltext .ltext.*)\n        _text_end = .;\n    } > ROM\n");
    out.push_str(&fstart_image_build::linker::layout_section(
        descriptor, "ROM",
    ));
    writeln!(
        out,
        "    .fstart.anchor : ALIGN(16) {{\n        _fstart_anchor_early = .;\n        *(.fstart.anchor)\n        _fstart_early_microcode_enabled = .;\n        LONG(0)\n        . = ALIGN(8);\n        _FSTART_HEAP_SIZE = .;\n        QUAD({:#x})\n    }} > ROM\n",
        heap.size
    )
    .map_err(|e| e.to_string())?;
    out.push_str("    .rodata : ALIGN(16) {\n        _rodata_start = .;\n        *(.rodata .rodata.* .lrodata .lrodata.*)\n        _rodata_end = .;\n    } > ROM\n");
    out.push_str("    .data : ALIGN(16) {\n        _data_start = .;\n        *(.data .data.* .ldata .ldata.*)\n        _data_end = .;\n    } > RAM AT > ROM\n    _data_load = LOADADDR(.data);\n");
    out.push_str("    .bss (NOLOAD) : ALIGN(16) {\n        _bss_start = .;\n        *(.bss .bss.* .lbss .lbss.*)\n        *(COMMON)\n        _bss_end = .;\n    } > RAM\n");
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
    out.push_str("\n    /* x86 CAR/postcar symbols (CAR disabled) */\n    _has_car = 0;\n    _car_base = 0;\n    _car_size = 0;\n    _ecar_stack = _stack_top;\n    _rom_mtrr_base = 0;\n    _rom_mtrr_mask = 0;\n");
    let boot_block_addr = flash.base + flash.size - 0x1000;
    let reset_addr = flash.base + flash.size - 16;
    writeln!(
        out,
        "\n    /* x86: 16-bit/32-bit/64-bit entry code + reset vector */\n    .x86boot {boot_block_addr:#x} : AT({boot_block_addr:#x}) {{\n        KEEP(*(.x86boot))\n    }} > ROM\n\n    .reset {reset_addr:#x} : AT({reset_addr:#x}) {{\n        KEEP(*(.reset))\n    }} > ROM\n"
    )
    .map_err(|e| e.to_string())?;
    writeln!(out, "    _binary_end = .;").map_err(|e| e.to_string())?;
    out.push_str("    ASSERT(_bootblock >= ORIGIN(ROM), \"x86 bootblock does not fit in ROM\")\n");
    out.push_str("    ASSERT(_data_load + SIZEOF(.data) <= _bootblock_top, \"x86 bootblock overlaps reset entry area\")\n");
    out.push_str(
        "    ASSERT(_bss_end <= ORIGIN(HEAP), \"stage data/BSS exceed resolved capacity\")\n}\n",
    );
    Ok(out)
}

/// LIM-resident stage with the linked descriptor. Heap/stack sit at the top
/// of LIM so the early entry never depends on DRAM; the SPI firmware window
/// stays a separate storage reservation.
fn qemu_lim_linker_script(
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

fn resolve_qemu(machine: VirtMachine, selection: BuildSelection) -> Result<BuildPlan, String> {
    use fstart_core::layout::RegionKind as Kind;
    use fstart_image_build::elf::{Architecture, Descriptor, Expectations};
    let policy = machine.qemu_policy()?;
    let payload = selection
        .payload
        .unwrap_or_else(|| policy.default_payload.into());
    if payload != "halt" && payload != "linux" && payload != "uefi" {
        return Err("QEMU machine supports halt, Linux and (where available) UEFI".into());
    }
    if payload == "linux" && policy.linux.is_none() {
        return Err(std::format!(
            "QEMU {} does not support Linux",
            policy.bundle
        ));
    }
    if payload == "uefi" && !policy.uefi {
        return Err(std::format!("QEMU {} does not support UEFI", policy.bundle));
    }
    let linux = (payload == "linux")
        .then_some(policy.linux.as_ref())
        .flatten();
    let keep_firmware = payload != "halt";
    let (writable, stack, heap) = match policy.link {
        QemuLink::Xip { writable, .. } => {
            let (stack, heap) = qemu_stack_heap(writable, policy.stack_size, policy.heap_size)?;
            (writable, stack, heap)
        }
        QemuLink::X86 { ram, .. } => {
            let (stack, heap) = qemu_stack_heap(ram, policy.stack_size, policy.heap_size)?;
            (ram, stack, heap)
        }
        QemuLink::Lim { lim } => {
            let (stack, heap) = qemu_stack_heap(lim, policy.stack_size, policy.heap_size)?;
            (lim, stack, heap)
        }
    };
    // A DTB reservation exists only for machines with Linux payload files.
    let dtb: Option<Span> = match (keep_firmware, linux) {
        (true, Some(_)) => policy.linux.as_ref().map(|p| span(p.dtb_addr, 0x0001_0000)),
        _ => None,
    };
    let mut regions = vec![
        writable.region(Kind::Writable),
        stack.region(Kind::Stack),
        heap.region(Kind::Heap),
    ];
    // Linker text and ELF expectations per link model.
    let (linker_script, expectations, load_address, flat_capacity) = match policy.link {
        QemuLink::Xip {
            image, execution, ..
        } => {
            let code = execution.unwrap_or(image);
            let mut core = vec![
                image.region(Kind::Image),
                image.region(Kind::Flash),
                policy.firmware_image.region(Kind::Firmware),
            ];
            regions.append(&mut core);
            if let Some(exec) = execution {
                regions.push(exec.region(Kind::Execution));
            }
            if let Some(p) = linux {
                regions.push(span(p.kernel_addr, p.kernel_capacity).region(Kind::Payload));
            }
            if keep_firmware {
                if let Some(p) = &policy.linux {
                    regions.push(
                        span(p.firmware_addr, p.firmware_capacity).region(Kind::PayloadFirmware),
                    );
                }
            }
            if let Some(dtb) = dtb {
                regions.push(dtb.region(Kind::DeviceTree));
            }
            let layout = Xip {
                platform: policy.platform,
                boot_hart_id: policy.boot_hart_id,
                image,
                execution,
                writable,
                heap,
                stack,
                descriptor: EncodedLayout::encode(0, &regions).map_err(|e| e.to_string())?,
            };
            let script = fstart_image_build::linker::resolved_xip(&layout)?;
            let expected = layout.elf_expectations()?;
            (script, expected, code.base, image.size)
        }
        QemuLink::X86 { flash, .. } => {
            regions.push(flash.region(Kind::Image));
            regions.push(flash.region(Kind::Flash));
            regions.push(policy.firmware_image.region(Kind::Firmware));
            let descriptor = EncodedLayout::encode(0, &regions).map_err(|e| e.to_string())?;
            let script = qemu_q35_linker_script(
                flash,
                writable,
                heap,
                stack,
                policy.boot_hart_id,
                &descriptor,
            )?;
            let expected = Expectations {
                architecture: Architecture::X86_64,
                elf64: true,
                little_endian: true,
                stored: vec![flash],
                runtime: vec![flash, writable],
                identity_mapping: false,
                entry: Some(0xffff_fff0),
                copy: None,
                descriptor: Descriptor {
                    section: ".fstart.layout".into(),
                    start_symbol: "_fstart_layout_start".into(),
                    end_symbol: "_fstart_layout_end".into(),
                    bytes: descriptor.as_bytes().to_vec(),
                    reservation: flash,
                },
                symbols: std::collections::BTreeMap::from([
                    ("_stack_bottom".into(), stack.base),
                    ("_stack_top".into(), stack.end()?),
                    ("_FSTART_HEAP".into(), heap.base),
                ]),
            };
            expected.validate().map_err(|e| e.to_string())?;
            (script, expected, flash.base, flash.size)
        }
        QemuLink::Lim { lim } => {
            regions.push(lim.region(Kind::Image));
            regions.push(policy.firmware_image.region(Kind::Flash));
            regions.push(policy.firmware_image.region(Kind::Firmware));
            if let Some(p) = linux {
                regions.push(span(p.kernel_addr, p.kernel_capacity).region(Kind::Payload));
            }
            if keep_firmware {
                if let Some(p) = &policy.linux {
                    regions.push(
                        span(p.firmware_addr, p.firmware_capacity).region(Kind::PayloadFirmware),
                    );
                }
            }
            if let Some(dtb) = dtb {
                regions.push(dtb.region(Kind::DeviceTree));
            }
            let descriptor = EncodedLayout::encode(0, &regions).map_err(|e| e.to_string())?;
            let script =
                qemu_lim_linker_script(lim, heap, stack, policy.boot_hart_id, &descriptor)?;
            let expected = Expectations {
                architecture: Architecture::Riscv64,
                elf64: true,
                little_endian: true,
                stored: vec![lim],
                runtime: vec![lim],
                identity_mapping: true,
                entry: Some(lim.base),
                copy: None,
                descriptor: Descriptor {
                    section: ".fstart.layout".into(),
                    start_symbol: "_fstart_layout_start".into(),
                    end_symbol: "_fstart_layout_end".into(),
                    bytes: descriptor.as_bytes().to_vec(),
                    reservation: lim,
                },
                symbols: std::collections::BTreeMap::from([
                    ("_stack_bottom".into(), stack.base),
                    ("_stack_top".into(), stack.end()?),
                    ("_FSTART_HEAP".into(), heap.base),
                ]),
            };
            expected.validate().map_err(|e| e.to_string())?;
            (script, expected, lim.base, lim.size)
        }
    };
    let mut features = vec![policy.bundle.into()];
    if payload == "linux" {
        features.push("linux".into());
    } else if payload == "uefi" {
        features.push("crabefi".into());
    }
    let security = if policy.dev_security {
        fstart_core::dev_security_config("keys/dev-signing.pub")
    } else {
        crate::qemu_virt_security_config("keys/dev-signing.pub")
    };
    let assembly_payload = if payload == "halt" {
        None
    } else if let Some(p) = linux {
        Some(PayloadConfig {
            kind: PayloadKind::LinuxBoot,
            kernel_file: Some(hstr(p.kernel_file)),
            kernel_load_addr: Some(p.kernel_addr),
            fdt: match p.dtb_override {
                Some(name) => FdtSource::Override(hstr(name)),
                None => FdtSource::Platform,
            },
            dtb_addr: Some(p.dtb_addr),
            src_dtb_addr: None,
            bootargs: Some(hstr(p.bootargs)),
            print_x86_mtrrs: false,
            compression: Compression::Lz4,
            firmware: Some(FirmwareConfig {
                kind: FirmwareKind::OpenSbi,
                file: hstr(p.firmware_file),
                load_addr: p.firmware_addr,
            }),
            fit_file: None,
            fit_config: None,
            fit_parse: None,
        })
    } else {
        Some(PayloadConfig {
            kind: PayloadKind::UefiPayload,
            kernel_file: None,
            kernel_load_addr: None,
            fdt: FdtSource::Platform,
            dtb_addr: None,
            src_dtb_addr: None,
            bootargs: None,
            print_x86_mtrrs: false,
            compression: Compression::Lz4,
            firmware: policy.linux.as_ref().map(|p| FirmwareConfig {
                kind: FirmwareKind::OpenSbi,
                file: hstr(p.firmware_file),
                load_addr: p.firmware_addr,
            }),
            fit_file: None,
            fit_config: None,
            fit_parse: None,
        })
    };
    let assembly = Assembly {
        platform: policy.platform,
        memory: policy.memory,
        flash: None,
        bootstrap: vec![],
        stages: StageLayout::Monolithic(MonolithicConfig {
            build: StageBuildConfig {
                firmware_image: Some(FirmwareImageConfig {
                    temp_ram_buffer: None,
                }),
                verify_firmware: true,
                payload: true,
                fdt: policy.fdt,
                pci: policy.pci,
                ..Default::default()
            },
            load_addr: load_address,
            data_addr: match policy.link {
                QemuLink::Xip { writable, .. } => Some(writable.base),
                QemuLink::X86 { ram, .. } => Some(ram.base),
                QemuLink::Lim { .. } => None,
            },
            stack_size: stack.size as u32,
            heap_size: Some(heap.size as u32),
            page_table_addr: policy.page_table.map(|p| (p.base, p.size)),
            page_size: if matches!(policy.link, QemuLink::X86 { .. }) {
                fstart_core::stage::PageSize::Size1GiB
            } else {
                Default::default()
            },
        }),
        security,
        payload: assembly_payload,
        microcode: None,
        full_flash_image: policy.full_flash_image,
        soc_image_format: SocImageFormat::None,
        boot_hart_id: policy.boot_hart_id,
        build: BoardBuildPolicy {
            qemu_machine: policy.qemu_machine,
            firmware_image: FirmwareImagePolicy::memory_mapped(
                policy.firmware_image.base,
                policy.firmware_image.size,
            ),
            flash_image: policy
                .flash_image
                .map(|f| FirmwareImagePolicy::memory_mapped(f.base, f.size)),
            ..Default::default()
        },
    };
    let inputs = match (keep_firmware, &policy.linux) {
        (true, Some(p)) => {
            let mut inputs = vec![];
            if linux.is_some() {
                inputs.push(InputFile {
                    name: "kernel".into(),
                    default: Some(p.kernel_file.into()),
                    capacity: p.kernel_capacity,
                });
            }
            inputs.push(InputFile {
                name: "firmware".into(),
                default: Some(p.firmware_file.into()),
                capacity: p.firmware_capacity,
            });
            inputs
        }
        _ => vec![],
    };
    // x86_64 links a static non-PIE image with a large code model, matching
    // the retired BoardConfig builds and the Intel family plan.
    let rustflags = if policy.target.starts_with("x86_64") {
        vec![
            "-Zub-checks=no".into(),
            "-Crelocation-model=static".into(),
            "-Ccode-model=large".into(),
            "--cfg".into(),
            "curve25519_dalek_backend=\"serial\"".into(),
        ]
    } else {
        vec!["-Zub-checks=no".into()]
    };
    // Q35 carries a standalone SMM image producer like the Intel family plan:
    // the board crate binds its handler via `smm_bin!` and the monolithic
    // stage embeds the image through `FSTART_SMM_IMAGE`.
    let mut units = vec![];
    if matches!(machine, VirtMachine::Q35) {
        units.push(CompilationUnit {
            name: "smm".into(),
            cargo_target: CargoTarget::BoardLibrary,
            target: policy.target.into(),
            entry: policy.entry.into(),
            cfg_schema: compiler_cfg_schema(),
            environment: "smm".into(),
            payload: "halt".into(),
            features: vec!["smm".into(), "bundle-smm".into()],
            build_std: None,
            release_only: true,
            linker_script: None,
            rustflags: [
                "-Cpanic=abort",
                "-Copt-level=s",
                "-Crelocation-model=pic",
                "-Cno-redzone=yes",
                "-Clinker-plugin-lto=no",
                "-Cembed-bitcode=no",
                "-Zfunction-sections=yes",
            ]
            .into_iter()
            .map(Into::into)
            .collect(),
            environment_values: BTreeMap::new(),
            bindings: vec![],
            output: UnitOutput::SmmImage {
                entry_count: 8,
                stack_size: 0x400,
                coreboot_module_args: false,
                coreboot_header: false,
            },
        });
    }
    let stage_bindings = if matches!(machine, VirtMachine::Q35) {
        vec![ArtifactBinding {
            producer: "smm".into(),
            artifact: "image".into(),
            environment: "FSTART_SMM_IMAGE".into(),
        }]
    } else {
        vec![]
    };
    units.push(CompilationUnit {
        name: "stage".into(),
        cargo_target: CargoTarget::BoardBinary,
        target: policy.target.into(),
        entry: policy.entry.into(),
        cfg_schema: compiler_cfg_schema(),
        environment: "monolithic".into(),
        payload: if payload == "uefi" {
            "crabefi".into()
        } else {
            payload.clone()
        },
        features,
        build_std: Some("core,alloc".into()),
        rustflags,
        release_only: false,
        linker_script: Some(linker_script),
        environment_values: BTreeMap::new(),
        bindings: stage_bindings,
        output: UnitOutput::Executable {
            expectations,
            load_address,
            flat_capacity,
        },
    });
    let plan = BuildPlan {
        payload: payload.clone(),
        units,
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
                let firmware = layout.region(Kind::Firmware).unwrap();
                if matches!(machine, VirtMachine::Riscv64) {
                    assert_eq!((firmware.base, firmware.size), (flash, 0x0200_0000));
                } else {
                    assert_eq!((firmware.base, firmware.size), (0x0400_0000, 0x0400_0000));
                }
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
    fn new_machines_keep_legacy_load_stack_heap_and_firmware_windows() {
        use fstart_core::layout::{Layout, RegionKind as Kind};
        for (machine, load, stack_size, heap_size, firmware, bundle) in [
            (
                VirtMachine::Q35,
                0xff00_0000,
                0x0040_0000,
                0x0020_0000,
                (0xff10_0000, 0x00ef_f000),
                "bundle-q35",
            ),
            (
                VirtMachine::Sbsa,
                0x100_0010_0000,
                0x04_0000,
                0x04_0000,
                (0x1010_0000, 0x0ff0_0000),
                "bundle-sbsa",
            ),
            (
                VirtMachine::SifiveU,
                0x8000_0000,
                0x04_0000,
                0x04_0000,
                (0x8000_0000, 0x0100_0000),
                "bundle-sifive-u",
            ),
            (
                VirtMachine::Unmatched,
                0x0800_0000,
                0x4000,
                0x4000,
                (0x2000_0000, 0x0200_0000),
                "bundle-unmatched",
            ),
        ] {
            let plan = resolve(
                machine,
                BuildSelection {
                    payload: Some("halt".into()),
                },
            )
            .unwrap();
            // Q35 now carries an `smm` producer unit ahead of the stage.
            let unit = plan.units.iter().find(|u| u.name == "stage").unwrap();
            let UnitOutput::Executable {
                expectations,
                load_address,
                ..
            } = &unit.output
            else {
                panic!("not executable")
            };
            assert_eq!(*load_address, load);
            assert_eq!(unit.features[0], bundle);
            let layout = Layout::parse(&expectations.descriptor.bytes).unwrap();
            assert_eq!(layout.region(Kind::Stack).unwrap().size, stack_size);
            assert_eq!(layout.region(Kind::Heap).unwrap().size, heap_size);
            let window = layout.region(Kind::Firmware).unwrap();
            assert_eq!((window.base, window.size), firmware);
            assert!(plan.assembly.payload.is_none());
            assert!(plan.inputs.is_empty());
            // Stack and heap stay inside the writable reservation with the
            // heap directly below the stack at its top.
            let writable = layout.region(Kind::Writable).unwrap();
            let stack = layout.region(Kind::Stack).unwrap();
            let heap = layout.region(Kind::Heap).unwrap();
            assert_eq!(stack.base + stack.size, writable.base + writable.size);
            assert_eq!(heap.base + heap.size, stack.base);
            if matches!(machine, VirtMachine::Q35) {
                // Top 16 MiB of low RAM is reserved for TSEG: firmware
                // statics must end below it so locking SMRAM cannot hide
                // the stack (see the `ram` span in `qemu_policy`).
                assert_eq!(stack.base + stack.size, 0x3f00_0000);
            }
        }
    }

    #[test]
    fn new_machine_payload_selection_matches_legacy_contract() {
        use fstart_core::layout::{Layout, RegionKind as Kind};
        // SBSA is halt-only; q35 rejects direct Linux like the legacy plan.
        for (machine, payload) in [
            (VirtMachine::Sbsa, "linux"),
            (VirtMachine::Sbsa, "uefi"),
            (VirtMachine::Q35, "linux"),
        ] {
            assert!(
                resolve(
                    machine,
                    BuildSelection {
                        payload: Some(payload.into()),
                    }
                )
                .is_err()
            );
        }
        // q35 UEFI keeps the legacy empty payload config: no kernel, DTB or
        // firmware files, only the OpenSBI firmware on the RISC-V machines.
        let uefi = resolve(
            VirtMachine::Q35,
            BuildSelection {
                payload: Some("uefi".into()),
            },
        )
        .unwrap();
        let uefi_stage = uefi.units.iter().find(|u| u.name == "stage").unwrap();
        assert_eq!(uefi_stage.payload, "crabefi");
        assert!(uefi_stage.features.contains(&"crabefi".to_string()));
        let payload = uefi.assembly.payload.unwrap();
        assert!(matches!(payload.kind, PayloadKind::UefiPayload));
        assert!(payload.firmware.is_none());
        assert!(uefi.inputs.is_empty());
        for machine in [VirtMachine::SifiveU, VirtMachine::Unmatched] {
            // The legacy Linux defaults survive: kernel plus OpenSBI firmware
            // at the historic DRAM addresses with the SIF0 bootargs.
            let linux = resolve(machine, BuildSelection { payload: None }).unwrap();
            assert_eq!(linux.payload, "linux");
            let payload = linux.assembly.payload.unwrap();
            assert!(matches!(payload.kind, PayloadKind::LinuxBoot));
            assert_eq!(payload.kernel_file.unwrap().as_str(), "Image-riscv64");
            assert_eq!(payload.kernel_load_addr, Some(0x8400_0000));
            assert_eq!(payload.dtb_addr, Some(0x8f00_0000));
            assert_eq!(
                payload.bootargs.unwrap().as_str(),
                "console=ttySIF0 earlycon=sbi"
            );
            let firmware = payload.firmware.unwrap();
            assert!(matches!(firmware.kind, FirmwareKind::OpenSbi));
            assert_eq!(firmware.load_addr, 0x8300_0000);
            assert!(linux.inputs.iter().any(|i| i.name == "kernel"));
            assert!(linux.inputs.iter().any(|i| i.name == "firmware"));
            let UnitOutput::Executable { expectations, .. } = &linux.units[0].output else {
                panic!("not executable")
            };
            let layout = Layout::parse(&expectations.descriptor.bytes).unwrap();
            assert_eq!(layout.region(Kind::DeviceTree).unwrap().base, 0x8f00_0000);
            // UEFI keeps the OpenSBI firmware file like the legacy override.
            let uefi = resolve(
                machine,
                BuildSelection {
                    payload: Some("uefi".into()),
                },
            )
            .unwrap();
            assert!(uefi.assembly.payload.unwrap().firmware.is_some());
        }
        // Unmatched keeps its board DTB override; sifive-u uses the platform DTB.
        let unmatched = resolve(
            VirtMachine::Unmatched,
            BuildSelection {
                payload: Some("linux".into()),
            },
        )
        .unwrap()
        .assembly
        .payload
        .unwrap();
        assert!(matches!(
            unmatched.fdt,
            FdtSource::Override(name) if name.as_str() == "unmatched.dtb"
        ));
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
