//! The ThinkPad options of the H8 EC and the PMH7, under coreboot's names
//! and values (`ec/lenovo/h8`, `ec/lenovo/pmh7`): the radios, the pointing
//! devices, the Fn key, USB power, the beeper, the volume and the battery
//! charged first. A board offers the knobs its hardware has.

#[cfg(test)]
use fstart_options::Knob;
use fstart_options::{Choice, Kind, Meta};

/// What a ThinkPad's EC options can change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Options {
    pub wlan: bool,
    pub wwan: bool,
    /// The Bluetooth module's radio, if the card is fitted.
    pub bluetooth: bool,
    /// The UWB slot's radio, on boards with the switch.
    pub uwb: bool,
    pub trackpoint: bool,
    pub touchpad: bool,
    pub sticky_fn: bool,
    pub fn_ctrl_swap: bool,
    /// USB power while off: 0 never, 1 always, 2 on AC only.
    pub usb_always_on: u8,
    pub power_management_beeps: bool,
    pub low_battery_beep: bool,
    /// 0 to 14, written on a cold boot.
    pub volume: u8,
    /// The battery charged first: 0 secondary, 1 primary.
    pub first_battery: u8,
}

impl Options {
    /// coreboot's defaults (`cmos.default` of the ThinkPads).
    pub const DEFAULT: Self = Self {
        wlan: true,
        wwan: true,
        bluetooth: true,
        uwb: true,
        trackpoint: true,
        touchpad: true,
        sticky_fn: false,
        fn_ctrl_swap: false,
        usb_always_on: 0,
        power_management_beeps: true,
        low_battery_beep: true,
        volume: 3,
        first_battery: 1,
    };

    /// coreboot `h8_enable()`'s beeper masks over the board's: none
    /// without power-management beeps, the low-battery beep as chosen.
    pub const fn beep_masks(&self, board: [u8; 2]) -> [u8; 2] {
        let [mask0, mask1] = if self.power_management_beeps {
            board
        } else {
            [0, 0]
        };
        let mask0 = if self.low_battery_beep {
            mask0 | 2
        } else {
            mask0 & !2
        };
        [mask0, mask1]
    }
}

macro_rules! switch {
    ($name:ident, $field:ident, $key:literal, $form:literal, $label:literal, $help:literal) => {
        fstart_options::knob! {
            #[doc = $help]
            pub $name: Options = Meta::bool($key, $form, $label, $help);
            get(o) => o.$field as u32;
            set(o, v) => o.$field = v != 0;
        }
    };
}

switch!(
    Wlan,
    wlan,
    "wlan",
    "Radios",
    "WLAN",
    "The WLAN card's radio."
);
switch!(
    Wwan,
    wwan,
    "wwan",
    "Radios",
    "WWAN",
    "The WWAN card's radio."
);
switch!(
    Bluetooth,
    bluetooth,
    "bluetooth",
    "Radios",
    "Bluetooth",
    "The Bluetooth module's radio."
);
switch!(Uwb, uwb, "uwb", "Radios", "UWB", "The UWB card's radio.");
switch!(
    Trackpoint,
    trackpoint,
    "trackpoint",
    "Input",
    "TrackPoint",
    "The TrackPoint."
);
switch!(
    Touchpad,
    touchpad,
    "touchpad",
    "Input",
    "Touchpad",
    "The touchpad."
);
switch!(
    StickyFn,
    sticky_fn,
    "sticky_fn",
    "Input",
    "Sticky Fn",
    "Fn stays pressed until the next key."
);
switch!(
    FnCtrlSwap,
    fn_ctrl_swap,
    "fn_ctrl_swap",
    "Input",
    "Swap Fn and Ctrl",
    "Exchange the Fn and left Ctrl keys."
);
switch!(
    PowerManagementBeeps,
    power_management_beeps,
    "power_management_beeps",
    "Sound",
    "Power management beeps",
    "Beep on power events: AC, docking, suspend."
);
switch!(
    LowBatteryBeep,
    low_battery_beep,
    "low_battery_beep",
    "Sound",
    "Low battery beep",
    "Beep when the battery runs low."
);

fstart_options::knob! {
    /// USB power while the machine is off.
    pub UsbAlwaysOn: Options = Meta {
        key: "usb_always_on",
        form: "Power",
        label: "USB power when off",
        help: "Keep the USB ports powered while the machine is off.",
        kind: Kind::Enum(&[
            Choice::new(0, "Off"),
            Choice::new(1, "On AC and battery"),
            Choice::new(2, "On AC only"),
        ]),
    };
    get(o) => o.usb_always_on as u32;
    set(o, v) => o.usb_always_on = v as u8;
}

fstart_options::knob! {
    /// The volume a cold boot sets.
    pub Volume: Options = Meta {
        key: "volume",
        form: "Sound",
        label: "Volume",
        help: "The speaker volume after power-on, 0 to 14.",
        kind: Kind::Number {
            min: 0,
            max: 14,
            step: 1,
        },
    };
    get(o) => o.volume as u32;
    set(o, v) => o.volume = v as u8;
}

fstart_options::knob! {
    /// The battery the EC charges first.
    pub FirstBattery: Options = Meta {
        key: "first_battery",
        form: "Power",
        label: "Battery charged first",
        help: "Which battery charges first when both are fitted.",
        kind: Kind::Enum(&[Choice::new(0, "Secondary"), Choice::new(1, "Primary")]),
    };
    get(o) => o.first_battery as u32;
    set(o, v) => o.first_battery = v as u8;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_and_their_cmos_hashes_are_distinct() {
        const ALL: [Knob<Options>; 13] = [
            Wlan::KNOB,
            Wwan::KNOB,
            Bluetooth::KNOB,
            Uwb::KNOB,
            Trackpoint::KNOB,
            Touchpad::KNOB,
            StickyFn::KNOB,
            FnCtrlSwap::KNOB,
            UsbAlwaysOn::KNOB,
            PowerManagementBeeps::KNOB,
            LowBatteryBeep::KNOB,
            Volume::KNOB,
            FirstBattery::KNOB,
        ];
        let metas = ALL.iter().map(|k| k.meta);
        assert_eq!(fstart_options::duplicate_key(metas.clone()), None);
        assert_eq!(fstart_options::cmos::collision(metas.clone()), None);
        // The chipset's knobs share the CMOS window with these.
        for key in [
            "debug_level",
            "power_on_after_fail",
            "sata_mode",
            "nmi",
            "gfx_uma_size",
            "tft_brightness",
        ] {
            assert!(
                metas
                    .clone()
                    .all(|m| fstart_options::cmos::hash(m.key) != fstart_options::cmos::hash(key)),
                "{key}"
            );
        }
    }

    #[test]
    fn beeps_follow_coreboot() {
        let o = Options::DEFAULT;
        assert_eq!(o.beep_masks([0xfe, 0x96]), [0xfe, 0x96]);
        let quiet = Options {
            power_management_beeps: false,
            ..o
        };
        assert_eq!(quiet.beep_masks([0xfe, 0x96]), [0x02, 0x00]);
        let no_low = Options {
            low_battery_beep: false,
            ..o
        };
        assert_eq!(no_low.beep_masks([0xfe, 0x96]), [0xfc, 0x96]);
    }
}
