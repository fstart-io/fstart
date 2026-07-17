//! A20 legacy-bank PIO access.

use fstart_core::{mmio, Mmio32, MmioAddr};

/// A20 uses the original sunxi PIO bank layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PioGen {
    /// A10/A20 legacy banks: 0x24-byte stride and PULL at 0x1c.
    Legacy,
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

const BANK_STRIDE: usize = 0x24;
const CFG0_OFF: usize = 0;
const DRV0_OFF: usize = 0x14;
const PULL0_OFF: usize = 0x1c;

/// A20 PIO register handle.
pub struct SunxiPio {
    base: usize,
}

impl SunxiPio {
    /// Construct an A20 legacy PIO handle.
    #[must_use]
    pub const fn new(base: MmioAddr<Mmio32>, _generation: PioGen) -> Self {
        Self {
            base: base.raw() as usize,
        }
    }

    #[inline(always)]
    fn bank_base(&self, port: u8) -> usize {
        self.base + port as usize * BANK_STRIDE
    }

    /// Set a pin's alternate function.
    pub fn set_function(&self, port: u8, pin: u8, function: u8) {
        let addr = (self.bank_base(port) + CFG0_OFF + (pin as usize >> 3) * 4) as *mut u32;
        let shift = (pin as u32 & 7) * 4;
        // SAFETY: A20 PIO register calculated from its fixed MMIO mapping.
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
        let addr = (self.bank_base(port) + PULL0_OFF + (pin as usize >> 4) * 4) as *mut u32;
        let shift = (pin as u32 & 15) * 2;
        // SAFETY: A20 PIO register calculated from its fixed MMIO mapping.
        unsafe {
            let value = mmio::read32(addr.cast_const());
            mmio::write32(addr, (value & !(3 << shift)) | (pull as u32) << shift);
        }
    }

    /// Write one raw pin-function register.
    pub fn write_cfg_raw(&self, port: u8, index: usize, value: u32) {
        // SAFETY: A20 PIO register calculated from its fixed MMIO mapping.
        unsafe {
            mmio::write32(
                (self.bank_base(port) + CFG0_OFF + index * 4) as *mut u32,
                value,
            )
        }
    }

    /// Write one raw pin-drive register.
    pub fn write_drv_raw(&self, port: u8, index: usize, value: u32) {
        // SAFETY: A20 PIO register calculated from its fixed MMIO mapping.
        unsafe {
            mmio::write32(
                (self.bank_base(port) + DRV0_OFF + index * 4) as *mut u32,
                value,
            )
        }
    }

    /// Write one raw pin-pull register.
    pub fn write_pull_raw(&self, port: u8, index: usize, value: u32) {
        // SAFETY: A20 PIO register calculated from its fixed MMIO mapping.
        unsafe {
            mmio::write32(
                (self.bank_base(port) + PULL0_OFF + index * 4) as *mut u32,
                value,
            )
        }
    }
}
