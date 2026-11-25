// SPDX-License-Identifier: MIT OR Apache-2.0
//! Realtek ALC283 (Peppy hda_verb.c). CPU display audio is a separate codec.
use fstart_driver_intel::southbridge::hda::{
    HdaConfig, HdaVerbTable, PinColor as C, PinConn as K, PinConnector as T, PinDevice as D,
    PinGeoLoc as G, PinLoc as L, pin_config as pin, pin_not_connected as nc,
};

pub static ALC283: HdaConfig = {
    let mut codec = HdaVerbTable::new(0x10ec_0283, 0x1179_0670);
    codec.pre_verbs = &[0x0017ff00, 0x0017ff00, 0x000f0000, 0x000f0002];
    codec = codec
        .pin(nc(0x12, 0))
        .pin(pin(
            0x14,
            D::Speaker,
            K::Integrated,
            L::Internal,
            G::NA,
            T::OtherAnalog,
            C::ColorUnknown,
            1,
            1,
            0,
        ))
        .pin(nc(0x17, 0))
        .pin(nc(0x18, 0))
        .pin(pin(
            0x19,
            D::MicIn,
            K::Jack,
            L::External,
            G::Left,
            T::StereoMono18,
            C::Black,
            0,
            2,
            0,
        ))
        .pin(pin(
            0x1a,
            D::MicIn,
            K::Integrated,
            L::Internal,
            G::NA,
            T::OtherAnalog,
            C::ColorUnknown,
            1,
            1,
            1,
        ))
        .pin(nc(0x1b, 0))
        .pin(pin(
            0x1d,
            D::Speaker,
            K::Nc,
            L::External,
            G::NA,
            T::Optical,
            C::Purple,
            1,
            2,
            13,
        ))
        .pin(nc(0x1e, 0))
        .pin(pin(
            0x21,
            D::HpOut,
            K::Jack,
            L::External,
            G::Left,
            T::StereoMono18,
            C::Black,
            0,
            2,
            1,
        ));
    let verbs = [
        // Realtek coefficients for beep generation and jack detection.
        0x02050010, 0x02040c20, 0x0205001b, 0x0204081b, 0x02050043, 0x0204a614, 0x02050047,
        0x02049470, // pc_beep_verbs: power, speaker, EAPD and mixers.
        0x00170500, 0x01470740, 0x01470c02, 0x0143b01f, 0x00c37100, 0x00b37410,
    ];
    let mut i = 0;
    while i < verbs.len() {
        codec = codec.extra_verb(verbs[i]);
        i += 1;
    }
    HdaConfig::new().verb(codec)
};
