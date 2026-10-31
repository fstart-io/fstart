//! HiFive Unmatched: its DDR, where the FFS sits on the SPI flash, the
//! payload addresses and the console UART. The firmware and the host build
//! plan both read these.

use fstart_core::mmio32;
use fstart_driver_uart::sifive::SifiveUartConfig;
use fstart_platform_sifive::{Fu740Board, Fu740Config, Fu740DdrConfig, Fu740PrciConfig};

use crate::ddr_regs::{HIFIVE_UNMATCHED_DENALI_CTL, HIFIVE_UNMATCHED_DENALI_PHY};

impl Fu740Board for crate::Board {
    const CONFIG: &'static Fu740Config = &HIFIVE_UNMATCHED;
    const DEVICE_TREE: &'static str = "unmatched.dtb";
}

/// 16 GiB of DDR4, the SPI flash's FFS window and the payload placement.
pub static HIFIVE_UNMATCHED: Fu740Config = Fu740Config {
    prci: Fu740PrciConfig::new(26_000_000),
    ddr: Fu740DdrConfig::new(
        &HIFIVE_UNMATCHED_DENALI_CTL,
        &HIFIVE_UNMATCHED_DENALI_PHY,
        0x4_0000_0000,
    ),
    firmware_base: 0x2000_0000,
    firmware_size: 0x0200_0000,
    dtb_addr: 0x8f00_0000,
    kernel_addr: 0x8400_0000,
    firmware_addr: 0x8300_0000,
    boot_hart_id: 1,
    bootargs: "console=ttySIF0 earlycon=sbi",
}
.build();

/// UART0 at 115200 baud from the 130 MHz peripheral clock.
pub static HIFIVE_UNMATCHED_UART: SifiveUartConfig =
    SifiveUartConfig::new(mmio32(0x1001_0000), 130_000_000, 115_200);
