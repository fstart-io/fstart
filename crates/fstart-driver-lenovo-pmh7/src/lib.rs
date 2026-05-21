//! Lenovo PMH7 power-management hub driver.
//!
//! Ported from coreboot `ec/lenovo/pmh7`. PMH7 uses a simple LPC I/O indirect
//! register window at base 0x15e0 on X200/X61-era ThinkPads.

#![no_std]

use fstart_services::device::{Device, DeviceError};
use serde::{Deserialize, Serialize};

const DEFAULT_BASE: u16 = 0x15e0;
const REG_ID: u16 = 0x00c2;
const REG_REV: u16 = 0x00c3;

/// Lenovo PMH7 configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LenovoPmh7Config {
    /// LPC I/O base, normally 0x15e0.
    #[serde(default = "default_base")]
    pub base: u16,
    /// Enable panel backlight output, coreboot `backlight_enable`.
    #[serde(default)]
    pub backlight_enable: bool,
    /// Enable dock events, coreboot `dock_event_enable`.
    #[serde(default)]
    pub dock_event_enable: bool,
    /// Enable touchpad; coreboot defaults CMOS option `touchpad` to on.
    #[serde(default = "default_true")]
    pub touchpad_enable: bool,
    /// Enable trackpoint; coreboot defaults CMOS option `trackpoint` to on.
    #[serde(default = "default_true")]
    pub trackpoint_enable: bool,
}

impl Default for LenovoPmh7Config {
    fn default() -> Self {
        Self {
            base: DEFAULT_BASE,
            backlight_enable: false,
            dock_event_enable: false,
            touchpad_enable: true,
            trackpoint_enable: true,
        }
    }
}

const fn default_base() -> u16 {
    DEFAULT_BASE
}

const fn default_true() -> bool {
    true
}

/// Lenovo PMH7 device.
pub struct LenovoPmh7 {
    config: &'static LenovoPmh7Config,
}

impl LenovoPmh7 {
    fn addr_l(&self) -> u16 {
        self.config.base + 0x0c
    }

    fn addr_h(&self) -> u16 {
        self.config.base + 0x0d
    }

    fn data(&self) -> u16 {
        self.config.base + 0x0e
    }

    /// Read a PMH7 register through the indirect I/O window.
    pub fn read_reg(&self, reg: u16) -> u8 {
        // SAFETY: PMH7 LPC I/O range is decoded by the southbridge board config.
        unsafe {
            fstart_pio::outb(self.addr_l(), reg as u8);
            fstart_pio::outb(self.addr_h(), (reg >> 8) as u8);
            fstart_pio::inb(self.data())
        }
    }

    /// Write a PMH7 register through the indirect I/O window.
    pub fn write_reg(&self, reg: u16, val: u8) {
        // SAFETY: PMH7 LPC I/O range is decoded by the southbridge board config.
        unsafe {
            fstart_pio::outb(self.addr_l(), reg as u8);
            fstart_pio::outb(self.addr_h(), (reg >> 8) as u8);
            fstart_pio::outb(self.data(), val);
        }
    }

    /// Set one PMH7 register bit.
    pub fn set_bit(&self, reg: u16, bit: u8) {
        self.write_reg(reg, self.read_reg(reg) | (1 << bit));
    }

    /// Clear one PMH7 register bit.
    pub fn clear_bit(&self, reg: u16, bit: u8) {
        self.write_reg(reg, self.read_reg(reg) & !(1 << bit));
    }

    /// Enable or disable panel backlight (PMH7 0x50 bit 5).
    pub fn backlight_enable(&self, on: bool) {
        if on {
            self.set_bit(0x50, 5);
        } else {
            self.clear_bit(0x50, 5);
        }
    }

    /// Enable or disable dock events (PMH7 0x60 bit 3).
    pub fn dock_event_enable(&self, on: bool) {
        if on {
            self.set_bit(0x60, 3);
        } else {
            self.clear_bit(0x60, 3);
        }
    }

    /// Enable or disable touchpad; PMH7 bit is active-low.
    pub fn touchpad_enable(&self, on: bool) {
        if on {
            self.clear_bit(0x51, 2);
        } else {
            self.set_bit(0x51, 2);
        }
    }

    /// Enable or disable trackpoint; PMH7 bit is active-low.
    pub fn trackpoint_enable(&self, on: bool) {
        if on {
            self.clear_bit(0x51, 0);
        } else {
            self.set_bit(0x51, 0);
        }
    }

    /// Enable or disable ultrabay power; PMH7 bit is active-low.
    pub fn ultrabay_power_enable(&self, on: bool) {
        if on {
            self.clear_bit(0x62, 0);
        } else {
            self.set_bit(0x62, 0);
        }
    }

    /// Enable or disable discrete GPU power using the coreboot sequence.
    pub fn dgpu_power_enable(&self, on: bool) {
        if on {
            self.clear_bit(0x50, 7);
            self.set_bit(0x50, 3);
            fstart_arch_x86::udelay(10_000);
            self.set_bit(0x50, 7);
            fstart_arch_x86::udelay(50_000);
        } else {
            self.clear_bit(0x50, 7);
            fstart_arch_x86::udelay(100);
            self.clear_bit(0x50, 3);
        }
    }

    /// Return whether PMH7 reports discrete GPU power enabled.
    pub fn dgpu_power_state(&self) -> bool {
        (self.read_reg(0x50) & 0x08) == 0x08
    }
}

impl Device for LenovoPmh7 {
    const NAME: &'static str = "lenovo-pmh7";
    const COMPATIBLE: &'static [&'static str] = &["lenovo,pmh7"];
    type Config = LenovoPmh7Config;

    fn new(config: &'static Self::Config) -> Result<Self, DeviceError> {
        Ok(Self { config })
    }

    fn init(&mut self) -> Result<(), DeviceError> {
        self.backlight_enable(self.config.backlight_enable);
        self.dock_event_enable(self.config.dock_event_enable);
        self.touchpad_enable(self.config.touchpad_enable);
        self.trackpoint_enable(self.config.trackpoint_enable);
        fstart_log::info!(
            "PMH7: ID={:#x} rev={:#x}",
            self.read_reg(REG_ID) as u32,
            self.read_reg(REG_REV) as u32
        );
        Ok(())
    }
}
