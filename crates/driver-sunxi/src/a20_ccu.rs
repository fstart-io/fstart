//! A20 clock, timer, and UART0 pin setup.

use fstart_arch::{sdelay, set_cntfrq};
use fstart_core::mmio;
use fstart_core::services::ServiceError;
use fstart_core::{Mmio32, MmioAddr, mmio32};
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};

use crate::ccu_regs::{PLL6_CFG, SunxiA20CcuRegs};

/// A20 SRAM base address.
pub const A20_SRAM_BASE: u64 = 0x0000_0000;
/// A20 CCU base address.
pub const A20_CCU_BASE: u64 = 0x01c2_0000;
/// A20 PIO base address.
pub const A20_PIO_BASE: u64 = 0x01c2_0800;
/// A20 DRAMC base address.
pub const A20_DRAMC_BASE: u64 = 0x01c0_1000;
/// A20 UART0 base address.
pub const A20_UART0_BASE: u64 = 0x01c2_8000;
/// A20 MMC0 base address.
pub const A20_MMC0_BASE: u64 = 0x01c0_f000;
/// eGON's MMC image offset.
pub const A20_EGON_MMC_OFFSET: u64 = 8192;

const OSC24M_FREQ: u32 = 24_000_000;
const PLL1_CFG_DEFAULT: u32 = 0xa100_5000;
const PLL6_CFG_DEFAULT: u32 = 0xa100_9911;
const CPU_CLK_SRC_PLL1: u32 = 0x02 << 16;
const CPU_CLK_SRC_OSC24M: u32 = 0x01 << 16;
const AXI_AHB_APB0_DEFAULT: u32 = 1 << 4;
const AHB_GATE_DMA: u32 = 1 << 6;
const AHB_GATE_SATA: u32 = 1 << 25;
const APB1_GATE_UART0: u32 = 1 << 16;

/// A20 UART whose APB1 clock gate the board uses for its console.
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
#[repr(u8)]
pub enum A20Uart {
    Uart0 = 0,
    Uart1 = 1,
    Uart2 = 2,
    Uart3 = 3,
    Uart4 = 4,
    Uart5 = 5,
    Uart6 = 6,
    Uart7 = 7,
}
const TIMER0_CTRL_OFF: usize = 0xc10;
const TIMER0_INTV_OFF: usize = 0xc14;
const TIMER0_EN: u32 = 1;
const TIMER0_RELOAD: u32 = 1 << 1;
const TIMER0_CLK_SRC_OSC24M: u32 = 1 << 2;

/// A20 early-clock policy.
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct A20CcuConfig {
    pub console: A20Uart,
}

impl A20CcuConfig {
    /// Construct the A20 clock policy for the selected board console.
    #[must_use]
    pub const fn new(console: A20Uart) -> Self {
        Self { console }
    }

    /// Validate and return the fixed policy.
    #[must_use]
    pub const fn build(self) -> Self {
        self
    }
}

impl Default for A20CcuConfig {
    fn default() -> Self {
        Self::new(A20Uart::Uart0)
    }
}

/// A20 clock controller initialized from static platform policy.
pub struct A20Ccu {
    ccu: &'static SunxiA20CcuRegs,
    _config: &'static A20CcuConfig,
}

// SAFETY: early firmware accesses fixed MMIO from one execution context.
unsafe impl Send for A20Ccu {}
// SAFETY: early firmware accesses fixed MMIO from one execution context.
unsafe impl Sync for A20Ccu {}

impl A20Ccu {
    /// Construct without touching hardware.
    #[must_use]
    pub fn new_from_config(config: &'static A20CcuConfig) -> Self {
        Self {
            // SAFETY: A20 CCU has a fixed physical MMIO mapping.
            ccu: unsafe { &*(A20_CCU_BASE as *const SunxiA20CcuRegs) },
            _config: config,
        }
    }

    /// UART0's fixed NS16550 MMIO base.
    #[must_use]
    pub const fn uart0_addr() -> MmioAddr<Mmio32> {
        mmio32(A20_UART0_BASE)
    }

    #[inline(always)]
    fn timer_write(&self, offset: usize, value: u32) {
        // SAFETY: Timer0 belongs to the A20 CCU MMIO region.
        unsafe { mmio::write32((A20_CCU_BASE as usize + offset) as *mut u32, value) }
    }

    #[inline(always)]
    fn set_bits(&self, register: &fstart_core::mmio::MmioReadWrite<u32>, bits: u32) {
        register.set(register.get() | bits);
    }

    fn timer_init(&self) {
        self.timer_write(TIMER0_INTV_OFF, u32::MAX);
        self.timer_write(
            TIMER0_CTRL_OFF,
            TIMER0_EN | TIMER0_RELOAD | TIMER0_CLK_SRC_OSC24M,
        );
    }

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

        self.set_bits(&self.ccu.ahb_gate0, AHB_GATE_DMA);
        self.ccu.pll6_cfg.set(PLL6_CFG_DEFAULT);
        self.set_bits(&self.ccu.ahb_gate0, AHB_GATE_SATA);
        self.ccu.pll6_cfg.modify(PLL6_CFG::SATA_EN::SET);
    }

    fn clock_init_uart(&self) {
        self.ccu.apb1_clk_div.set(0);
        self.set_bits(
            &self.ccu.apb1_gate,
            APB1_GATE_UART0 << self._config.console as u8,
        );
    }

    /// Program Timer0, PLL1/PLL6, CNTFRQ, and the board-selected UART gate.
    pub fn init_early(&self) -> Result<(), ServiceError> {
        self.timer_init();
        self.clock_init_safe();
        set_cntfrq(OSC24M_FREQ);
        self.clock_init_uart();
        sdelay(10_000);
        Ok(())
    }
}
