//! What a board tells the FU740 flow, readable on the host too.

use fstart_core::description::Range;
use fstart_driver_sifive::fu740_ddr::{FU740_DRAM_BASE, Fu740DdrConfig};
use fstart_driver_sifive::fu740_prci::Fu740PrciConfig;

/// FU740 PCIe ECAM aperture.
pub const FU740_ECAM_BASE: u64 = 0x3000_0000;

/// FU740 device MMIO a UEFI payload maps: CLINT, PLIC, the peripheral block
/// (PRCI, UARTs, SPI, GPIO, Ethernet, DDR controller) and the PCIe 32-bit
/// window.
pub const FU740_MMIO: [Range; 4] = [
    Range::new(0x0200_0000, 0x1_0000),
    Range::new(0x0c00_0000, 0x0400_0000),
    Range::new(0x1000_0000, 0x0010_0000),
    Range::new(0x6000_0000, 0x2000_0000),
];

/// One FU740 board: clocks, DDR, where the FFS sits on the SPI flash and
/// where the payload goes.
#[derive(Debug, Clone, Copy)]
pub struct Fu740Config {
    pub prci: Fu740PrciConfig,
    pub ddr: Fu740DdrConfig,
    pub firmware_base: u64,
    pub firmware_size: u64,
    pub dtb_addr: u64,
    pub kernel_addr: u64,
    pub firmware_addr: u64,
    pub boot_hart_id: u64,
    pub bootargs: &'static str,
}

impl Fu740Config {
    #[must_use]
    pub const fn build(self) -> Self {
        let _ = self.prci.build();
        let _ = self.ddr.build();
        if self.firmware_size == 0 || self.firmware_base.checked_add(self.firmware_size).is_none() {
            panic!("FU740 firmware window is invalid");
        }
        if self.boot_hart_id != 1 {
            panic!("FU740 must boot on U74 hart 1");
        }
        if self.kernel_addr < FU740_DRAM_BASE
            || self.firmware_addr < FU740_DRAM_BASE
            || self.dtb_addr < FU740_DRAM_BASE
        {
            panic!("FU740 payload addresses must be in DRAM");
        }
        self
    }
}
