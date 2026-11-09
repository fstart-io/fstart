//! The FU740 build plan: a bootblock running from the L2 LIM and a
//! mainstage it authenticates into DRAM, the FFS on the SPI flash, and
//! the payload (Linux or CrabEFI with OpenSBI) in DRAM. Addresses come
//! from the board's [`Fu740Config`], the one the firmware reads.

extern crate std;

use fstart_core::layout::RegionKind as Kind;
use fstart_core::*;
use fstart_image_build::build_plan::{
    BootstrapRole, BuildPlan, CargoTarget, CompilationUnit, FfsAssembly as Assembly, InputFile,
    UnitOutput,
};
use fstart_image_build::layout::EncodedLayout;
use fstart_image_build::linker::{RamStage, resolved_ram};
use fstart_image_build::plan::{BuildSelection, Span};
use std::{
    collections::BTreeMap,
    string::{String, ToString},
    vec,
    vec::Vec,
};

use crate::{FU740_DRAM_BASE, Fu740Board, Fu740Config};

/// The L2 LIM aperture at reset: way zero is always cache, so only the
/// remaining 15 of 16 ways (1.875 MiB of the 2 MiB window) back it.
const LIM: Span = span(0x0800_0000, 0x001e_0000);
const BOOT_STACK_SIZE: u64 = 0x4000;
const BOOT_HEAP_SIZE: u64 = 0x4000;
/// The DRAM window the mainstage is loaded into and runs from.
const MAIN: Span = span(0x8100_0000, 0x0200_0000);
const MAIN_STACK_SIZE: u64 = 0x1_0000;
const MAIN_HEAP_SIZE: u64 = 0x10_0000;
/// Serialized bootblock-to-mainstage handoff at the start of DRAM.
const HANDOFF: Span = span(
    FU740_DRAM_BASE,
    fstart_core::handoff::HANDOFF_MAX_SIZE as u64,
);
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

/// One stage's linker script and executable output: its layout
/// descriptor carries its own regions first, then the payload
/// destinations both stages agree on.
fn stage(
    config: &Fu740Config,
    memory: Span,
    heap_size: u64,
    stack_size: u64,
    index: u16,
    own: &[fstart_core::layout::Region],
    payload_regions: &[fstart_core::layout::Region],
) -> Result<(String, UnitOutput), String> {
    let stack = Span {
        base: memory.end()? - stack_size,
        size: stack_size,
    };
    let heap = Span {
        base: stack.base - heap_size,
        size: heap_size,
    };
    let mut regions = vec![
        memory.region(Kind::Image),
        memory.region(Kind::Writable),
        stack.region(Kind::Stack),
        heap.region(Kind::Heap),
        HANDOFF.region(Kind::Reserved),
    ];
    regions.extend_from_slice(own);
    regions.extend_from_slice(payload_regions);
    let layout = RamStage {
        platform: Platform::Riscv64,
        boot_hart_id: u32::try_from(config.boot_hart_id).map_err(|e| e.to_string())?,
        memory,
        heap,
        stack,
        descriptor: EncodedLayout::encode(index, &regions).map_err(|e| e.to_string())?,
    };
    Ok((
        resolved_ram(&layout)?,
        UnitOutput::Executable {
            expectations: layout.elf_expectations()?,
            load_address: memory.base,
            flat_capacity: memory.size,
        },
    ))
}

fn unit(
    name: &str,
    features: Vec<String>,
    linker_script: String,
    output: UnitOutput,
) -> CompilationUnit {
    CompilationUnit {
        name: name.into(),
        cargo_target: CargoTarget::BoardBinary,
        target: "riscv64gc-unknown-none-elf".into(),
        features,
        build_std: Some("core,alloc".into()),
        rustflags: vec!["-Zub-checks=no".into()],
        release_only: false,
        linker_script: Some(linker_script),
        environment_values: BTreeMap::new(),
        bindings: vec![],
        file_bindings: vec![],
        board_features: vec![],
        output,
    }
}

/// The plan for `config` and the payload `selection` names.
pub fn resolve(
    config: &Fu740Config,
    device_tree: &'static str,
    selection: BuildSelection,
) -> Result<BuildPlan, String> {
    selection.reject_debug()?;
    let payload = selection.payload.unwrap_or_else(|| "linux".into());
    if !matches!(payload.as_str(), "halt" | "linux" | "uefi") {
        return Err("FU740 supports halt, Linux and UEFI".into());
    }
    let linux = payload == "linux";
    let keep_firmware = payload != "halt";
    let firmware_image = span(config.firmware_base, config.firmware_size);
    let dram = span(FU740_DRAM_BASE, config.ddr.dram_size);
    for range in [MAIN, HANDOFF] {
        if !dram.contains(range.base, range.size) {
            return Err("FU740 stage window outside populated DRAM".into());
        }
    }

    // Payload destinations both stages see; overlap with either stage's
    // footprint or the handoff is rejected by the common plan validation.
    let mut payload_regions = vec![
        firmware_image.region(Kind::Flash),
        firmware_image.region(Kind::Firmware),
    ];
    if linux {
        payload_regions.push(span(config.kernel_addr, KERNEL_CAPACITY).region(Kind::Payload));
        payload_regions.push(span(config.dtb_addr, DTB_CAPACITY).region(Kind::DeviceTree));
    }
    if keep_firmware {
        payload_regions
            .push(span(config.firmware_addr, OPENSBI_CAPACITY).region(Kind::PayloadFirmware));
    }
    if payload == "uefi" {
        payload_regions.push(span(config.dtb_addr, DTB_CAPACITY).region(Kind::DeviceTree));
    }
    let (boot_script, boot_output) = stage(
        config,
        LIM,
        BOOT_HEAP_SIZE,
        BOOT_STACK_SIZE,
        0,
        &[MAIN.region(Kind::BootstrapMainstage)],
        &payload_regions,
    )?;
    let (main_script, main_output) = stage(
        config,
        MAIN,
        MAIN_HEAP_SIZE,
        MAIN_STACK_SIZE,
        1,
        &[],
        &payload_regions,
    )?;

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
            compression: crate::compression::PAYLOAD,
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
            compression: crate::compression::PAYLOAD,
            firmware: Some(opensbi()),
            fit_file: None,
            fit_config: None,
            fit_parse: None,
        }),
        _ => None,
    };
    let boot_hart_id = u32::try_from(config.boot_hart_id).map_err(|e| e.to_string())?;
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
        // The assembler patches the named mainstage descriptor into the
        // bootblock's bootstrap pin; the bootblock itself has no role.
        bootstrap: vec![("main".into(), BootstrapRole::Mainstage)],
        stages: StageLayout::MultiStage(hvec([
            StageConfig {
                name: hstr("bootblock"),
                build: StageBuildConfig {
                    load_next_stage: Some(hstr("main")),
                    ..Default::default()
                },
                load_addr: LIM.base,
                stack_size: BOOT_STACK_SIZE as u32,
                heap_size: Some(BOOT_HEAP_SIZE as u32),
                runs_from: RunsFrom::Ram,
                compression: Compression::None,
                data_addr: None,
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
                stack_size: MAIN_STACK_SIZE as u32,
                heap_size: Some(MAIN_HEAP_SIZE as u32),
                runs_from: RunsFrom::Ram,
                compression: crate::compression::MAIN_STAGE,
                data_addr: None,
            },
        ])),
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
    let bootblock = unit(
        "bootblock",
        vec!["bundle-fu740".into()],
        boot_script,
        boot_output,
    );
    let mut main_features = vec!["bundle-fu740".into()];
    match payload.as_str() {
        "linux" => main_features.push("linux".into()),
        "uefi" => main_features.push("crabefi".into()),
        _ => {}
    }
    let main = unit("main", main_features, main_script, main_output);
    let plan = BuildPlan {
        payload,
        units: vec![bootblock, main],
        stages: vec!["bootblock".into(), "main".into()],
        assembly: assembly.into(),
        inputs,
    };
    plan.order()?;
    Ok(plan)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Fu740DdrConfig, Fu740PrciConfig, PllConfig, PllSettings};

    static CTL: [u32; 265] = [0; 265];
    static PHY: [u32; 1215] = [0; 1215];
    static CONFIG: Fu740Config = Fu740Config {
        prci: Fu740PrciConfig::new(
            24_000_000,
            PllConfig {
                core: PllSettings::new(0, 80, 2, 4),
                ddr: PllSettings::new(0, 75, 2, 4),
                gemgxl: PllSettings::new(0, 80, 5, 4),
                hfpclk: PllSettings::new(1, 42, 2, 4),
                cltx: PllSettings::new(1, 42, 2, 4),
            },
        ),
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
