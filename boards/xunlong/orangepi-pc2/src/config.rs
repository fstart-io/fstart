//! Orange Pi PC2 static platform policy and host build metadata.

use fstart_core::Platform;
#[cfg(feature = "host")]
use fstart_core::{
    dev_security_config, hstr, hvec, BoardBuildPolicy, BoardConfig, Compression, FdtSource,
    FirmwareConfig, FirmwareKind, MemoryMap, MemoryRegion, PayloadConfig, PayloadKind, RegionKind,
    RunsFrom, SocImageFormat, StageBuildConfig, StageConfig, StageLayout,
};
use fstart_driver_sunxi::h3_ccu::H3CcuConfig;
use fstart_driver_sunxi::h3_dramc::{H3DramcConfig, SunxiDramcVariant};
use fstart_platform_sunxi::h3::H3Config;

pub const BOARD_NAME: &str = "orangepi-pc2";
pub const BOARD_PACKAGE: &str = "fstart-board-orangepi-pc2";
pub const PLATFORM: Platform = Platform::Aarch64;
/// H5 BROM loads the eGON image into SRAM A1 at 0x10000.
const SRAM_BASE: u64 = 0x0001_0000;
const MAINSTAGE_LOAD_ADDR: u64 = 0x4100_0000;
const HANDOFF_ADDR: u64 = MAINSTAGE_LOAD_ADDR - 0x1000;

/// The attic's 672 MHz H5 policy; retain until validated against hardware.
pub static ORANGEPI_PC2_H5: H3Config = H3Config::new(
    H3DramcConfig::new(672, 3_881_977, true).variant(SunxiDramcVariant::H5),
    MAINSTAGE_LOAD_ADDR,
    HANDOFF_ADDR,
)
.ccu(H3CcuConfig::new(0))
.sram_base(SRAM_BASE)
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
                    base: SRAM_BASE,
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
            kernel_load_addr: Some(0x4a00_0000),
            fdt: FdtSource::Override(hstr("sun50i-h5-orangepi-pc2.dtb")),
            dtb_addr: Some(0x4b00_0000),
            src_dtb_addr: None,
            bootargs: Some(hstr("earlycon console=ttyS0,115200")),
            print_x86_mtrrs: false,
            // ponytail: lz4 kernel decompression through block-backed media is
            // pathologically slow (byte-granular reads); store flat until the
            // block reader grows a buffered window.
            compression: Compression::None,
            firmware: Some(FirmwareConfig {
                kind: FirmwareKind::ArmTrustedFirmware,
                file: hstr("bl31-sun50i-a64.bin"),
                load_addr: 0x0004_4000,
            }),
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
