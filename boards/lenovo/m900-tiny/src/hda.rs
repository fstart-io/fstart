// SPDX-License-Identifier: MIT OR Apache-2.0
//! Native mainboard/lenovo/m900_tiny/hda_verb.c inventory.
//! Source SHA256: ecdc45e849122fbb59a237901603e0d4a9bd10c06217dcccfb7806c17ad2aaac.
//! ALC233 is on SDI1
//! (not SDI0), and Skylake HDMI is codec 2. This is source board policy, not
//! captured hardware/connector validation. No PC-beep verbs exist in source.
use fstart_driver_intel::southbridge::hda::{
    HdaConfig, HdaVerbTable, PinColor::*, PinConn::*, PinConnector::*, PinDevice::*, PinGeoLoc::*,
    PinLoc::*, pin_config, pin_not_connected,
};

pub const ANALOG_CODEC_ADDRESS: u8 = 1;
pub const DISPLAY_CODEC_ADDRESS: u8 = 2;

pub static CONFIG: HdaConfig = HdaConfig::new()
    .verb(
        HdaVerbTable::new(0x10ec_0233, 0x17aa_30d0)
            .pin(pin_config(
                0x12,
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
                1,
                1,
                0,
            ))
            .pin(pin_not_connected(0x17, 0))
            .pin(pin_not_connected(0x18, 0))
            .pin(pin_config(
                0x19,
                MicIn,
                Jack,
                External,
                Front,
                StereoMono18,
                Black,
                0,
                3,
                0,
            ))
            .pin(pin_config(
                0x1a,
                MicIn,
                Jack,
                External,
                Front,
                StereoMono18,
                Black,
                0,
                4,
                0,
            ))
            .pin(pin_config(
                0x1b,
                LineOut,
                Jack,
                External,
                Rear,
                StereoMono18,
                Black,
                0,
                2,
                0,
            ))
            .pin(pin_config(
                0x1d,
                SpdifOut,
                Nc,
                External,
                NA,
                Unknown,
                ColorUnknown,
                0,
                0,
                1,
            ))
            .pin(pin_not_connected(0x1e, 0))
            .pin(pin_config(
                0x21,
                HpOut,
                Jack,
                External,
                Front,
                StereoMono18,
                Black,
                0,
                1,
                15,
            )),
    )
    .verb(
        HdaVerbTable::new(0x8086_2809, 0x8086_0101)
            .pin(pin_config(
                0x05,
                DigitalOtherOut,
                Jack,
                Internal,
                Special8,
                OtherDigital,
                ColorUnknown,
                0,
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
                0,
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
                0,
                3,
                0,
            )),
    );
