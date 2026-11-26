// SPDX-License-Identifier: MIT OR Apache-2.0
//! Source mainboard/lenovo/m900_tiny/devicetree.cb USB wiring and soc/usb.h
//! electrical presets. This is source policy, not measured eye/OC/connector
//! validation. Unspecified entries retain the C zero-initialized AFE values.
//! devicetree.cb SHA256 4e865152d729b563a227b863c37fe55a4c991acdef5fc0251c6d60f1bb07d8a1.
use fstart_driver_intel::sunrise_point::usb::{Config, Usb2Port, Usb3Port};
pub static CONFIG: Config = Config {
    usb2: [
        Usb2Port::mid(Some(0)),
        Usb2Port::mid(Some(0)),
        Usb2Port::mid(Some(1)),
        Usb2Port::mid(Some(2)),
        Usb2Port::mid(Some(3)),
        Usb2Port::mid(Some(4)),
        Usb2Port::mid(Some(1)),
        Usb2Port::DISABLED,
        Usb2Port::short(None),
        Usb2Port::DISABLED,
        Usb2Port::DISABLED,
        Usb2Port::DISABLED,
        Usb2Port::DISABLED,
        Usb2Port::DISABLED,
        Usb2Port::DISABLED,
        Usb2Port::DISABLED,
    ],
    usb3: [
        Usb3Port::standard(Some(0)),
        Usb3Port::standard(Some(0)),
        Usb3Port::standard(Some(3)),
        Usb3Port::standard(Some(3)),
        Usb3Port::standard(Some(1)),
        Usb3Port::standard(Some(1)),
        Usb3Port::DISABLED,
        Usb3Port::DISABLED,
        Usb3Port::DISABLED,
        Usb3Port::DISABLED,
    ],
};
