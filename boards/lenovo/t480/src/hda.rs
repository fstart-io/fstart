// SPDX-License-Identifier: MIT OR Apache-2.0
//! ThinkPad T480 HD Audio verb tables from
//! `mainboard/lenovo/sklkbl_thinkpad/variants/t480/hda_verb.c`:
//! Realtek ALC257 on SDI0 (`0x10ec:0x0257`, subsystem `0x17aa:0x225d`) plus
//! Intel Kaby Lake HDMI/DP audio on SDI2 (`0x8086:0x280b`, subsystem `0x8086:0x0101`).

use fstart_driver_intel::southbridge::hda::{
    HdaConfig, HdaVerbTable, MISC_NO_PRESENCE_DETECT, MISC_PRESENCE_DETECT, PinColor::*,
    PinConn::*, PinConnector::*, PinDevice::*, PinGeoLoc::*, PinLoc::*, pin_config,
    pin_not_connected,
};

pub const ANALOG_CODEC_ADDRESS: u8 = 0;
pub const DISPLAY_CODEC_ADDRESS: u8 = 2;

pub static CONFIG: HdaConfig = HdaConfig::new()
    .verb(
        HdaVerbTable::new(0x10ec_0257, 0x17aa_225d)
            .pin(pin_config(
                0x12,
                MicIn,
                Integrated,
                Internal,
                NA,
                OtherDigital,
                ColorUnknown,
                MISC_NO_PRESENCE_DETECT,
                2,
                0,
            ))
            // 0x40000000: does not describe a jack or internal device.
            .pin(pin_config(
                0x13,
                LineOut,
                Nc,
                External,
                NA,
                Unknown,
                ColorUnknown,
                0,
                0,
                0,
            ))
            .pin(pin_config(
                0x14,
                Speaker,
                Integrated,
                Internal,
                NA,
                OtherAnalog,
                ColorUnknown,
                MISC_NO_PRESENCE_DETECT,
                1,
                0,
            ))
            .pin(pin_not_connected(0x18, 0))
            .pin(pin_config(
                0x19,
                MicIn,
                Jack,
                External,
                Right,
                StereoMono18,
                Black,
                MISC_PRESENCE_DETECT,
                3,
                0,
            ))
            .pin(pin_not_connected(0x1a, 0))
            .pin(pin_not_connected(0x1b, 0))
            // 0x40661b45: does not describe a jack or internal device.
            .pin(pin_config(
                0x1d,
                ModemLineSide,
                Nc,
                External,
                NA,
                OtherDigital,
                Black,
                0xb,
                4,
                5,
            ))
            .pin(pin_not_connected(0x1e, 0))
            .pin(pin_config(
                0x21,
                HpOut,
                Jack,
                External,
                Right,
                StereoMono18,
                Black,
                MISC_PRESENCE_DETECT,
                1,
                15,
            ))
            // Widget node 0x20 - 0: hidden register SW reset.
            .extra_verb(0x0205_001a)
            .extra_verb(0x0204_c003)
            .extra_verb(0x0205_001a)
            .extra_verb(0x0204_c003)
            .extra_verb(0x0585_0000)
            .extra_verb(0x0584_f880)
            .extra_verb(0x0585_0000)
            .extra_verb(0x0584_f880)
            // Widget node 0x20 - 1: ClassD 2W.
            .extra_verb(0x0205_0038)
            .extra_verb(0x0204_8981)
            .extra_verb(0x0205_001b)
            .extra_verb(0x0204_0a4b)
            // Widget node 0x20 - 2.
            .extra_verb(0x0205_003c)
            .extra_verb(0x0204_3154)
            .extra_verb(0x0205_003c)
            .extra_verb(0x0204_3114)
            // Widget node 0x20 - 3.
            .extra_verb(0x0205_0046)
            .extra_verb(0x0204_0004)
            .extra_verb(0x0575_0003)
            .extra_verb(0x0574_09a3)
            // Widget node 0x20 - 4: JD1 enable 1JD port for HP JD.
            .extra_verb(0x0205_0009)
            .extra_verb(0x0204_6003)
            .extra_verb(0x0205_000a)
            .extra_verb(0x0204_7770)
            // Widget node 0x20 - 5: silence data mode threshold (-84 dB).
            .extra_verb(0x0205_0037)
            .extra_verb(0x0204_fe15)
            .extra_verb(0x0205_0030)
            .extra_verb(0x0204_9004),
    )
    .verb(
        HdaVerbTable::new(0x8086_280b, 0x8086_0101)
            .pin(pin_config(
                0x05,
                DigitalOtherOut,
                Jack,
                Internal,
                Special8,
                OtherDigital,
                ColorUnknown,
                MISC_PRESENCE_DETECT,
                1,
                0,
            ))
            .pin(pin_config(
                0x06,
                DigitalOtherOut,
                Jack,
                Internal,
                Special8,
                OtherDigital,
                ColorUnknown,
                MISC_PRESENCE_DETECT,
                2,
                0,
            ))
            .pin(pin_config(
                0x07,
                DigitalOtherOut,
                Jack,
                Internal,
                Special8,
                OtherDigital,
                ColorUnknown,
                MISC_PRESENCE_DETECT,
                3,
                0,
            )),
    );

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_alc257_pin_descriptors_match_coreboot_table() {
        let alc257 = CONFIG.verbs.as_slice()[0];
        let pins = alc257.pins.as_slice();
        assert_eq!(pins[1].nid, 0x13);
        assert_eq!(pins[1].encode(), 0x4000_0000);
        assert_eq!(pins[7].nid, 0x1d);
        assert_eq!(pins[7].encode(), 0x4066_1b45);
        assert_eq!(alc257.extra_verbs.len(), 28);
    }
}
