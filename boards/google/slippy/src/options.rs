// SPDX-License-Identifier: MIT OR Apache-2.0
//! Peppy's three device-population options (Slippy cfr.c), in fstart CMOS.
use fstart_options::{Choice, Kind, Knob, Meta, OptionSet};

#[derive(Clone, Copy)]
pub struct Devices {
    pub touchpad_type: u8,
    pub ambient_light: bool,
    pub lte_ngff: bool,
}

pub static DEVICES: OptionSet<Devices> = OptionSet {
    board: Devices {
        touchpad_type: 0,
        ambient_light: false,
        lte_ngff: false,
    },
    knobs: &[
        Knob::new(
            Meta {
                key: "touchpad_type",
                form: "Devices",
                label: "Touchpad type",
                help: "Auto exposes both supported touchpads.",
                kind: Kind::Enum(&[
                    Choice::new(0, "Auto"),
                    Choice::new(1, "Elan"),
                    Choice::new(2, "Cypress"),
                ]),
            },
            |c: &Devices| c.touchpad_type.into(),
            |c, v| c.touchpad_type = v as u8,
        ),
        Knob::new(
            Meta::bool(
                "ambient_light",
                "Devices",
                "Ambient light sensor",
                "Expose the optional ISL29018 sensor on I2C1.",
            ),
            |c: &Devices| c.ambient_light.into(),
            |c, v| c.ambient_light = v != 0,
        ),
        Knob::new(
            Meta::bool(
                "lte_ngff",
                "Devices",
                "LTE / NGFF slot",
                "Enable physical PCIe root port 2 and the SIM USB connector.",
            ),
            |c: &Devices| c.lte_ngff.into(),
            |c, v| c.lte_ngff = v != 0,
        ),
    ],
};
