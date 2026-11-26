//! T400-series policy around the H8 EC and PMH7 (coreboot `t400/devicetree.cb`
//! and `variants/r500/overridetree.cb`).

use embedded_hal::digital::InputPin;

use fstart_driver_lenovo::ec::Ec;
use fstart_driver_lenovo::h8::{H8, H8Config};
use fstart_driver_lenovo::pmh7::Pmh7;

/// Coreboot `t400/devicetree.cb` `event2_enable`..`eventd_enable`.
const T400_EVENT_MASKS: [u8; 16] = [
    0x00, 0x00, 0xff, 0xff, 0xf4, 0x3c, 0x80, 0x01, 0x01, 0xff, 0xff, 0xff, 0xff, 0xff, 0x00, 0x00,
];

/// Coreboot `variants/r500/overridetree.cb` overrides on event6/7/8/a/b.
const R500_EVENT_MASKS: [u8; 16] = [
    0x00, 0x00, 0xff, 0xff, 0xf4, 0x3c, 0x87, 0x09, 0x5b, 0xff, 0x83, 0x00, 0xff, 0xff, 0x00, 0x00,
];

/// EC CONFIG0 through CONFIG3 for each variant.
pub(crate) const CONTROLS: [u8; 4] = if crate::config::IS_R500 {
    [0xa6, 0x05, 0xa0, 0x40]
} else {
    [0xa6, 0x04, 0xa0, 0x01]
};

pub(crate) const EVENT_MASKS: [u8; 16] = if crate::config::IS_R500 {
    R500_EVENT_MASKS
} else {
    T400_EVENT_MASKS
};

/// ICH GPIO reading low when the Bluetooth daughter card is fitted
/// (`t400/devicetree.cb`: `bdc_gpio_num = 48`, `bdc_gpio_lvl = 0`).
pub const BDC_PRESENCE_GPIO: u8 = 48;

/// Runtime EC/PMH7 bring-up. Returns whether the Bluetooth card is fitted
/// and every EC write was acknowledged.
pub fn init(
    bdc_presence: &mut impl InputPin,
    h8_config: &H8Config,
    options: &fstart_driver_lenovo::options::Options,
    resume: bool,
) -> (bool, bool) {
    let bluetooth = if crate::config::IS_R500 {
        // coreboot t400/Kconfig: H8_HAS_BDC_GPIO_DETECTION is !BOARD_LENOVO_R500.
        true
    } else {
        bdc_presence
            .is_low()
            .inspect_err(|_| fstart_log::error!("lenovo-t400: Bluetooth presence GPIO read failed"))
            .unwrap_or(false)
    };
    let pmh7 = Pmh7::new(h8_config.resources.pmh7_base.raw());
    pmh7.backlight_enable(true);
    pmh7.dock_event_enable(true);
    pmh7.trackpoint_enable(options.trackpoint);
    pmh7.touchpad_enable(options.touchpad);

    let ec = Ec::new(h8_config.resources.os);
    let initialized = H8::new(ec).enable(h8_config, bluetooth, options, resume);
    (bluetooth, initialized)
}
