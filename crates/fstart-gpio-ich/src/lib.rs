//! Intel ICH/PCH southbridge GPIO controller.
//!
//! Provides GPIO pad configuration and runtime get/set/input/output for
//! Intel southbridges that use the legacy GPIOBASE I/O port interface:
//! ICH7, ICH8, ICH9, ICH10, NM10, and 5/6-series PCH.
//!
//! The GPIO space is divided into three sets of 32 pins each:
//!
//! | Set | Pins   | GPIOBASE offsets |
//! |-----|--------|------------------|
//! | 1   | 0–31   | 0x00 / 0x04 / 0x0C / 0x18 / 0x2C / 0x60 |
//! | 2   | 32–63  | 0x30 / 0x34 / 0x38 / 0x64 |
//! | 3   | 64–75  | 0x40 / 0x44 / 0x48 / 0x68 |
//!
//! Each set has registers for:
//! - **USE_SEL**: 0 = native function, 1 = GPIO mode
//! - **IO_SEL**: 0 = output, 1 = input (only meaningful in GPIO mode)
//! - **LVL**: output level (0 = low, 1 = high) / input state read-back
//! - **RST_SEL**: reset type (0 = PWROK, 1 = RSMRST#)
//!
//! Set 1 additionally has:
//! - **BLINK**: 0 = no blink, 1 = blink
//! - **INV**: input inversion (GPI_INV)
//!
//! # Board configuration
//!
//! GPIO pads are configured from the board RON file using [`GpioConfig`],
//! which contains three [`GpioSet`] bitfields.  The RON syntax is:
//!
//! ```ron
//! gpio: (
//!     set1: ( mode: 0x1F0FF4C1, direction: 0, level: 0, blink: 0, invert: 0, reset: 0 ),
//!     set2: ( mode: 0x00000066, direction: 0x00000066, level: 0, blink: 0, invert: 0, reset: 0 ),
//!     set3: ( mode: 0, direction: 0, level: 0, blink: 0, invert: 0, reset: 0 ),
//! )
//! ```

#![no_std]

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// GPIOBASE register offsets
// ---------------------------------------------------------------------------

// Set 1 (pins 0–31)
const GPIO_USE_SEL: u16 = 0x00;
const GP_IO_SEL: u16 = 0x04;
const GP_LVL: u16 = 0x0C;
const GPO_BLINK: u16 = 0x18;
const GPI_INV: u16 = 0x2C;
const GP_RST_SEL1: u16 = 0x60;

// Set 2 (pins 32–63)
const GPIO_USE_SEL2: u16 = 0x30;
const GP_IO_SEL2: u16 = 0x34;
const GP_LVL2: u16 = 0x38;
const GP_RST_SEL2: u16 = 0x64;

// Set 3 (pins 64–75)
const GPIO_USE_SEL3: u16 = 0x40;
const GP_IO_SEL3: u16 = 0x44;
const GP_LVL3: u16 = 0x48;
const GP_RST_SEL3: u16 = 0x68;

/// Maximum GPIO pin number (zero-based, inclusive).
const MAX_GPIO: u8 = 75;

// Register offset tables indexed by set (0, 1, 2).
const USE_SEL_REGS: [u16; 3] = [GPIO_USE_SEL, GPIO_USE_SEL2, GPIO_USE_SEL3];
const IO_SEL_REGS: [u16; 3] = [GP_IO_SEL, GP_IO_SEL2, GP_IO_SEL3];
const LVL_REGS: [u16; 3] = [GP_LVL, GP_LVL2, GP_LVL3];

// ---------------------------------------------------------------------------
// Configuration types (serde, for board RON)
// ---------------------------------------------------------------------------

/// GPIO pad configuration for one set of 32 pins.
///
/// Each bit position corresponds to a pin within the set.
/// Unused fields default to 0.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct GpioSet {
    /// GPIO mode select: 0 = native function, 1 = GPIO mode.
    #[serde(default)]
    pub mode: u32,
    /// I/O direction: 0 = output, 1 = input (GPIO-mode pins only).
    #[serde(default)]
    pub direction: u32,
    /// Output level: 0 = low, 1 = high.
    #[serde(default)]
    pub level: u32,
    /// Blink enable (set 1 only; ignored for sets 2/3).
    #[serde(default)]
    pub blink: u32,
    /// Input inversion (set 1 only; ignored for sets 2/3).
    #[serde(default)]
    pub invert: u32,
    /// Reset select: 0 = PWROK, 1 = RSMRST#.
    #[serde(default)]
    pub reset: u32,
}

/// Full GPIO configuration for an ICH/PCH southbridge.
///
/// Three sets cover all 76 GPIO pins (0–75). Most boards only use
/// sets 1 and 2; set 3 can be left at defaults.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct GpioConfig {
    /// GPIO set 1 — pins 0–31.
    #[serde(default)]
    pub set1: GpioSet,
    /// GPIO set 2 — pins 32–63.
    #[serde(default)]
    pub set2: GpioSet,
    /// GPIO set 3 — pins 64–75 (ICH7 has 12 pins here).
    #[serde(default)]
    pub set3: GpioSet,
}

// ---------------------------------------------------------------------------
// GPIO controller
// ---------------------------------------------------------------------------

/// ICH/PCH GPIO controller.
///
/// Wraps the GPIOBASE I/O port and provides batch setup plus
/// individual pin get/set/input/output operations.
pub struct IchGpio {
    /// GPIOBASE I/O port base address.
    base: u16,
}

impl IchGpio {
    /// Create a new GPIO controller with the given GPIOBASE.
    ///
    /// The caller must ensure GPIOBASE has been programmed in the LPC
    /// PCI config space (register 0x48) and GPIO_CNTL is enabled.
    pub const fn new(gpiobase: u16) -> Self {
        Self { base: gpiobase }
    }

    // -------------------------------------------------------------------
    // Batch setup (from board config)
    // -------------------------------------------------------------------

    /// Program all GPIO pads from a [`GpioConfig`].
    ///
    /// Ported from coreboot `setup_pch_gpios()`. The write order
    /// matters on ICH7/ICH9M and earlier: level is written both
    /// *before* and *after* the mode/direction registers to prevent
    /// glitches when pins transition between native and GPIO mode.
    #[cfg(target_arch = "x86_64")]
    pub fn setup(&self, cfg: &GpioConfig) {
        // SAFETY: GPIOBASE was programmed by the southbridge driver
        // and is a valid I/O port range (0x80 bytes).
        unsafe {
            // Set 1 (pins 0–31).
            fstart_pio::outl(self.base + GP_LVL, cfg.set1.level);
            fstart_pio::outl(self.base + GPIO_USE_SEL, cfg.set1.mode);
            fstart_pio::outl(self.base + GP_IO_SEL, cfg.set1.direction);
            fstart_pio::outl(self.base + GP_LVL, cfg.set1.level);
            fstart_pio::outl(self.base + GP_RST_SEL1, cfg.set1.reset);
            fstart_pio::outl(self.base + GPI_INV, cfg.set1.invert);
            fstart_pio::outl(self.base + GPO_BLINK, cfg.set1.blink);

            // Set 2 (pins 32–63).
            fstart_pio::outl(self.base + GP_LVL2, cfg.set2.level);
            fstart_pio::outl(self.base + GPIO_USE_SEL2, cfg.set2.mode);
            fstart_pio::outl(self.base + GP_IO_SEL2, cfg.set2.direction);
            fstart_pio::outl(self.base + GP_LVL2, cfg.set2.level);
            fstart_pio::outl(self.base + GP_RST_SEL2, cfg.set2.reset);

            // Set 3 (pins 64–75).
            fstart_pio::outl(self.base + GP_LVL3, cfg.set3.level);
            fstart_pio::outl(self.base + GPIO_USE_SEL3, cfg.set3.mode);
            fstart_pio::outl(self.base + GP_IO_SEL3, cfg.set3.direction);
            fstart_pio::outl(self.base + GP_LVL3, cfg.set3.level);
            fstart_pio::outl(self.base + GP_RST_SEL3, cfg.set3.reset);
        }

        fstart_log::info!("ich-gpio: pads configured (3 sets)");
    }

    #[cfg(not(target_arch = "x86_64"))]
    pub fn setup(&self, _cfg: &GpioConfig) {
        fstart_log::info!("ich-gpio: setup (stub, non-x86)");
    }

    // -------------------------------------------------------------------
    // Runtime single-pin operations
    // -------------------------------------------------------------------

    /// Read the current level of a GPIO pin.
    ///
    /// Returns `true` for high, `false` for low. If the pin number
    /// is out of range (> 75), returns `false`.
    #[cfg(target_arch = "x86_64")]
    pub fn get(&self, pin: u8) -> bool {
        if pin > MAX_GPIO {
            return false;
        }
        let (set, bit) = (pin / 32, pin % 32);
        let reg = LVL_REGS[set as usize];
        // SAFETY: GPIOBASE is valid.
        let val = unsafe { fstart_pio::inl(self.base + reg) };
        val & (1 << bit) != 0
    }

    #[cfg(not(target_arch = "x86_64"))]
    pub fn get(&self, _pin: u8) -> bool {
        false
    }

    /// Set the output level of a GPIO pin.
    ///
    /// Writes to the GP_LVL register for the pin's set. The pin must
    /// be configured as GPIO mode + output for this to take effect.
    #[cfg(target_arch = "x86_64")]
    pub fn set(&self, pin: u8, high: bool) {
        if pin > MAX_GPIO {
            return;
        }
        let (set, bit) = (pin / 32, pin % 32);
        let reg = LVL_REGS[set as usize];
        // SAFETY: GPIOBASE is valid.
        unsafe {
            let mut val = fstart_pio::inl(self.base + reg);
            if high {
                val |= 1 << bit;
            } else {
                val &= !(1 << bit);
            }
            fstart_pio::outl(self.base + reg, val);
        }
    }

    #[cfg(not(target_arch = "x86_64"))]
    pub fn set(&self, _pin: u8, _high: bool) {}

    /// Check if a pin is in native mode (not GPIO).
    ///
    /// Returns `true` if the USE_SEL bit is 0 (native function).
    #[cfg(target_arch = "x86_64")]
    pub fn is_native(&self, pin: u8) -> bool {
        if pin > MAX_GPIO {
            return false;
        }
        let (set, bit) = (pin / 32, pin % 32);
        let reg = USE_SEL_REGS[set as usize];
        let val = unsafe { fstart_pio::inl(self.base + reg) };
        val & (1 << bit) == 0
    }

    #[cfg(not(target_arch = "x86_64"))]
    pub fn is_native(&self, _pin: u8) -> bool {
        false
    }

    /// Configure a pin as GPIO mode.
    ///
    /// Sets the USE_SEL bit for the pin. Does not change direction or level.
    #[cfg(target_arch = "x86_64")]
    fn set_gpio_mode(&self, pin: u8) {
        if pin > MAX_GPIO {
            return;
        }
        let (set, bit) = (pin / 32, pin % 32);
        let reg = USE_SEL_REGS[set as usize];
        unsafe {
            let val = fstart_pio::inl(self.base + reg);
            if val & (1 << bit) == 0 {
                fstart_pio::outl(self.base + reg, val | (1 << bit));
            }
        }
    }

    #[cfg(not(target_arch = "x86_64"))]
    fn set_gpio_mode(&self, _pin: u8) {}

    /// Configure a pin as a GPIO input.
    ///
    /// Switches the pin to GPIO mode (if native) and sets the IO_SEL
    /// bit to input direction.
    #[cfg(target_arch = "x86_64")]
    pub fn input(&self, pin: u8) {
        if pin > MAX_GPIO {
            return;
        }
        self.set_gpio_mode(pin);
        let (set, bit) = (pin / 32, pin % 32);
        let reg = IO_SEL_REGS[set as usize];
        unsafe {
            let val = fstart_pio::inl(self.base + reg);
            if val & (1 << bit) == 0 {
                fstart_pio::outl(self.base + reg, val | (1 << bit));
            }
        }
    }

    #[cfg(not(target_arch = "x86_64"))]
    pub fn input(&self, _pin: u8) {}

    /// Configure a pin as a GPIO output with the given initial level.
    ///
    /// Switches the pin to GPIO mode (if native), sets the output
    /// level, then clears the IO_SEL bit for output direction.
    /// The level is set again after direction change in case the
    /// output register was gated.
    #[cfg(target_arch = "x86_64")]
    pub fn output(&self, pin: u8, high: bool) {
        if pin > MAX_GPIO {
            return;
        }
        self.set_gpio_mode(pin);
        self.set(pin, high);
        let (set, bit) = (pin / 32, pin % 32);
        let reg = IO_SEL_REGS[set as usize];
        unsafe {
            let val = fstart_pio::inl(self.base + reg);
            if val & (1 << bit) != 0 {
                fstart_pio::outl(self.base + reg, val & !(1 << bit));
            }
        }
        // Set level again in case output register was gated.
        self.set(pin, high);
    }

    #[cfg(not(target_arch = "x86_64"))]
    pub fn output(&self, _pin: u8, _high: bool) {}

    /// Set or clear the GPI_INV bit for a pin (set 1 only, pins 0–31).
    ///
    /// When inverted, the input value is logically inverted before
    /// being read from the GP_LVL register.
    #[cfg(target_arch = "x86_64")]
    pub fn invert(&self, pin: u8, enable: bool) {
        if pin >= 32 {
            return; // GPI_INV only exists for set 1.
        }
        unsafe {
            let mut val = fstart_pio::inl(self.base + GPI_INV);
            if enable {
                val |= 1 << pin;
            } else {
                val &= !(1 << pin);
            }
            fstart_pio::outl(self.base + GPI_INV, val);
        }
    }

    #[cfg(not(target_arch = "x86_64"))]
    pub fn invert(&self, _pin: u8, _enable: bool) {}
}
