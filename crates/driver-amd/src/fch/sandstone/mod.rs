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

/// The GPIO pads of the Sandstone FCH.
pub const GPIO_BANK: &crate::fch::gpio::Bank = &crate::fch::gpio::Bank {
    pins: 145,
    reserved: &[63],
    gpio_function: pinmux::gpio_function,
    events: crate::fch::gpio::FAMILY17H_EVENTS,
};

/// MMIO bases of the I2C controllers, numbered like the vendor
/// AmdI2CMasterPei instances.
pub const I2C: &[usize] = &[
    0xfedc_2000,
    0xfedc_3000,
    0xfedc_4000,
    0xfedc_5000,
    0xfedc_6000,
];

/// Sandstone's ACPI I/O blocks (ACPI_IO_BASE = 0x400).
pub const ACPI_PORTS: crate::fch::AcpiPorts = crate::fch::AcpiPorts {
    event: 0x400,
    control: 0x404,
    timer: 0x408,
    gpe: 0x420,
};
