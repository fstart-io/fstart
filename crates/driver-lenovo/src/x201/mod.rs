//! ThinkPad X201 policy around the H8 EC and PMH7 (coreboot
//! `mainboard/lenovo/x201`): EC bring-up, the Mini Dock connection
//! (`dock.c`), the SMM EC event and ACPI-ownership handling
//! (`smihandler.c`) and the DSDT glue.

#[cfg(feature = "acpi")]
pub mod acpi;

use embedded_hal::digital::{InputPin, OutputPin, PinState};
use tock_registers::register_bitfields;

use crate::ec::{Ec, EcRegister};
use crate::h8::{CONFIG2, CONFIG3, H8, H8Config, H8DockEvent, H8Led, H8LedMode};
use crate::pmh7::Pmh7;

/// coreboot `event2_enable`..`eventd_enable`; event 0/1 and e/f stay off.
const EVENT_MASKS: [u8; 16] = [
    0x00, 0x00, 0xff, 0xff, 0xf4, 0x3c, 0x80, 0x01, 0x01, 0xff, 0xff, 0xff, 0xff, 0xff, 0x00, 0x00,
];

/// devicetree `config1`.
const CONFIG1: u8 = 0x05;

/// PCH GPIO reading low when the Bluetooth daughter card is fitted
/// (devicetree `bdc_gpio_num` 48, `bdc_gpio_lvl` 0).
pub const BDC_PRESENCE_GPIO: u8 = 48;
/// Dock ID straps, GPIO3 (bit 0) to GPIO5 (bit 2); 7 means no dock.
pub const DOCK_ID_GPIOS: [u8; 3] = [3, 4, 5];
/// Connects the dock, high when docked.
pub const DOCK_CONNECT_GPIO: u8 = 28;
/// The EC's SCI on GPI 1 (`GPE_EC_SCI`) and its wake on GPI 13
/// (`GPE_EC_WAKE`).
pub const EC_SCI_GPI: u8 = 1;
pub const EC_WAKE_GPI: u8 = 13;

register_bitfields![u8,
    /// EC RAM 0x1a: the second dock link enable (ACPI `DKR2`).
    pub DOCK_LINK2 [CONNECT OFFSET(0) NUMBITS(1) []],
    /// EC RAM 0x32: the wake sources the OS armed.
    pub WAKE_CONTROL [
        EVENTS OFFSET(2) NUMBITS(1) [],
        LID OFFSET(4) NUMBITS(1) []
    ],
    /// EC RAM 0xfe: the third dock link enable (ACPI `DKR3`).
    pub DOCK_LINK3 [CONNECT OFFSET(4) NUMBITS(1) []]
];

impl EcRegister for DOCK_LINK2::Register {
    const INDEX: u8 = 0x1a;
}
impl EcRegister for WAKE_CONTROL::Register {
    const INDEX: u8 = 0x32;
}
impl EcRegister for DOCK_LINK3::Register {
    const INDEX: u8 = 0xfe;
}

/// coreboot `bootblock_mainboard_early_init()`: EC USB power before the
/// EHCI debug console needs the port.
pub fn early_usb_power(h8_config: &H8Config) -> bool {
    H8::new(Ec::new(h8_config.resources.os)).usb_power_enable(true)
}

/// coreboot `dock_connect()`: the three EC dock links, then the connect GPIO.
pub fn dock_connect(ec: Ec, connect: &mut impl OutputPin) -> bool {
    let linked = [
        ec.modify_register(CONFIG2::DOCK_USB_POWER::SET),
        ec.modify_register(DOCK_LINK2::CONNECT::SET),
        ec.modify_register(DOCK_LINK3::CONNECT::SET),
    ]
    .into_iter()
    .all(core::convert::identity);
    linked && connect.set_state(PinState::High).is_ok()
}

/// coreboot `dock_disconnect()`.
pub fn dock_disconnect(ec: Ec, connect: &mut impl OutputPin) -> bool {
    let unlinked = [
        ec.modify_register(CONFIG2::DOCK_USB_POWER::CLEAR),
        ec.modify_register(DOCK_LINK2::CONNECT::CLEAR),
        ec.modify_register(DOCK_LINK3::CONNECT::CLEAR),
    ]
    .into_iter()
    .all(core::convert::identity);
    unlinked && connect.set_state(PinState::Low).is_ok()
}

/// coreboot `dock_present()`: the dock ID straps read anything but 7.
pub fn dock_present(id: &mut [impl InputPin; 3]) -> bool {
    let mut value = 0;
    for (bit, pin) in id.iter_mut().enumerate() {
        match pin.is_high() {
            Ok(high) => value |= u8::from(high) << bit,
            Err(_) => return false,
        }
    }
    value != 7
}

/// Runtime EC/PMH7 bring-up with the user's `options` (coreboot
/// `h8_enable()` and the PMH7 `enable_dev()`), then `h8_mb_init()`:
/// connect a present dock. On S3 resume the suspend LED blinks (coreboot
/// `mainboard_enable()`). Returns whether the Bluetooth card is fitted and
/// every EC write was acknowledged.
pub fn ec_init(
    bdc_presence: &mut impl InputPin,
    dock_id: &mut [impl InputPin; 3],
    dock_connect_pin: &mut impl OutputPin,
    h8_config: &H8Config,
    options: &crate::options::Options,
    resume: bool,
) -> (bool, bool) {
    let bluetooth = bdc_presence.is_low().unwrap_or(false);
    let pmh7 = Pmh7::new(h8_config.resources.pmh7_base.raw());
    pmh7.backlight_enable(true);
    pmh7.dock_event_enable(true);
    pmh7.trackpoint_enable(options.trackpoint);
    pmh7.touchpad_enable(options.touchpad);

    let ec = Ec::new(h8_config.resources.os);
    let h8 = H8::new(ec);
    let mut acknowledged =
        h8.init_thinkpad(CONFIG1, &EVENT_MASKS, bluetooth, false, options, resume);
    if dock_present(dock_id) {
        fstart_log::info!("x201: dock is connected");
        acknowledged &= dock_connect(ec, dock_connect_pin);
    }
    if resume {
        acknowledged &= h8.set_led(H8Led::Suspend, H8LedMode::Blink);
    }
    (bluetooth, acknowledged)
}

/// What the board's SMI handler must change after an EC event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DockAction {
    Disconnect,
    Connect,
}

/// coreboot `mainboard_smi_handle_ec_sci()`: the dock latch and links for
/// the EC's dock events before OSPM owns them.
pub fn smm_ec_event(ec: Ec, event: u8) -> Option<DockAction> {
    match H8DockEvent::from_query(event)? {
        H8DockEvent::FnF9 | H8DockEvent::AcLost | H8DockEvent::DockDisconnected => {
            ec.modify_register(CONFIG3::DOCK_LATCH::CLEAR);
            Some(DockAction::Disconnect)
        }
        H8DockEvent::DockConnected | H8DockEvent::DockConnectedAlternate => {
            ec.modify_register(CONFIG3::DOCK_LATCH::CLEAR);
            Some(DockAction::Connect)
        }
    }
}

/// The rest of a dock connect after [`smm_ec_event`]: wait for the dock,
/// connect it, latch it and light its LED.
pub fn smm_dock_connected(ec: Ec, connect: &mut impl OutputPin) {
    fstart_arch::udelay(250_000);
    dock_connect(ec, connect);
    ec.modify_register(CONFIG3::DOCK_LATCH::SET);
    let h8 = H8::new(ec);
    h8.set_led(H8Led::Dock2, H8LedMode::Off);
    h8.set_led(H8Led::Dock1, H8LedMode::On);
}

/// coreboot `mainboard_smi_sleep()`: whether S3 should arm the EC wake GPE
/// as an SCI, which the OS asked for through EC RAM 0x32.
pub fn smm_wake_from_ec(ec: Ec) -> bool {
    ec.read_register::<WAKE_CONTROL::Register>()
        .is_some_and(|wake| wake.is_set(WAKE_CONTROL::EVENTS) || wake.is_set(WAKE_CONTROL::LID))
}
