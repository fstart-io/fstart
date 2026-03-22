//! SiFive FU740 PRCI (Power Reset Clocks Interrupts) driver.
//!
//! Configures all PLLs on the FU740 SoC, deasserts device resets, and
//! provides the peripheral clock frequency for downstream peripherals
//! (UART, SPI, I2C, GPIO).
//!
//! ## Init sequence (follows coreboot `src/soc/sifive/fu740/clock.c`)
//!
//! 1. Enable all U74 microarchitectural features (CSR 0x7C1 = 0)
//! 2. Switch core clock to HFCLK, program COREPLL (~1 GHz), switch back
//! 3. Assert all device resets
//! 4. Program DDRPLL (~933 MHz), deassert DDR resets (CTL, then AXI+AHB+PHY)
//! 5. Program HFPCLKPLL (260 MHz peripheral clock)
//! 6. Program GEMGXLPLL (125 MHz Ethernet) + GPIO PHY reset + deassert
//!
//! ## PRCI register block: 0x1000_0000
//!
//! Reference: FU740-C000 Manual Chapter 7, coreboot clock.c, U-Boot fu740-prci.c

#![no_std]

use core::sync::atomic::{compiler_fence, Ordering};

use fstart_mmio::{read32, write32};
use fstart_services::device::{Device, DeviceError};
use fstart_services::{ClockController, ServiceError};

// ---------------------------------------------------------------------------
// PRCI register offsets (from base 0x1000_0000)
// ---------------------------------------------------------------------------

const CORE_PLLCFG: usize = 0x04;
const DDR_PLLCFG: usize = 0x0C;
const DDR_PLLOUTDIV: usize = 0x10;
const GEMGXL_PLLCFG: usize = 0x1C;
const GEMGXL_PLLOUTDIV: usize = 0x20;
const CORE_CLK_SEL: usize = 0x24;
const DEVICES_RESET_N: usize = 0x28;
const CLTX_PLLCFG: usize = 0x30;
const CLTX_PLLOUTDIV: usize = 0x34;
const HFPCLK_PLLCFG: usize = 0x50;
const HFPCLK_PLLOUTDIV: usize = 0x54;
const HFPCLKPLLSEL: usize = 0x58;
const HFPCLK_DIV_REG: usize = 0x5C;
const PRCI_PLLS: usize = 0xE0;

// ---------------------------------------------------------------------------
// PLL config register fields
// ---------------------------------------------------------------------------

const PLLCFG_DIVR_SHIFT: u32 = 0;
const PLLCFG_DIVF_SHIFT: u32 = 6;
const PLLCFG_DIVQ_SHIFT: u32 = 15;
const PLLCFG_RANGE_SHIFT: u32 = 18;
const PLLCFG_BYPASS_SHIFT: u32 = 24;
const PLLCFG_FSE_SHIFT: u32 = 25;
const PLLCFG_LOCK: u32 = 1 << 31;

const PLLCFG_DIVR_MASK: u32 = 0x3F << PLLCFG_DIVR_SHIFT;
const PLLCFG_DIVF_MASK: u32 = 0x1FF << PLLCFG_DIVF_SHIFT;
const PLLCFG_DIVQ_MASK: u32 = 0x7 << PLLCFG_DIVQ_SHIFT;
const PLLCFG_RANGE_MASK: u32 = 0x7 << PLLCFG_RANGE_SHIFT;
const PLLCFG_BYPASS_MASK: u32 = 1 << PLLCFG_BYPASS_SHIFT;
const PLLCFG_FSE_MASK: u32 = 1 << PLLCFG_FSE_SHIFT;

/// All PLL config field bits combined.
const PLLCFG_ALL_FIELDS: u32 = PLLCFG_DIVR_MASK
    | PLLCFG_DIVF_MASK
    | PLLCFG_DIVQ_MASK
    | PLLCFG_RANGE_MASK
    | PLLCFG_BYPASS_MASK
    | PLLCFG_FSE_MASK;

// ---------------------------------------------------------------------------
// PLL output enable bits
// ---------------------------------------------------------------------------

const DDR_PLLOUTDIV_EN: u32 = 1 << 31;
const GEMGXL_PLLOUTDIV_EN: u32 = 1 << 31;
const HFPCLK_PLLOUTDIV_EN: u32 = 1 << 31; // coreboot uses bit 31 (u-boot says 24, but 24 hangs)
const CLTX_PLLOUTDIV_EN: u32 = 1 << 24;

// ---------------------------------------------------------------------------
// Clock mux select values
// ---------------------------------------------------------------------------

const CORECLKSEL_HFCLK: u32 = 1;
const CORECLKSEL_CORECLKPLL: u32 = 0;
const HFPCLKSEL_HFCLK: u32 = 1;
// ---------------------------------------------------------------------------
// Device reset bits (active-high deassert in devices_reset_n)
// ---------------------------------------------------------------------------

const RST_DDR_CTRL: u32 = 1 << 0;
const RST_DDR_AXI: u32 = 1 << 1;
const RST_DDR_AHB: u32 = 1 << 2;
const RST_DDR_PHY: u32 = 1 << 3;
const RST_GEMGXL: u32 = 1 << 5;

// ---------------------------------------------------------------------------
// PLL presence bits (from PRCI_PLLS register at 0xE0)
// ---------------------------------------------------------------------------

const PLLS_CLTXPLL: u32 = 1 << 0;
const PLLS_GEMGXLPLL: u32 = 1 << 1;
const PLLS_DDRPLL: u32 = 1 << 2;
const PLLS_HFPCLKPLL: u32 = 1 << 3;
const PLLS_COREPLL: u32 = 1 << 5;

// ---------------------------------------------------------------------------
// GPIO register offsets (from GPIO base 0x1006_0000)
// ---------------------------------------------------------------------------

const GPIO_OUTPUT_EN: usize = 0x08;
const GPIO_OUTPUT_VAL: usize = 0x0C;

/// Ethernet PHY reset GPIO pin (GEMGXL_RST on the HiFive Unmatched).
const GEMGXL_RST_PIN: u32 = 12;

// ---------------------------------------------------------------------------
// HFCLK frequency
// ---------------------------------------------------------------------------

/// On-board crystal oscillator frequency (26 MHz on HiFive Unmatched).
const HFCLK_FREQ: u64 = 26_000_000;

// ---------------------------------------------------------------------------
// PLL settings
// ---------------------------------------------------------------------------

/// PLL configuration parameters.
#[derive(Clone, Copy)]
struct PllSettings {
    divr: u32,
    divf: u32,
    divq: u32,
    range: u32,
}

impl PllSettings {
    /// Encode as a PLLCFG register value (bypass=0, fse=1).
    const fn to_reg(&self) -> u32 {
        (self.divr << PLLCFG_DIVR_SHIFT)
            | (self.divf << PLLCFG_DIVF_SHIFT)
            | (self.divq << PLLCFG_DIVQ_SHIFT)
            | (self.range << PLLCFG_RANGE_SHIFT)
            | (1 << PLLCFG_FSE_SHIFT) // fsebypass = 1 (internal feedback)
    }

    /// Compute the PLL output frequency given a reference clock.
    const fn output_freq(&self, ref_clk: u64) -> u64 {
        // f_out = f_ref * 2*(divf+1) / ((divr+1) * 2^divq)
        let vco = ref_clk / (self.divr as u64 + 1) * (2 * (self.divf as u64 + 1));
        vco / (1u64 << self.divq)
    }
}

/// COREPLL: ~1001 MHz (HFCLK=26 MHz, divr=0, divf=76, divq=2).
/// VCO = 26 * 154 = 4004 MHz, output = 4004 / 4 = 1001 MHz.
const COREPLL: PllSettings = PllSettings {
    divr: 0,
    divf: 76,
    divq: 2,
    range: 4,
};

/// DDRPLL: ~936 MHz (HFCLK=26 MHz, divr=0, divf=71, divq=2).
/// VCO = 26 * 144 = 3744 MHz, output = 3744 / 4 = 936 MHz.
const DDRPLL: PllSettings = PllSettings {
    divr: 0,
    divf: 71,
    divq: 2,
    range: 4,
};

/// GEMGXLPLL: ~125 MHz (HFCLK=26 MHz, divr=0, divf=76, divq=5).
/// VCO = 26 * 154 = 4004 MHz, output = 4004 / 32 = 125.125 MHz.
const GEMGXLPLL: PllSettings = PllSettings {
    divr: 0,
    divf: 76,
    divq: 5,
    range: 4,
};

/// HFPCLKPLL: 260 MHz (HFCLK=26 MHz, divr=1, divf=39, divq=2).
/// VCO = 13 * 80 = 1040 MHz, output = 1040 / 4 = 260 MHz.
const HFPCLKPLL: PllSettings = PllSettings {
    divr: 1,
    divf: 39,
    divq: 2,
    range: 4,
};

/// CLTXPLL: 260 MHz (same as HFPCLKPLL, alternative peripheral clock path).
const CLTXPLL: PllSettings = PllSettings {
    divr: 1,
    divf: 39,
    divq: 2,
    range: 4,
};

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

/// Typed configuration for the FU740 PRCI driver.
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
pub struct Fu740PrciConfig {
    /// PRCI register base address (0x1000_0000).
    pub base_addr: u64,
    /// GPIO register base address (0x1006_0000).
    pub gpio_base: u64,
}

// ---------------------------------------------------------------------------
// Driver
// ---------------------------------------------------------------------------

/// FU740 PRCI clock controller driver.
///
/// Configures all on-chip PLLs and manages device reset lines.
/// After `init()`, the peripheral clock (PCLK) is available for UART,
/// SPI, I2C, and GPIO peripherals.
pub struct Fu740Prci {
    base: usize,
    gpio_base: usize,
}

// SAFETY: MMIO registers are at fixed hardware addresses from the board RON.
unsafe impl Send for Fu740Prci {}
unsafe impl Sync for Fu740Prci {}

impl Fu740Prci {
    /// Read a 32-bit PRCI register.
    #[inline(always)]
    fn read(&self, offset: usize) -> u32 {
        // SAFETY: base + offset is a valid PRCI MMIO register address.
        unsafe { read32((self.base + offset) as *const u32) }
    }

    /// Write a 32-bit PRCI register.
    #[inline(always)]
    fn write(&self, offset: usize, val: u32) {
        // SAFETY: base + offset is a valid PRCI MMIO register address.
        unsafe { write32((self.base + offset) as *mut u32, val) }
    }

    /// Read a 32-bit GPIO register.
    #[inline(always)]
    fn gpio_read(&self, offset: usize) -> u32 {
        // SAFETY: gpio_base + offset is a valid GPIO MMIO register address.
        unsafe { read32((self.gpio_base + offset) as *const u32) }
    }

    /// Write a 32-bit GPIO register.
    #[inline(always)]
    fn gpio_write(&self, offset: usize, val: u32) {
        // SAFETY: gpio_base + offset is a valid GPIO MMIO register address.
        unsafe { write32((self.gpio_base + offset) as *mut u32, val) }
    }

    /// Set bits in a PRCI register.
    #[inline(always)]
    fn set_bits(&self, offset: usize, bits: u32) {
        let val = self.read(offset);
        self.write(offset, val | bits);
    }

    /// Clear bits in a PRCI register.
    #[inline(always)]
    fn clear_bits(&self, offset: usize, bits: u32) {
        let val = self.read(offset);
        self.write(offset, val & !bits);
    }

    /// Configure a PLL and wait for lock.
    ///
    /// Returns `Err` if the PLL fails to lock within ~100 ms (at ~1 GHz core).
    fn configure_pll(&self, cfg_offset: usize, settings: &PllSettings) -> Result<(), DeviceError> {
        let old = self.read(cfg_offset);
        let new = (old & !PLLCFG_ALL_FIELDS) | settings.to_reg();
        self.write(cfg_offset, new);

        // Poll for PLL lock (bit 31) with timeout.
        let mut timeout: u32 = 1_000_000;
        while self.read(cfg_offset) & PLLCFG_LOCK == 0 {
            core::hint::spin_loop();
            timeout = timeout.wrapping_sub(1);
            if timeout == 0 {
                return Err(DeviceError::InitFailed);
            }
        }
        Ok(())
    }

    /// Deassert the given reset bit(s) in DEVICES_RESET_N.
    fn reset_deassert(&self, bits: u32) {
        self.set_bits(DEVICES_RESET_N, bits);
    }

    /// Step 1: Configure the core PLL (~1 GHz).
    fn init_coreclk(&self) -> Result<(), DeviceError> {
        // Switch core clock to HFCLK (26 MHz safe fallback) while we
        // reprogram the PLL.
        self.write(CORE_CLK_SEL, CORECLKSEL_HFCLK);

        if self.read(PRCI_PLLS) & PLLS_COREPLL == 0 {
            return Ok(());
        }

        self.configure_pll(CORE_PLLCFG, &COREPLL)?;

        // Switch back to COREPLL.
        self.write(CORE_CLK_SEL, CORECLKSEL_CORECLKPLL);
        Ok(())
    }

    /// Step 2: Configure the DDR PLL (~933 MHz) and deassert DDR resets.
    fn init_ddrclk(&self) -> Result<(), DeviceError> {
        if self.read(PRCI_PLLS) & PLLS_DDRPLL == 0 {
            return Ok(());
        }

        // Disable DDR PLL output before reconfiguring.
        self.clear_bits(DDR_PLLOUTDIV, DDR_PLLOUTDIV_EN);

        self.configure_pll(DDR_PLLCFG, &DDRPLL)?;

        // Enable DDR PLL output.
        self.set_bits(DDR_PLLOUTDIV, DDR_PLLOUTDIV_EN);
        Ok(())
    }

    /// Step 3: Deassert DDR controller and PHY resets.
    ///
    /// Must be called after `init_ddrclk()`. The reset sequence has
    /// strict ordering requirements from the FU740 manual:
    /// 1. Deassert DDR_CTRL_RST first
    /// 2. Fence (wait one DDR controller clock cycle)
    /// 3. Deassert DDR_AXI + DDR_AHB + DDR_PHY
    /// 4. Wait 256 DDR controller clock cycles
    fn deassert_ddr_resets(&self) {
        // DDR controller out of reset first.
        self.reset_deassert(RST_DDR_CTRL);

        // Wait at least one full DDR controller clock cycle.
        #[cfg(target_arch = "riscv64")]
        // SAFETY: fence is a valid RISC-V instruction.
        unsafe {
            core::arch::asm!("fence");
        }

        // DDR register interface and PHY out of reset.
        self.reset_deassert(RST_DDR_AXI | RST_DDR_AHB | RST_DDR_PHY);

        // Wait 256 DDR controller clock cycles for the subsystem to stabilize.
        for _ in 0..256 {
            core::hint::spin_loop();
        }
    }

    /// Step 4: Configure the peripheral clock PLL (260 MHz).
    fn init_hfpclk(&self) -> Result<(), DeviceError> {
        // Switch peripheral clock to HFCLK while reprogramming PLL.
        self.set_bits(HFPCLKPLLSEL, HFPCLKSEL_HFCLK);

        self.configure_pll(HFPCLK_PLLCFG, &HFPCLKPLL)?;

        // Enable PLL output.
        self.set_bits(HFPCLK_PLLOUTDIV, HFPCLK_PLLOUTDIV_EN);

        // Wait for PLL output to stabilize.
        spin_delay_us(1000);

        // Switch peripheral clock back to PLL.
        self.clear_bits(HFPCLKPLLSEL, HFPCLKSEL_HFCLK);

        spin_delay_us(70);
        Ok(())
    }

    /// Step 4 (alternative): Configure ChipLink TX PLL (260 MHz).
    fn init_cltx(&self) -> Result<(), DeviceError> {
        self.clear_bits(CLTX_PLLOUTDIV, CLTX_PLLOUTDIV_EN);
        self.configure_pll(CLTX_PLLCFG, &CLTXPLL)?;
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
            self.configure_pll(GEMGXL_PLLCFG, &GEMGXLPLL)?;
            self.set_bits(GEMGXL_PLLOUTDIV, GEMGXL_PLLOUTDIV_EN);
        }

        // Deassert Ethernet reset.
        self.reset_deassert(RST_GEMGXL);
        Ok(())
    }

    /// Compute the current peripheral clock (PCLK) frequency in Hz.
    ///
    /// PCLK = HFPCLKPLL_output / (hfpclk_div_reg + 2).
    pub fn pclk_freq(&self) -> u32 {
        let pll_freq = if self.read(PRCI_PLLS) & PLLS_HFPCLKPLL != 0
            && self.read(HFPCLK_PLLOUTDIV) & HFPCLK_PLLOUTDIV_EN != 0
        {
            HFPCLKPLL.output_freq(HFCLK_FREQ)
        } else {
            HFCLK_FREQ
        };

        let div = self.read(HFPCLK_DIV_REG);
        (pll_freq / (div as u64 + 2)) as u32
    }
}

impl Device for Fu740Prci {
    const NAME: &'static str = "fu740-prci";
    const COMPATIBLE: &'static [&'static str] = &["sifive,fu740-c000-prci"];
    type Config = Fu740PrciConfig;

    fn new(config: &Fu740PrciConfig) -> Result<Self, DeviceError> {
        Ok(Self {
            base: config.base_addr as usize,
            gpio_base: config.gpio_base as usize,
        })
    }

    fn init(&self) -> Result<(), DeviceError> {
        // Enable all U74 microarchitectural features.
        // CSR 0x7C1 (U74 Feature Disable): writing 0 enables everything.
        // Both coreboot and u-boot do this as the very first thing.
        // SAFETY: CSR 0x7C1 is a valid U74-specific CSR for feature control.
        #[cfg(target_arch = "riscv64")]
        unsafe {
            core::arch::asm!("csrwi 0x7C1, 0");
        }

        // 1. Configure core PLL (~1 GHz).
        self.init_coreclk()?;

        // 2. Assert all device resets before configuring their clocks.
        self.write(DEVICES_RESET_N, 0);

        // 3. Configure DDR PLL (~933 MHz).
        self.init_ddrclk()?;

        // 4. Deassert DDR resets (strict ordering: CTRL first, then AXI+AHB+PHY).
        self.deassert_ddr_resets();

        // 5. Configure peripheral clock PLL (260 MHz for UART/SPI/I2C/GPIO).
        let plls = self.read(PRCI_PLLS);
        if plls & PLLS_HFPCLKPLL != 0 {
            self.init_hfpclk()?;
        } else if plls & PLLS_CLTXPLL != 0 {
            self.init_cltx()?;
        }

        // 6. Ethernet PHY reset + GEMGXL PLL.
        self.init_ethernet()?;

        compiler_fence(Ordering::SeqCst);

        Ok(())
    }
}

impl ClockController for Fu740Prci {
    fn enable_clock(&self, _gate_id: u32) -> Result<(), ServiceError> {
        // FU740 PRCI doesn't have per-peripheral clock gates like Allwinner.
        // All peripherals are clocked once the corresponding PLL is configured.
        Ok(())
    }

    fn disable_clock(&self, _gate_id: u32) -> Result<(), ServiceError> {
        Ok(())
    }

    fn get_frequency(&self, clock_id: u32) -> Result<u32, ServiceError> {
        match clock_id {
            // COREPLL output
            0 => Ok(COREPLL.output_freq(HFCLK_FREQ) as u32),
            // DDRPLL output
            1 => Ok(DDRPLL.output_freq(HFCLK_FREQ) as u32),
            // GEMGXLPLL output
            2 => Ok(GEMGXLPLL.output_freq(HFCLK_FREQ) as u32),
            // HFPCLKPLL output (raw, before divider)
            4 => Ok(HFPCLKPLL.output_freq(HFCLK_FREQ) as u32),
            // PCLK (peripheral clock, after divider) — this is what UART uses
            7 => Ok(self.pclk_freq()),
            // HFCLK (26 MHz crystal)
            8 => Ok(HFCLK_FREQ as u32),
            _ => Err(ServiceError::NotSupported),
        }
    }
}

// ---------------------------------------------------------------------------
// Simple spin delay (no timer, just NOP loops)
//
// At ~1 GHz core clock, 1000 NOPs ~= 1 us. This is a rough approximation
// used only during early init before any timer is configured.
// ---------------------------------------------------------------------------

fn spin_delay_us(us: u32) {
    // Conservative estimate: assume ~500 MHz (post-HFCLK, pre-COREPLL)
    // which gives ~2 ns per NOP. We need us * 500 NOPs per microsecond.
    let count = us as u64 * 500;
    for _ in 0..count {
        core::hint::spin_loop();
    }
}
