//! FU740 power-reset-clock-interrupt controller.
//!
//! The board supplies PLL divisors for its reference clock. The PRCI owns
//! the programming sequence for core, DDR and peripheral clocks; a board with
//! Ethernet resets its PHY, then enables the GEMGXL clock.

use core::sync::atomic::{Ordering, compiler_fence};

use fstart_core::services::DeviceError;

use crate::spin_delay_us;

const FU740_PRCI_BASE: u64 = 0x1000_0000;

const CORE_PLLCFG: usize = 0x04;
const DDR_PLLCFG: usize = 0x0c;
const DDR_PLLOUTDIV: usize = 0x10;
const GEMGXL_PLLCFG: usize = 0x1c;
const GEMGXL_PLLOUTDIV: usize = 0x20;
const CORE_CLK_SEL: usize = 0x24;
const DEVICES_RESET_N: usize = 0x28;
const CLTX_PLLCFG: usize = 0x30;
const CLTX_PLLOUTDIV: usize = 0x34;
const HFPCLK_PLLCFG: usize = 0x50;
const HFPCLK_PLLOUTDIV: usize = 0x54;
const HFPCLKPLLSEL: usize = 0x58;
const PRCI_PLLS: usize = 0xe0;

const PLLCFG_DIVR_SHIFT: u32 = 0;
const PLLCFG_DIVF_SHIFT: u32 = 6;
const PLLCFG_DIVQ_SHIFT: u32 = 15;
const PLLCFG_RANGE_SHIFT: u32 = 18;
const PLLCFG_BYPASS_SHIFT: u32 = 24;
const PLLCFG_FSE_SHIFT: u32 = 25;
const PLLCFG_LOCK: u32 = 1 << 31;
const PLLCFG_ALL_FIELDS: u32 = (0x3f << PLLCFG_DIVR_SHIFT)
    | (0x1ff << PLLCFG_DIVF_SHIFT)
    | (0x7 << PLLCFG_DIVQ_SHIFT)
    | (0x7 << PLLCFG_RANGE_SHIFT)
    | (1 << PLLCFG_BYPASS_SHIFT)
    | (1 << PLLCFG_FSE_SHIFT);

const DDR_PLLOUTDIV_EN: u32 = 1 << 31;
const GEMGXL_PLLOUTDIV_EN: u32 = 1 << 31;
// coreboot uses bit 31 (u-boot says 24, but 24 hangs)
const HFPCLK_PLLOUTDIV_EN: u32 = 1 << 31;
const CLTX_PLLOUTDIV_EN: u32 = 1 << 24;
const CORECLKSEL_HFCLK: u32 = 1;
const CORECLKSEL_CORECLKPLL: u32 = 0;
const HFPCLKSEL_HFCLK: u32 = 1;
const RST_DDR_CTRL: u32 = 1 << 0;
const RST_DDR_AXI: u32 = 1 << 1;
const RST_DDR_AHB: u32 = 1 << 2;
const RST_DDR_PHY: u32 = 1 << 3;
const RST_GEMGXL: u32 = 1 << 5;
const PLLS_CLTXPLL: u32 = 1 << 0;
const PLLS_GEMGXLPLL: u32 = 1 << 1;
const PLLS_DDRPLL: u32 = 1 << 2;
const PLLS_HFPCLKPLL: u32 = 1 << 3;
const PLLS_COREPLL: u32 = 1 << 5;

/// PLL divisors and input range selected for the board's reference clock.
#[derive(Debug, Clone, Copy)]
pub struct PllSettings {
    divr: u32,
    divf: u32,
    divq: u32,
    range: u32,
}

impl PllSettings {
    #[must_use]
    pub const fn new(divr: u32, divf: u32, divq: u32, range: u32) -> Self {
        assert!(divr <= 0x3f && divf <= 0x1ff && divq <= 7 && range <= 7);
        Self {
            divr,
            divf,
            divq,
            range,
        }
    }

    const fn to_reg(self) -> u32 {
        (self.divr << PLLCFG_DIVR_SHIFT)
            | (self.divf << PLLCFG_DIVF_SHIFT)
            | (self.divq << PLLCFG_DIVQ_SHIFT)
            | (self.range << PLLCFG_RANGE_SHIFT)
            | (1 << PLLCFG_FSE_SHIFT)
    }
}

/// Board-selected PLL programming, not a driver-owned default clock tree.
#[derive(Debug, Clone, Copy)]
pub struct PllConfig {
    pub core: PllSettings,
    pub ddr: PllSettings,
    pub gemgxl: PllSettings,
    pub hfpclk: PllSettings,
    pub cltx: PllSettings,
}

/// FU740 clock/reset driver.
pub struct Fu740Prci {
    config: &'static Fu740PrciConfig,
}

impl Fu740Prci {
    pub fn new(config: &'static Fu740PrciConfig) -> Result<Self, DeviceError> {
        if config.reference_clock_hz == 0 {
            return Err(DeviceError::ConfigError);
        }
        Ok(Self { config })
    }

    #[inline(always)]
    fn read(&self, offset: usize) -> u32 {
        // SAFETY: FU740 PRCI registers have a fixed, aligned MMIO address.
        unsafe { fstart_core::mmio::read32((FU740_PRCI_BASE as usize + offset) as *const u32) }
    }

    #[inline(always)]
    fn write(&self, offset: usize, value: u32) {
        // SAFETY: FU740 PRCI registers have a fixed, aligned MMIO address.
        unsafe {
            fstart_core::mmio::write32((FU740_PRCI_BASE as usize + offset) as *mut u32, value)
        }
    }

    #[inline(always)]
    fn set_bits(&self, offset: usize, bits: u32) {
        self.write(offset, self.read(offset) | bits);
    }

    #[inline(always)]
    fn clear_bits(&self, offset: usize, bits: u32) {
        self.write(offset, self.read(offset) & !bits);
    }

    fn reset_deassert(&self, bits: u32) {
        self.set_bits(DEVICES_RESET_N, bits);
    }

    fn configure_pll(&self, offset: usize, settings: PllSettings) -> Result<(), DeviceError> {
        self.write(
            offset,
            (self.read(offset) & !PLLCFG_ALL_FIELDS) | settings.to_reg(),
        );
        let mut timeout = 1_000_000_u32;
        while self.read(offset) & PLLCFG_LOCK == 0 {
            core::hint::spin_loop();
            timeout -= 1;
            if timeout == 0 {
                return Err(DeviceError::InitFailed);
            }
        }
        Ok(())
    }

    fn init_coreclk(&self) -> Result<(), DeviceError> {
        self.write(CORE_CLK_SEL, CORECLKSEL_HFCLK);
        if self.read(PRCI_PLLS) & PLLS_COREPLL != 0 {
            self.configure_pll(CORE_PLLCFG, self.config.plls.core)?;
            self.write(CORE_CLK_SEL, CORECLKSEL_CORECLKPLL);
        }
        Ok(())
    }

    fn init_ddrclk(&self) -> Result<(), DeviceError> {
        if self.read(PRCI_PLLS) & PLLS_DDRPLL != 0 {
            self.clear_bits(DDR_PLLOUTDIV, DDR_PLLOUTDIV_EN);
            self.configure_pll(DDR_PLLCFG, self.config.plls.ddr)?;
            self.set_bits(DDR_PLLOUTDIV, DDR_PLLOUTDIV_EN);
        }
        Ok(())
    }

    fn deassert_ddr_resets(&self) {
        self.set_bits(DEVICES_RESET_N, RST_DDR_CTRL);
        #[cfg(target_arch = "riscv64")]
        // SAFETY: `fence` is valid on the FU740 RISC-V core.
        unsafe {
            core::arch::asm!("fence")
        };
        self.set_bits(DEVICES_RESET_N, RST_DDR_AXI | RST_DDR_AHB | RST_DDR_PHY);
        for _ in 0..256 {
            core::hint::spin_loop();
        }
    }

    fn init_hfpclk(&self) -> Result<(), DeviceError> {
        self.set_bits(HFPCLKPLLSEL, HFPCLKSEL_HFCLK);
        self.configure_pll(HFPCLK_PLLCFG, self.config.plls.hfpclk)?;
        self.set_bits(HFPCLK_PLLOUTDIV, HFPCLK_PLLOUTDIV_EN);
        spin_delay_us(1_000);
        self.clear_bits(HFPCLKPLLSEL, HFPCLKSEL_HFCLK);
        spin_delay_us(70);
        Ok(())
    }

    fn init_cltx(&self) -> Result<(), DeviceError> {
        self.clear_bits(CLTX_PLLOUTDIV, CLTX_PLLOUTDIV_EN);
        self.configure_pll(CLTX_PLLCFG, self.config.plls.cltx)?;
        self.set_bits(CLTX_PLLOUTDIV, CLTX_PLLOUTDIV_EN);
        spin_delay_us(70);
        Ok(())
    }

    /// Clock the Ethernet MAC (GEMGXL PLL, 125 MHz) and release its reset.
    /// The board resets its PHY first.
    pub fn enable_ethernet(&self) -> Result<(), DeviceError> {
        if self.read(PRCI_PLLS) & PLLS_GEMGXLPLL != 0 {
            self.clear_bits(GEMGXL_PLLOUTDIV, GEMGXL_PLLOUTDIV_EN);
            self.configure_pll(GEMGXL_PLLCFG, self.config.plls.gemgxl)?;
            self.set_bits(GEMGXL_PLLOUTDIV, GEMGXL_PLLOUTDIV_EN);
        }
        self.reset_deassert(RST_GEMGXL);
        Ok(())
    }

    /// Program clocks and release DDR in the hardware-required order.
    pub fn init(&mut self) -> Result<(), DeviceError> {
        #[cfg(target_arch = "riscv64")]
        // SAFETY: CSR 0x7c1 is the U74 feature-control CSR.
        unsafe {
            core::arch::asm!("csrwi 0x7C1, 0")
        };

        self.init_coreclk()?;
        self.write(DEVICES_RESET_N, 0);
        self.init_ddrclk()?;
        self.deassert_ddr_resets();
        if self.read(PRCI_PLLS) & PLLS_HFPCLKPLL != 0 {
            self.init_hfpclk()?;
        } else if self.read(PRCI_PLLS) & PLLS_CLTXPLL != 0 {
            self.init_cltx()?;
        }
        compiler_fence(Ordering::SeqCst);
        Ok(())
    }
}

/// Static FU740 PRCI policy.
#[derive(Debug, Clone, Copy)]
pub struct Fu740PrciConfig {
    /// The PLL settings are explicitly selected for this reference, not scaled.
    pub reference_clock_hz: u32,
    pub plls: PllConfig,
}

impl Fu740PrciConfig {
    #[must_use]
    pub const fn new(reference_clock_hz: u32, plls: PllConfig) -> Self {
        Self {
            reference_clock_hz,
            plls,
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

#[cfg(test)]
mod tests {
    use super::*;

    const fn plls(divf: u32) -> PllConfig {
        let setting = PllSettings::new(0, divf, 2, 4);
        PllConfig {
            core: setting,
            ddr: setting,
            gemgxl: setting,
            hfpclk: setting,
            cltx: setting,
        }
    }
    static CLOCK26: Fu740PrciConfig = Fu740PrciConfig::new(26_000_000, plls(76));
    static CLOCK24: Fu740PrciConfig = Fu740PrciConfig::new(24_000_000, plls(80));

    #[test]
    fn retains_caller_clock_plan_instead_of_substituting_a_board_default() {
        let a = Fu740Prci::new(&CLOCK26).unwrap();
        let b = Fu740Prci::new(&CLOCK24).unwrap();
        for settings in [
            a.config.plls.core,
            a.config.plls.ddr,
            a.config.plls.gemgxl,
            a.config.plls.hfpclk,
            a.config.plls.cltx,
        ] {
            assert_eq!(settings.to_reg(), 0x0211_1300);
        }
        assert_eq!(b.config.plls.core.to_reg(), 0x0211_1400);
        assert_eq!(b.config.reference_clock_hz, 24_000_000);
        static INVALID: Fu740PrciConfig = Fu740PrciConfig::new(0, plls(76));
        assert!(matches!(
            Fu740Prci::new(&INVALID),
            Err(DeviceError::ConfigError)
        ));
    }

    #[test]
    #[should_panic]
    fn rejects_divisors_outside_the_register_fields() {
        let _ = PllSettings::new(64, 0, 0, 0);
    }
}
