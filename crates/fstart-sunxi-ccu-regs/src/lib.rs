//! Allwinner sunxi Clock Control Unit — shared register definitions.
//!
//! This module defines the CCU register blocks and bitfield definitions
//! used by multiple drivers: the CCU driver itself, the DRAM controller
//! (which programs PLL5 and MBUS), and the MMC controller (which
//! configures its module clock and AHB gate).
//!
//! Supports both A20 (sun7i) and H3/H2+ (sun8i) SoC families.
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

// ===================================================================
// H3/H2+ (sun8i) CCU register bitfield definitions
// ===================================================================

register_bitfields! [u32,
    /// H3 PLL_CPUX configuration (PLL1).
    ///
    /// freq = 24MHz * N * K / (M * P)
    pub H3_PLL_CPUX [
        /// Pre-divider M (actual = raw + 1).
        M OFFSET(0) NUMBITS(2) [],
        /// Multiplier K (actual = raw + 1).
        K OFFSET(4) NUMBITS(2) [],
        /// Multiplier N (actual = raw + 1).
        N OFFSET(8) NUMBITS(5) [],
        /// Post-divider P (actual = 2^raw).
        P OFFSET(16) NUMBITS(2) [],
        /// PLL enable.
        EN OFFSET(31) NUMBITS(1) []
    ],
    /// H3 PLL_DDR (PLL5) configuration — DRAM PLL.
    ///
    /// freq = 24MHz * N * K / M
    pub H3_PLL5_CFG [
        /// Pre-divider M (actual = raw + 1).
        M OFFSET(0) NUMBITS(2) [],
        /// Multiplier K (actual = raw + 1).
        K OFFSET(4) NUMBITS(2) [],
        /// Multiplier N (actual = raw + 1).
        N OFFSET(8) NUMBITS(5) [],
        /// PLL update trigger (self-clearing).
        UPD OFFSET(20) NUMBITS(1) [],
        /// Sigma-delta modulation enable.
        SIGMA_DELTA_EN OFFSET(24) NUMBITS(1) [],
        /// PLL enable.
        EN OFFSET(31) NUMBITS(1) []
    ],
    /// H3 PLL_PERIPH0 (PLL6) configuration — peripheral PLL.
    ///
    /// freq = 24MHz * N * K / 2
    pub H3_PLL6_CFG [
        /// Multiplier K (actual = raw + 1).
        K OFFSET(4) NUMBITS(2) [],
        /// Multiplier N (actual = raw + 1).
        N OFFSET(8) NUMBITS(5) [],
        /// PLL lock status (read-only). Polls to 1 when PLL is stable.
        LOCK OFFSET(28) NUMBITS(1) [],
        /// PLL enable.
        EN OFFSET(31) NUMBITS(1) []
    ],
    /// H3 DRAM clock configuration register (0xF4).
    pub H3_DRAM_CLK [
        /// Clock divider M (actual = raw + 1).
        M OFFSET(0) NUMBITS(4) [],
        /// Clock update trigger (self-clearing).
        UPD OFFSET(16) NUMBITS(1) [],
        /// Clock source: 0=PLL_DDR (PLL5), 1=PLL_DDR1.
        CLK_SRC OFFSET(20) NUMBITS(2) [],
        /// DRAM controller reset (active-high).
        RST OFFSET(31) NUMBITS(1) []
    ],
    /// H3 CCU security switch register (0x2F0).
    ///
    /// On H3, the BROM boots in secure mode. These bits must be set
    /// to allow non-secure access to clocks and bus controllers.
    pub H3_CCU_SEC_SWITCH [
        /// Allow non-secure access to PLL configuration.
        PLL_NONSEC OFFSET(0) NUMBITS(1) [],
        /// Allow non-secure access to bus clocks.
        BUS_NONSEC OFFSET(1) NUMBITS(1) [],
        /// Allow non-secure access to MBUS.
        MBUS_NONSEC OFFSET(2) NUMBITS(1) []
    ]
];

// ===================================================================
// H3/H2+ (sun8i) CCU register block
// ===================================================================

register_structs! {
    /// Allwinner H3/H2+ CCU register block (sun6i family).
    ///
    /// Covers the subset of registers used by the CCU, DRAM, and MMC
    /// drivers. The H3 has separate bus-reset registers (unlike the A20
    /// which only has gate registers).
    ///
    /// Base address: `0x01C2_0000`.
    pub SunxiH3CcuRegs {
        /// PLL_CPUX (CPU PLL) configuration.
        (0x000 => pub pll_cpux: MmioReadWrite<u32, H3_PLL_CPUX::Register>),
        (0x004 => _res0: [u8; 0x1C]),
        /// PLL_DDR (DRAM PLL, also called PLL5) configuration.
        (0x020 => pub pll5_cfg: MmioReadWrite<u32, H3_PLL5_CFG::Register>),
        (0x024 => _res1: [u8; 0x04]),
        /// PLL_PERIPH0 (peripheral PLL, also called PLL6) configuration.
        (0x028 => pub pll6_cfg: MmioReadWrite<u32, H3_PLL6_CFG::Register>),
        (0x02C => _res2: [u8; 0x24]),
        /// CPU / AXI clock divider configuration.
        (0x050 => pub cpu_axi_cfg: MmioReadWrite<u32>),
        /// AHB1 / APB1 clock divider configuration.
        (0x054 => pub ahb1_apb1_div: MmioReadWrite<u32>),
        /// APB2 clock divider (UART clock source on H3).
        (0x058 => pub apb2_div: MmioReadWrite<u32>),
        (0x05C => _res3: [u8; 0x04]),
        /// Bus clock gating register 0 (AHB1 gates: DMA, MMC, DRAM, etc.).
        (0x060 => pub bus_gate0: MmioReadWrite<u32>),
        /// Bus clock gating register 1.
        (0x064 => pub bus_gate1: MmioReadWrite<u32>),
        /// Bus clock gating register 2 (APB1 gates).
        (0x068 => pub bus_gate2: MmioReadWrite<u32>),
        /// Bus clock gating register 3 (APB2 gates: UART, I2C, etc.).
        (0x06C => pub bus_gate3: MmioReadWrite<u32>),
        (0x070 => _res4: [u8; 0x18]),
        /// MMC module clock 0.
        (0x088 => pub mmc_clk0: MmioReadWrite<u32, MMC_CLK::Register>),
        /// MMC module clock 1.
        (0x08C => pub mmc_clk1: MmioReadWrite<u32, MMC_CLK::Register>),
        /// MMC module clock 2.
        (0x090 => pub mmc_clk2: MmioReadWrite<u32, MMC_CLK::Register>),
        (0x094 => _res5: [u8; 0x0C]),
        /// SPI module clock 0.
        (0x0A0 => pub spi0_clk: MmioReadWrite<u32, SPI_CLK::Register>),
        /// SPI module clock 1.
        (0x0A4 => pub spi1_clk: MmioReadWrite<u32, SPI_CLK::Register>),
        (0x0A8 => _res6: [u8; 0x4C]),
        /// DRAM clock configuration (clock source, divider, reset).
        (0x0F4 => pub dram_clk_cfg: MmioReadWrite<u32, H3_DRAM_CLK::Register>),
        (0x0F8 => _res7: [u8; 0x04]),
        /// MBUS reset register.
        (0x0FC => pub mbus_reset: MmioReadWrite<u32>),
        /// DRAM gate register.
        (0x100 => pub dram_gate: MmioReadWrite<u32>),
        (0x104 => _res8a: [u8; 0x58]),
        /// MBUS module clock configuration (0x15C).
        (0x15C => pub mbus_clk_cfg: MmioReadWrite<u32, MBUS_CLK::Register>),
        (0x160 => _res8b: [u8; 0x160]),
        /// Bus soft-reset register 0 (AHB1 resets: DMA, MMC, DRAM, etc.).
        (0x2C0 => pub bus_reset0: MmioReadWrite<u32>),
        /// Bus soft-reset register 1.
        (0x2C4 => pub bus_reset1: MmioReadWrite<u32>),
        (0x2C8 => _res9: [u8; 0x10]),
        /// APB2 bus soft-reset register (UART, I2C resets).
        (0x2D8 => pub apb2_reset: MmioReadWrite<u32>),
        (0x2DC => _res10: [u8; 0x14]),
        /// CCU security switch register.
        (0x2F0 => pub ccu_sec_switch: MmioReadWrite<u32, H3_CCU_SEC_SWITCH::Register>),
        (0x2F4 => @END),
    }
}

impl SunxiH3CcuRegs {
    /// Get a reference to the MMC module clock register by index (0-2).
    pub fn mmc_clk(&self, index: u8) -> &MmioReadWrite<u32, MMC_CLK::Register> {
        match index {
            0 => &self.mmc_clk0,
            1 => &self.mmc_clk1,
            2 => &self.mmc_clk2,
            _ => &self.mmc_clk0, // unreachable in practice
        }
    }

    /// Read PLL6 (PERIPH0) frequency in Hz.
    ///
    /// PLL6 output = 24MHz * N * K / 2
    pub fn pll6_freq(&self) -> u32 {
        use tock_registers::interfaces::Readable;
        let n = self.pll6_cfg.read(H3_PLL6_CFG::N) + 1;
        let k = self.pll6_cfg.read(H3_PLL6_CFG::K) + 1;
        24_000_000 * n * k / 2
    }
}
