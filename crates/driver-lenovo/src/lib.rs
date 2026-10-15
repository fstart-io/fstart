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
//! - `x200`: ThinkPad X200 DSDT glue (dock, ThinkLight, sleep hooks).
//! - [`x6`]: X60/X61 family policy: EC/PMH7 setup, DSDT glue and the X6
//!   UltraBase dock (DLPC LPC switch and dock-side PC87392 SuperIO).
//!
//! Both EC drivers are chipset-independent: they only need the LPC decode of
//! their IO ranges, which the board's southbridge config already programs.

#![no_std]

pub mod ec;

#[cfg(any(test, fstart_stage_env = "ram"))]
pub mod eeprom;

pub mod h8;
pub mod pmh7;
pub mod wacom;
pub mod x6;

#[cfg(feature = "acpi")]
pub mod x200;

#[cfg(feature = "acpi")]
pub mod h8_acpi;
