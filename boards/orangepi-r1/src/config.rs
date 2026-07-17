//! Orange Pi R1 static platform policy and host build metadata.

use fstart_core::Platform;
#[cfg(feature = "host")]
use fstart_core::{
    dev_security_config, hstr, hvec, BoardBuildPolicy, BoardConfig, Compression, FdtSource,
    MemoryMap, MemoryRegion, PayloadConfig, PayloadKind, QemuMachine, RegionKind, RunsFrom,
    SocImageFormat, StageBuildConfig, StageConfig, StageLayout,
};
use fstart_driver_sunxi::h3_ccu::H3CcuConfig;
use fstart_driver_sunxi::h3_dramc::H3DramcConfig;
use fstart_platform_sunxi::h3::H3Config;

pub const BOARD_NAME: &str = "orangepi-r1";
pub const BOARD_PACKAGE: &str = "fstart-board-orangepi-r1";
pub const PLATFORM: Platform = Platform::Armv7;
const MAINSTAGE_LOAD_ADDR: u64 = 0x4100_0000;
const HANDOFF_ADDR: u64 = 0x40ff_f000;

/// The attic's 408 MHz R1 policy; retain until validated against hardware.
pub static ORANGEPI_R1_H3: H3Config = H3Config::new(
    H3DramcConfig::new(408, 3_881_979, false),
    MAINSTAGE_LOAD_ADDR,
    HANDOFF_ADDR,
)
.ccu(H3CcuConfig::new(0))
.bootargs("earlycon=uart8250,mmio32,0x01c28000 console=ttyS0,115200")
.fdt_dst_addr(0x4400_0000)
.build();

#[cfg(feature = "host")]
#[must_use]
pub fn board_config() -> BoardConfig {
    BoardConfig {
        name: hstr(BOARD_NAME),
        platform: PLATFORM,
        memory: MemoryMap {
            regions: hvec([
                MemoryRegion {
                    name: hstr("sram"),
                    base: 0,
                    size: 0x8000,
                    kind: RegionKind::Ram,
                },
                MemoryRegion {
                    name: hstr("dram"),
                    base: 0x4000_0000,
                    size: 0x1000_0000,
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
                load_addr: 0,
                stack_size: 0x1000,
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
                stack_size: 0x10000,
                heap_size: None,
                runs_from: RunsFrom::Ram,
                compression: Compression::None,
                data_addr: None,
                page_table_addr: None,
                page_size: Default::default(),
            },
        ])),
        security: dev_security_config("keys/dev-signing.pub"),
        payload: Some(PayloadConfig {
            kind: PayloadKind::LinuxBoot,
            kernel_file: None,
            kernel_load_addr: Some(0x4200_0000),
            fdt: FdtSource::Override(hstr("sun8i-h2-plus-orangepi-r1.dtb")),
            dtb_addr: Some(0x4300_0000),
            src_dtb_addr: None,
            bootargs: Some(hstr("earlycon=uart8250,mmio32,0x01c28000 console=ttyS0,115200")),
            print_x86_mtrrs: false,
            // ponytail: lz4 kernel decompression through block-backed media is
            // pathologically slow (byte-granular reads); store flat until the
            // block reader grows a buffered window.
            compression: Compression::None,
            firmware: None,
            fit_file: None,
            fit_config: None,
            fit_parse: None,
        }),
        microcode: None,
        soc_image_format: SocImageFormat::AllwinnerEgon,
        full_flash_image: false,
        build: BoardBuildPolicy {
            qemu_machine: Some(QemuMachine::OrangePiPc),
            ..Default::default()
        },
        acpi: None,
        smbios: None,
        smm: None,
        boot_hart_id: 0,
    }
}
#[must_use]
pub const fn board_name() -> &'static str {
    BOARD_NAME
}
