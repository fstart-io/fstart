//! Banana Pi M1 static platform policy and host build metadata.

#[cfg(feature = "host")]
use fstart_core::{
    dev_security_config, hstr, hvec, BoardConfig, Compression, FdtSource, MemoryMap,
    MemoryRegion, PayloadConfig, PayloadKind, RegionKind, RunsFrom, SocImageFormat,
    StageBuildConfig, StageConfig, StageLayout,
};
use fstart_core::Platform;
use fstart_driver_sunxi::a20_ccu::{A20CcuConfig, A20Uart};
use fstart_driver_sunxi::a20_dramc::A20DramcConfig;
use fstart_platform_sunxi::a20::A20Config;

pub const BOARD_NAME: &str = "bananapi-m1";
pub const BOARD_PACKAGE: &str = "fstart-board-bananapi-m1";
pub const PLATFORM: Platform = Platform::Armv7;

const MAINSTAGE_LOAD_ADDR: u64 = 0x4100_0000;
const HANDOFF_ADDR: u64 = MAINSTAGE_LOAD_ADDR - 0x1000;

/// Banana Pi M1's known-good A20 DDR3 timing and calibration policy.
pub static BANANAPI_M1_A20: A20Config = A20Config::new(
    A20DramcConfig::new(
        432,
        0,
        123,
        false,
        6,
        0x3092_6692,
        0x1090,
        0x0001_a0c8,
        0,
        0,
        4,
        0,
        0,
        0,
        false,
    ),
    MAINSTAGE_LOAD_ADDR,
    HANDOFF_ADDR,
)
.ccu(A20CcuConfig::new(A20Uart::Uart0))
.bootargs("earlycon console=ttyS0,115200")
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
                    size: 0x4000_0000,
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
                compression: fstart_core::Compression::None,
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
                compression: fstart_core::Compression::None,
                data_addr: None,
                page_table_addr: None,
                page_size: Default::default(),
            },
        ])),
        security: dev_security_config("keys/dev-signing.pub"),
        // Host defaults only: fbuild may replace the boot mode or input files.
        payload: Some(PayloadConfig {
            kind: PayloadKind::LinuxBoot,
            kernel_file: None,
            kernel_load_addr: Some(0x4200_0000),
            fdt: FdtSource::Override(hstr("sun7i-a20-bananapi.dtb")),
            dtb_addr: Some(0x4300_0000),
            src_dtb_addr: None,
            bootargs: Some(hstr("earlycon console=ttyS0,115200")),
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
        build: Default::default(),
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
