//! FU740 power-reset-clock-interrupt controller.
//!
//! The PLL programming sequence follows the proven HiFive Unmatched setup.

use core::sync::atomic::{Ordering, compiler_fence};

use fstart_core::services::DeviceError;

use crate::config::Fu740PrciConfig;

const FU740_PRCI_BASE: u64 = 0x1000_0000;
const FU740_GPIO_BASE: u64 = 0x1006_0000;

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
const HFPCLK_DIV_REG: usize = 0x5c;
const PRCI_PLLS: usize = 0xe0;
const GPIO_OUTPUT_EN: usize = 0x08;
const GPIO_OUTPUT_VAL: usize = 0x0c;
const GEMGXL_RST_PIN: u32 = 12;

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

#[derive(Clone, Copy)]
struct PllSettings {
    divr: u32,
    divf: u32,
    divq: u32,
    range: u32,
}

impl PllSettings {
    const fn to_reg(self) -> u32 {
        (self.divr << PLLCFG_DIVR_SHIFT)
            | (self.divf << PLLCFG_DIVF_SHIFT)
            | (self.divq << PLLCFG_DIVQ_SHIFT)
            | (self.range << PLLCFG_RANGE_SHIFT)
            | (1 << PLLCFG_FSE_SHIFT)
    }

    const fn output_freq(self, reference_clock_hz: u64) -> u64 {
        reference_clock_hz / (self.divr as u64 + 1) * (2 * (self.divf as u64 + 1))
            / (1_u64 << self.divq)
    }
}

// Keep these proven HiFive Unmatched programming values verbatim.
const COREPLL: PllSettings = PllSettings {
    divr: 0,
    divf: 76,
    divq: 2,
    range: 4,
};
const DDRPLL: PllSettings = PllSettings {
    divr: 0,
    divf: 71,
    divq: 2,
    range: 4,
};
const GEMGXLPLL: PllSettings = PllSettings {
    divr: 0,
    divf: 76,
    divq: 5,
    range: 4,
};
const HFPCLKPLL: PllSettings = PllSettings {
    divr: 1,
    divf: 39,
    divq: 2,
    range: 4,
};
const CLTXPLL: PllSettings = PllSettings {
    divr: 1,
    divf: 39,
    divq: 2,
    range: 4,
};

/// FU740 clock/reset driver.
pub struct Fu740Prci {
    config: &'static Fu740PrciConfig,
}

unsafe impl Send for Fu740Prci {}
unsafe impl Sync for Fu740Prci {}

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
    fn gpio_read(&self, offset: usize) -> u32 {
        // SAFETY: FU740 GPIO registers have a fixed, aligned MMIO address.
        unsafe { fstart_core::mmio::read32((FU740_GPIO_BASE as usize + offset) as *const u32) }
    }

    #[inline(always)]
    fn gpio_write(&self, offset: usize, value: u32) {
        // SAFETY: FU740 GPIO registers have a fixed, aligned MMIO address.
        unsafe {
            fstart_core::mmio::write32((FU740_GPIO_BASE as usize + offset) as *mut u32, value)
        }
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
            self.configure_pll(CORE_PLLCFG, COREPLL)?;
            self.write(CORE_CLK_SEL, CORECLKSEL_CORECLKPLL);
        }
        Ok(())
    }

    fn init_ddrclk(&self) -> Result<(), DeviceError> {
        if self.read(PRCI_PLLS) & PLLS_DDRPLL != 0 {
            self.clear_bits(DDR_PLLOUTDIV, DDR_PLLOUTDIV_EN);
            self.configure_pll(DDR_PLLCFG, DDRPLL)?;
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
        self.configure_pll(HFPCLK_PLLCFG, HFPCLKPLL)?;
        self.set_bits(HFPCLK_PLLOUTDIV, HFPCLK_PLLOUTDIV_EN);
        spin_delay_us(1_000);
        self.clear_bits(HFPCLKPLLSEL, HFPCLKSEL_HFCLK);
        spin_delay_us(70);
        Ok(())
    }

    fn init_cltx(&self) -> Result<(), DeviceError> {
        self.clear_bits(CLTX_PLLOUTDIV, CLTX_PLLOUTDIV_EN);
        self.configure_pll(CLTX_PLLCFG, CLTXPLL)?;
        self.set_bits(CLTX_PLLOUTDIV, CLTX_PLLOUTDIV_EN);
        spin_delay_us(70);
        Ok(())
    }

    /// Step 5: Ethernet PHY reset via GPIO + GEMGXL PLL.
    fn init_ethernet(&self) -> Result<(), DeviceError> {
        // GPIO 12 = GEMGXL_RST: output, high -> low -> high (reset pulse).
        let pin_mask = 1u32 << GEMGXL_RST_PIN;

        // Enable GPIO 12 as output.
        self.gpio_write(GPIO_OUTPUT_EN, self.gpio_read(GPIO_OUTPUT_EN) | pin_mask);

        // Drive high.
        self.gpio_write(GPIO_OUTPUT_VAL, self.gpio_read(GPIO_OUTPUT_VAL) | pin_mask);
        spin_delay_us(1);

        // Reset pulse: drive low, wait, drive high.
        self.gpio_write(GPIO_OUTPUT_VAL, self.gpio_read(GPIO_OUTPUT_VAL) & !pin_mask);
        spin_delay_us(1);
        self.gpio_write(GPIO_OUTPUT_VAL, self.gpio_read(GPIO_OUTPUT_VAL) | pin_mask);

        // Wait 15 ms for PHY to enter unmanaged mode.
        spin_delay_us(15_000);

        // Configure GEMGXL PLL (125 MHz).
        if self.read(PRCI_PLLS) & PLLS_GEMGXLPLL != 0 {
            self.clear_bits(GEMGXL_PLLOUTDIV, GEMGXL_PLLOUTDIV_EN);
            self.configure_pll(GEMGXL_PLLCFG, GEMGXLPLL)?;
            self.set_bits(GEMGXL_PLLOUTDIV, GEMGXL_PLLOUTDIV_EN);
        }

        // Deassert Ethernet reset.
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
        self.init_ethernet()?;
        compiler_fence(Ordering::SeqCst);
        Ok(())
    }

    /// Peripheral clock after the PRCI divider.
    #[must_use]
    pub fn pclk_freq(&self) -> u32 {
        let pll = if self.read(PRCI_PLLS) & PLLS_HFPCLKPLL != 0
            && self.read(HFPCLK_PLLOUTDIV) & HFPCLK_PLLOUTDIV_EN != 0
        {
            HFPCLKPLL.output_freq(u64::from(self.config.reference_clock_hz))
        } else {
            u64::from(self.config.reference_clock_hz)
        };
        (pll / (u64::from(self.read(HFPCLK_DIV_REG)) + 2)) as u32
    }
}

fn spin_delay_us(us: u32) {
    for _ in 0..u64::from(us) * 500 {
        core::hint::spin_loop();
    }
}
