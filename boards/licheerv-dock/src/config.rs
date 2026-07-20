//! Lichee RV Dock static platform policy and host build metadata.

use fstart_core::Platform;
#[cfg(feature = "host")]
use fstart_core::{
    dev_security_config, hstr, hvec, BoardBuildPolicy, BoardConfig, Compression, FdtSource,
    MemoryMap, MemoryRegion, PayloadConfig, PayloadKind, RegionKind, RunsFrom, SocImageFormat,
    StageBuildConfig, StageConfig, StageLayout,
};
use fstart_driver_sunxi::d1_ccu::D1CcuConfig;
use fstart_driver_sunxi::d1_dramc::D1DramcConfig;
use fstart_platform_sunxi::d1::D1Config;

pub const BOARD_NAME: &str = "licheerv-dock";
pub const BOARD_PACKAGE: &str = "fstart-board-licheerv-dock";
pub const PLATFORM: Platform = Platform::Riscv64;
/// D1 BROM loads the eGON image into SRAM at 0x20000.
#[cfg(feature = "host")]
const SRAM_BASE: u64 = 0x0002_0000;
const MAINSTAGE_LOAD_ADDR: u64 = 0x4100_0000;
const HANDOFF_ADDR: u64 = MAINSTAGE_LOAD_ADDR - 0x1000;

/// The attic's 792 MHz D1 DDR3 policy; retain until validated on hardware.
pub static LICHEERV_DOCK_D1: D1Config = D1Config::new(
    D1DramcConfig {
        dram_clk: 792,
        dram_type: 3,
        dram_zq: 0x007b_7bfb,
        dram_odt_en: 0,
        dram_mr1: 0x42,
        dram_tpr11: 0x0034_0000,
        dram_tpr12: 0x46,
        dram_tpr13: 0x3400_0100,
    },
    MAINSTAGE_LOAD_ADDR,
    HANDOFF_ADDR,
)
.ccu(D1CcuConfig::new(0))
.bootargs("console=ttyS0,115200 earlycon=sbi loglevel=8 rootwait")
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
                    base: SRAM_BASE,
                    size: 0x2_0000,
                    kind: RegionKind::Ram,
                },
                MemoryRegion {
                    name: hstr("dram"),
                    base: 0x4000_0000,
                    size: 0x2000_0000,
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
                load_addr: SRAM_BASE,
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
            fdt: FdtSource::Override(hstr("sun20i-d1-lichee-rv-dock.dtb")),
            dtb_addr: Some(0x47f0_0000),
            src_dtb_addr: None,
            bootargs: Some(hstr(
                "console=ttyS0,115200 earlycon=sbi loglevel=8 rootwait",
            )),
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
        build: BoardBuildPolicy::default(),
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
