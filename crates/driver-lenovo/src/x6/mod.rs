//! ThinkPad X60/X61 ("X6") family policy shared by both mainboards.
//!
//! The two boards differ in chipset (i945GM/ICH7-M vs GM965/ICH8-M) but carry
//! the same H8 EC and PMH7 setup (identical coreboot devicetree entries), the
//! same X6 UltraBase dock and the same board DSDT glue around them. Only the
//! chipset-specific facts (EC SCI GPIO/GPE, PCI topology) stay in the boards.

pub mod dock;

#[cfg(feature = "acpi")]
pub mod acpi;

use core::sync::atomic::{AtomicBool, Ordering};
use fstart_core::services::Southbridge;

use crate::ec::{Ec, EcPorts};
use crate::h8::{
    H8, H8_CONFIG0_EVENTS_ENABLE, H8Config, H8Illumination, H8Led, H8LedMode, H8Resources,
};
use crate::pmh7::Pmh7;

/// coreboot `event2_enable`..`eventd_enable`; event 0/1 and e/f stay off.
const EVENT_MASKS: [u8; 16] = [
    0x00, 0x00, 0xff, 0xff, 0xf4, 0x3c, 0x80, 0x01, 0x01, 0xff, 0xff, 0xff, 0xff, 0xff, 0x00, 0x00,
];

/// ICH GPIO reading low when the Bluetooth daughter card is fitted (BDC).
const BDC_PRESENCE_GPIO: u32 = 7;

/// H8 EC configuration of the X6 family for a chipset-specific EC SCI GPE.
#[must_use]
pub const fn h8_config(ec_gpe: u8) -> H8Config {
    H8Config {
        has_bluetooth: true,
        // No WWAN detection GPIO: coreboot assumes the card is installed.
        has_wwan: true,
        has_thinklight: true,
        second_thermal_zone: true,
        // coreboot `mainboard_fill_gnvs()`.
        critical_temp_celsius: 100,
        passive_temp_celsius: 90,
        ..H8Config::new(
            H8Resources::new(EcPorts::new(0x62, 0x66), 0x1600, 0x15e0),
            ec_gpe,
            "IBM0068",
        )
    }
}

// Sample BDC once during board init; ACPI generation must not perform I/O.
static BLUETOOTH_PRESENT: AtomicBool = AtomicBool::new(false);

/// Bluetooth presence sampled by [`ec_init`].
pub fn bluetooth_present() -> bool {
    BLUETOOTH_PRESENT.load(Ordering::Relaxed)
}

fn detect_bluetooth(southbridge: &impl Southbridge) -> bool {
    southbridge
        .gpio_get(BDC_PRESENCE_GPIO)
        .is_ok_and(|high| !high)
}

/// Runtime EC/PMH7 bring-up, mirroring coreboot's `h8_enable()` and the
/// PMH7 `enable_dev()`: thermal management and event/hotkey reporting on,
/// trackpoint and USB power on, radios per presence, PMH7 backlight and
/// dock events. Returns whether every EC write was acknowledged.
pub fn ec_init(southbridge: &impl Southbridge, h8: &H8Config, resume: bool) -> bool {
    let bluetooth = detect_bluetooth(southbridge);
    BLUETOOTH_PRESENT.store(bluetooth, Ordering::Relaxed);
    let pmh7 = Pmh7::new(h8.resources.pmh7_base.raw());
    pmh7.log_identity();
    pmh7.backlight_enable(true);
    pmh7.dock_event_enable(true);
    pmh7.touchpad_enable(true);
    pmh7.trackpoint_enable(true);

    let h8 = H8::new(Ec::new(h8.resources.os));
    h8.clear_out_queue();
    // CONFIG0 0xa6: init_config0 adds the SMM-H8 and thermal enables.
    [
        h8.init_config0(H8_CONFIG0_EVENTS_ENABLE),
        h8.init_controls(
            [0x05, 0xa0, 0x01],
            Some(H8Illumination::Thinklight),
            [0xfe, 0x96],
        ),
        h8.program_event_masks(&EVENT_MASKS),
        h8.trackpoint_enable(true),
        h8.usb_power_enable(true),
        h8.wlan_enable(true),
        h8.bluetooth_enable(bluetooth),
        h8.wwan_enable(true),
        h8.usb_always_on_disable(),
        h8.fn_ctrl_swap(false),
        h8.sticky_fn(false),
        h8.charge_primary_first(true),
        // cmos.default sets volume=3; do not overwrite the OS volume on resume.
        resume || h8.volume(3),
        h8.audio_mute(false),
        h8.dock_latch(false)
            && (!dock::connected()
                || (h8.dock_latch(true) && h8.set_led(H8Led::Dock1, H8LedMode::On))),
    ]
    .into_iter()
    .all(core::convert::identity)
}

/// EC firmware id for the SMBIOS OEM string Linux `thinkpad_acpi` reads.
pub fn ec_oem_string(h8: &H8Config) -> Option<crate::h8::EcOemString> {
    H8::new(Ec::new(h8.resources.os)).smbios_oem_string()
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
