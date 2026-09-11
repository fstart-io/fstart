//! Concrete Allwinner sunxi plans: an SRAM SPL plus a DRAM mainstage.
//!
//! Policy values are copied verbatim from the retired per-board `BoardConfig`
//! builders; only the build-policy boundary moved into shared platform code.
//! Hardware flows and the eGON finalizer contract are unchanged.
extern crate std;
use crate::facts::{BoardFacts, SunxiBoardFacts, SunxiSoc};
use fstart_core::layout::RegionKind as LayoutKind;
use fstart_core::{
    BoardBuildPolicy, BoardConfig, Compression, FdtSource, FirmwareConfig, MemoryMap, MemoryRegion,
    PayloadConfig, PayloadKind, Platform, RegionKind, RunsFrom, SocImageFormat, StageBuildConfig,
    StageConfig, StageLayout, dev_security_config, hstr, hvec,
};
use fstart_image_build::{
    build_plan::{
        Assembly, BootstrapRole, BuildPlan, CargoTarget, CompilationUnit, InputFile, UnitOutput,
    },
    linker::resolved_ram_stage,
    plan::{BuildSelection, Span},
};
use std::{
    collections::BTreeMap,
    string::{String, ToString},
    vec,
    vec::Vec,
};

/// Sunxi physical DRAM window. The controller detects the populated size live.
pub const DRAM_BASE: u64 = 0x4000_0000;
pub const DRAM_END: u64 = 0x8000_0000;
/// DRAM destination shared by every sunxi mainstage; the handoff buffer sits
/// one page below it.
pub const MAINSTAGE_LOAD_ADDR: u64 = 0x4100_0000;
pub const HANDOFF_ADDR: u64 = 0x40ff_f000;
pub const BOOTBLOCK_STACK_SIZE: u32 = 0x1000;
pub const MAINSTAGE_STACK_SIZE: u32 = 0x10000;

fn compiler_cfg_schema() -> fstart_image_build::build_plan::CompilerCfgSchema {
    let strings = |values: &[&str]| values.iter().map(|v| (*v).into()).collect();
    fstart_image_build::build_plan::CompilerCfgSchema {
        entries: strings(&["armv7", "aarch64", "riscv64"]),
        environments: strings(&["car", "ram"]),
        payloads: strings(&["halt", "linux"]),
    }
}

/// Conventional platform export consumed by the generic fbuild runner.
pub struct Plan<B>(core::marker::PhantomData<B>);
impl<B: SunxiBoardFacts> Plan<B> {
    pub fn emit(selection_json: &str) {
        let selection = serde_json::from_str(selection_json).expect("invalid build selection");
        let plan = resolve(B::FACTS, selection).expect("invalid sunxi board plan");
        std::println!(
            "{}",
            serde_json::to_string(&plan).expect("serialize sunxi plan")
        );
    }
}

struct SocPolicy {
    platform: Platform,
    target: &'static str,
    entry: &'static str,
    bundle_bootblock: &'static str,
    bundle_main: &'static str,
}

fn policy(soc: SunxiSoc) -> SocPolicy {
    match soc {
        SunxiSoc::A20 => SocPolicy {
            platform: Platform::Armv7,
            target: "armv7a-none-eabi",
            entry: "armv7",
            bundle_bootblock: "bundle-a20-bootblock",
            bundle_main: "bundle-a20-main",
        },
        SunxiSoc::H3 => SocPolicy {
            platform: Platform::Armv7,
            target: "armv7a-none-eabi",
            entry: "armv7",
            bundle_bootblock: "bundle-h3-bootblock",
            bundle_main: "bundle-h3-main",
        },
        SunxiSoc::H5 => SocPolicy {
            platform: Platform::Aarch64,
            target: "aarch64-unknown-none",
            entry: "aarch64",
            bundle_bootblock: "bundle-h5-bootblock",
            bundle_main: "bundle-h5-main",
        },
        SunxiSoc::D1 => SocPolicy {
            platform: Platform::Riscv64,
            target: "riscv64gc-unknown-none-elf",
            entry: "riscv64",
            bundle_bootblock: "bundle-d1-bootblock",
            bundle_main: "bundle-d1-main",
        },
    }
}

/// Shared board geometry as the retired builders declared it: two memory
/// regions, an SRAM SPL loading the DRAM mainstage, no compression, no heap.
fn board_config(facts: BoardFacts, policy: &SocPolicy, payload: &Option<PayloadConfig>) -> BoardConfig {
    BoardConfig {
        name: hstr("sunxi"),
        platform: policy.platform,
        memory: MemoryMap {
            regions: hvec([
                MemoryRegion {
                    name: hstr("sram"),
                    base: facts.sram_base,
                    size: facts.sram_size,
                    kind: RegionKind::Ram,
                },
                MemoryRegion {
                    name: hstr("dram"),
                    base: DRAM_BASE,
                    size: facts.dram_size,
                    kind: RegionKind::Ram,
                },
            ]),
            flash_layout: None,
            car: None,
        },
        stages: StageLayout::MultiStage(hvec([
            StageConfig {
                name: hstr("bootblock"),
                build: StageBuildConfig {
                    load_next_stage: Some(hstr("main")),
                    ..StageBuildConfig::default()
                },
                load_addr: facts.sram_base,
                stack_size: BOOTBLOCK_STACK_SIZE,
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
                    ..StageBuildConfig::default()
                },
                load_addr: MAINSTAGE_LOAD_ADDR,
                stack_size: MAINSTAGE_STACK_SIZE,
                heap_size: None,
                runs_from: RunsFrom::Ram,
                compression: Compression::None,
                data_addr: None,
                page_table_addr: None,
                page_size: Default::default(),
            },
        ])),
        security: dev_security_config("keys/dev-signing.pub"),
        payload: payload.clone(),
        microcode: None,
        soc_image_format: SocImageFormat::AllwinnerEgon,
        full_flash_image: false,
        build: BoardBuildPolicy {
            qemu_machine: facts.qemu_machine,
            ..Default::default()
        },
        acpi: None,
        smbios: None,
        smm: None,
        boot_hart_id: 0,
    }
}

fn payload_config(facts: BoardFacts) -> PayloadConfig {
    PayloadConfig {
        kind: PayloadKind::LinuxBoot,
        kernel_file: None,
        kernel_load_addr: Some(facts.kernel_load_addr),
        fdt: FdtSource::Override(hstr(facts.dtb)),
        dtb_addr: Some(facts.dtb_addr),
        src_dtb_addr: None,
        bootargs: Some(hstr(facts.bootargs)),
        print_x86_mtrrs: false,
        // ponytail: lz4 kernel decompression through block-backed media is
        // pathologically slow (byte-granular reads); store flat until the
        // block reader grows a buffered window.
        compression: Compression::None,
        firmware: facts.firmware.map(|firmware| FirmwareConfig {
            kind: firmware.kind,
            file: hstr(firmware.file),
            load_addr: firmware.load_addr,
        }),
        fit_file: None,
        fit_config: None,
        fit_parse: None,
    }
}

fn unit(
    name: &str,
    environment: &str,
    payload: &str,
    bundle: &str,
    policy: &SocPolicy,
    linker_script: String,
    output: UnitOutput,
) -> CompilationUnit {
    CompilationUnit {
        name: name.into(),
        cargo_target: CargoTarget::BoardBinary,
        target: policy.target.into(),
        entry: policy.entry.into(),
        cfg_schema: compiler_cfg_schema(),
        environment: environment.into(),
        payload: payload.into(),
        features: vec![bundle.into()],
        build_std: Some("core,alloc".into()),
        rustflags: vec!["-Zub-checks=no".into()],
        release_only: false,
        linker_script: Some(linker_script),
        environment_values: BTreeMap::new(),
        bindings: vec![],
        output,
    }
}

pub fn resolve(facts: BoardFacts, selection: BuildSelection) -> Result<BuildPlan, String> {
    let payload = selection.payload.unwrap_or_else(|| "halt".into());
    // The D1 mainstage is halt-only: no D1 Linux launcher exists. Sunxi has
    // no UEFI/FIT/Shell/ELF flow either.
    if !matches!(payload.as_str(), "halt" | "linux") {
        return Err("sunxi supports halt and direct Linux, not UEFI/FIT payloads".into());
    }
    if payload == "linux" && matches!(facts.soc, SunxiSoc::D1) {
        return Err("D1 mainstage is halt-only; no D1 Linux launcher exists".into());
    }
    let dram_end = DRAM_BASE
        .checked_add(facts.dram_size)
        .ok_or("DRAM window overflows")?;
    // Declared DRAM always starts at the fixed base; the linker sizes the
    // mainstage window from its actual end.
    if facts.dram_size == 0 || dram_end > DRAM_END || dram_end <= MAINSTAGE_LOAD_ADDR {
        return Err("sunxi DRAM must cover the fixed mainstage window".into());
    }
    let linux = payload == "linux";
    let policy = policy(facts.soc);
    let payload_config = linux.then(|| payload_config(facts));
    let config = board_config(facts, &policy, &payload_config);

    let sram = Span {
        base: facts.sram_base,
        size: facts.sram_size,
    };
    let boot_descriptor = fstart_image_build::layout::EncodedLayout::encode(
        0,
        &[
            sram.region(LayoutKind::Image),
            sram.region(LayoutKind::Writable),
        ],
    )
    .map_err(|e| e.to_string())?;
    let bootblock = resolved_ram_stage(&config, "bootblock", &boot_descriptor)?;

    let main_window = Span {
        base: MAINSTAGE_LOAD_ADDR,
        size: dram_end
            .checked_sub(MAINSTAGE_LOAD_ADDR)
            .ok_or("mainstage load address is outside DRAM")?,
    };
    let handoff = Span {
        base: HANDOFF_ADDR,
        size: fstart_core::handoff::HANDOFF_MAX_SIZE as u64,
    };
    let main_descriptor = fstart_image_build::layout::EncodedLayout::encode(
        1,
        &[
            main_window.region(LayoutKind::Image),
            main_window.region(LayoutKind::Writable),
            handoff.region(LayoutKind::Reserved),
        ],
    )
    .map_err(|e| e.to_string())?;
    let main = resolved_ram_stage(&config, "main", &main_descriptor)?;

    let StageLayout::MultiStage(stages) = config.stages.clone() else {
        return Err("sunxi plans always carry both stages".into());
    };
    let assembly = Assembly {
        platform: policy.platform,
        memory: vec![
            MemoryRegion {
                name: hstr("sram"),
                base: facts.sram_base,
                size: facts.sram_size,
                kind: RegionKind::Ram,
            },
            MemoryRegion {
                name: hstr("dram"),
                base: DRAM_BASE,
                size: facts.dram_size,
                kind: RegionKind::Ram,
            },
        ],
        flash: None,
        // The eGON finalizer patches the SPL bootstrap pin from the named
        // mainstage descriptor; the SPL itself carries no directory role.
        bootstrap: vec![("main".into(), BootstrapRole::Mainstage)],
        stages: StageLayout::MultiStage(stages),
        security: dev_security_config("keys/dev-signing.pub"),
        payload: payload_config,
        microcode: None,
        full_flash_image: false,
        soc_image_format: SocImageFormat::AllwinnerEgon,
        boot_hart_id: 0,
        build: BoardBuildPolicy {
            qemu_machine: facts.qemu_machine,
            ..Default::default()
        },
    };
    let mut inputs = Vec::new();
    if linux {
        // The kernel must not overlap the DTB scratch address; the assembler
        // loads both blobs verbatim at these fixed addresses.
        let kernel_capacity = facts
            .dtb_addr
            .checked_sub(facts.kernel_load_addr)
            .ok_or("kernel window underflows the DTB address")?;
        inputs.push(InputFile {
            name: "kernel".into(),
            default: None,
            capacity: kernel_capacity,
        });
        if let Some(firmware) = facts.firmware {
            inputs.push(InputFile {
                name: "firmware".into(),
                default: Some(firmware.file.into()),
                // CLI validation bound only; the assembler packs the actual
                // file bytes at the configured load address.
                capacity: 0x10_0000,
            });
        }
    }
    let bootblock_output = UnitOutput::Executable {
        expectations: bootblock.expectations,
        load_address: bootblock.memory.base,
        flat_capacity: bootblock.memory.size,
    };
    let main_output = UnitOutput::Executable {
        expectations: main.expectations,
        load_address: main.memory.base,
        flat_capacity: main.memory.size,
    };
    let plan = BuildPlan {
        payload: payload.clone(),
        units: vec![
            unit(
                "bootblock",
                "car",
                "halt",
                policy.bundle_bootblock,
                &policy,
                bootblock.linker_script,
                bootblock_output,
            ),
            unit(
                "main",
                "ram",
                &payload,
                policy.bundle_main,
                &policy,
                main.linker_script,
                main_output,
            ),
        ],
        stages: vec!["bootblock".into(), "main".into()],
        assembly,
        inputs,
    };
    plan.order()?;
    Ok(plan)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::facts::SunxiFirmware;
    use fstart_core::{FirmwareKind, QemuMachine};

    const BANANAPI: BoardFacts = BoardFacts {
        soc: SunxiSoc::A20,
        sram_base: 0,
        sram_size: 0x8000,
        dram_size: 0x4000_0000,
        kernel_load_addr: 0x4200_0000,
        dtb: "sun7i-a20-bananapi.dtb",
        dtb_addr: 0x4300_0000,
        bootargs: "earlycon console=ttyS0,115200",
        firmware: None,
        qemu_machine: None,
    };
    const ORANGEPI_PC2: BoardFacts = BoardFacts {
        soc: SunxiSoc::H5,
        sram_base: 0x0001_0000,
        sram_size: 0x8000,
        dram_size: 0x4000_0000,
        kernel_load_addr: 0x4a00_0000,
        dtb: "sun50i-h5-orangepi-pc2.dtb",
        dtb_addr: 0x4b00_0000,
        bootargs: "earlycon console=ttyS0,115200",
        firmware: Some(SunxiFirmware {
            kind: FirmwareKind::ArmTrustedFirmware,
            file: "bl31-sun50i-a64.bin",
            load_addr: 0x0004_4000,
        }),
        qemu_machine: None,
    };

    fn select(payload: &str) -> BuildSelection {
        BuildSelection {
            payload: Some(payload.into()),
        }
    }

    #[test]
    fn linux_plan_preserves_the_legacy_two_stage_geometry() {
        let plan = resolve(BANANAPI, select("linux")).unwrap();
        assert_eq!(plan.payload, "linux");
        assert_eq!(
            plan.stages,
            vec!["bootblock".to_string(), "main".to_string()]
        );
        let bootblock = plan.unit("bootblock").unwrap();
        assert_eq!(bootblock.target, "armv7a-none-eabi");
        assert_eq!(bootblock.environment, "car");
        assert_eq!(bootblock.payload, "halt");
        assert_eq!(bootblock.features, ["bundle-a20-bootblock"]);
        let main = plan.unit("main").unwrap();
        assert_eq!(main.environment, "ram");
        assert_eq!(main.payload, "linux");
        assert_eq!(main.features, ["bundle-a20-main"]);
        for unit in &plan.units {
            assert_eq!(unit.entry, "armv7");
            assert_eq!(unit.build_std.as_deref(), Some("core,alloc"));
        }
        let UnitOutput::Executable { load_address, flat_capacity, .. } = &main.output else {
            panic!("main is not executable")
        };
        assert_eq!(
            (*load_address, *flat_capacity),
            (MAINSTAGE_LOAD_ADDR, 0x8000_0000 - MAINSTAGE_LOAD_ADDR)
        );
        let assembly = &plan.assembly;
        assert_eq!(assembly.platform, Platform::Armv7);
        assert_eq!(assembly.soc_image_format, SocImageFormat::AllwinnerEgon);
        assert!(!assembly.full_flash_image);
        assert!(matches!(
            assembly.bootstrap.as_slice(),
            [(name, BootstrapRole::Mainstage)] if name == "main"
        ));
        let StageLayout::MultiStage(stages) = &assembly.stages else {
            panic!("sunxi assembly is not multistage")
        };
        assert_eq!(stages[0].load_addr, 0);
        assert_eq!(stages[0].stack_size, BOOTBLOCK_STACK_SIZE);
        assert_eq!(stages[1].load_addr, MAINSTAGE_LOAD_ADDR);
        assert_eq!(stages[1].stack_size, MAINSTAGE_STACK_SIZE);
        assert!(stages.iter().all(|s| s.compression == Compression::None));
        let payload = assembly.payload.as_ref().unwrap();
        assert_eq!(payload.kernel_load_addr, Some(0x4200_0000));
        assert_eq!(payload.dtb_addr, Some(0x4300_0000));
        assert!(payload.firmware.is_none());
        assert_eq!(plan.inputs.len(), 1);
        assert_eq!(plan.inputs[0].name, "kernel");
        assert_eq!(plan.inputs[0].capacity, 0x100_0000);
    }

    #[test]
    fn halt_has_no_payload_or_inputs_and_d1_rejects_linux() {
        let plan = resolve(BANANAPI, select("halt")).unwrap();
        assert!(plan.assembly.payload.is_none());
        assert!(plan.inputs.is_empty());
        assert_eq!(plan.unit("main").unwrap().payload, "halt");
        let dock = BoardFacts {
            soc: SunxiSoc::D1,
            sram_base: 0x0002_0000,
            sram_size: 0x2_0000,
            dram_size: 0x2000_0000,
            ..BANANAPI
        };
        let halt = resolve(dock, select("halt")).unwrap();
        assert_eq!(halt.unit("bootblock").unwrap().target, "riscv64gc-unknown-none-elf");
        assert!(resolve(dock, select("linux")).unwrap_err().contains("halt-only"));
        assert!(resolve(BANANAPI, select("uefi")).unwrap_err().contains("halt and direct Linux"));
    }

    #[test]
    fn h5_firmware_and_h3_qemu_machine_follow_the_legacy_boards() {
        let plan = resolve(ORANGEPI_PC2, select("linux")).unwrap();
        assert_eq!(plan.unit("bootblock").unwrap().target, "aarch64-unknown-none");
        assert_eq!(plan.unit("main").unwrap().features, ["bundle-h5-main"]);
        let payload = plan.assembly.payload.as_ref().unwrap();
        let firmware = payload.firmware.as_ref().unwrap();
        assert_eq!(firmware.load_addr, 0x0004_4000);
        assert_eq!(firmware.file.as_str(), "bl31-sun50i-a64.bin");
        assert_eq!(payload.kernel_load_addr, Some(0x4a00_0000));
        assert_eq!(payload.dtb_addr, Some(0x4b00_0000));
        assert_eq!(plan.inputs.len(), 2);
        assert_eq!(plan.inputs[1].name, "firmware");
        let r1 = BoardFacts {
            soc: SunxiSoc::H3,
            dram_size: 0x1000_0000,
            qemu_machine: Some(QemuMachine::OrangePiPc),
            ..BANANAPI
        };
        let plan = resolve(r1, select("halt")).unwrap();
        assert_eq!(plan.assembly.build.qemu_machine, Some(QemuMachine::OrangePiPc));
        assert_eq!(plan.unit("main").unwrap().features, ["bundle-h3-main"]);
        let UnitOutput::Executable { flat_capacity, .. } = &plan.unit("main").unwrap().output
        else {
            panic!("main is not executable")
        };
        assert_eq!(*flat_capacity, 0x5000_0000 - MAINSTAGE_LOAD_ADDR);
    }
}
