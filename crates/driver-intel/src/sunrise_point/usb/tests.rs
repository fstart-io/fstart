// SPDX-License-Identifier: MIT OR Apache-2.0
use super::*;
#[test]
fn overcurrent_maps_ignore_disabled_ports_but_include_usbr_without_pdo() {
    let mut config = Config {
        usb2: [Usb2Port::DISABLED; 16],
        usb3: [Usb3Port::DISABLED; 10],
    };
    config.usb2[1] = Usb2Port::mid(Some(3));
    config.usb2[15] = Usb2Port::mid(Some(3)); // USBr: OC mapping, not physical PDO
    config.usb2[2].overcurrent = Some(3); // disabled port must NOT claim OC
    config.usb3[4] = Usb3Port::standard(Some(1));
    let maps = config.maps().unwrap();
    assert_eq!(maps.usb2_oc[3], (1 << 1) | (1 << 15));
    assert_eq!(maps.usb3_oc[1], 1 << 4);
    assert_eq!(maps.usb2_disable, 0x3fff & !(1 << 1));
    assert_eq!(maps.usb3_disable, 0x3ff & !(1 << 4));
}
#[test]
fn invalid_usb_afe_and_oc_policy_is_rejected_without_transport_activity() {
    let mut config = Config {
        usb2: [Usb2Port::DISABLED; 16],
        usb3: [Usb3Port::DISABLED; 10],
    };
    config.usb2[0].pre_bias = 8;
    assert_eq!(config.validate(), Err(Error::Policy));
    config.usb2[0].pre_bias = 0;
    config.usb3[0].overcurrent = Some(8);
    assert_eq!(config.validate(), Err(Error::Policy));
    config.usb3[0].overcurrent = None;
    config.usb3[0].de_emphasis = 64;
    assert_eq!(config.validate(), Err(Error::Policy));
}
#[test]
fn port_control_updates_do_not_implicitly_ack_events_disable_or_restart_reset() {
    let pending = CHANGE_MASK
        | PORTSC::PED::SET.value
        | PORTSC::PR::SET.value
        | PORTSC::WPR::SET.value
        | PORTSC::PP::SET.value;
    let update = portsc_value(pending, PORTSC::PLS.val(5) + PORTSC::LWS::SET);
    assert_eq!(
        update
            & (CHANGE_MASK
                | PORTSC::PED::SET.value
                | PORTSC::PR::SET.value
                | PORTSC::WPR::SET.value),
        0
    );
    assert_ne!(update & PORTSC::PP::SET.value, 0);
    // Explicitly acknowledge one observed event; the other pending events
    // must receive zero, even if they appeared after the first snapshot.
    let ack = portsc_value(pending, PORTSC::WRC::SET);
    assert_eq!(ack & CHANGE_MASK, PORTSC::WRC::SET.value);
    let disable = portsc_value(pending, PORTSC::PED::SET);
    assert_ne!(disable & PORTSC::PED::SET.value, 0);
}
