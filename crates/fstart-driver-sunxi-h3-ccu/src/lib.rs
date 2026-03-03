//! Allwinner H3/H2+ (sun8i) Clock Control Unit driver.
//!
//! Programs PLL_CPUX (CPU), PLL_PERIPH0 (peripherals), opens UART clock
//! gates, deasserts UART reset, muxes UART0 GPIO pins (PA4=TX, PA5=RX),
//! and configures the CCU security switch for non-secure access.
//!
//! The H3 is a sun6i-family SoC with separate bus-reset registers
//! (unlike the A20 which only has gate registers). UART clock setup
//! goes through APB2 (not APB1 as on the A20).
//!
//! Reference: u-boot `arch/arm/mach-sunxi/clock_sun6i.c`
//! Register defs: u-boot `arch/arm/include/asm/arch-sunxi/clock_sun6i.h`
//!
//! The H3 CCU register block is at `0x01C2_0000`.
//! The PIO (GPIO) register block is at `0x01C2_0800`.

#![no_std]
#![allow(clippy::identity_op)] // Bit-field shifts like (x << 0) document register layout

use tock_registers::interfaces::{Readable, Writeable};

use fstart_services::device::{Device, DeviceError};
use fstart_services::{ClockController, ServiceError};

use fstart_sunxi_ccu_regs::{SunxiH3CcuRegs, H3_CCU_SEC_SWITCH};

use fstart_arch::sdelay;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// OSC24M frequency (Hz) — the H3's fixed 24 MHz crystal oscillator.
///
/// Also the source clock for the ARM Generic Timer (CNTFRQ).
const OSC24M_FREQ: u32 = 24_000_000;

/// PLL_CPUX default: 408 MHz = 24 * 17 * 1 / (1 * 1)
/// N=17, K=1, M=1, P=1 → 24 * 17 = 408 MHz.
/// Raw: N-1=16 << 8, K-1=0 << 4, M-1=0, P=0 << 16, EN=1 << 31
const PLL_CPUX_408MHZ: u32 = (1 << 31) | (16 << 8);

/// PLL6 default: 600 MHz (from U-Boot).
/// Raw value: 0x90041811 → N=24(+1=25), K=1(+1=2), EN=1
/// 24 * 25 * 2 / 2 = 600 MHz
const PLL6_DEFAULT: u32 = 0x9004_1811;

/// CPU clock source: OSC24M (safe for PLL reconfiguration).
const CPU_CLK_SRC_OSC24M: u32 = 0x00 << 16;
/// CPU clock source: PLL_CPUX.
const CPU_CLK_SRC_PLL_CPUX: u32 = 0x02 << 16;
/// CPU source mask (bits [17:16]).
const CPU_CLK_SRC_MASK: u32 = 0x03 << 16;

/// AHB1/APB1 dividers: AHB1 = PLL6/3 = 200 MHz, APB1 = AHB1/2 = 100 MHz.
///
/// Matches U-Boot `AHB1_ABP1_DIV_DEFAULT` (0x3180):
///   bits [13:12] = 11  → AHB1_CLK_SRC = PLL6
///   bits  [9:8]  = 01  → APB1_CLK_RATIO = 2 (÷2  → 100 MHz)
///   bits  [7:6]  = 10  → AHB1_PRE_DIV = 3  (÷3  → 200 MHz)
///   bits  [5:4]  = 00  → AHB1_CLK_RATIO = 1 (÷1, no extra div)
const AHB1_APB1_DEFAULT: u32 = (0x03 << 12) | (0x01 << 8) | (0x02 << 6);

/// APB2 clock source: OSC24M
const APB2_CLK_SRC_OSC24M: u32 = 0x01 << 24;

/// MBUS clock: gate=enable, source=PLL6, N=1, M=4 → 600/4 = 150 MHz.
///
/// Matches U-Boot `MBUS_CLK_DEFAULT` (0x81000003) for sun8i/H3.
const MBUS_CLK_DEFAULT: u32 = (1 << 31) | (0x01 << 24) | (3 << 0);

/// PLL6 (PERIPH0) lock status bit — bit 28 of `pll6_cfg`.
///
/// The BROM / U-Boot poll this bit after enabling PLL6 before switching
/// AHB1 clock source to PLL6.  Without waiting for lock the AHB1 bus
/// clock is derived from an unlocked / inactive PLL, which halts all
/// APB1-connected peripherals (including the CCU itself) on cold boot.
const PLL6_LOCK_BIT: u32 = 1 << 28;

/// Timer 0 control register offset (from CCU base, Timer block at +0xC00).
const TIMER0_CTRL_OFF: usize = 0xC10;
/// Timer 0 interval value register offset.
const TIMER0_INTV_OFF: usize = 0xC14;

const TIMER0_EN: u32 = 1 << 0;
const TIMER0_RELOAD: u32 = 1 << 1;
const TIMER0_CLK_SRC_OSC24M: u32 = 1 << 2;

// Bus gate bits (offset 0x60 — bus_gate0)
const BUS_GATE0_DMA: u32 = 1 << 6;

// APB2 bus gate bits (offset 0x6C — bus_gate3): UART0-3 at bits 16-19
const BUS_GATE3_UART0: u32 = 1 << 16;

// APB2 reset bits (offset 0x2D8): UART0-3 at bits 16-19
const APB2_RST_UART0: u32 = 1 << 16;

// PRCM register base
const PRCM_BASE: usize = 0x01F0_1400;

// PRCM security switch register offset
const PRCM_SEC_SWITCH_OFF: usize = 0x1D0;
const PRCM_SEC_SWITCH_APB0_CLK_NONSEC: u32 = 1 << 0;
const PRCM_SEC_SWITCH_PLL_CFG_NONSEC: u32 = 1 << 1;
const PRCM_SEC_SWITCH_PWR_GATE_NONSEC: u32 = 1 << 2;

// ---------------------------------------------------------------------------
// PIO (GPIO) constants for H3 UART0 on PA4/PA5
// ---------------------------------------------------------------------------

/// Port A configuration register 0 offset (from PIO base).
const PIO_PA_CFG0_OFF: u32 = 0x00;
/// Port A pull register 0 offset.
const PIO_PA_PULL0_OFF: u32 = 0x1C;

/// PA4 function 2 = UART0_TX
const PA4_UART0_FUNC: u32 = 2;
/// PA5 function 2 = UART0_RX
const PA5_UART0_FUNC: u32 = 2;
/// PA4 config shift (pin 4 × 4 bits = bit 16)
const PA4_CFG_SHIFT: u32 = 4 * 4;
/// PA5 config shift (pin 5 × 4 bits = bit 20)
const PA5_CFG_SHIFT: u32 = 5 * 4;
/// PA5 pull shift (pin 5 × 2 bits = bit 10)
const PA5_PULL_SHIFT: u32 = 5 * 2;
/// Pull-up value.
const GPIO_PULL_UP: u32 = 1;

/// Typed configuration for the H3/H2+ CCU driver.
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
pub struct SunxiH3CcuConfig {
    /// CCU register base address (typically `0x01C2_0000`).
    pub ccu_base: u64,
    /// PIO (GPIO) register base address (typically `0x01C2_0800`).
    pub pio_base: u64,
    /// UART index to configure (0-based).
    pub uart_index: u8,
}

/// Allwinner H3/H2+ Clock Control Unit + GPIO pin mux driver.
pub struct SunxiH3Ccu {
    ccu: &'static SunxiH3CcuRegs,
    ccu_base: usize,
    pio_base: usize,
    uart_index: u8,
}

// SAFETY: MMIO registers are at fixed hardware addresses from the board RON.
unsafe impl Send for SunxiH3Ccu {}
unsafe impl Sync for SunxiH3Ccu {}

impl SunxiH3Ccu {
    #[inline(always)]
    fn pio_read(&self, offset: u32) -> u32 {
        unsafe { fstart_mmio::read32((self.pio_base + offset as usize) as *const u32) }
    }

    #[inline(always)]
    fn pio_write(&self, offset: u32, val: u32) {
        unsafe { fstart_mmio::write32((self.pio_base + offset as usize) as *mut u32, val) }
    }

    /// Write to a timer register (at CCU base + offset).
    #[inline(always)]
    fn timer_write(&self, offset: usize, val: u32) {
        unsafe { fstart_mmio::write32((self.ccu_base + offset) as *mut u32, val) }
    }

    /// Read-modify-write: set bits in a raw u32 register.
    #[inline(always)]
    fn set_bits_raw(&self, reg: &fstart_mmio::MmioReadWrite<u32>, bits: u32) {
        let val = reg.get();
        reg.set(val | bits);
    }

    /// Read-modify-write: clear bits in a raw u32 register.
    #[inline(always)]
    fn clear_bits_raw(&self, reg: &fstart_mmio::MmioReadWrite<u32>, bits: u32) {
        let val = reg.get();
        reg.set(val & !bits);
    }

    /// Step 0: Start hardware timer 0 — matches U-Boot `timer_init()`.
    fn timer_init(&self) {
        self.timer_write(TIMER0_INTV_OFF, 0xFFFF_FFFF);
        self.timer_write(
            TIMER0_CTRL_OFF,
            TIMER0_EN | TIMER0_RELOAD | TIMER0_CLK_SRC_OSC24M,
        );
    }

    /// Step 1: Safe clock initialization.
    ///
    /// Ported from U-Boot `clock_init_safe()` / `clock_set_pll1()` (sun6i
    /// path for H3).  Sets PLL_CPUX to 408 MHz, PLL6 to 600 MHz,
    /// AHB1/APB1 dividers, and MBUS clock.
    ///
    /// **Ordering is critical for SD-card boot correctness.**
    /// The BROM leaves PLL6 running after FEL (USB) boot but NOT after
    /// SD-card boot.  AHB1 must only be switched to PLL6 *after* PLL6 is
    /// locked; otherwise the AHB1/APB1 bus clock drops to 0 Hz, which
    /// freezes all APB1 peripherals (including CCU register access) before
    /// the UART can ever be initialised.
    fn clock_init_safe(&self) {
        // 1. Switch CPU to safe OSC24M before reprogramming PLL_CPUX.
        let cfg = self.ccu.cpu_axi_cfg.get();
        self.ccu
            .cpu_axi_cfg
            .set((cfg & !CPU_CLK_SRC_MASK) | CPU_CLK_SRC_OSC24M);
        sdelay(20);

        // 2. Program PLL_CPUX to 408 MHz and enable.
        self.ccu.pll_cpux.set(PLL_CPUX_408MHZ);
        sdelay(200);

        // 3. Switch CPU back to PLL_CPUX.
        //    Must happen before touching PLL6/AHB1 so the CPU is not
        //    sourced from OSC24M while AHB1 transitions.
        let cfg = self.ccu.cpu_axi_cfg.get();
        self.ccu
            .cpu_axi_cfg
            .set((cfg & !CPU_CLK_SRC_MASK) | CPU_CLK_SRC_PLL_CPUX);
        sdelay(20);

        // 4. Enable PLL6 (PERIPH0) at 600 MHz and wait for lock.
        //    Only after lock is it safe to select PLL6 as AHB1 source.
        self.ccu.pll6_cfg.set(PLL6_DEFAULT);
        while (self.ccu.pll6_cfg.get() & PLL6_LOCK_BIT) == 0 {
            core::hint::spin_loop();
        }

        // 5. Set AHB1/APB1 dividers now that PLL6 is stable.
        //    AHB1 = PLL6/3 = 200 MHz, APB1 = AHB1/2 = 100 MHz.
        self.ccu.ahb1_apb1_div.set(AHB1_APB1_DEFAULT);

        // 6. Program MBUS clock to PLL6/4 = 150 MHz, then deassert reset.
        self.ccu.mbus_clk_cfg.set(MBUS_CLK_DEFAULT);
        self.ccu.mbus_reset.set(1 << 31);
        sdelay(20);

        // 7. Enable DMA AHB gate.
        self.set_bits_raw(&self.ccu.bus_gate0, BUS_GATE0_DMA);
    }

    /// Step 2: H3-specific security switch.
    ///
    /// The H3 boots in secure mode. We must set the CCU and PRCM
    /// security switches to allow non-secure access to peripherals.
    /// This matches U-Boot `clock_init_sec()`.
    fn clock_init_sec(&self) {
        self.ccu.ccu_sec_switch.write(
            H3_CCU_SEC_SWITCH::MBUS_NONSEC::SET
                + H3_CCU_SEC_SWITCH::BUS_NONSEC::SET
                + H3_CCU_SEC_SWITCH::PLL_NONSEC::SET,
        );

        // PRCM security switch: allow non-secure access to APB0, PLL config, power gate
        // SAFETY: PRCM base address is fixed hardware.
        unsafe {
            let prcm_sec = (PRCM_BASE + PRCM_SEC_SWITCH_OFF) as *mut u32;
            let val = core::ptr::read_volatile(prcm_sec);
            core::ptr::write_volatile(
                prcm_sec,
                val | PRCM_SEC_SWITCH_APB0_CLK_NONSEC
                    | PRCM_SEC_SWITCH_PLL_CFG_NONSEC
                    | PRCM_SEC_SWITCH_PWR_GATE_NONSEC,
            );
        }
    }

    /// Step 3: Configure UART clock (APB2 source = OSC24M, gate + reset).
    ///
    /// On the H3, UARTs are on APB2 (not APB1 as on A20). We set APB2
    /// to 24 MHz from OSC24M, then open the clock gate and deassert
    /// the reset for the selected UART.
    fn clock_init_uart(&self) {
        // APB2 = OSC24M (24 MHz direct)
        self.ccu.apb2_div.set(APB2_CLK_SRC_OSC24M);

        let uart_bit = BUS_GATE3_UART0 << self.uart_index;
        let reset_bit = APB2_RST_UART0 << self.uart_index;

        // Open UART clock gate (bus_gate3 = APB2 gates)
        self.set_bits_raw(&self.ccu.bus_gate3, uart_bit);

        // Deassert UART reset (apb2_reset)
        self.set_bits_raw(&self.ccu.apb2_reset, reset_bit);
    }

    /// Step 4: Mux GPIO pins for UART0 (PA4=TX, PA5=RX).
    ///
    /// On the H3, UART0 uses port A pins 4 and 5 (function 2),
    /// unlike the A20 which uses PB22/PB23.
    fn gpio_init_uart(&self) {
        if self.uart_index != 0 {
            return;
        }

        // PA_CFG0: set PA4 and PA5 to function 2 (UART0)
        let mut cfg = self.pio_read(PIO_PA_CFG0_OFF);
        cfg &= !(0xf << PA4_CFG_SHIFT);
        cfg |= PA4_UART0_FUNC << PA4_CFG_SHIFT;
        cfg &= !(0xf << PA5_CFG_SHIFT);
        cfg |= PA5_UART0_FUNC << PA5_CFG_SHIFT;
        self.pio_write(PIO_PA_CFG0_OFF, cfg);

        // PA_PULL0: set PA5 (RX) to pull-up
        let mut pull = self.pio_read(PIO_PA_PULL0_OFF);
        pull &= !(0x3 << PA5_PULL_SHIFT);
        pull |= GPIO_PULL_UP << PA5_PULL_SHIFT;
        self.pio_write(PIO_PA_PULL0_OFF, pull);
    }
}

impl Device for SunxiH3Ccu {
    const NAME: &'static str = "sunxi-h3-ccu";
    const COMPATIBLE: &'static [&'static str] = &["allwinner,sun8i-h3-ccu"];
    type Config = SunxiH3CcuConfig;

    fn new(config: &SunxiH3CcuConfig) -> Result<Self, DeviceError> {
        Ok(Self {
            // SAFETY: addresses come from the board RON, validated by codegen.
            ccu: unsafe { &*(config.ccu_base as *const SunxiH3CcuRegs) },
            ccu_base: config.ccu_base as usize,
            pio_base: config.pio_base as usize,
            uart_index: config.uart_index,
        })
    }

    fn init(&self) -> Result<(), DeviceError> {
        self.timer_init();
        self.clock_init_safe();
        self.clock_init_sec();

        // Program the ARM Generic Timer frequency register (CNTFRQ).
        // The H3's Generic Timer is clocked from OSC24M. The BROM
        // leaves CNTFRQ at 0. Must be done from secure mode (we are).
        fstart_arch::set_cntfrq(OSC24M_FREQ);

        self.clock_init_uart();
        self.gpio_init_uart();

        sdelay(10_000);

        Ok(())
    }
}

impl ClockController for SunxiH3Ccu {
    fn enable_clock(&self, gate_id: u32) -> Result<(), ServiceError> {
        self.set_bits_raw(&self.ccu.bus_gate3, 1 << gate_id);
        Ok(())
    }

    fn disable_clock(&self, gate_id: u32) -> Result<(), ServiceError> {
        self.clear_bits_raw(&self.ccu.bus_gate3, 1 << gate_id);
        Ok(())
    }

    fn get_frequency(&self, clock_id: u32) -> Result<u32, ServiceError> {
        match clock_id {
            0 => Ok(24_000_000),  // OSC24M
            1 => Ok(600_000_000), // PLL6 (PERIPH0)
            _ => Err(ServiceError::NotSupported),
        }
    }
}
