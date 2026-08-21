//! Lenovo PMH7 deep-sleep hub (IO base 0x15e0).
//!
//! Ported from coreboot `ec/lenovo/pmh7`. The hub exposes an indexed
//! register file: address low/high at base+0x0c/0x0d, data at base+0x0e.
//! It gates the display backlight, dock-event SMI source, touchpad and
//! trackpoint power, ultrabay power, and (on boards that have one) the
//! discrete GPU.

use fstart_core::pio::{inb, outb};
use fstart_log::{Hex, info};

const PMH7_ADDR_L: u16 = 0x0c;
const PMH7_ADDR_H: u16 = 0x0d;
const PMH7_DATA: u16 = 0x0e;

const REG_ID: u8 = 0xc2;
const REG_REV: u8 = 0xc3;

/// An initialized PMH7 hub handle.
#[derive(Debug, Clone, Copy)]
pub struct Pmh7 {
    base: u16,
}

impl Pmh7 {
    /// Create a hub handle for the given IO base (`0x15e0` on ThinkPads).
    #[must_use]
    pub const fn new(base: u16) -> Self {
        Self { base }
    }

    fn read_register(&self, reg: u8) -> u8 {
        // SAFETY: the PMH7 IO window is decoded by board LPC config.
        unsafe {
            outb(self.base + PMH7_ADDR_L, reg);
            outb(self.base + PMH7_ADDR_H, 0);
            inb(self.base + PMH7_DATA)
        }
    }

    fn write_register(&self, reg: u8, val: u8) {
        // SAFETY: see `read_register`.
        unsafe {
            outb(self.base + PMH7_ADDR_L, reg);
            outb(self.base + PMH7_ADDR_H, 0);
            outb(self.base + PMH7_DATA, val);
        }
    }

    fn set_bit(&self, reg: u8, bit: u8) {
        let val = self.read_register(reg);
        self.write_register(reg, val | (1 << bit));
    }

    fn clear_bit(&self, reg: u8, bit: u8) {
        let val = self.read_register(reg);
        self.write_register(reg, val & !(1 << bit));
    }

    /// Probe the hub by reading its ID register; returns `(id, revision)`.
    pub fn probe(&self) -> Option<(u8, u8)> {
        let id = self.read_register(REG_ID);
        if id == 0 || id == 0xff {
            return None;
        }
        Some((id, self.read_register(REG_REV)))
    }

    pub fn backlight_enable(&self, on: bool) {
        if on {
            self.set_bit(0x50, 5);
        } else {
            self.clear_bit(0x50, 5);
        }
    }

    pub fn dock_event_enable(&self, on: bool) {
        if on {
            self.set_bit(0x60, 3);
        } else {
            self.clear_bit(0x60, 3);
        }
    }

    /// Note: the touchpad/trackpoint bits are active-low (clear = powered).
    pub fn touchpad_enable(&self, on: bool) {
        if on {
            self.clear_bit(0x51, 2);
        } else {
            self.set_bit(0x51, 2);
        }
    }

    pub fn trackpoint_enable(&self, on: bool) {
        if on {
            self.clear_bit(0x51, 0);
        } else {
            self.set_bit(0x51, 0);
        }
    }

    pub fn ultrabay_power_enable(&self, on: bool) {
        if on {
            self.clear_bit(0x62, 0);
        } else {
            self.set_bit(0x62, 0);
        }
    }

    pub fn log_identity(&self) {
        match self.probe() {
            Some((id, rev)) => info!(
                "lenovo-pmh7: id={} revision={} at base {}",
                Hex(u64::from(id)),
                Hex(u64::from(rev)),
                Hex(u64::from(self.base))
            ),
            None => info!(
                "lenovo-pmh7: no device at base {}",
                Hex(u64::from(self.base))
            ),
        }
    }
}
