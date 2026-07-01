//! Allwinner sunxi Clock Control Unit — shared register definitions.
//!
//! This module defines the CCU register blocks and bitfield definitions
//! used by multiple drivers: the CCU driver itself, the DRAM controller
//! (which programs PLL5 and MBUS), and the MMC controller (which
//! configures its module clock and AHB gate).
//!
//! Supports A20 (sun7i), H3/H2+ (sun8i), and D1/T113 (sun20i) SoC families.
//!
//! CCU register bases:
//! - A20/H3: `0x01C2_0000`
//! - D1/T113: `0x0200_1000`

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

// ===================================================================
// D1/T113 (sun20i) CCU register bitfield definitions
// ===================================================================

register_bitfields! [u32,
    /// D1 PLL_CPUX configuration.
    ///
    /// freq = 24MHz * N  (N in bits [15:8], raw = N-1)
    ///
    /// Bits 31/30/29/27: EN, LDO_EN, LOCK_EN, OUT_EN.
    /// Bit 28: LOCK (read-only).
    pub D1_PLL_CPUX [
        /// Multiplier N (actual = raw + 1). Range: 12..90.
        N OFFSET(8) NUMBITS(8) [],
        /// Output gating enable (required for NCAT2).
        OUT_EN OFFSET(27) NUMBITS(1) [],
        /// Lock status (read-only). Polls to 1 when PLL is stable.
        LOCK OFFSET(28) NUMBITS(1) [],
        /// Lock enable — must be set for LOCK bit to function.
        LOCK_EN OFFSET(29) NUMBITS(1) [],
        /// LDO enable — must be set for NCAT2 PLLs.
        LDO_EN OFFSET(30) NUMBITS(1) [],
        /// PLL enable.
        EN OFFSET(31) NUMBITS(1) []
    ],
    /// D1 PLL_DDR0 configuration.
    ///
    /// freq = 24MHz * N  (DRAM clock = PLL_DDR0 / 2)
    ///
    /// Note: bits [10:8] (M0 pre-divider) and [1:0] (M1 pre-divider)
    /// must be zero for DDR3 operation. The N field at [15:8] covers
    /// the M0 bits; this is safe as long as M0 = 0 (which it must be).
    pub D1_PLL_DDR0 [
        /// Pre-divider M1 (bits [1:0]). Must be cleared for DDR3.
        M1 OFFSET(0) NUMBITS(2) [],
        /// Multiplier N (actual = raw + 1). Bits [15:8].
        /// Includes the M0 field at [10:8] which must be 0.
        N OFFSET(8) NUMBITS(8) [],
        /// Output gating enable.
        OUT_EN OFFSET(27) NUMBITS(1) [],
        /// Lock status (read-only).
        LOCK OFFSET(28) NUMBITS(1) [],
        /// Lock enable.
        LOCK_EN OFFSET(29) NUMBITS(1) [],
        /// LDO enable.
        LDO_EN OFFSET(30) NUMBITS(1) [],
        /// PLL enable.
        EN OFFSET(31) NUMBITS(1) []
    ],
    /// D1 PLL_PERIPH0 configuration.
    ///
    /// freq = 24MHz * N / div1 / div2 (PLL6 equivalent)
    /// Default: 0xe8216300 → N=100, P0=2 → 24*100/2 = 1200 MHz,
    /// output /2 = 600 MHz.
    pub D1_PLL_PERIPH0 [
        /// Factor M0 (bits [4:0]) — typically 0.
        M0 OFFSET(0) NUMBITS(5) [],
        /// Factor M1 (bit 1) — typically 0.
        M1 OFFSET(1) NUMBITS(1) [],
        /// Multiplier N (actual = raw + 1). Bits [15:8].
        N OFFSET(8) NUMBITS(8) [],
        /// Post-divider P0 (bits [18:16], actual = raw + 1).
        P0 OFFSET(16) NUMBITS(3) [],
        /// Post-divider P1 (bits [21:20]).
        P1 OFFSET(20) NUMBITS(2) [],
        /// Output gating enable.
        OUT_EN OFFSET(27) NUMBITS(1) [],
        /// Lock status (read-only).
        LOCK OFFSET(28) NUMBITS(1) [],
        /// Lock enable.
        LOCK_EN OFFSET(29) NUMBITS(1) [],
        /// LDO enable.
        LDO_EN OFFSET(30) NUMBITS(1) [],
        /// PLL enable.
        EN OFFSET(31) NUMBITS(1) []
    ],
    /// D1 DRAM clock configuration (CCU + 0x800).
    pub D1_DRAM_CLK [
        /// Clock divider M (actual = raw + 1).
        M OFFSET(0) NUMBITS(2) [],
        /// Clock divider N (actual = 2^raw).
        N OFFSET(8) NUMBITS(2) [],
        /// Clock source: 0=PLL_DDR0, 1=PLL_AUDIO1_DIV2, 2=PLL_PERIPH0_2X.
        CLK_SRC OFFSET(24) NUMBITS(3) [],
        /// SCLK gating enable.
        SCLK_GATE OFFSET(31) NUMBITS(1) []
    ],
    /// D1 DRAM bus gating + reset (CCU + 0x80C).
    pub D1_DRAM_BGR [
        /// DRAM bus clock gate.
        GATE OFFSET(0) NUMBITS(1) [],
        /// DRAM bus reset (active-high deassert).
        RST OFFSET(16) NUMBITS(1) []
    ],
    /// D1 MMC module clock configuration (one per MMC controller).
    ///
    /// Sources: 0=OSC24M, 1=PLL_PERIPH0, 2=PLL_PERIPH0_2X
    pub D1_MMC_CLK [
        /// Clock divider M (actual = raw + 1).
        M OFFSET(0) NUMBITS(4) [],
        /// Pre-divider N (actual = 2^raw).
        N OFFSET(8) NUMBITS(2) [],
        /// Clock source select.
        CLK_SRC OFFSET(24) NUMBITS(3) [
            Osc24M = 0,
            PllPeriph0 = 1,
            PllPeriph0_2x = 2
        ],
        /// Module clock enable.
        ENABLE OFFSET(31) NUMBITS(1) []
    ],
    /// D1 MMC bus gating + reset (CCU + 0x84C).
    ///
    /// Bits [2:0] = gate for MMC0/1/2.
    /// Bits [18:16] = reset for MMC0/1/2.
    pub D1_MMC_BGR [
        /// MMC0 bus clock gate.
        MMC0_GATE OFFSET(0) NUMBITS(1) [],
        /// MMC1 bus clock gate.
        MMC1_GATE OFFSET(1) NUMBITS(1) [],
        /// MMC2 bus clock gate.
        MMC2_GATE OFFSET(2) NUMBITS(1) [],
        /// MMC0 bus reset.
        MMC0_RST OFFSET(16) NUMBITS(1) [],
        /// MMC1 bus reset.
        MMC1_RST OFFSET(17) NUMBITS(1) [],
        /// MMC2 bus reset.
        MMC2_RST OFFSET(18) NUMBITS(1) []
    ],
    /// D1 UART bus gating + reset (CCU + 0x90C).
    ///
    /// Combined gate + reset register (NCAT2 style).
    /// Bits [5:0] = gate for UART0-5.
    /// Bits [21:16] = reset for UART0-5.
    pub D1_UART_BGR [
        /// UART0 bus clock gate.
        UART0_GATE OFFSET(0) NUMBITS(1) [],
        /// UART0 bus reset.
        UART0_RST OFFSET(16) NUMBITS(1) []
    ],
    /// D1 CPU AXI configuration (CCU + 0x500).
    pub D1_CPUX_AXI_CFG [
        /// M factor (bits [1:0]).
        FACTOR_M OFFSET(0) NUMBITS(2) [],
        /// N factor (bits [9:8]).
        FACTOR_N OFFSET(8) NUMBITS(2) [],
        /// Clock source select: 0=OSC24M, 1=CLK32K, 2=CLK16M_RC, 3=PLL_CPUX, 4=PLL_PERIPH0, 5=PLL_PERIPH0_2X.
        CLK_SRC OFFSET(24) NUMBITS(3) [
            Osc24M = 0,
            PllCpux = 3,
            PllPeriph0 = 4,
            PllPeriph0_2x = 5
        ]
    ],
    /// D1 PSI/AHB1/AHB2 configuration (CCU + 0x510).
    pub D1_PSI_CLK [
        /// M factor (bits [1:0], actual = raw + 1).
        FACTOR_M OFFSET(0) NUMBITS(2) [],
        /// N factor (bits [9:8], actual = 2^raw).
        FACTOR_N OFFSET(8) NUMBITS(2) [],
        /// Clock source: 0=OSC24M, 1=CLK32K, 2=CLK16M_RC, 3=PLL_PERIPH0.
        CLK_SRC OFFSET(24) NUMBITS(2) [
            Osc24M = 0,
            PllPeriph0 = 3
        ]
    ],
    /// D1 APB0 configuration (CCU + 0x520).
    pub D1_APB0_CLK [
        /// M factor (bits [4:0], actual = raw + 1).
        FACTOR_M OFFSET(0) NUMBITS(5) [],
        /// N factor (bits [9:8], actual = 2^raw).
        FACTOR_N OFFSET(8) NUMBITS(2) [],
        /// Clock source: 0=OSC24M, 1=CLK32K, 2=PSI, 3=PLL_PERIPH0.
        CLK_SRC OFFSET(24) NUMBITS(2) [
            Osc24M = 0,
            Psi = 2,
            PllPeriph0 = 3
        ]
    ],
    /// D1 APB1 configuration (CCU + 0x524).
    ///
    /// APB1 clocks UARTs and I2C on D1/T113.
    pub D1_APB1_CLK [
        /// M factor (bits [4:0], actual = raw + 1).
        FACTOR_M OFFSET(0) NUMBITS(5) [],
        /// N factor (bits [9:8], actual = 2^raw).
        FACTOR_N OFFSET(8) NUMBITS(2) [],
        /// Clock source: 0=OSC24M, 1=CLK32K, 2=PSI, 3=PLL_PERIPH0.
        CLK_SRC OFFSET(24) NUMBITS(2) [
            Osc24M = 0,
            Psi = 2,
            PllPeriph0 = 3
        ]
    ],
    /// D1 MBUS clock configuration (CCU + 0x540).
    pub D1_MBUS_CLK [
        /// Clock divider M (actual = raw + 1).
        M OFFSET(0) NUMBITS(4) [],
        /// Pre-divider N (actual = 2^raw).
        N OFFSET(8) NUMBITS(2) [],
        /// Clock source: 0=OSC24M, 1=PLL_PERIPH0_2X, 2=PLL_DDR0, 3=PLL_PERIPH0_800M.
        CLK_SRC OFFSET(24) NUMBITS(3) [
            Osc24M = 0,
            PllPeriph0_2x = 1,
            PllDdr0 = 2,
            PllPeriph0_800M = 3
        ],
        /// MBUS reset (active-high deassert).
        RST OFFSET(30) NUMBITS(1) [],
        /// Clock gate enable.
        GATE OFFSET(31) NUMBITS(1) []
    ],
    /// D1 RISC-V clock configuration (CCU + 0xD00).
    pub D1_RISCV_CLK [
        /// M factor (bits [4:0]).
        FACTOR_M OFFSET(0) NUMBITS(5) [],
        /// Clock source: 0=OSC24M, 1=CLK32K, 2=CLK16M_RC, 3=PLL_CPUX, 4=PLL_PERIPH0, 5=PLL_PERIPH0_2X, 6=PLL_PERIPH0_800M.
        CLK_SRC OFFSET(24) NUMBITS(3) [
            Osc24M = 0,
            PllCpux = 3,
            PllPeriph0 = 4,
            PllPeriph0_800M = 6
        ]
    ],
    /// D1 RISC-V gating (CCU + 0xD04).
    pub D1_RISCV_GATE [
        /// RISC-V clock gate.
        GATE OFFSET(31) NUMBITS(1) [],
        /// RISC-V reset.
        RST OFFSET(16) NUMBITS(1) []
    ]
];

// ===================================================================
// D1/T113 (sun20i) CCU register block
// ===================================================================

register_structs! {
    /// Allwinner D1/T113 CCU register block (NCAT2 generation).
    ///
    /// Covers the subset of registers used by the CCU, DRAM, and MMC
    /// drivers. The D1 uses combined gate+reset registers (unlike H3
    /// which has separate reset registers).
    ///
    /// Base address: `0x0200_1000`.
    pub SunxiD1CcuRegs {
        /// PLL_CPUX configuration.
        (0x000 => pub pll_cpux: MmioReadWrite<u32, D1_PLL_CPUX::Register>),
        (0x004 => _res0: [u8; 0x0C]),
        /// PLL_DDR0 configuration.
        (0x010 => pub pll_ddr0: MmioReadWrite<u32, D1_PLL_DDR0::Register>),
        (0x014 => _res1: [u8; 0x0C]),
        /// PLL_PERIPH0 configuration.
        (0x020 => pub pll_periph0: MmioReadWrite<u32, D1_PLL_PERIPH0::Register>),
        (0x024 => _res2: [u8; 0x4DC]),
        /// CPU/AXI clock configuration.
        (0x500 => pub cpux_axi_cfg: MmioReadWrite<u32, D1_CPUX_AXI_CFG::Register>),
        (0x504 => _res3: [u8; 0x0C]),
        /// PSI/AHB1/AHB2 clock configuration.
        (0x510 => pub psi_clk: MmioReadWrite<u32, D1_PSI_CLK::Register>),
        (0x514 => _res4: [u8; 0x0C]),
        /// APB0 clock configuration.
        (0x520 => pub apb0_clk: MmioReadWrite<u32, D1_APB0_CLK::Register>),
        /// APB1 clock configuration (UART/I2C source on D1).
        (0x524 => pub apb1_clk: MmioReadWrite<u32, D1_APB1_CLK::Register>),
        (0x528 => _res5: [u8; 0x18]),
        /// MBUS clock configuration.
        (0x540 => pub mbus_clk: MmioReadWrite<u32, D1_MBUS_CLK::Register>),
        (0x544 => _res6: [u8; 0x2EC]),
        /// MMC0 module clock.
        (0x830 => pub mmc0_clk: MmioReadWrite<u32, D1_MMC_CLK::Register>),
        /// MMC1 module clock.
        (0x834 => pub mmc1_clk: MmioReadWrite<u32, D1_MMC_CLK::Register>),
        /// MMC2 module clock.
        (0x838 => pub mmc2_clk: MmioReadWrite<u32, D1_MMC_CLK::Register>),
        (0x83C => _res7: [u8; 0x10]),
        /// MMC bus gating + reset register.
        (0x84C => pub mmc_bgr: MmioReadWrite<u32, D1_MMC_BGR::Register>),
        // Note: DRAM_CLK is at offset 0x800 which falls inside this gap.
        // The DRAM driver accesses it via raw pointer from ccu_base instead,
        // since register_structs! requires monotonic offsets.
        (0x850 => _res8: [u8; 0xBC]),
        /// UART bus gating + reset register.
        (0x90C => pub uart_bgr: MmioReadWrite<u32, D1_UART_BGR::Register>),
        (0x910 => _res9: [u8; 0x3F0]),
        /// RISC-V clock configuration.
        (0xD00 => pub riscv_clk: MmioReadWrite<u32, D1_RISCV_CLK::Register>),
        /// RISC-V gating + reset.
        (0xD04 => pub riscv_gate: MmioReadWrite<u32, D1_RISCV_GATE::Register>),
        (0xD08 => @END),
    }
}

impl SunxiD1CcuRegs {
    /// Get a reference to the MMC module clock register by index (0-2).
    pub fn mmc_clk(&self, index: u8) -> &MmioReadWrite<u32, D1_MMC_CLK::Register> {
        match index {
            0 => &self.mmc0_clk,
            1 => &self.mmc1_clk,
            2 => &self.mmc2_clk,
            _ => &self.mmc0_clk, // unreachable in practice
        }
    }

    /// Read PLL_PERIPH0 frequency in Hz.
    ///
    /// PLL_PERIPH0 = 24MHz * N / P0 / 2
    /// Default: N=100, P0=2 → 24*100/2/2 = 600 MHz
    pub fn pll_periph0_freq(&self) -> u32 {
        use tock_registers::interfaces::Readable;
        let n = self.pll_periph0.read(D1_PLL_PERIPH0::N) + 1;
        let p0 = self.pll_periph0.read(D1_PLL_PERIPH0::P0) + 1;
        24_000_000 * n / p0 / 2
    }
}
