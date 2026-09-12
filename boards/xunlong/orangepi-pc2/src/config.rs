//! Orange Pi PC2 static platform policy and board facts.

use fstart_core::FirmwareKind;
use fstart_driver_sunxi::h3_ccu::H3CcuConfig;
use fstart_driver_sunxi::h3_dramc::{H3DramcConfig, SunxiDramcVariant};
use fstart_platform_sunxi::facts::{BoardFacts, SunxiBoardFacts, SunxiFirmware, SunxiSoc};
use fstart_platform_sunxi::h3::H3Config;

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

impl SunxiBoardFacts for crate::Board {
    const FACTS: BoardFacts = BoardFacts {
        soc: SunxiSoc::H5,
        sram_base: SRAM_BASE,
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
}
