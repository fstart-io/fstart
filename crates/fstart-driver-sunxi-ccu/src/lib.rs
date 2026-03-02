//! Allwinner A20 (sun7i) Clock Control Unit driver.
//!
//! Programs PLL1 (CPU), PLL6 (peripherals), opens UART clock gates,
//! and muxes UART0 GPIO pins. This is the first device initialized
//! in the boot sequence.
//!
//! Reference: u-boot `arch/arm/mach-sunxi/clock_sun4i.c`
//! Register defs: u-boot `arch/arm/include/asm/arch-sunxi/clock_sun4i.h`
//!
//! The A20 CCU register block is at `0x01C2_0000`.
//! The PIO (GPIO) register block is at `0x01C2_0800`.

#![no_std]

use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};

use fstart_services::device::{Device, DeviceError};
use fstart_services::{ClockController, ServiceError};

use fstart_sunxi_ccu_regs::{SunxiA20CcuRegs, PLL6_CFG};

use fstart_arch::sdelay;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// OSC24M frequency (Hz) — the A20's fixed 24 MHz crystal oscillator.
///
/// This is the source clock for the ARM Generic Timer (CNTFRQ) on all
/// Allwinner ARMv7 SoCs.  We program CNTFRQ during clock init because:
///
/// - The BROM does NOT set CNTFRQ (U-Boot's comments confirm this).
/// - CNTFRQ is a secure-only register — it must be written before any
///   non-secure transition (our firmware stays in secure mode, so this
///   is always valid).
/// - U-Boot programs it in `board_init()` (board/sunxi/board.c) and
///   again as a safety net in `_nonsec_init()` (nonsec_virt.S).
/// - Clock init is the natural place: the Generic Timer frequency IS
///   a clock configuration, and it must be correct before any timer-
///   dependent code runs (DRAM training delays, Linux boot).
const OSC24M_FREQ: u32 = 24_000_000;

// PLL1 default: safe value from U-Boot
const PLL1_CFG_DEFAULT: u32 = 0xa100_5000;

// PLL6: enable, 24MHz * N * K = 600 MHz (N=25, K=1)
const PLL6_CFG_DEFAULT: u32 = 0xa100_9911;

// CPU clock source: PLL1
const CPU_CLK_SRC_PLL1: u32 = 0x02 << 16;
// CPU clock source: OSC24M (safe for PLL reconfiguration)
const CPU_CLK_SRC_OSC24M: u32 = 0x01 << 16;
// AXI div 1, AHB div 2, APB0 div 1
#[allow(clippy::identity_op)]
const AXI_AHB_APB0_DEFAULT: u32 = (0 << 0) | (1 << 4) | (0 << 8);

// APB1: source = OSC24M, prescaler N=1, divider M=1 -> 24 MHz
const APB1_CLK_SRC_OSC24M: u32 = 0x00 << 24;
const APB1_PRESCALER_N1: u32 = 0x00 << 16;
const APB1_DIVIDER_M1: u32 = 0x00;

// AHB gate bits
const AHB_GATE_DMA: u32 = 1 << 6;
const AHB_GATE_SATA: u32 = 1 << 25;

// APB1 gate: bit 16 = UART0, bit 17 = UART1, ...
const APB1_GATE_UART0: u32 = 1 << 16;

// ---------------------------------------------------------------------------
// Timer register offsets (from CCU base — Timer block is at CCU + 0xC00)
// Too far from main CCU block to include in the shared struct.
// ---------------------------------------------------------------------------

/// Timer 0 control register (TMR0_CTRL_REG).
const TIMER0_CTRL_OFF: usize = 0xC10;
/// Timer 0 interval value register (TMR0_INTV_VALUE_REG).
const TIMER0_INTV_OFF: usize = 0xC14;

const TIMER0_EN: u32 = 1 << 0;
const TIMER0_RELOAD: u32 = 1 << 1;
const TIMER0_CLK_SRC_OSC24M: u32 = 1 << 2;

// ---------------------------------------------------------------------------
// PIO (GPIO) register offsets (from PIO base 0x01C2_0800)
// ---------------------------------------------------------------------------

const PIO_PB_CFG2_OFF: u32 = 0x2C;
const PIO_PB_PULL1_OFF: u32 = 0x24 + 0x20;

const PB22_UART0_FUNC: u32 = 2;
const PB23_UART0_FUNC: u32 = 2;
const PB22_CFG_SHIFT: u32 = (22 - 16) * 4; // = 24
const PB23_CFG_SHIFT: u32 = (23 - 16) * 4; // = 28
const PB23_PULL_SHIFT: u32 = (23 - 16) * 2; // = 14
const GPIO_PULL_UP: u32 = 1;

/// Typed configuration for the A20 CCU driver.
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
pub struct SunxiA20CcuConfig {
    /// CCU register base address (typically `0x01C2_0000`).
    pub ccu_base: u64,
    /// PIO (GPIO) register base address (typically `0x01C2_0800`).
    pub pio_base: u64,
    /// UART index to configure (0-based).
    pub uart_index: u8,
}

/// Allwinner A20 Clock Control Unit + GPIO pin mux driver.
pub struct SunxiA20Ccu {
    ccu: &'static SunxiA20CcuRegs,
    ccu_base: usize,
    pio_base: usize,
    uart_index: u8,
}

// SAFETY: MMIO registers are at fixed hardware addresses from the board RON.
unsafe impl Send for SunxiA20Ccu {}
unsafe impl Sync for SunxiA20Ccu {}

impl SunxiA20Ccu {
    #[inline(always)]
    fn pio_read(&self, offset: u32) -> u32 {
        unsafe { fstart_mmio::read32((self.pio_base + offset as usize) as *const u32) }
    }

    #[inline(always)]
    fn pio_write(&self, offset: u32, val: u32) {
        unsafe { fstart_mmio::write32((self.pio_base + offset as usize) as *mut u32, val) }
    }

    /// Write a u32 to a timer register (at CCU base + offset).
    #[inline(always)]
    fn timer_write(&self, offset: usize, val: u32) {
        unsafe { fstart_mmio::write32((self.ccu_base + offset) as *mut u32, val) }
    }

    /// Read-modify-write: set bits in a CCU register.
    #[inline(always)]
    fn ccu_set_bits_raw(&self, reg: &fstart_mmio::MmioReadWrite<u32>, bits: u32) {
        let val = reg.get();
        reg.set(val | bits);
    }

    /// Step 0: Start hardware timer 0 — matches U-Boot `timer_init()`.
    fn timer_init(&self) {
        self.timer_write(TIMER0_INTV_OFF, 0xFFFF_FFFF);
        self.timer_write(
            TIMER0_CTRL_OFF,
            TIMER0_EN | TIMER0_RELOAD | TIMER0_CLK_SRC_OSC24M,
        );
    }

    /// Step 1: Switch CPU to 24 MHz OSC, reprogram PLL1, switch back.
    fn clock_init_safe(&self) {
        self.ccu
            .cpu_ahb_apb0_cfg
            .set(AXI_AHB_APB0_DEFAULT | CPU_CLK_SRC_OSC24M);
        sdelay(20);

        self.ccu.pll1_cfg.set(PLL1_CFG_DEFAULT);
        sdelay(200);

        self.ccu
            .cpu_ahb_apb0_cfg
            .set(AXI_AHB_APB0_DEFAULT | CPU_CLK_SRC_PLL1);
        sdelay(20);

        // Enable DMA AHB gate (required for DRAM init)
        self.ccu_set_bits_raw(&self.ccu.ahb_gate0, AHB_GATE_DMA);

        // Program PLL6
        self.ccu.pll6_cfg.set(PLL6_CFG_DEFAULT);

        // SATA gates — for exact parity with U-Boot SPL binary
        self.ccu_set_bits_raw(&self.ccu.ahb_gate0, AHB_GATE_SATA);
        self.ccu.pll6_cfg.modify(PLL6_CFG::SATA_EN::SET);
    }

    /// Step 2: Configure APB1 clock (UART source) to 24 MHz direct from OSC.
    fn clock_init_uart(&self) {
        self.ccu
            .apb1_clk_div
            .set(APB1_CLK_SRC_OSC24M | APB1_PRESCALER_N1 | APB1_DIVIDER_M1);

        let uart_gate_bit = APB1_GATE_UART0 << self.uart_index;
        self.ccu_set_bits_raw(&self.ccu.apb1_gate, uart_gate_bit);
    }

    /// Step 3: Mux GPIO pins for UART0 (PB22=TX, PB23=RX).
    fn gpio_init_uart(&self) {
        if self.uart_index != 0 {
            return;
        }

        let mut cfg = self.pio_read(PIO_PB_CFG2_OFF);
        cfg &= !(0xf << PB22_CFG_SHIFT);
        cfg |= PB22_UART0_FUNC << PB22_CFG_SHIFT;
        cfg &= !(0xf << PB23_CFG_SHIFT);
        cfg |= PB23_UART0_FUNC << PB23_CFG_SHIFT;
        self.pio_write(PIO_PB_CFG2_OFF, cfg);

        let mut pull = self.pio_read(PIO_PB_PULL1_OFF);
        pull &= !(0x3 << PB23_PULL_SHIFT);
        pull |= GPIO_PULL_UP << PB23_PULL_SHIFT;
        self.pio_write(PIO_PB_PULL1_OFF, pull);
    }
}

impl Device for SunxiA20Ccu {
    const NAME: &'static str = "sunxi-a20-ccu";
    const COMPATIBLE: &'static [&'static str] = &["allwinner,sun7i-a20-ccu"];
    type Config = SunxiA20CcuConfig;

    fn new(config: &SunxiA20CcuConfig) -> Result<Self, DeviceError> {
        Ok(Self {
            // SAFETY: addresses come from the board RON, validated by codegen.
            ccu: unsafe { &*(config.ccu_base as *const SunxiA20CcuRegs) },
            ccu_base: config.ccu_base as usize,
            pio_base: config.pio_base as usize,
            uart_index: config.uart_index,
        })
    }

    fn init(&self) -> Result<(), DeviceError> {
        self.timer_init();
        self.clock_init_safe();

        // Program the ARM Generic Timer frequency register (CNTFRQ).
        //
        // The A20's Generic Timer is clocked from OSC24M.  The BROM
        // leaves CNTFRQ at 0, which causes Linux to panic with
        // "Division by zero" in the arch_timer driver.
        //
        // Must be done from secure mode (we are in secure SVC).
        // U-Boot does this in board_init() (board/sunxi/board.c:222)
        // and again in _nonsec_init() (nonsec_virt.S:197).
        //
        // We do it here in clock init — after PLLs are stable but
        // before any timer-dependent code (DRAM training, UART delays).
        fstart_arch::set_cntfrq(OSC24M_FREQ);

        self.clock_init_uart();
        self.gpio_init_uart();

        sdelay(10_000);

        Ok(())
    }
}

impl ClockController for SunxiA20Ccu {
    fn enable_clock(&self, gate_id: u32) -> Result<(), ServiceError> {
        self.ccu_set_bits_raw(&self.ccu.apb1_gate, 1 << gate_id);
        Ok(())
    }

    fn disable_clock(&self, gate_id: u32) -> Result<(), ServiceError> {
        let val = self.ccu.apb1_gate.get();
        self.ccu.apb1_gate.set(val & !(1 << gate_id));
        Ok(())
    }

    fn get_frequency(&self, clock_id: u32) -> Result<u32, ServiceError> {
        match clock_id {
            0 => Ok(24_000_000),
            1 => Ok(600_000_000),
            _ => Err(ServiceError::NotSupported),
        }
    }
}
