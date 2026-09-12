//! Allwinner D1/T113 (sun20i) Clock Control Unit driver.
//!
//! Programs PLL_CPUX (CPU), PLL_PERIPH0 (peripherals), opens UART clock
//! gates, deasserts UART reset, and muxes UART0 GPIO pins (PB8=TX, PB9=RX).
//!
//! The D1 is a NCAT2-generation SoC with combined gate+reset registers
//! (single register per bus, with gate in low bits and reset in high bits).
//! UARTs are clocked from APB1 (offset 0x524) which defaults to 24 MHz OSC.
//!
//! The D1 RISC-V core (T-Head C906) starts in M-mode from the BROM.
//! There is no security mode switch needed (unlike H3's AArch32 secure mode).
//!
//! Reference: U-Boot `arch/arm/mach-sunxi/clock_sun50i_h6.c` (NCAT2 path)
//! Register defs: U-Boot `arch/arm/include/asm/arch-sunxi/clock_sun50i_h6.h`
//! Clock driver: U-Boot `drivers/clk/sunxi/clk_d1.c`

#![allow(clippy::identity_op)] // Bit-field shifts like (x << 0) document register layout

use crate::ccu_regs::{D1_CPUX_AXI_CFG, D1_PLL_CPUX, D1_PLL_PERIPH0, SunxiD1CcuRegs};
use crate::pio::{PORT_B, PioGen, Pull, SunxiPio};
use fstart_arch::udelay;
use fstart_core::{mmio, mmio32};
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};

/// D1 CCU register block base.
pub const D1_CCU_BASE: u64 = 0x0200_1000;
/// D1 PIO (GPIO) register block base.
pub const D1_PIO_BASE: u64 = 0x0200_0000;
/// D1 UART0 register base.
pub const D1_UART0_BASE: u64 = 0x0250_0000;
/// D1 MMC0 register base.
pub const D1_MMC0_BASE: u64 = 0x0402_0000;
/// D1 BROM loads the eGON image into SRAM at this base.
pub const D1_SRAM_BASE: u64 = 0x0002_0000;
/// eGON firmware image offset on SD/MMC (8 KiB).
pub const D1_EGON_MMC_OFFSET: u64 = 8 * 1024;

/// OSC24M frequency (Hz) — the D1's 24 MHz DCXO.
const OSC24M_FREQ: u32 = 24_000_000;

/// D1 UART0 TX alternate function on PB8.
const UART0_TX_FUNC: u8 = 6;
/// D1 UART0 RX alternate function on PB9.
const UART0_RX_FUNC: u8 = 6;

/// Typed configuration for the D1/T113 CCU driver.
#[derive(Debug, Clone, Copy, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct D1CcuConfig {
    /// UART index to configure (0-based, 0-5).
    pub uart_index: u8,
}

impl D1CcuConfig {
    #[must_use]
    pub const fn new(uart_index: u8) -> Self {
        Self { uart_index }
    }
}

/// Allwinner D1/T113 Clock Control Unit + GPIO pin mux driver.
pub struct D1Ccu {
    ccu: &'static SunxiD1CcuRegs,
    uart_index: u8,
}

impl D1Ccu {
    /// Construct the CCU driver over its fixed MMIO block.
    #[must_use]
    pub fn new_from_config(config: &D1CcuConfig) -> Self {
        Self {
            // SAFETY: the CCU register block is at its fixed hardware address.
            ccu: unsafe { &*(D1_CCU_BASE as usize as *const SunxiD1CcuRegs) },
            uart_index: config.uart_index,
        }
    }

    /// Read a raw CCU register by offset (for registers outside the typed
    /// struct, e.g. DMA_BGR at 0x70C, RISCV_CFG_BGR at 0xD0C).
    #[inline(always)]
    fn ccu_read(&self, offset: usize) -> u32 {
        // SAFETY: address is a valid MMIO register within the CCU block.
        unsafe { mmio::read32((D1_CCU_BASE as usize + offset) as *const u32) }
    }

    /// Write a raw CCU register by offset.
    #[inline(always)]
    fn ccu_write(&self, offset: usize, val: u32) {
        // SAFETY: address is a valid MMIO register within the CCU block.
        unsafe { mmio::write32((D1_CCU_BASE as usize + offset) as *mut u32, val) }
    }

    /// Full early clock init: pre-DRAM clocks, UART clock, UART pins.
    pub fn init(&self) {
        // The D1 BROM programs basic PLLs before loading the eGON image.
        // However, oreboot's main() programs several additional clocks
        // before DRAM init that the BROM may not fully configure.
        self.clock_init_pre_dram();
        self.clock_init_uart();
        self.gpio_init_uart();
        udelay(100);
    }

    /// Pre-DRAM clock setup — matches oreboot main() before mctl::init().
    ///
    /// The BROM sets up basic clocks, but oreboot still programs several
    /// PLLs and bus clocks in main() before DRAM init. Without this,
    /// the DRAM controller may not have the correct bus clocks.
    ///
    /// Specifically: CPU PLL, DMA gate/reset, CPUX AXI config,
    /// PLL_PERIPH0, and RISCV_CFG gate/reset.
    ///
    /// Uses read-modify-write (no PLL lock waits), matching oreboot.
    fn clock_init_pre_dram(&self) {
        // 1. CPU PLL: set N=42 → 24MHz * (42+1) = 1032 MHz, enable.
        //    Read-modify-write to preserve BROM-set bits.
        //    (oreboot main.rs lines 450-454)
        self.ccu
            .pll_cpux
            .modify(D1_PLL_CPUX::EN::SET + D1_PLL_CPUX::N.val(42));

        // 2. DMA BGR: deassert reset (bit 16), enable gate (bit 0).
        //    (oreboot main.rs lines 458-461)
        //    DMA_BGR is at CCU + 0x70C.
        let val = self.ccu_read(0x70C);
        self.ccu_write(0x70C, val | (1 << 16));
        let val = self.ccu_read(0x70C);
        self.ccu_write(0x70C, val | (1 << 0));

        // 3. Spin briefly for PLL/bus to stabilize.
        for _ in 0..1000 {
            core::hint::spin_loop();
        }

        // 4. CPUX AXI config: source=PLL_PERIPH0_2X, N=1 (÷2).
        //    (oreboot main.rs lines 466-470)
        self.ccu
            .cpux_axi_cfg
            .modify(D1_CPUX_AXI_CFG::CLK_SRC::PllPeriph0_2x + D1_CPUX_AXI_CFG::FACTOR_N.val(1));

        for _ in 0..1000 {
            core::hint::spin_loop();
        }

        // 5. PLL_PERIPH0: enable lock, then enable PLL.
        //    (oreboot main.rs lines 476-490)
        //    No lock wait — oreboot doesn't wait either.
        self.ccu.pll_periph0.modify(D1_PLL_PERIPH0::LOCK_EN::SET);
        self.ccu.pll_periph0.modify(D1_PLL_PERIPH0::EN::SET);

        // 6. RISCV_CFG_BGR: enable gate + deassert reset.
        //    (oreboot main.rs line 494)
        //    RISCV_CFG_BGR is at CCU + 0xD0C.
        self.ccu_write(0xD0C, 0x0001_0001);

        // 7. RISCV wakeup masks: all enabled.
        //    (oreboot main.rs lines 495-498)
        //    RISCV_CFG_BASE = 0x0601_0000, WAKEUP_MASK_REG0 = +0x24.
        const RISCV_CFG_BASE: usize = 0x0601_0000;
        for i in 0..5 {
            // SAFETY: address is a valid MMIO register within the RISCV_CFG
            // block at a fixed hardware address (0x0601_0000 + 0x24..0x34).
            unsafe {
                mmio::write32((RISCV_CFG_BASE + 0x24 + 4 * i) as *mut u32, 0xFFFF_FFFF);
            }
        }
    }

    /// Configure UART clock (APB1 source = OSC24M, gate + reset).
    ///
    /// On the D1, UART gate+reset are combined in a single register at
    /// CCU + 0x90C. Bits [5:0] = gate, bits [21:16] = reset.
    ///
    /// Uses the same 4-step sequence as oreboot's D1Serial::new():
    ///   1. assert_reset  (clear reset bit → hold UART in reset)
    ///   2. gating_mask   (clear gate bit → stop UART clock)
    ///   3. deassert_reset (set reset bit → release UART from reset)
    ///   4. gating_pass   (set gate bit → enable UART clock)
    fn clock_init_uart(&self) {
        let uart_gate_bit = 1u32 << self.uart_index;
        let uart_rst_bit = 1u32 << (16 + self.uart_index);

        // 1. Assert reset (clear reset bit).
        let val = self.ccu.uart_bgr.get();
        self.ccu.uart_bgr.set(val & !uart_rst_bit);

        // 2. Gate clock (clear gate bit).
        let val = self.ccu.uart_bgr.get();
        self.ccu.uart_bgr.set(val & !uart_gate_bit);

        // 3. Deassert reset (set reset bit).
        let val = self.ccu.uart_bgr.get();
        self.ccu.uart_bgr.set(val | uart_rst_bit);

        // 4. Pass clock (set gate bit).
        let val = self.ccu.uart_bgr.get();
        self.ccu.uart_bgr.set(val | uart_gate_bit);
    }

    /// Mux GPIO pins for UART0 (PB8=TX, PB9=RX).
    ///
    /// On the D1, UART0 uses port B pins 8 and 9 (function 6).
    fn gpio_init_uart(&self) {
        if self.uart_index != 0 {
            return;
        }

        let pio = SunxiPio::new(mmio32(D1_PIO_BASE), PioGen::Ncat2);
        pio.set_function(PORT_B, 8, UART0_TX_FUNC);
        pio.set_function(PORT_B, 9, UART0_RX_FUNC);
        pio.set_pull(PORT_B, 9, Pull::Up);
    }

    /// OSC24M frequency (UART clock source after `clock_init_uart`).
    #[must_use]
    pub const fn osc24m_freq(&self) -> u32 {
        OSC24M_FREQ
    }

    /// Read PLL_PERIPH0 frequency in Hz (MMC clock source).
    #[must_use]
    pub fn pll_periph0_freq(&self) -> u32 {
        self.ccu.pll_periph0_freq()
    }
}
