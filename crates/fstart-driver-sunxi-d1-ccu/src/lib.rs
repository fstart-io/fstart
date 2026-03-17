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

use fstart_sunxi_ccu_regs::{
    SunxiD1CcuRegs, D1_APB0_CLK, D1_APB1_CLK, D1_CPUX_AXI_CFG, D1_MBUS_CLK, D1_PLL_CPUX,
    D1_PLL_PERIPH0, D1_PSI_CLK,
};

use fstart_arch::udelay;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// OSC24M frequency (Hz) — the D1's 24 MHz DCXO.
const OSC24M_FREQ: u32 = 24_000_000;

/// PLL_CPUX target: 408 MHz = 24 * 17.
/// N = 17, raw N-1 = 16.
const PLL_CPUX_N_408MHZ: u32 = 16;

// ---------------------------------------------------------------------------
// PIO (GPIO) constants for D1 UART0 on PB8/PB9
// ---------------------------------------------------------------------------

/// Port B configuration register 1 offset (from PIO base).
/// PB8-PB11 are configured in PB_CFG1 (pins 8-11).
const PIO_PB_CFG1_OFF: u32 = 0x34;
/// Port B pull register 0 offset (from PIO base).
const PIO_PB_PULL0_OFF: u32 = 0x54;

/// PB8 function 6 = UART0_TX.
const PB8_UART0_FUNC: u32 = 6;
/// PB9 function 6 = UART0_RX.
const PB9_UART0_FUNC: u32 = 6;
/// PB8 config shift within PB_CFG1 (pin 8 = bit 0 of CFG1).
/// Formula: (pin_number - 8) * 4 = 0.
const PB8_CFG_SHIFT: u32 = 0;
/// PB9 config shift within PB_CFG1 (pin 9 = bit 4 of CFG1).
const PB9_CFG_SHIFT: u32 = (9 - 8) * 4;
/// PB9 pull shift within PB_PULL0 (pin 9 × 2 bits = bit 18).
const PB9_PULL_SHIFT: u32 = 9 * 2;
/// Pull-up value.
const GPIO_PULL_UP: u32 = 1;

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
    #[inline(always)]
    fn pio_read(&self, offset: u32) -> u32 {
        // SAFETY: address is a valid MMIO register within the PIO block at a
        // fixed hardware address (pio_base from board RON).
        unsafe { fstart_mmio::read32((self.pio_base + offset as usize) as *const u32) }
    }

    #[inline(always)]
    fn pio_write(&self, offset: u32, val: u32) {
        // SAFETY: address is a valid MMIO register within the PIO block at a
        // fixed hardware address (pio_base from board RON).
        unsafe { fstart_mmio::write32((self.pio_base + offset as usize) as *mut u32, val) }
    }

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

    /// Full PLL programming from a cold start (safe clock init).
    ///
    /// Ported from U-Boot `clock_init_safe()` (NCAT2/sun50i_h6 path).
    /// Sets PLL_CPUX to 408 MHz, PLL_PERIPH0 to 600 MHz,
    /// PSI/AHB/APB dividers, and MBUS clock.
    ///
    /// Currently unused — the D1 BROM programs these PLLs before
    /// loading the eGON image, so we trust its defaults (matching
    /// oreboot's approach). Retained for future use when running
    /// from a context where clocks need explicit programming
    /// (e.g. FEL boot, or a secondary stage that cannot trust the BROM).
    ///
    /// This is separate from [`clock_init_pre_dram`] because they serve
    /// different purposes: `clock_init_safe` performs a full cold-start PLL
    /// configuration (switching CPU to OSC24M, reprogramming all PLLs from
    /// scratch, waiting for locks), while `clock_init_pre_dram` only tweaks
    /// specific clocks via read-modify-write on top of BROM defaults to
    /// prepare for DRAM controller initialization.
    #[allow(dead_code)]
    fn clock_init_safe(&self) {
        // 1. Switch CPU to safe OSC24M before reprogramming PLL_CPUX.
        self.ccu
            .cpux_axi_cfg
            .write(D1_CPUX_AXI_CFG::CLK_SRC::Osc24M);
        udelay(1);

        // 2. Program PLL_CPUX to 408 MHz and enable.
        //    N=16 (raw) → actual = 16+1 = 17, freq = 24 * 17 = 408 MHz.
        self.ccu.pll_cpux.write(
            D1_PLL_CPUX::EN::SET
                + D1_PLL_CPUX::LDO_EN::SET
                + D1_PLL_CPUX::LOCK_EN::SET
                + D1_PLL_CPUX::N.val(PLL_CPUX_N_408MHZ),
        );
        // Wait for PLL lock.
        while self.ccu.pll_cpux.read(D1_PLL_CPUX::LOCK) == 0 {
            core::hint::spin_loop();
        }
        // Enable output gate after lock.
        self.ccu.pll_cpux.modify(D1_PLL_CPUX::OUT_EN::SET);
        udelay(1);

        // 3. Switch CPU to PLL_CPUX.
        self.ccu
            .cpux_axi_cfg
            .write(D1_CPUX_AXI_CFG::CLK_SRC::PllCpux);
        udelay(1);

        // 4. Enable PLL_PERIPH0 at 600 MHz and wait for lock.
        //    N=99 (raw) → actual = 100, P0=1 (raw) → actual = 2.
        //    freq = 24 * 100 / 2 / 2 = 600 MHz.
        self.ccu.pll_periph0.write(
            D1_PLL_PERIPH0::EN::SET
                + D1_PLL_PERIPH0::LDO_EN::SET
                + D1_PLL_PERIPH0::LOCK_EN::SET
                + D1_PLL_PERIPH0::OUT_EN::SET
                + D1_PLL_PERIPH0::N.val(99)
                + D1_PLL_PERIPH0::P0.val(1),
        );
        while self.ccu.pll_periph0.read(D1_PLL_PERIPH0::LOCK) == 0 {
            core::hint::spin_loop();
        }

        // 5. Set PSI/AHB = PLL_PERIPH0/3 = 200 MHz.
        //    Source=PLL_PERIPH0(3), N=0(÷1), M=2 (raw) → actual = 2+1 = 3.
        self.ccu
            .psi_clk
            .write(D1_PSI_CLK::CLK_SRC::PllPeriph0 + D1_PSI_CLK::FACTOR_M.val(2));

        // 6. Set APB0 = PSI/2 = 100 MHz.
        //    Source=PSI(2), N=0(÷1), M=1 (raw) → actual = 1+1 = 2.
        self.ccu
            .apb0_clk
            .write(D1_APB0_CLK::CLK_SRC::Psi + D1_APB0_CLK::FACTOR_M.val(1));

        // 7. Set APB1 = OSC24M (for UART).
        //    Source=OSC24M(0), N=0(÷1), M=0(÷1) — all fields zero.
        self.ccu.apb1_clk.write(D1_APB1_CLK::CLK_SRC::Osc24M);

        // 8. Program MBUS clock: PLL_PERIPH0_2X/3 = 400 MHz.
        //    Gate=1, Reset=1, Source=PLL_PERIPH0_2X(1), M=2 (raw) → actual = 2+1 = 3.
        //    PLL_PERIPH0_2X = 1200 MHz, /3 = 400 MHz.
        self.ccu.mbus_clk.write(
            D1_MBUS_CLK::GATE::SET
                + D1_MBUS_CLK::RST::SET
                + D1_MBUS_CLK::CLK_SRC::PllPeriph0_2x
                + D1_MBUS_CLK::M.val(2),
        );
        udelay(1);
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
    /// Uses read-modify-write and no PLL lock waits, matching oreboot.
    ///
    /// This is separate from [`clock_init_safe`] because it only performs
    /// incremental adjustments on top of BROM defaults (read-modify-write,
    /// no lock waits), while `clock_init_safe` is a full cold-start PLL
    /// programming sequence meant for contexts where no prior clock setup
    /// can be assumed.
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

        // PB_CFG1: set PB8 and PB9 to function 6 (UART0).
        let mut cfg = self.pio_read(PIO_PB_CFG1_OFF);
        cfg &= !(0xF << PB8_CFG_SHIFT);
        cfg |= PB8_UART0_FUNC << PB8_CFG_SHIFT;
        cfg &= !(0xF << PB9_CFG_SHIFT);
        cfg |= PB9_UART0_FUNC << PB9_CFG_SHIFT;
        self.pio_write(PIO_PB_CFG1_OFF, cfg);

        // PB_PULL0: set PB9 (RX) to pull-up.
        let mut pull = self.pio_read(PIO_PB_PULL0_OFF);
        pull &= !(0x3 << PB9_PULL_SHIFT);
        pull |= GPIO_PULL_UP << PB9_PULL_SHIFT;
        self.pio_write(PIO_PB_PULL0_OFF, pull);
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
        // NOTE: clock_init_safe() (full PLL programming) is intentionally
        // skipped. The D1 BROM already programs basic clocks before loading
        // the eGON image, and oreboot confirms this works.
        //
        // However, oreboot's main() DOES program several clocks before DRAM
        // init that the BROM may not fully configure. We replicate that here.
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
