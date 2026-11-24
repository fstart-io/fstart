// SPDX-License-Identifier: MIT OR Apache-2.0
//! ThinkPad T480 Sunrise Point-LP USB2/USB3 port wiring from
//! `mainboard/lenovo/sklkbl_thinkpad/variants/t480/overridetree.cb`.

use fstart_driver_intel::sunrise_point::usb::{Config, Usb2Port, Usb3Port};

pub static CONFIG: Config = Config {
    usb2: [
        Usb2Port::mid(Some(1)), // Port 0: USB-A
        Usb2Port::mid(Some(0)), // Port 1: USB-A (always on)
        Usb2Port::mid(None),    // Port 2: JSC-1 (smartcard slot)
        Usb2Port::mid(None),    // Port 3: USB-C (charging port)
        Usb2Port::mid(None),    // Port 4: JCAM1 (IR camera)
        Usb2Port::mid(None),    // Port 5: JWWAN1 (M.2 WWAN USB)
        Usb2Port::mid(None),    // Port 6: JWLAN1 (M.2 WLAN USB / Bluetooth)
        Usb2Port::mid(None),    // Port 7: JCAM1 (webcam)
        Usb2Port::mid(None),    // Port 8: JFPR1 (fingerprint reader)
        Usb2Port::mid(None),    // Port 9: JLCD1 (touch panel)
        Usb2Port::DISABLED,
        Usb2Port::DISABLED,
        Usb2Port::DISABLED,
        Usb2Port::DISABLED,
        Usb2Port::DISABLED,
        Usb2Port::DISABLED,
    ],
    usb3: [
        Usb3Port::standard(Some(1)), // Port 0: USB-A
        Usb3Port::standard(Some(0)), // Port 1: USB-A (always on)
        Usb3Port::standard(None),    // Port 2: RTS5344S (SD card reader)
        Usb3Port::standard(None),    // Port 3: USB-C (charging port)
        Usb3Port::DISABLED,
        Usb3Port::DISABLED,
        Usb3Port::DISABLED,
        Usb3Port::DISABLED,
        Usb3Port::DISABLED,
        Usb3Port::DISABLED,
    ],
};
