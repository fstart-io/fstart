//! Allwinner A20 (sun7i) Clock Control Unit — shared register definitions.
//!
//! This module defines the CCU register block and bitfield definitions
//! used by multiple drivers: the CCU driver itself, the DRAM controller
//! (which programs PLL5 and MBUS), and the MMC controller (which
//! configures its module clock and AHB gate).
//!
//! CCU register base: `0x01C2_0000`.

#![no_std]
#![allow(clippy::modulo_one)] // tock-registers alignment test

use fstart_mmio::MmioReadWrite;
use tock_registers::register_bitfields;
use tock_registers::register_structs;

// ===================================================================
// Register bitfield definitions
// ===================================================================

register_bitfields! [u32,
    /// PLL5 configuration — DRAM clock PLL.
    ///
    /// IMPORTANT: Several fields (M1, LDO, BW, BIAS, VCO_BIAS) have
    /// hardware-specific reset defaults that must be preserved. Always use
    /// `modify()` on this register — never `write()`.
    pub PLL5_CFG [
        /// Pre-divider M (actual = raw + 1).
        M OFFSET(0) NUMBITS(2) [],
        /// Secondary pre-divider M1 (actual = raw + 1). Undocumented,
        /// preserve the BROM default.
        M1 OFFSET(2) NUMBITS(2) [],
        /// Multiplier K (actual = raw + 1).
        K OFFSET(4) NUMBITS(2) [],
        /// PLL LDO enable. Must be preserved from BROM defaults.
        LDO OFFSET(7) NUMBITS(1) [],
        /// Multiplier N (actual value used directly).
        N OFFSET(8) NUMBITS(5) [],
        /// Post-divider P (divide by 2^P).
        P OFFSET(16) NUMBITS(2) [],
        /// PLL bandwidth control. Preserve BROM default.
        BW OFFSET(18) NUMBITS(1) [],
        /// VCO gain control.
        VCO_GAIN OFFSET(19) NUMBITS(1) [],
        /// PLL bias current (5-bit). Preserve BROM default.
        BIAS OFFSET(20) NUMBITS(5) [],
        /// VCO bias. Preserve BROM default.
        VCO_BIAS OFFSET(25) NUMBITS(1) [],
        /// Enable DDR clock output.
        DDR_CLK OFFSET(29) NUMBITS(1) [],
        /// PLL bypass mode.
        BYPASS OFFSET(30) NUMBITS(1) [],
        /// PLL enable.
        EN OFFSET(31) NUMBITS(1) []
    ],
    /// PLL6 configuration — peripheral PLL.
    pub PLL6_CFG [
        /// Multiplier K (actual = raw + 1).
        K OFFSET(4) NUMBITS(2) [],
        /// Multiplier N (actual = raw + 1).
        N OFFSET(8) NUMBITS(5) [],
        /// SATA clock output enable (bit 14).
        SATA_EN OFFSET(14) NUMBITS(1) [],
        /// PLL enable.
        EN OFFSET(31) NUMBITS(1) []
    ],
    /// MBUS clock configuration.
    pub MBUS_CLK [
        /// Clock divider M (actual = raw + 1).
        M OFFSET(0) NUMBITS(4) [],
        /// Clock divider N (actual = 2^raw).
        N OFFSET(16) NUMBITS(2) [],
        /// Clock source select: 0=HOSC, 1=PLL6, 2=PLL5P.
        CLK_SRC OFFSET(24) NUMBITS(2) [],
        /// Clock gate enable.
        GATE OFFSET(31) NUMBITS(1) []
    ],
    /// MMC module clock configuration (one per MMC controller).
    pub MMC_CLK [
        /// Clock divider M (actual = raw + 1).
        M OFFSET(0) NUMBITS(4) [],
        /// Output clock delay phase.
        OCLK_DLY OFFSET(8) NUMBITS(3) [],
        /// Pre-divider N (actual = 2^raw).
        N OFFSET(16) NUMBITS(2) [],
        /// Sample clock delay phase.
        SCLK_DLY OFFSET(20) NUMBITS(3) [],
        /// Clock source: 0=OSC24M, 1=PLL6.
        CLK_SRC OFFSET(24) NUMBITS(2) [
            Osc24M = 0,
            Pll6 = 1
        ],
        /// Module clock enable.
        ENABLE OFFSET(31) NUMBITS(1) []
    ],
    /// SPI module clock configuration (one per SPI controller).
    pub SPI_CLK [
        /// Clock divider M (actual = raw + 1).
        M OFFSET(0) NUMBITS(4) [],
        /// Pre-divider N (actual = 2^raw).
        N OFFSET(16) NUMBITS(2) [],
        /// Clock source: 0=OSC24M, 1=PLL6, 2=PLL5P.
        CLK_SRC OFFSET(24) NUMBITS(2) [
            Osc24M = 0,
            Pll6 = 1,
            Pll5P = 2
        ],
        /// Module clock enable.
        ENABLE OFFSET(31) NUMBITS(1) []
    ]
];

// ===================================================================
// Shared CCU register block
// ===================================================================

register_structs! {
    /// Allwinner A20 CCU register block.
    ///
    /// Covers the subset of registers used by the CCU, DRAM, and MMC
    /// drivers.  Registers not used are represented as padding.
    ///
    /// Base address: `0x01C2_0000`.
    pub SunxiA20CcuRegs {
        /// PLL1 (CPU PLL) configuration.
        (0x00 => pub pll1_cfg: MmioReadWrite<u32>),
        (0x04 => _res0: [u8; 0x1C]),
        /// PLL5 (DRAM PLL) configuration.
        (0x20 => pub pll5_cfg: MmioReadWrite<u32, PLL5_CFG::Register>),
        (0x24 => _res1: [u8; 0x04]),
        /// PLL6 (peripheral PLL) configuration.
        (0x28 => pub pll6_cfg: MmioReadWrite<u32, PLL6_CFG::Register>),
        (0x2C => _res2: [u8; 0x28]),
        /// CPU / AHB / APB0 clock divider configuration.
        (0x54 => pub cpu_ahb_apb0_cfg: MmioReadWrite<u32>),
        /// APB1 clock divider (UART clock source).
        (0x58 => pub apb1_clk_div: MmioReadWrite<u32>),
        (0x5C => _res3: [u8; 0x04]),
        /// AHB clock gating register 0 (SDRAM, DMA, MMC, SATA, etc.).
        (0x60 => pub ahb_gate0: MmioReadWrite<u32>),
        (0x64 => _res4: [u8; 0x08]),
        /// APB1 clock gating (UART gates).
        (0x6C => pub apb1_gate: MmioReadWrite<u32>),
        (0x70 => _res5: [u8; 0x18]),
        /// MMC module clock 0.
        (0x88 => pub mmc_clk0: MmioReadWrite<u32, MMC_CLK::Register>),
        /// MMC module clock 1.
        (0x8C => pub mmc_clk1: MmioReadWrite<u32, MMC_CLK::Register>),
        /// MMC module clock 2.
        (0x90 => pub mmc_clk2: MmioReadWrite<u32, MMC_CLK::Register>),
        /// MMC module clock 3.
        (0x94 => pub mmc_clk3: MmioReadWrite<u32, MMC_CLK::Register>),
        (0x98 => _res6a: [u8; 0x08]),
        /// SPI module clock 0 (SPI0).
        (0xA0 => pub spi0_clk: MmioReadWrite<u32, SPI_CLK::Register>),
        /// SPI module clock 1 (SPI1).
        (0xA4 => pub spi1_clk: MmioReadWrite<u32, SPI_CLK::Register>),
        /// SPI module clock 2 (SPI2).
        (0xA8 => pub spi2_clk: MmioReadWrite<u32, SPI_CLK::Register>),
        (0xAC => _res6b: [u8; 0x24]),
        /// GPS module clock configuration (reset control on sun7i).
        (0xD0 => pub gps_clk_cfg: MmioReadWrite<u32>),
        (0xD4 => _res7: [u8; 0x88]),
        /// MBUS clock configuration.
        (0x15C => pub mbus_clk_cfg: MmioReadWrite<u32, MBUS_CLK::Register>),
        (0x160 => @END),
    }
}

impl SunxiA20CcuRegs {
    /// Get a reference to the MMC module clock register by index (0-3).
    pub fn mmc_clk(&self, index: u8) -> &MmioReadWrite<u32, MMC_CLK::Register> {
        match index {
            0 => &self.mmc_clk0,
            1 => &self.mmc_clk1,
            2 => &self.mmc_clk2,
            3 => &self.mmc_clk3,
            _ => &self.mmc_clk0, // unreachable in practice
        }
    }

    /// Read PLL5P frequency in Hz (PLL5 with post-divider P).
    pub fn pll5p_freq(&self) -> u32 {
        use tock_registers::interfaces::Readable;
        let n = self.pll5_cfg.read(PLL5_CFG::N);
        let k = self.pll5_cfg.read(PLL5_CFG::K) + 1;
        let p = self.pll5_cfg.read(PLL5_CFG::P);
        (24_000_000 * n * k) >> p
    }

    /// Read PLL6 frequency in Hz (PLL6 output / 2).
    pub fn pll6_freq(&self) -> u32 {
        use tock_registers::interfaces::Readable;
        let n = self.pll6_cfg.read(PLL6_CFG::N) + 1;
        let k = self.pll6_cfg.read(PLL6_CFG::K) + 1;
        24_000_000 * n * k / 2
    }
}
