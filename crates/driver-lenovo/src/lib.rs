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
//!
//! Both drivers are chipset-independent: they only need the LPC decode of
//! their IO ranges, which the board's southbridge config already programs.

#![no_std]

pub mod ec;

pub mod h8;
pub mod pmh7;

#[cfg(feature = "acpi")]
pub mod h8_acpi;
