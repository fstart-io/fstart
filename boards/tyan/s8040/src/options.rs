// SPDX-License-Identifier: MIT OR Apache-2.0
//! Stock GM2NE Setup choices backed by the native board policy.

use crate::mpio::{Bifurcation8, Bifurcation16, Options};
use fstart_driver_amd::{dxio::rs::LinkSpeed, fch::tasks::env::PowerFailState};
use fstart_options::{Choice, Kind, Knob, Meta, OptionSet};

const X16: Kind = Kind::Enum(&[
    Choice::new(1, "x16"),
    Choice::new(3, "x8+x8"),
    Choice::new(6, "x4+x4+x4+x4"),
]);
const X8: Kind = Kind::Enum(&[Choice::new(8, "x8"), Choice::new(9, "x4+x4")]);
const SPEED: Kind = Kind::Enum(&[
    Choice::new(0, "Auto"),
    Choice::new(1, "Gen1"),
    Choice::new(2, "Gen2"),
    Choice::new(3, "Gen3"),
    Choice::new(4, "Gen4"),
    Choice::new(5, "Gen5"),
]);

const fn bifurcation(key: &'static str, label: &'static str, kind: Kind) -> Meta {
    Meta {
        key,
        form: "PCIe bifurcation",
        label,
        help: "How the connector group's lanes split into independent links.",
        kind,
    }
}

const fn split16(value: u32) -> Bifurcation16 {
    match value {
        3 => Bifurcation16::X8X8,
        6 => Bifurcation16::X4X4X4X4,
        _ => Bifurcation16::X16,
    }
}

const fn slot<const I: usize>(key: &'static str, label: &'static str) -> Knob<Options> {
    Knob::new(
        bifurcation(key, label, X16),
        |o| o.slots[I].setup_id().into(),
        |o, value| o.slots[I] = split16(value),
    )
}

const fn speed<const I: usize>(key: &'static str, label: &'static str) -> Knob<Options> {
    Knob::new(
        Meta {
            key,
            form: "PCIe link speed",
            label,
            help: "Maximum link speed for this connector group.",
            kind: SPEED,
        },
        |o: &Options| o.speed[I] as u32,
        |o, value| {
            o.speed[I] = match value {
                1 => LinkSpeed::Gen1,
                2 => LinkSpeed::Gen2,
                3 => LinkSpeed::Gen3,
                4 => LinkSpeed::Gen4,
                5 => LinkSpeed::Gen5,
                _ => LinkSpeed::Auto,
            };
        },
    )
}

const fn lan<const I: usize>(key: &'static str, label: &'static str) -> Knob<Options> {
    Knob::new(
        Meta::bool(
            key,
            "Onboard devices",
            label,
            "Trains the onboard i210 link.",
        ),
        |o: &Options| o.lan[I].into(),
        |o, value| o.lan[I] = value != 0,
    )
}

pub static PCIE: OptionSet<Options> = OptionSet {
    board: Options::DEFAULT,
    knobs: &[
        slot::<0>("s8040_pcie1_bifurcation", "PCIE#1"),
        slot::<1>("s8040_pcie2_bifurcation", "PCIE#2"),
        slot::<2>("s8040_pcie3_bifurcation", "PCIE#3"),
        slot::<3>("s8040_pcie4_bifurcation", "PCIE#4"),
        Knob::new(
            bifurcation("s8040_mcio01_bifurcation", "MCIO0/MCIO1", X16),
            |o| o.mcio01.setup_id().into(),
            |o, value| o.mcio01 = split16(value),
        ),
        Knob::new(
            bifurcation("s8040_mcio23_bifurcation", "MCIO2/MCIO3", X8),
            |o| o.mcio23.setup_id().into(),
            |o, value| {
                o.mcio23 = if value == 8 {
                    Bifurcation8::X8
                } else {
                    Bifurcation8::X4X4
                }
            },
        ),
        speed::<0>("s8040_pcie1_speed", "PCIE#1"),
        speed::<1>("s8040_pcie2_speed", "PCIE#2"),
        speed::<2>("s8040_pcie3_speed", "PCIE#3"),
        speed::<3>("s8040_pcie4_speed", "PCIE#4"),
        speed::<4>("s8040_mcio01_speed", "MCIO0/MCIO1"),
        speed::<5>("s8040_mcio23_speed", "MCIO2/MCIO3"),
        lan::<0>("s8040_lan1", "Onboard LAN1 (i210)"),
        lan::<1>("s8040_lan2", "Onboard LAN2 (i210)"),
        Knob::new(
            Meta::bool(
                "s8040_onboard_vga",
                "Onboard devices",
                "Onboard VGA (AST2600)",
                "Trains the BMC's VGA link. The serial console is unaffected.",
            ),
            |o| o.vga.into(),
            |o, value| o.vga = value != 0,
        ),
        Knob::new(
            Meta::bool(
                "s8040_pcie_compliance",
                "Advanced",
                "PCIe compliance mode",
                "Puts the slots and MCIO links in compliance mode for link testing. Devices may not enumerate.",
            ),
            |o| o.compliance.into(),
            |o, value| o.compliance = value != 0,
        ),
    ],
};

pub static POWER: OptionSet<PowerFailState> = OptionSet {
    board: PowerFailState::Previous,
    knobs: &[Knob::new(
        Meta {
            key: "power_on_after_fail",
            form: "Power",
            label: "Restore on AC power loss",
            help: "What the host does when AC power returns.",
            kind: Kind::Enum(PowerFailState::CHOICES),
        },
        |state| state.option(),
        |state, value| *state = PowerFailState::from_option(value),
    )],
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_menu_value_round_trips_into_policy() {
        for knob in PCIE.knobs {
            let mut options = PCIE.board;
            assert!(knob.meta.kind.contains((knob.get)(&options)));
            for value in 0..=20 {
                if knob.meta.kind.contains(value) {
                    (knob.set)(&mut options, value);
                    assert_eq!((knob.get)(&options), value, "{}", knob.meta.key);
                    assert_eq!(crate::check_options(&options), Ok(()));
                }
            }
        }
        assert_eq!(POWER.board, PowerFailState::Previous);
        let knob = &POWER.knobs[0];
        for state in [
            PowerFailState::Off,
            PowerFailState::On,
            PowerFailState::Previous,
        ] {
            let mut option = POWER.board;
            (knob.set)(&mut option, state.option());
            assert_eq!(option, state);
        }
    }
}
