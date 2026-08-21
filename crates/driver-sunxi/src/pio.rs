//! Allwinner sunxi PIO (GPIO) pin controller.
//!
//! Pin function selection, pull configuration, and raw bank writes for all
//! sunxi generations. The register layout within each port bank is identical
//! across A20/H3/H5 (legacy) and D1/T113 (NCAT2); the generations differ only
//! in bank stride and pull register offset:
//!
//! | Property     | Legacy (A20, H3, H5) | NCAT2 (D1, T113) |
//! |--------------|----------------------|------------------|
//! | Bank stride  | 0x24                 | 0x30             |
//! | PULL0 offset | +0x1C                | +0x24            |
//!
//! Modeled on U-Boot `drivers/gpio/sunxi_gpio.c` (`SUNXI_NEW_PINCTRL`).

use fstart_core::{Mmio32, MmioAddr, mmio};

/// PIO pin controller generation — determines bank stride and pull offset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PioGen {
    /// A10/A20/H3/H5 legacy banks: 0x24-byte stride and PULL at 0x1c.
    Legacy,
    /// D1, T113, R528 (NCAT2): 0x30-byte stride and PULL at 0x24.
    Ncat2,
}

/// Pin pull-up/down configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum Pull {
    Disabled = 0,
    Up = 1,
    Down = 2,
}

/// Port A index.
pub const PORT_A: u8 = 0;
/// Port B index.
pub const PORT_B: u8 = 1;
/// Port F index.
pub const PORT_F: u8 = 5;

const BANK_STRIDE_LEGACY: usize = 0x24;
const BANK_STRIDE_NCAT2: usize = 0x30;
const CFG0_OFF: usize = 0;
const DRV0_OFF: usize = 0x14;
const PULL0_OFF_LEGACY: usize = 0x1c;
const PULL0_OFF_NCAT2: usize = 0x24;

/// Sunxi PIO register handle.
pub struct SunxiPio {
    base: usize,
    bank_stride: usize,
    pull0_off: usize,
}

impl SunxiPio {
    /// Construct a PIO handle for the given controller generation.
    #[must_use]
    pub const fn new(base: MmioAddr<Mmio32>, generation: PioGen) -> Self {
        let (bank_stride, pull0_off) = match generation {
            PioGen::Legacy => (BANK_STRIDE_LEGACY, PULL0_OFF_LEGACY),
            PioGen::Ncat2 => (BANK_STRIDE_NCAT2, PULL0_OFF_NCAT2),
        };
        Self {
            base: base.raw() as usize,
            bank_stride,
            pull0_off,
        }
    }

    #[inline(always)]
    fn bank_base(&self, port: u8) -> usize {
        self.base + port as usize * self.bank_stride
    }

    /// Set a pin's alternate function.
    pub fn set_function(&self, port: u8, pin: u8, function: u8) {
        let addr = (self.bank_base(port) + CFG0_OFF + (pin as usize >> 3) * 4) as *mut u32;
        let shift = (pin as u32 & 7) * 4;
        // SAFETY: sunxi PIO register calculated from its fixed MMIO mapping.
        unsafe {
            let value = mmio::read32(addr.cast_const());
            mmio::write32(
                addr,
                (value & !(0xf << shift)) | ((function as u32 & 0xf) << shift),
            );
        }
    }

    /// Set a pin's pull mode.
    pub fn set_pull(&self, port: u8, pin: u8, pull: Pull) {
        let addr = (self.bank_base(port) + self.pull0_off + (pin as usize >> 4) * 4) as *mut u32;
        let shift = (pin as u32 & 15) * 2;
        // SAFETY: sunxi PIO register calculated from its fixed MMIO mapping.
        unsafe {
            let value = mmio::read32(addr.cast_const());
            mmio::write32(addr, (value & !(3 << shift)) | (pull as u32) << shift);
        }
    }

    /// Write one raw pin-function register.
    pub fn write_cfg_raw(&self, port: u8, index: usize, value: u32) {
        // SAFETY: sunxi PIO register calculated from its fixed MMIO mapping.
        unsafe {
            mmio::write32(
                (self.bank_base(port) + CFG0_OFF + index * 4) as *mut u32,
                value,
            )
        }
    }

    /// Write one raw pin-drive register.
    pub fn write_drv_raw(&self, port: u8, index: usize, value: u32) {
        // SAFETY: sunxi PIO register calculated from its fixed MMIO mapping.
        unsafe {
            mmio::write32(
                (self.bank_base(port) + DRV0_OFF + index * 4) as *mut u32,
                value,
            )
        }
    }

    /// Write one raw pin-pull register.
    pub fn write_pull_raw(&self, port: u8, index: usize, value: u32) {
        // SAFETY: sunxi PIO register calculated from its fixed MMIO mapping.
        unsafe {
            mmio::write32(
                (self.bank_base(port) + self.pull0_off + index * 4) as *mut u32,
                value,
            )
        }
    }
}
