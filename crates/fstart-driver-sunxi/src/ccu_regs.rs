//! A20 Clock Control Unit register definitions.

#![allow(clippy::modulo_one)]

use fstart_core::mmio::MmioReadWrite;
use tock_registers::{register_bitfields, register_structs};

register_bitfields! [u32,
    pub PLL5_CFG [
        M OFFSET(0) NUMBITS(2) [],
        M1 OFFSET(2) NUMBITS(2) [],
        K OFFSET(4) NUMBITS(2) [],
        LDO OFFSET(7) NUMBITS(1) [],
        N OFFSET(8) NUMBITS(5) [],
        P OFFSET(16) NUMBITS(2) [],
        BW OFFSET(18) NUMBITS(1) [],
        VCO_GAIN OFFSET(19) NUMBITS(1) [],
        BIAS OFFSET(20) NUMBITS(5) [],
        VCO_BIAS OFFSET(25) NUMBITS(1) [],
        DDR_CLK OFFSET(29) NUMBITS(1) [],
        BYPASS OFFSET(30) NUMBITS(1) [],
        EN OFFSET(31) NUMBITS(1) []
    ],
    pub PLL6_CFG [
        K OFFSET(4) NUMBITS(2) [],
        N OFFSET(8) NUMBITS(5) [],
        SATA_EN OFFSET(14) NUMBITS(1) [],
        EN OFFSET(31) NUMBITS(1) []
    ],
    pub MBUS_CLK [
        M OFFSET(0) NUMBITS(4) [],
        N OFFSET(16) NUMBITS(2) [],
        CLK_SRC OFFSET(24) NUMBITS(2) [],
        GATE OFFSET(31) NUMBITS(1) []
    ],
    pub MMC_CLK [
        M OFFSET(0) NUMBITS(4) [],
        OCLK_DLY OFFSET(8) NUMBITS(3) [],
        N OFFSET(16) NUMBITS(2) [],
        SCLK_DLY OFFSET(20) NUMBITS(3) [],
        CLK_SRC OFFSET(24) NUMBITS(2) [Osc24M = 0, Pll6 = 1],
        ENABLE OFFSET(31) NUMBITS(1) []
    ]
];

register_structs! {
    /// A20 CCU subset used by early boot.
    pub SunxiA20CcuRegs {
        (0x00 => pub pll1_cfg: MmioReadWrite<u32>),
        (0x04 => _res0: [u8; 0x1c]),
        (0x20 => pub pll5_cfg: MmioReadWrite<u32, PLL5_CFG::Register>),
        (0x24 => _res1: [u8; 0x04]),
        (0x28 => pub pll6_cfg: MmioReadWrite<u32, PLL6_CFG::Register>),
        (0x2c => _res2: [u8; 0x28]),
        (0x54 => pub cpu_ahb_apb0_cfg: MmioReadWrite<u32>),
        (0x58 => pub apb1_clk_div: MmioReadWrite<u32>),
        (0x5c => _res3: [u8; 0x04]),
        (0x60 => pub ahb_gate0: MmioReadWrite<u32>),
        (0x64 => _res4: [u8; 0x08]),
        (0x6c => pub apb1_gate: MmioReadWrite<u32>),
        (0x70 => _res5: [u8; 0x18]),
        (0x88 => pub mmc_clk0: MmioReadWrite<u32, MMC_CLK::Register>),
        (0x8c => pub mmc_clk1: MmioReadWrite<u32, MMC_CLK::Register>),
        (0x90 => pub mmc_clk2: MmioReadWrite<u32, MMC_CLK::Register>),
        (0x94 => pub mmc_clk3: MmioReadWrite<u32, MMC_CLK::Register>),
        (0x98 => _res6: [u8; 0x38]),
        (0xd0 => pub gps_clk_cfg: MmioReadWrite<u32>),
        (0xd4 => _res7: [u8; 0x88]),
        (0x15c => pub mbus_clk_cfg: MmioReadWrite<u32, MBUS_CLK::Register>),
        (0x160 => @END),
    }
}

impl SunxiA20CcuRegs {
    /// Return an MMC module-clock register.
    pub fn mmc_clk(&self, index: u8) -> &MmioReadWrite<u32, MMC_CLK::Register> {
        match index {
            0 => &self.mmc_clk0,
            1 => &self.mmc_clk1,
            2 => &self.mmc_clk2,
            3 => &self.mmc_clk3,
            _ => &self.mmc_clk0,
        }
    }

    /// Return PLL5P frequency in Hz.
    pub fn pll5p_freq(&self) -> u32 {
        use tock_registers::interfaces::Readable;
        let n = self.pll5_cfg.read(PLL5_CFG::N);
        let k = self.pll5_cfg.read(PLL5_CFG::K) + 1;
        let p = self.pll5_cfg.read(PLL5_CFG::P);
        (24_000_000 * n * k) >> p
    }

    /// Return PLL6 frequency in Hz.
    pub fn pll6_freq(&self) -> u32 {
        use tock_registers::interfaces::Readable;
        let n = self.pll6_cfg.read(PLL6_CFG::N) + 1;
        let k = self.pll6_cfg.read(PLL6_CFG::K) + 1;
        24_000_000 * n * k / 2
    }
}
