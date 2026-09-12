//! Mainstage EC/PMH7 hardware initialization, independent of ACPI emission.

use fstart_driver_lenovo::h8::{H8, H8_CONFIG0_EVENTS_ENABLE};
use fstart_driver_lenovo::pmh7::Pmh7;

const X61_H8_EVENT_MASKS: [u8; 16] = [
    0x00, 0x00, 0xff, 0xff, 0xf4, 0x3c, 0x80, 0x01, 0x01, 0xff, 0xff, 0xff, 0xff, 0xff, 0x00, 0x00,
];

/// Runtime EC/PMH7 bring-up, mirroring coreboot's `h8_enable()` and
/// `pmh7` device init: thermal management + event/hotkey reporting on,
/// trackpoint and USB power on, WLAN/BT radios per board presence, and
/// the PMH7 backlight + dock-event sources the board declares.
pub fn x61_ec_init() {
    let pmh7 = Pmh7::new(0x15e0);
    pmh7.log_identity();
    pmh7.backlight_enable(true);
    pmh7.dock_event_enable(true);

    let h8 = H8;
    h8.clear_out_queue();
    // CONFIG0: events + hotkey enable (SMM H8 + thermal management are
    // set by init_config0 itself, matching coreboot).
    let config_ok = h8.init_config0(H8_CONFIG0_EVENTS_ENABLE);
    let events_ok = h8.program_event_masks(&X61_H8_EVENT_MASKS);
    let trackpoint_ok = h8.trackpoint_enable(true);
    let usb_ok = h8.usb_power_enable(true);
    let wlan_ok = h8.wlan_enable(true);
    let bluetooth_ok = h8.bluetooth_enable(true);
    if ![
        config_ok,
        events_ok,
        trackpoint_ok,
        usb_ok,
        wlan_ok,
        bluetooth_ok,
    ]
    .into_iter()
    .all(core::convert::identity)
    {
        fstart_log::error!("lenovo-x61: H8 EC initialization incomplete");
    }
}
