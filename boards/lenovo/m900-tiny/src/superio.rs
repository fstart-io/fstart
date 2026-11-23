// SPDX-License-Identifier: MIT OR Apache-2.0
//! M900 Tiny SuperIO board facts from bootblock.c and devicetree.cb.
//! This does not initialize the EC fan engine or assert a thermal-safe board.

use fstart_driver_superio::{ComPortConfig, SuperIoConfig, nct6687d::PinConfig};

pub const CONFIG_PORT: u16 = 0x2e;
pub const COM1_BASE: u16 = 0x3f8;
/// The board source assigns IRQ3, not the conventional default IRQ4.
pub const COM1_IRQ: u8 = 3;
pub const ACPI_BASE: u16 = 0xa10;
pub const EC_BASE: u16 = 0xa20;

/// Vendor multifunction/PECI values are board policy, not chip defaults.
pub const PIN_CONFIG: PinConfig = PinConfig {
    cr13: 0x0c,
    cr1d: 0x08,
    cr1f: 0xf0,
    cr22: 0xbc,
    cr23: 0xdf,
    cr24: 0x81,
    cr25: 0xff,
    cr29: 0x6d,
    cr2a: 0x8f,
    sleep_power_f3: 0x0c,
};

/// COM1 at IRQ 3, the firmware console.
pub fn console_config(baud: u32) -> SuperIoConfig {
    SuperIoConfig {
        com1: Some(ComPortConfig {
            io_base: COM1_BASE,
            irq: COM1_IRQ,
            baud_rate: baud,
        }),
        console_port: Some("com1".try_into().expect("constant console name fits")),
        ..SuperIoConfig::default()
    }
}
