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
//!
//! CCU register block: `0x0200_1000`
//! PIO (GPIO) register block: `0x0200_0000`

#![no_std]
#![allow(clippy::identity_op)] // Bit-field shifts like (x << 0) document register layout

use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};

use fstart_services::device::{Device, DeviceError};
use fstart_services::{ClockController, ServiceError};

use fstart_sunxi_ccu_regs::{SunxiD1CcuRegs, D1_CPUX_AXI_CFG, D1_PLL_CPUX, D1_PLL_PERIPH0};
use fstart_sunxi_pio::{PioGen, Pull, SunxiPio, PORT_B};

use fstart_arch::udelay;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// OSC24M frequency (Hz) — the D1's 24 MHz DCXO.
const OSC24M_FREQ: u32 = 24_000_000;

/// D1 UART0 TX alternate function on PB8.
const UART0_TX_FUNC: u8 = 6;
/// D1 UART0 RX alternate function on PB9.
const UART0_RX_FUNC: u8 = 6;

/// Typed configuration for the D1/T113 CCU driver.
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
pub struct SunxiD1CcuConfig {
    /// CCU register base address (typically `0x0200_1000`).
    pub ccu_base: u64,
    /// PIO (GPIO) register base address (typically `0x0200_0000`).
    pub pio_base: u64,
    /// UART index to configure (0-based, 0-5).
    pub uart_index: u8,
}

/// Allwinner D1/T113 Clock Control Unit + GPIO pin mux driver.
pub struct SunxiD1Ccu {
    ccu: &'static SunxiD1CcuRegs,
    /// Raw base address for registers not covered by the typed struct
    /// (e.g. DRAM_CLK at offset 0x800, DMA_BGR at 0x70C).
    ccu_base: usize,
    pio_base: usize,
    uart_index: u8,
}

// SAFETY: MMIO registers are at fixed hardware addresses from the board RON.
unsafe impl Send for SunxiD1Ccu {}
unsafe impl Sync for SunxiD1Ccu {}

impl SunxiD1Ccu {
    /// Read a raw CCU register by offset.
    #[inline(always)]
    fn ccu_read(&self, offset: usize) -> u32 {
        // SAFETY: address is a valid MMIO register within the CCU block at a
        // fixed hardware address (ccu_base from board RON).
        unsafe { fstart_mmio::read32((self.ccu_base + offset) as *const u32) }
    }

    /// Write a raw CCU register by offset.
    #[inline(always)]
    fn ccu_write(&self, offset: usize, val: u32) {
        // SAFETY: address is a valid MMIO register within the CCU block at a
        // fixed hardware address (ccu_base from board RON).
        unsafe { fstart_mmio::write32((self.ccu_base + offset) as *mut u32, val) }
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
                fstart_mmio::write32((RISCV_CFG_BASE + 0x24 + 4 * i) as *mut u32, 0xFFFF_FFFF);
            }
        }
    }

    /// Step 2: Configure UART clock (APB1 source = OSC24M, gate + reset).
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

    /// Step 3: Mux GPIO pins for UART0 (PB8=TX, PB9=RX).
    ///
    /// On the D1, UART0 uses port B pins 8 and 9 (function 6).
    fn gpio_init_uart(&self) {
        if self.uart_index != 0 {
            return;
        }

        let pio = SunxiPio::new(self.pio_base, PioGen::Ncat2);
        pio.set_function(PORT_B, 8, UART0_TX_FUNC);
        pio.set_function(PORT_B, 9, UART0_RX_FUNC);
        pio.set_pull(PORT_B, 9, Pull::Up);
    }
}

impl Device for SunxiD1Ccu {
    const NAME: &'static str = "sunxi-d1-ccu";
    const COMPATIBLE: &'static [&'static str] = &["allwinner,sun20i-d1-ccu"];
    type Config = SunxiD1CcuConfig;

    fn new(config: &SunxiD1CcuConfig) -> Result<Self, DeviceError> {
        Ok(Self {
            // SAFETY: addresses come from the board RON, validated by codegen.
            ccu: unsafe { &*(config.ccu_base as *const SunxiD1CcuRegs) },
            ccu_base: config.ccu_base as usize,
            pio_base: config.pio_base as usize,
            uart_index: config.uart_index,
        })
    }

    fn init(&self) -> Result<(), DeviceError> {
        // The D1 BROM programs basic PLLs before loading the eGON image.
        // However, oreboot's main() programs several additional clocks
        // before DRAM init that the BROM may not fully configure.
        self.clock_init_pre_dram();
        self.clock_init_uart();
        self.gpio_init_uart();
        udelay(100);
        Ok(())
    }
}

impl ClockController for SunxiD1Ccu {
    fn enable_clock(&self, gate_id: u32) -> Result<(), ServiceError> {
        // Generic clock gate enable via the UART BGR register (for UART gates).
        let val = self.ccu.uart_bgr.get();
        self.ccu.uart_bgr.set(val | (1 << gate_id));
        Ok(())
    }

    fn disable_clock(&self, gate_id: u32) -> Result<(), ServiceError> {
        let val = self.ccu.uart_bgr.get();
        self.ccu.uart_bgr.set(val & !(1 << gate_id));
        Ok(())
    }

    fn get_frequency(&self, clock_id: u32) -> Result<u32, ServiceError> {
        match clock_id {
            0 => Ok(OSC24M_FREQ),                 // OSC24M
            1 => Ok(self.ccu.pll_periph0_freq()), // PLL_PERIPH0
            _ => Err(ServiceError::NotSupported),
        }
    }
}
