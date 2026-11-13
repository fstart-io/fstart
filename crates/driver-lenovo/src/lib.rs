//! Lenovo embedded-controller drivers shared by ThinkPad boards.
//!
//! - [`h8`]: the H8 EC (IO 0x62/0x66) — runtime control (events, hotkeys,
//!   LEDs, radios, USB power). The ACPI side ([`h8_acpi`]) ports the complete
//!   DSDT surface coreboot ships for this EC: battery, thermal zones with the
//!   fan power resource, lid, AC, sleep button, beeper, system-status
//!   indicator, and the HKEY hotkey hub.
//! - [`pmh7`]: the PMH7 hub (IO 0x15e0) — backlight, dock-event, touchpad/
//!   trackpoint and ultrabay power switching, plus its PNP0C02 resource
//!   device.
//! - [`wacom`]: tablet digitizer detection and its ACPI device.
//! - [`ec`], [`eeprom`], [`options`]: the EC channel, the AT24RF08C identity
//!   EEPROM and the user's EC options.
//!
//! These are devices. What a board programs into them is its devicetree:
//! [`h8::H8Config`] and [`pmh7::Pmh7Config`] carry coreboot's `chip.h`
//! registers, and the board crate holds the values, its GPIO wiring and its
//! DSDT glue (sleep hooks, wake GPEs) and its dock.
//!
//! Both EC drivers are chipset-independent: they only need the LPC decode of
//! their IO ranges, which the board's southbridge config already programs.

#![no_std]

pub mod ec;

pub mod eeprom;

pub mod h8;
pub mod options;
pub mod pmh7;
pub mod wacom;

#[cfg(feature = "acpi")]
pub mod h8_acpi;

/// An input pin with a fixed level, for the drivers' tests.
#[cfg(test)]
mod test_pin {
    pub struct Pin(pub Result<bool, ()>);

    impl embedded_hal::digital::ErrorType for Pin {
        type Error = embedded_hal::digital::ErrorKind;
    }

    impl embedded_hal::digital::InputPin for Pin {
        fn is_high(&mut self) -> Result<bool, Self::Error> {
            self.0.map_err(|()| embedded_hal::digital::ErrorKind::Other)
        }

        fn is_low(&mut self) -> Result<bool, Self::Error> {
            self.is_high().map(|high| !high)
        }
    }
}
