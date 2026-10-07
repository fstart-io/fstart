//! Mainstage EC/PMH7 hardware initialization, independent of ACPI emission.

use core::sync::atomic::{AtomicBool, Ordering};
use fstart_core::services::Southbridge;
use fstart_driver_lenovo::h8::{H8, H8_CONFIG0_EVENTS_ENABLE, H8Illumination};
use fstart_driver_lenovo::pmh7::Pmh7;

const X61_H8_EVENT_MASKS: [u8; 16] = [
    0x00, 0x00, 0xff, 0xff, 0xf4, 0x3c, 0x80, 0x01, 0x01, 0xff, 0xff, 0xff, 0xff, 0xff, 0x00, 0x00,
];

// Sample BDC once during board init; ACPI generation must not perform I/O.
static BLUETOOTH_PRESENT: AtomicBool = AtomicBool::new(false);

pub(super) fn bluetooth_present() -> bool {
    BLUETOOTH_PRESENT.load(Ordering::Relaxed)
}

pub(super) fn detect_bluetooth(southbridge: &impl Southbridge) -> bool {
    southbridge.gpio_get(7).is_ok_and(|high| !high)
}

/// Runtime EC/PMH7 bring-up, mirroring coreboot's `h8_enable()` and
/// `pmh7` device init: thermal management + event/hotkey reporting on,
/// trackpoint and USB power on, WLAN/BT radios per board presence, and
/// the PMH7 backlight + dock-event sources the board declares.
pub fn x61_ec_init(southbridge: &impl Southbridge, resume: bool) {
    let bluetooth = detect_bluetooth(southbridge);
    BLUETOOTH_PRESENT.store(bluetooth, Ordering::Relaxed);
    let pmh7 = Pmh7::new(0x15e0);
    pmh7.log_identity();
    pmh7.backlight_enable(true);
    pmh7.dock_event_enable(true);
    pmh7.touchpad_enable(true);
    pmh7.trackpoint_enable(true);

    let h8 = H8;
    h8.clear_out_queue();
    // CONFIG0: events + hotkey enable (SMM H8 + thermal management are
    // set by init_config0 itself, matching coreboot).
    let config_ok = h8.init_config0(H8_CONFIG0_EVENTS_ENABLE);
    let controls_ok = h8.init_controls(
        [0x05, 0xa0, 0x01],
        Some(H8Illumination::Thinklight),
        [0xfe, 0x96],
    );
    let events_ok = h8.program_event_masks(&X61_H8_EVENT_MASKS);
    let trackpoint_ok = h8.trackpoint_enable(true);
    let usb_ok = h8.usb_power_enable(true);
    let wlan_ok = h8.wlan_enable(true);
    let bluetooth_ok = h8.bluetooth_enable(bluetooth);
    // Like coreboot, assume WWAN present when the board has no detection GPIO.
    let wwan_ok = h8.wwan_enable(true);
    let usb_always_on_ok = h8.usb_always_on_disable();
    let fn_ctrl_ok = h8.fn_ctrl_swap(false);
    let sticky_fn_ok = h8.sticky_fn(false);
    let charge_priority_ok = h8.charge_primary_first(true);
    // X61 cmos.default sets volume=3; do not overwrite OS volume on resume.
    let volume_ok = resume || h8.volume(3);
    let audio_ok = h8.audio_mute(false);
    let ec = fstart_driver_lenovo::ec::Ec::LEGACY;
    let dock_ok = ec.clear_bit(0x03, 2)
        && (!super::dock::connected() || (ec.set_bit(0x03, 2) && ec.write(0x0c, 0x88)));
    if ![
        config_ok,
        controls_ok,
        events_ok,
        trackpoint_ok,
        usb_ok,
        wlan_ok,
        bluetooth_ok,
        wwan_ok,
        usb_always_on_ok,
        fn_ctrl_ok,
        sticky_fn_ok,
        charge_priority_ok,
        volume_ok,
        audio_ok,
        dock_ok,
    ]
    .into_iter()
    .all(core::convert::identity)
    {
        fstart_log::error!("lenovo-x61: H8 EC initialization incomplete");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fstart_core::services::ServiceError;

    struct Gpio(Result<bool, ServiceError>);
    impl Southbridge for Gpio {
        fn gpio_get(&self, pin: u32) -> Result<bool, ServiceError> {
            assert_eq!(pin, 7);
            self.0
        }
    }

    #[test]
    fn bluetooth_daughter_card_detection_is_active_low_gpio7() {
        assert!(detect_bluetooth(&Gpio(Ok(false))));
        assert!(!detect_bluetooth(&Gpio(Ok(true))));
        assert!(!detect_bluetooth(&Gpio(Err(ServiceError::IoError))));
    }
}
