//! HiFive Unmatched static policy and board facts.
//!
//! Platform Rust owns fixed image budgets; the FU740 SoC policy below stays
//! board-local and is evaluated by the fixed board flow.

use fstart_core::mmio32;
use fstart_driver_uart::sifive::SifiveUartConfig;
use fstart_platform_qemu::facts::{VirtBoardFacts, VirtMachine};

impl VirtBoardFacts for crate::Board {
    const MACHINE: VirtMachine = VirtMachine::Unmatched;
}

const LIM_BASE: u64 = 0x0800_0000;
const LIM_SIZE: u64 = 0x0020_0000;
const SPI_XIP_BASE: u64 = 0x2000_0000;
const SPI_XIP_SIZE: u64 = 0x0200_0000;
const DRAM_BASE: u64 = 0x8000_0000;
const DRAM_SIZE: u64 = 0x4_0000_0000;
const DTB_ADDR: u64 = 0x8f00_0000;
const KERNEL_ADDR: u64 = 0x8400_0000;
const FIRMWARE_ADDR: u64 = 0x8300_0000;
const BOOTARGS: &str = "console=ttySIF0 earlycon=sbi";

/// DDR register profile supported by the fixed FU740 flow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Fu740DdrProfile {
    /// HiFive Unmatched DIMM/controller values from SiFive's reference setup.
    HiFiveUnmatched,
}

/// Static FU740 DDR policy.
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fu740DdrConfig {
    pub profile: Fu740DdrProfile,
    pub dram_size: u64,
}

impl Fu740DdrConfig {
    #[must_use]
    pub const fn hifive_unmatched(dram_size: u64) -> Self {
        Self {
            profile: Fu740DdrProfile::HiFiveUnmatched,
            dram_size,
        }
    }

    #[must_use]
    pub const fn build(self) -> Self {
        if self.dram_size < 0x4000 || self.dram_size > u64::MAX - DRAM_BASE {
            panic!("FU740 DRAM window is invalid");
        }
        self
    }
}

/// Static FU740 PRCI policy.
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fu740PrciConfig {
    pub reference_clock_hz: u32,
}

impl Fu740PrciConfig {
    #[must_use]
    pub const fn hifive_unmatched() -> Self {
        Self {
            reference_clock_hz: 26_000_000,
        }
    }

    #[must_use]
    pub const fn build(self) -> Self {
        if self.reference_clock_hz == 0 {
            panic!("FU740 reference clock must not be zero");
        }
        self
    }
}

/// Closed FU740 policy consumed by the fixed early flow.
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
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
    pub const fn new(
        ddr: Fu740DdrConfig,
        firmware_base: u64,
        firmware_size: u64,
        dtb_addr: u64,
        kernel_addr: u64,
        firmware_addr: u64,
        bootargs: &'static str,
    ) -> Self {
        Self {
            prci: Fu740PrciConfig::hifive_unmatched(),
            ddr,
            firmware_base,
            firmware_size,
            dtb_addr,
            kernel_addr,
            firmware_addr,
            boot_hart_id: 1,
            bootargs,
        }
    }

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
        if self.kernel_addr < DRAM_BASE
            || self.firmware_addr < DRAM_BASE
            || self.dtb_addr < DRAM_BASE
        {
            panic!("FU740 payload addresses must be in DRAM");
        }
        self
    }
}

/// Unmatched's DDR profile and SPI FFS window, retained in ROM.
pub static HIFIVE_UNMATCHED: Fu740Config = Fu740Config::new(
    Fu740DdrConfig::hifive_unmatched(DRAM_SIZE),
    SPI_XIP_BASE,
    SPI_XIP_SIZE,
    DTB_ADDR,
    KERNEL_ADDR,
    FIRMWARE_ADDR,
    BOOTARGS,
)
.build();

/// Unmatched UART wiring and precomputed divisor, retained in ROM.
pub static HIFIVE_UNMATCHED_UART: SifiveUartConfig =
    SifiveUartConfig::new(mmio32(0x1001_0000), 130_000_000, 115_200);
