// SPDX-License-Identifier: MIT OR Apache-2.0
//! The Sandstone FCH of Raven/Picasso (AGESA FchSandstone): the
//! A-Link and clock init, the
//! legacy/RTC/IRQ policy ([`legacy`]), the pre-console LPC/SPI/SMBus setup
//! ([`preinit`]), the USB controllers ([`usb`]) and the pin functions
//! ([`pinmux`]).
mod init;
pub mod legacy;
#[cfg(test)]
mod legacy_tests;
pub mod pinmux;
pub mod preinit;
pub mod usb;
#[cfg(test)]
mod usb_tests;

pub use init::{alink_init, clock_init};

/// Sandstone's ACPI I/O blocks (ACPI_IO_BASE = 0x400).
pub const ACPI_PORTS: crate::fch::AcpiPorts = crate::fch::AcpiPorts {
    event: 0x400,
    control: 0x404,
    timer: 0x408,
    gpe: 0x420,
};
