//! Power management controller: the PMC MMIO block (PBASE) and the ACPI
//! I/O block (ABASE).

use super::{ACPI_BASE, PMC_BASE};
use crate::southbridge::pmio_ich::PmIo;
use fstart_core::mmio::{MmioReadOnly, MmioReadWrite};
use tock_registers::register_bitfields;
use tock_registers::register_structs;

register_bitfields![u32,
    pub PRSTS [
        PMC_WAKE_STS OFFSET(4) NUMBITS(1) [],
        WOL_OVR_WK_STS OFFSET(5) NUMBITS(1) [],
        SEC_WDT_STS OFFSET(6) NUMBITS(1) [],
        SEC_GBLRST_STS OFFSET(7) NUMBITS(1) [],
        PMC_WDT_STS OFFSET(15) NUMBITS(1) []
    ],
    pub PMC_CFG [
        TIMING_T581 OFFSET(0) NUMBITS(2) [Us10 = 0, Us100 = 1, Ms1 = 2, Ms10 = 3],
        SX_ENT_TO_EN OFFSET(3) NUMBITS(1) [],
        NO_REBOOT OFFSET(4) NUMBITS(1) [],
        SPS OFFSET(5) NUMBITS(1) []
    ],
    pub GEN_PMCON1 [
        AFTERG3_EN OFFSET(0) NUMBITS(1) [],
        /// RTC power status: the RTC well lost power.
        RPS OFFSET(2) NUMBITS(1) [],
        GEN_RST_STS OFFSET(9) NUMBITS(1) [],
        DIS_SLP_X_STRCH_SUS_UP OFFSET(12) NUMBITS(1) [],
        WOL_EN_OVRD OFFSET(13) NUMBITS(1) [],
        SUS_PWR_FLR OFFSET(14) NUMBITS(1) [],
        PME_B0_S5_DIS OFFSET(15) NUMBITS(1) [],
        PWR_FLR OFFSET(16) NUMBITS(1) [],
        MS4V OFFSET(18) NUMBITS(1) [],
        CTS OFFSET(19) NUMBITS(1) [],
        SRS OFFSET(20) NUMBITS(1) [],
        MEM_SR OFFSET(21) NUMBITS(1) [],
        /// DRAM initialization scratch bit.
        DISB OFFSET(23) NUMBITS(1) [],
        UART_EN OFFSET(24) NUMBITS(1) []
    ],
    pub GEN_PMCON2 [
        SMI_LOCK OFFSET(4) NUMBITS(1) [],
        PWRBTN_LVL OFFSET(9) NUMBITS(1) [],
        BIOS_PCI_EXP_EN OFFSET(10) NUMBITS(1) [],
        SLPSX_STR_POL_LOCK OFFSET(18) NUMBITS(1) []
    ],
    pub ETR [
        /// CF9 without resume-well reset.
        CWORWRE OFFSET(18) NUMBITS(1) [],
        /// CF9 global reset: reset the TXE with the host.
        CF9GR OFFSET(20) NUMBITS(1) [],
        IGNORE_HPET OFFSET(21) NUMBITS(1) [],
        LTR_DEF OFFSET(22) NUMBITS(1) [],
        CF9LOCK OFFSET(31) NUMBITS(1) []
    ],
    /// Function disable: one bit per south cluster function.
    pub FUNC_DIS [
        SIO_DMA2 OFFSET(0) NUMBITS(1) [],
        PWM1 OFFSET(1) NUMBITS(1) [],
        PWM2 OFFSET(2) NUMBITS(1) [],
        HSUART1 OFFSET(3) NUMBITS(1) [],
        HSUART2 OFFSET(4) NUMBITS(1) [],
        SPI OFFSET(5) NUMBITS(1) [],
        MMC OFFSET(8) NUMBITS(1) [],
        SDIO OFFSET(9) NUMBITS(1) [],
        SD OFFSET(10) NUMBITS(1) [],
        MMC45 OFFSET(11) NUMBITS(1) [],
        HDA OFFSET(12) NUMBITS(1) [],
        LPE OFFSET(13) NUMBITS(1) [],
        OTG OFFSET(14) NUMBITS(1) [],
        XHCI OFFSET(15) NUMBITS(1) [],
        SATA OFFSET(17) NUMBITS(1) [],
        EHCI OFFSET(18) NUMBITS(1) [],
        TXE OFFSET(19) NUMBITS(1) [],
        PCIE_PORT1 OFFSET(20) NUMBITS(1) [],
        PCIE_PORT2 OFFSET(21) NUMBITS(1) [],
        PCIE_PORT3 OFFSET(22) NUMBITS(1) [],
        PCIE_PORT4 OFFSET(23) NUMBITS(1) [],
        SIO_DMA1 OFFSET(24) NUMBITS(1) [],
        I2C1 OFFSET(25) NUMBITS(1) [],
        I2C2 OFFSET(26) NUMBITS(1) [],
        I2C3 OFFSET(27) NUMBITS(1) [],
        I2C4 OFFSET(28) NUMBITS(1) [],
        I2C5 OFFSET(29) NUMBITS(1) [],
        I2C6 OFFSET(30) NUMBITS(1) [],
        I2C7 OFFSET(31) NUMBITS(1) []
    ],
    pub FUNC_DIS2 [
        SMBUS OFFSET(0) NUMBITS(1) [],
        OTG_SS_PHY OFFSET(1) NUMBITS(1) [],
        USH_SS_PHY OFFSET(2) NUMBITS(1) []
    ],
    /// Two bits per GPIO: none, SMI or SCI.
    pub GPIO_ROUT [ ROUTE OFFSET(0) NUMBITS(32) [] ],
    pub PLT_CLK_CTL [
        CLK_CTL OFFSET(0) NUMBITS(2) [D3Lpe = 0, On = 1, Off = 2],
        FREQ OFFSET(2) NUMBITS(1) [Mhz25 = 0, Mhz19p2 = 1]
    ]
];

register_structs! {
    /// The PMC MMIO block at [`PMC_BASE`].
    pub PmcRegs {
        (0x00 => pub prsts: MmioReadWrite<u32, PRSTS::Register>),
        (0x04 => _reserved0),
        (0x08 => pub pmc_cfg: MmioReadWrite<u32, PMC_CFG::Register>),
        (0x0c => pub vlv_pm_sts: MmioReadOnly<u32>),
        (0x10 => _reserved1),
        (0x20 => pub gen_pmcon1: MmioReadWrite<u32, GEN_PMCON1::Register>),
        (0x24 => pub gen_pmcon2: MmioReadWrite<u32, GEN_PMCON2::Register>),
        (0x28 => _reserved2),
        (0x34 => pub func_dis: MmioReadWrite<u32, FUNC_DIS::Register>),
        (0x38 => pub func_dis2: MmioReadWrite<u32, FUNC_DIS2::Register>),
        (0x3c => _reserved3),
        (0x48 => pub etr: MmioReadWrite<u32, ETR::Register>),
        (0x4c => _reserved4),
        (0x58 => pub gpio_rout: MmioReadWrite<u32, GPIO_ROUT::Register>),
        (0x5c => _reserved5),
        (0x60 => pub plt_clk_ctl: [MmioReadWrite<u32, PLT_CLK_CTL::Register>; 6]),
        (0x78 => _reserved6),
        (0xc0 => pub pme_sts: MmioReadWrite<u32>),
        (0xc4 => pub gpe_level_edge: MmioReadWrite<u32>),
        (0xc8 => pub gpe_polarity: MmioReadWrite<u32>),
        (0xcc => pub lock: MmioReadWrite<u32>),
        (0xd0 => @END),
    }
}

/// The PMC MMIO registers.
pub fn regs() -> &'static PmcRegs {
    // SAFETY: the south cluster programs PBASE before anything reads it
    // and the block is never remapped.
    unsafe { &*(PMC_BASE as *const PmcRegs) }
}

// ACPI I/O offsets that differ from the ICH layout.
pub const GPE0_STS: u16 = 0x20;
pub const GPE0_EN: u16 = 0x28;
/// GPIO SMI enables (15:0) and status (31:16).
pub const ALT_GPIO_SMI: u16 = 0x38;
/// USB per-port register write control.
pub const UPRWC: u16 = 0x3c;
pub const UPRWC_WR_EN: u16 = 1 << 1;
pub const GPE_CTRL: u16 = 0x40;
pub const PM2A_CNT_BLK: u16 = 0x50;

// GPE0 bits.
pub const PME_B0_EN: u32 = 1 << 13;
pub const BATLOW_EN: u32 = 1 << 10;
pub const PCI_EXP_EN: u32 = 1 << 9;
pub const PCIE_WAKE3_EN: u32 = 1 << 8;
pub const PCIE_WAKE2_EN: u32 = 1 << 7;
pub const PCIE_WAKE1_EN: u32 = 1 << 6;
pub const PCIE_WAKE0_EN: u32 = 1 << 3;
pub const SWGPE_EN: u32 = 1 << 2;
pub const HOT_PLUG_EN: u32 = 1 << 1;
/// Suspend well GPIO `n` (0..7) as a GPE.
pub const fn sus_gpio_gpe(n: u32) -> u32 {
    1 << (16 + n)
}
/// Core well GPIO `n` (0..7) as a GPE.
pub const fn core_gpio_gpe(n: u32) -> u32 {
    1 << (24 + n)
}

/// The ACPI I/O block at [`ACPI_BASE`]: PM1, SMI and TCO as on ICH.
pub const fn pm() -> PmIo {
    PmIo::new(ACPI_BASE)
}

/// Power state latched at boot, as coreboot's `chipset_power_state`.
#[derive(Debug, Clone, Copy, Default)]
pub struct PowerState {
    pub pm1_sts: u16,
    pub pm1_en: u16,
    pub pm1_cnt: u32,
    pub gpe0_sts: u32,
    pub gpe0_en: u32,
    pub tco_sts: u32,
    pub prsts: u32,
    pub gen_pmcon1: u32,
    pub gen_pmcon2: u32,
}

impl PowerState {
    pub fn read() -> Self {
        use crate::southbridge::pmio_ich as pmio;
        use tock_registers::interfaces::Readable;
        let pm = pm();
        let pmc = regs();
        Self {
            pm1_sts: pm.read16(pmio::PM1_STS),
            pm1_en: pm.read16(pmio::PM1_EN),
            pm1_cnt: pm.read32(pmio::PM1_CNT),
            gpe0_sts: pm.read32(GPE0_STS),
            gpe0_en: pm.read32(GPE0_EN),
            tco_sts: pm.tco().read32(pmio::TCO1_STS),
            prsts: pmc.prsts.get(),
            gen_pmcon1: pmc.gen_pmcon1.get(),
            gen_pmcon2: pmc.gen_pmcon2.get(),
        }
    }

    /// The RTC well lost power.
    pub fn rtc_failed(&self) -> bool {
        self.gen_pmcon1 & GEN_PMCON1::RPS::SET.value != 0
    }

    /// Power was lost: the previous state counts as S5.
    pub fn power_failed(&self) -> bool {
        self.gen_pmcon1 & (GEN_PMCON1::PWR_FLR::SET.value | GEN_PMCON1::SUS_PWR_FLR::SET.value) != 0
    }
}
