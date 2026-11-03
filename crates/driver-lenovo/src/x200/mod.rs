//! ThinkPad X200 board policy around the H8 EC and PMH7: EC bring-up, the
//! dock connection (coreboot `variants/x200/dock.c`) and the X200t tablet
//! digitizer behind the PC87382.

#[cfg(feature = "acpi")]
pub mod acpi;

use embedded_hal::digital::{InputPin, OutputPin, PinState};

use crate::ec::Ec;
use crate::h8::{H8, H8Config};
use crate::pmh7::Pmh7;

/// coreboot `event2_enable`..`eventd_enable`; event 0/1 and e/f stay off.
const EVENT_MASKS: [u8; 16] = [
    0x00, 0x00, 0xff, 0xff, 0xf4, 0x3c, 0x80, 0x01, 0x01, 0xff, 0xff, 0xff, 0xff, 0xff, 0x00, 0x00,
];

/// devicetree `config1`; X60/X61 use 0x05.
const CONFIG1: u8 = 0x04;

/// ICH GPIO reading low when the Bluetooth daughter card is fitted
/// (devicetree `bdc_gpio_num`/`bdc_gpio_lvl`).
pub const BDC_PRESENCE_GPIO: u8 = 7;
/// Dock ID straps, GPIO2 (bit 0) to GPIO4 (bit 2); 7 means no dock.
pub const DOCK_ID_GPIOS: [u8; 3] = [2, 3, 4];
/// Connects the dock (`dock_connect()`), high when docked.
pub const DOCK_CONNECT_GPIO: u8 = 28;

/// The PC87382 config port. Only its LDN 3 serial port is wired, to the
/// X200t digitizer (`variants/x200/overridetree.cb`).
pub const PC87382_BASE: u16 = 0x164e;

/// `variants/x200/overridetree.cb`: the digitizer on LDN 3 at 0x200/IRQ 5;
/// IR (2), GPIO (7) and DLPC (0x19) are not connected.
pub const PC87382_DEVICES: &[fstart_driver_superio::pc87382::PnpDevice] = {
    use fstart_driver_superio::pc87382::PnpDevice;
    const fn off(ldn: u8) -> PnpDevice {
        PnpDevice {
            ldn,
            enabled: false,
            io: None,
            registers: &[],
        }
    }
    &[
        PnpDevice {
            ldn: 3,
            enabled: true,
            io: Some(0x200),
            registers: &[(0x29, 0xb0), (0x70, 0x05), (0xf0, 0x82)],
        },
        off(2),
        off(7),
        off(0x19),
    ]
};

/// Runtime EC/PMH7 bring-up (coreboot `h8_enable()` and the PMH7
/// `enable_dev()`), then `h8_mb_init()`: connect a present dock. Returns
/// whether the Bluetooth card is fitted and every EC write was
/// acknowledged.
pub fn ec_init(
    bdc_presence: &mut impl InputPin,
    dock_id: &mut [impl InputPin; 3],
    dock_connect: &mut impl OutputPin,
    h8_config: &H8Config,
    resume: bool,
) -> (bool, bool) {
    let bluetooth = bdc_presence.is_low().unwrap_or(false);
    let pmh7 = Pmh7::new(h8_config.resources.pmh7_base.raw());
    pmh7.backlight_enable(true);
    pmh7.dock_event_enable(true);
    pmh7.trackpoint_enable(true);
    pmh7.touchpad_enable(true);

    let ec = Ec::new(h8_config.resources.os);
    let h8 = H8::new(ec);
    let initialized = h8.init_thinkpad(CONFIG1, &EVENT_MASKS, bluetooth, Some(true), resume);
    let docked = dock_present(dock_id);
    // `dock_connect()`: dock USB power in the EC, then the connect GPIO.
    let dock = !docked
        || (ec.modify_register(crate::h8::CONFIG2::DOCK_USB_POWER::SET)
            && dock_connect.set_state(PinState::High).is_ok());
    (bluetooth, initialized && dock)
}

/// `dock_present()`: the dock ID straps read anything but 7.
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

/// The X200t digitizer: program the PC87382 like the overridetree and
/// report a tablet (coreboot `drivers/lenovo/wacom.c`): a responding
/// SuperIO and a tablet machine type.
pub fn digitizer_init(part_number: Option<&str>) -> bool {
    fstart_driver_superio::pc87382::configure(PC87382_BASE, PC87382_DEVICES);
    fstart_driver_superio::pc87382::sid(PC87382_BASE) != 0xff
        && part_number.is_some_and(crate::wacom::is_tablet)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_pin::Pin;

    #[test]
    fn dock_presence_follows_the_id_straps() {
        assert!(!dock_present(&mut [
            Pin(Ok(true)),
            Pin(Ok(true)),
            Pin(Ok(true))
        ]));
        assert!(dock_present(&mut [
            Pin(Ok(true)),
            Pin(Ok(false)),
            Pin(Ok(true))
        ]));
        assert!(!dock_present(&mut [
            Pin(Err(())),
            Pin(Ok(false)),
            Pin(Ok(true))
        ]));
    }
}
