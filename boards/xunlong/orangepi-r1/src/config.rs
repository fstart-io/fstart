//! Orange Pi R1 static platform policy and board facts.

use fstart_core::QemuMachine;
use fstart_driver_sunxi::h3_ccu::H3CcuConfig;
use fstart_driver_sunxi::h3_dramc::H3DramcConfig;
use fstart_platform_sunxi::facts::{BoardFacts, SunxiBoardFacts, SunxiSoc};
use fstart_platform_sunxi::h3::H3Config;

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

impl SunxiBoardFacts for crate::Board {
    const FACTS: BoardFacts = BoardFacts {
        soc: SunxiSoc::H3,
        sram_base: 0,
        sram_size: 0x8000,
        dram_size: 0x1000_0000,
        kernel_load_addr: 0x4200_0000,
        dtb: "sun8i-h2-plus-orangepi-r1.dtb",
        dtb_addr: 0x4300_0000,
        bootargs: "earlycon=uart8250,mmio32,0x01c28000 console=ttyS0,115200",
        firmware: None,
        qemu_machine: Some(QemuMachine::OrangePiPc),
    };
}
