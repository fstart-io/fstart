//! Intel ICH/PCH southbridge GPIO controller.
//!
//! Provides per-pin GPIO pad configuration and runtime get/set/input/output
//! for Intel southbridges that use the legacy GPIOBASE I/O port interface:
//! ICH7, ICH8, ICH9, ICH10, NM10, and 5/6-series PCH.
//!
//! # GPIO space layout
//!
//! The GPIO space is divided into three sets of 32 pins each (76 total):
//!
//! | Set | Pins  | GPIOBASE offsets                          |
//! |-----|-------|-------------------------------------------|
//! | 1   | 0–31  | USE_SEL 0x00, IO_SEL 0x04, LVL 0x0C, ... |
//! | 2   | 32–63 | USE_SEL2 0x30, IO_SEL2 0x34, LVL2 0x38   |
//! | 3   | 64–75 | USE_SEL3 0x40, IO_SEL3 0x44, LVL3 0x48   |
//!
//! # Board configuration
//!
//! GPIO pads are configured from the board RON file using [`GpioConfig`],
//! which contains a list of [`GpioPin`] entries — one per pin that differs
//! from the default (Native mode).  Pins not listed are left in their
//! native/reset state.
//!
//! ```ron
//! gpio: (pins: [
//!     // GPIO outputs (default: mode=Gpio, dir=Output, level=Low)
//!     ( pin: 0 ),
//!     ( pin: 6 ),
//!     ( pin: 7 ),
//!     // GPIO input
//!     ( pin: 33, dir: Input ),
//!     // GPIO output, driven high
//!     ( pin: 24, level: High ),
//!     // Full explicit form
//!     ( pin: 10, mode: Gpio, dir: Output, level: Low, reset: Rsmrst ),
//! ])
//! ```
//!
//! Since you only list pins that are GPIO (not Native), the default for
//! `mode` is `Gpio` — if you list a pin, you want it in GPIO mode.

#![no_std]

use heapless::Vec as HVec;
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
// Per-pin configuration types (serde, for board RON)
// ---------------------------------------------------------------------------

/// GPIO pin function select.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GpioMode {
    /// Pin controlled by its native hardware function (USE_SEL = 0).
    Native,
    /// Pin is a general-purpose I/O (USE_SEL = 1).
    Gpio,
}

impl Default for GpioMode {
    /// Defaults to `Gpio` — if you list a pin, you want it in GPIO mode.
    fn default() -> Self {
        Self::Gpio
    }
}

/// GPIO pin direction (only meaningful in GPIO mode).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GpioDir {
    /// Pin drives an output (IO_SEL = 0).
    Output,
    /// Pin reads an input (IO_SEL = 1).
    Input,
}

impl Default for GpioDir {
    fn default() -> Self {
        Self::Output
    }
}

/// GPIO output level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GpioLevel {
    /// Output driven low / reads as low (LVL = 0).
    Low,
    /// Output driven high / reads as high (LVL = 1).
    High,
}

impl Default for GpioLevel {
    fn default() -> Self {
        Self::Low
    }
}

/// GPIO reset type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GpioReset {
    /// Reset on PWROK de-assertion (RST_SEL = 0).
    Pwrok,
    /// Reset on RSMRST# de-assertion — survives S3/S4 (RST_SEL = 1).
    Rsmrst,
}

impl Default for GpioReset {
    fn default() -> Self {
        Self::Pwrok
    }
}

/// Configuration for a single GPIO pin.
///
/// Only pins that differ from the default (Native mode) need to be
/// listed in the board RON.  For the most common case — a GPIO output
/// driven low — just `( pin: N )` suffices.
///
/// # RON examples
///
/// ```ron
/// ( pin: 0 )                          // GPIO output, low (all defaults)
/// ( pin: 33, dir: Input )             // GPIO input
/// ( pin: 24, level: High )            // GPIO output, high
/// ( pin: 7, blink: true )             // GPIO output with blink
/// ( pin: 10, reset: Rsmrst )          // survives S3/S4
/// ( pin: 5, mode: Native )            // explicitly native (rare)
/// ```
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct GpioPin {
    /// Pin number (0–75).
    pub pin: u8,
    /// Function select: `Gpio` (default) or `Native`.
    #[serde(default)]
    pub mode: GpioMode,
    /// I/O direction: `Output` (default) or `Input`.
    #[serde(default)]
    pub dir: GpioDir,
    /// Output level: `Low` (default) or `High`.
    #[serde(default)]
    pub level: GpioLevel,
    /// Blink enable (set 1 only, pins 0–31). Default: `false`.
    #[serde(default)]
    pub blink: bool,
    /// Input inversion (set 1 only, pins 0–31). Default: `false`.
    #[serde(default)]
    pub invert: bool,
    /// Reset type: `Pwrok` (default) or `Rsmrst`.
    #[serde(default)]
    pub reset: GpioReset,
}

/// Full GPIO configuration for an ICH/PCH southbridge.
///
/// Contains a sparse list of per-pin configurations. Pins not listed
/// remain in their power-on default state (typically Native mode).
///
/// # RON example
///
/// ```ron
/// gpio: (pins: [
///     // GPIO outputs (active-low LEDs, active-low resets)
///     ( pin: 0 ),
///     ( pin: 6 ),
///     ( pin: 7 ),
///     ( pin: 8 ),
///     // GPIO inputs (buttons, jumpers, detect pins)
///     ( pin: 33, dir: Input ),
///     ( pin: 34, dir: Input ),
/// ])
/// ```
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GpioConfig {
    /// Per-pin configurations. Only list pins that differ from defaults.
    #[serde(default)]
    pub pins: HVec<GpioPin, 76>,
}

// ---------------------------------------------------------------------------
// Internal: register-level representation
// ---------------------------------------------------------------------------

/// Raw GPIO register values, computed from per-pin config.
struct GpioRegisters {
    use_sel: [u32; 3],
    io_sel: [u32; 3],
    lvl: [u32; 3],
    blink: u32,
    invert: u32,
    rst_sel: [u32; 3],
}

impl GpioRegisters {
    /// Convert a [`GpioConfig`] into raw register values.
    fn from_config(cfg: &GpioConfig) -> Self {
        let mut regs = Self {
            use_sel: [0; 3],
            io_sel: [0; 3],
            lvl: [0; 3],
            blink: 0,
            invert: 0,
            rst_sel: [0; 3],
        };

        for p in &cfg.pins {
            if p.pin > MAX_GPIO {
                continue;
            }
            let set = (p.pin / 32) as usize;
            let bit = 1u32 << (p.pin % 32);

            // USE_SEL: 1 = GPIO mode.
            if p.mode == GpioMode::Gpio {
                regs.use_sel[set] |= bit;
            }
            // IO_SEL: 1 = input.
            if p.dir == GpioDir::Input {
                regs.io_sel[set] |= bit;
            }
            // LVL: 1 = high.
            if p.level == GpioLevel::High {
                regs.lvl[set] |= bit;
            }
            // RST_SEL: 1 = RSMRST.
            if p.reset == GpioReset::Rsmrst {
                regs.rst_sel[set] |= bit;
            }
            // Blink and invert are set 1 only.
            if set == 0 {
                if p.blink {
                    regs.blink |= bit;
                }
                if p.invert {
                    regs.invert |= bit;
                }
            }
        }

        regs
    }
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
    /// Converts the per-pin configuration into register values, then
    /// writes all three sets with the correct glitch-free ordering
    /// from coreboot `setup_pch_gpios()`:
    ///
    /// 1. Write level first (to set output values before pins switch)
    /// 2. Write USE_SEL (switch pins to GPIO mode)
    /// 3. Write IO_SEL (set direction)
    /// 4. Write level again (in case pins were gated)
    /// 5. Write RST_SEL, GPI_INV, GPO_BLINK
    #[cfg(target_arch = "x86_64")]
    pub fn setup(&self, cfg: &GpioConfig) {
        let regs = GpioRegisters::from_config(cfg);

        // SAFETY: GPIOBASE was programmed by the southbridge driver
        // and is a valid I/O port range (0x80 bytes).
        unsafe {
            // Set 1 (pins 0–31): level-first ordering.
            fstart_pio::outl(self.base + GP_LVL, regs.lvl[0]);
            fstart_pio::outl(self.base + GPIO_USE_SEL, regs.use_sel[0]);
            fstart_pio::outl(self.base + GP_IO_SEL, regs.io_sel[0]);
            fstart_pio::outl(self.base + GP_LVL, regs.lvl[0]);
            fstart_pio::outl(self.base + GP_RST_SEL1, regs.rst_sel[0]);
            fstart_pio::outl(self.base + GPI_INV, regs.invert);
            fstart_pio::outl(self.base + GPO_BLINK, regs.blink);

            // Set 2 (pins 32–63).
            fstart_pio::outl(self.base + GP_LVL2, regs.lvl[1]);
            fstart_pio::outl(self.base + GPIO_USE_SEL2, regs.use_sel[1]);
            fstart_pio::outl(self.base + GP_IO_SEL2, regs.io_sel[1]);
            fstart_pio::outl(self.base + GP_LVL2, regs.lvl[1]);
            fstart_pio::outl(self.base + GP_RST_SEL2, regs.rst_sel[1]);

            // Set 3 (pins 64–75).
            fstart_pio::outl(self.base + GP_LVL3, regs.lvl[2]);
            fstart_pio::outl(self.base + GPIO_USE_SEL3, regs.use_sel[2]);
            fstart_pio::outl(self.base + GP_IO_SEL3, regs.io_sel[2]);
            fstart_pio::outl(self.base + GP_LVL3, regs.lvl[2]);
            fstart_pio::outl(self.base + GP_RST_SEL3, regs.rst_sel[2]);
        }

        fstart_log::info!("ich-gpio: {} pins configured", cfg.pins.len());
    }

    #[cfg(not(target_arch = "x86_64"))]
    pub fn setup(&self, cfg: &GpioConfig) {
        fstart_log::info!("ich-gpio: setup ({} pins, stub)", cfg.pins.len());
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_config_produces_zero_registers() {
        let cfg = GpioConfig::default();
        let regs = GpioRegisters::from_config(&cfg);
        for i in 0..3 {
            assert_eq!(regs.use_sel[i], 0);
            assert_eq!(regs.io_sel[i], 0);
            assert_eq!(regs.lvl[i], 0);
            assert_eq!(regs.rst_sel[i], 0);
        }
        assert_eq!(regs.blink, 0);
        assert_eq!(regs.invert, 0);
    }

    #[test]
    fn single_gpio_output_low() {
        let mut cfg = GpioConfig::default();
        cfg.pins
            .push(GpioPin {
                pin: 6,
                mode: GpioMode::Gpio,
                dir: GpioDir::Output,
                level: GpioLevel::Low,
                blink: false,
                invert: false,
                reset: GpioReset::Pwrok,
            })
            .ok();
        let regs = GpioRegisters::from_config(&cfg);
        assert_eq!(regs.use_sel[0], 1 << 6); // GPIO mode
        assert_eq!(regs.io_sel[0], 0); // output
        assert_eq!(regs.lvl[0], 0); // low
    }

    #[test]
    fn gpio_input_pin_33() {
        let mut cfg = GpioConfig::default();
        cfg.pins
            .push(GpioPin {
                pin: 33,
                mode: GpioMode::Gpio,
                dir: GpioDir::Input,
                level: GpioLevel::Low,
                blink: false,
                invert: false,
                reset: GpioReset::Pwrok,
            })
            .ok();
        let regs = GpioRegisters::from_config(&cfg);
        // Pin 33 → set 1 (index 1), bit 1.
        assert_eq!(regs.use_sel[1], 1 << 1);
        assert_eq!(regs.io_sel[1], 1 << 1);
    }

    /// Verify the foxconn-d41s GPIO config produces correct register values.
    ///
    /// These values were manually verified against coreboot's
    /// `mainboard/foxconn/d41s/gpio.c` per-pin definitions.
    #[test]
    fn foxconn_d41s_gpio_registers() {
        let mut cfg = GpioConfig::default();

        // Set 1: GPIO outputs, low.
        for pin in [0, 6, 7, 8, 9, 10, 12, 13, 14, 15, 24, 25, 26, 27, 28] {
            cfg.pins
                .push(GpioPin {
                    pin,
                    mode: GpioMode::Gpio,
                    dir: GpioDir::Output,
                    level: GpioLevel::Low,
                    blink: false,
                    invert: false,
                    reset: GpioReset::Pwrok,
                })
                .ok();
        }
        // Set 2: GPIO inputs.
        for pin in [33, 34, 38, 39] {
            cfg.pins
                .push(GpioPin {
                    pin,
                    mode: GpioMode::Gpio,
                    dir: GpioDir::Input,
                    level: GpioLevel::Low,
                    blink: false,
                    invert: false,
                    reset: GpioReset::Pwrok,
                })
                .ok();
        }

        let regs = GpioRegisters::from_config(&cfg);

        // set1 mode: pins 0,6,7,8,9,10,12,13,14,15,24,25,26,27,28
        assert_eq!(regs.use_sel[0], 0x1F00_F7C1, "set1 USE_SEL");
        assert_eq!(regs.io_sel[0], 0, "set1 IO_SEL (all output)");
        assert_eq!(regs.lvl[0], 0, "set1 LVL (all low)");

        // set2 mode: pins 33,34,38,39 (bits 1,2,6,7 in set2)
        assert_eq!(regs.use_sel[1], 0xC6, "set2 USE_SEL");
        assert_eq!(regs.io_sel[1], 0xC6, "set2 IO_SEL (all input)");

        // set3: nothing configured.
        assert_eq!(regs.use_sel[2], 0);
    }

    #[test]
    fn blink_and_invert_only_set1() {
        let mut cfg = GpioConfig::default();
        // Pin 3 with blink.
        cfg.pins
            .push(GpioPin {
                pin: 3,
                mode: GpioMode::Gpio,
                dir: GpioDir::Output,
                level: GpioLevel::Low,
                blink: true,
                invert: false,
                reset: GpioReset::Pwrok,
            })
            .ok();
        // Pin 40 with blink — should be ignored (set 2).
        cfg.pins
            .push(GpioPin {
                pin: 40,
                mode: GpioMode::Gpio,
                dir: GpioDir::Output,
                level: GpioLevel::Low,
                blink: true,
                invert: true,
                reset: GpioReset::Pwrok,
            })
            .ok();

        let regs = GpioRegisters::from_config(&cfg);
        assert_eq!(regs.blink, 1 << 3); // only set 1 pin
        assert_eq!(regs.invert, 0); // pin 40's invert ignored
    }

    #[test]
    fn rsmrst_reset_type() {
        let mut cfg = GpioConfig::default();
        cfg.pins
            .push(GpioPin {
                pin: 10,
                mode: GpioMode::Gpio,
                dir: GpioDir::Output,
                level: GpioLevel::High,
                blink: false,
                invert: false,
                reset: GpioReset::Rsmrst,
            })
            .ok();
        let regs = GpioRegisters::from_config(&cfg);
        assert_eq!(regs.use_sel[0], 1 << 10);
        assert_eq!(regs.lvl[0], 1 << 10); // high
        assert_eq!(regs.rst_sel[0], 1 << 10); // rsmrst
    }

    #[test]
    fn pin_out_of_range_ignored() {
        let mut cfg = GpioConfig::default();
        cfg.pins
            .push(GpioPin {
                pin: 80, // invalid
                mode: GpioMode::Gpio,
                dir: GpioDir::Output,
                level: GpioLevel::High,
                blink: false,
                invert: false,
                reset: GpioReset::Pwrok,
            })
            .ok();
        let regs = GpioRegisters::from_config(&cfg);
        for i in 0..3 {
            assert_eq!(regs.use_sel[i], 0);
        }
    }

    /// Verify RON deserialization works with minimal per-pin syntax.
    #[test]
    fn ron_deserialize_minimal() {
        let ron_str = r#"(pins: [
            ( pin: 0 ),
            ( pin: 6 ),
            ( pin: 33, dir: Input ),
        ])"#;
        let cfg: GpioConfig = ron::from_str(ron_str).expect("RON parse");
        assert_eq!(cfg.pins.len(), 3);
        assert_eq!(cfg.pins[0].pin, 0);
        assert_eq!(cfg.pins[0].mode, GpioMode::Gpio);
        assert_eq!(cfg.pins[0].dir, GpioDir::Output);
        assert_eq!(cfg.pins[1].pin, 6);
        assert_eq!(cfg.pins[2].pin, 33);
        assert_eq!(cfg.pins[2].dir, GpioDir::Input);
    }
}
