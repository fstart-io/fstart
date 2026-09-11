//! Banana Pi M1 static platform policy and board facts.

use fstart_driver_sunxi::a20_ccu::{A20CcuConfig, A20Uart};
use fstart_driver_sunxi::a20_dramc::A20DramcConfig;
use fstart_platform_sunxi::a20::A20Config;
use fstart_platform_sunxi::facts::{BoardFacts, SunxiBoardFacts, SunxiSoc};

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

impl SunxiBoardFacts for crate::Board {
    const FACTS: BoardFacts = BoardFacts {
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
}
