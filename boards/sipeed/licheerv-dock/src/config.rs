//! Lichee RV Dock static platform policy and board facts.

use fstart_driver_sunxi::d1_ccu::D1CcuConfig;
use fstart_driver_sunxi::d1_dramc::D1DramcConfig;
use fstart_platform_sunxi::d1::D1Config;
use fstart_platform_sunxi::facts::{BoardFacts, SunxiBoardFacts, SunxiSoc};

/// D1 BROM loads the eGON image into SRAM at 0x20000.
const SRAM_BASE: u64 = 0x0002_0000;
const SRAM_SIZE: u64 = 0x2_0000;
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

impl SunxiBoardFacts for crate::Board {
    const FACTS: BoardFacts = BoardFacts {
        soc: SunxiSoc::D1,
        sram_base: SRAM_BASE,
        sram_size: SRAM_SIZE,
        dram_size: 0x2000_0000,
        kernel_load_addr: 0x4200_0000,
        dtb: "sun20i-d1-lichee-rv-dock.dtb",
        dtb_addr: 0x47f0_0000,
        bootargs: "console=ttyS0,115200 earlycon=sbi loglevel=8 rootwait",
        firmware: None,
        qemu_machine: None,
    };
}
