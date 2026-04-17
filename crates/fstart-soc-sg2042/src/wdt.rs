//! SG2042 DesignWare APB watchdog timer driver.
//!
//! Starts the watchdog at boot with a configurable timeout and provides
//! a `stop()` method for clean shutdown after successful boot.
//!
//! Hardware reference: `mango_common.c` — `bm_wdt_start()`, `bm_wdt_stop()`.

use serde::{Deserialize, Serialize};
use tock_registers::{
    interfaces::{Readable, Writeable},
    register_bitfields, register_structs,
    registers::ReadWrite,
};

use fstart_services::device::{Device, DeviceError};

// ===================================================================
// Register definitions
// ===================================================================

register_structs! {
    /// DesignWare APB watchdog registers — `WDT_BASE = 0x7030_0040_00`.
    pub WdtRegs {
        (0x00 => pub cr:   ReadWrite<u32>), // control: bit0=EN, bit1=RMOD, bits4:2=RPL
        (0x04 => pub torr: ReadWrite<u32>), // timeout range: 2^(16+top)@100MHz
        (0x08 => pub ccvr: ReadWrite<u32>), // current counter value (RO)
        (0x0C => pub crr:  ReadWrite<u32>), // counter restart (write 0x76)
        (0x10 => pub stat: ReadWrite<u32>), // interrupt status
        (0x14 => pub eoi:  ReadWrite<u32>), // end of interrupt / clear
        (0x18 => @END),
    }
}

const _: () = assert!(core::mem::size_of::<WdtRegs>() == 0x18);

/// Magic value written to WDT_CRR to restart (kick) the counter.
const WDT_CRR_KICK: u32 = 0x76;

// ===================================================================
// Config
// ===================================================================

/// Configuration for the SG2042 watchdog driver.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Sg2042WdtConfig {
    /// Watchdog register base address (`WDT_BASE = 0x7030_0040_00`).
    pub wdt_base: u64,
    /// SYS_CTRL (TOP) base address — needed to toggle warm-reset enable.
    pub sys_ctrl_base: u64,
    /// Timeout exponent: timeout = 2^(16 + top) cycles at 100 MHz.
    /// Range 0–15. 0 ≈ 655 ms, 15 ≈ 21474 s (Pioneer boot default: 15).
    pub timeout_top: u8,
}

// ===================================================================
// Driver struct
// ===================================================================

/// SG2042 DesignWare APB watchdog driver.
pub struct Sg2042Wdt {
    regs: &'static WdtRegs,
    /// SYS_CTRL TOP_CTRL register address (for warm-reset enable bit).
    top_ctrl_addr: usize,
    /// SYS_CTRL SOFT_RST0 register address (for WDT soft-reset).
    soft_rst0_addr: usize,
    /// SYS_CTRL WDT_RST_STATUS address (for reset status clear).
    wdt_rst_status_addr: usize,
    timeout_top: u8,
}

// SAFETY: MMIO registers are at fixed hardware addresses from the board RON.
// Early AArch64 boot is single-threaded.
unsafe impl Send for Sg2042Wdt {}
unsafe impl Sync for Sg2042Wdt {}

impl Device for Sg2042Wdt {
    const NAME: &'static str = "sg2042-wdt";
    const COMPATIBLE: &'static [&'static str] = &["sophgo,sg2042-wdt", "snps,dw-wdt"];
    type Config = Sg2042WdtConfig;

    fn new(config: &Sg2042WdtConfig) -> Result<Self, DeviceError> {
        Ok(Self {
            // SAFETY: address from board RON validated against platform_def.h.
            regs: unsafe { &*(config.wdt_base as *const WdtRegs) },
            top_ctrl_addr: config.sys_ctrl_base as usize + 0x008,
            soft_rst0_addr: config.sys_ctrl_base as usize + 0xC00,
            wdt_rst_status_addr: config.sys_ctrl_base as usize + 0x01C,
            timeout_top: config.timeout_top,
        })
    }

    fn init(&mut self) -> Result<(), DeviceError> {
        self.start(self.timeout_top);
        Ok(())
    }
}

impl Sg2042Wdt {
    /// Start the watchdog with the given timeout exponent.
    ///
    /// Sequence mirrors `bm_wdt_start()` in `mango_common.c`.
    pub fn start(&self, top: u8) {
        let top = (top & 0xF) as u32;

        // mango_common.c: mmio_setbits_32(TOP+REG_TOP_CTRL, BIT(2))
        // SAFETY: top_ctrl_addr is within the SYS_CTRL MMIO window.
        unsafe {
            let v = core::ptr::read_volatile(self.top_ctrl_addr as *const u32);
            core::ptr::write_volatile(self.top_ctrl_addr as *mut u32, v | (1 << 2));
        }

        // mango_common.c: TORR = top | (top << 4)
        self.regs.torr.set(top | (top << 4));
        // mango_common.c: counter restart kick
        self.regs.crr.set(WDT_CRR_KICK);
        // mango_common.c: CR = 0x11 — EN(bit0)=1, RMOD(bit1)=0 (reset-only)
        self.regs.cr.set(0x11);
    }

    /// Stop the watchdog.
    ///
    /// Sequence mirrors `bm_wdt_stop()` in `mango_common.c`.
    pub fn stop(&self) {
        // mango_common.c: mmio_clrbits_32(TOP+REG_TOP_CTRL, BIT(2))
        // SAFETY: addresses within SYS_CTRL MMIO window.
        unsafe {
            let v = core::ptr::read_volatile(self.top_ctrl_addr as *const u32);
            core::ptr::write_volatile(self.top_ctrl_addr as *mut u32, v & !(1 << 2));

            // Assert WDT soft-reset (clear bit 10)
            // mango_common.c: mmio_clrbits_32(TOP+SOFT_RST0, BIT(10))
            let v = core::ptr::read_volatile(self.soft_rst0_addr as *const u32);
            core::ptr::write_volatile(self.soft_rst0_addr as *mut u32, v & !(1 << 10));
            fstart_arch::udelay(1);
            // Deassert: mango_common.c: mmio_setbits_32(TOP+SOFT_RST0, BIT(10))
            let v = core::ptr::read_volatile(self.soft_rst0_addr as *const u32);
            core::ptr::write_volatile(self.soft_rst0_addr as *mut u32, v | (1 << 10));

            // Clear WDT reset status: mango_common.c: mmio_write_32(TOP+RST_STATUS, 1)
            core::ptr::write_volatile(self.wdt_rst_status_addr as *mut u32, 1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_wdt_regs_size() {
        assert_eq!(core::mem::size_of::<WdtRegs>(), 0x18);
    }
}
