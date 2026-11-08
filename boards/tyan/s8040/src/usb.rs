// SPDX-License-Identifier: MIT OR Apache-2.0
//! The board's USB wiring and PHY policy, from the vendor firmware's PCD
//! database: token 15Fh's table disables the USB2 overrides and enables
//! the USB3 ones; ffa5493d consumes the four bonded-port records below.
use fstart_driver_amd::fch::songshan::usb::{Policy, Usb3Tuning};
const PHY: Usb3Tuning = Usb3Tuning {
    word_1242d8: 7,
    bit_12c064: false,
    nibble_12c064: 0,
    word_13004c: 7,
    bit_120088: true,
    mid_120088: 5,
    low_bit_120088: false,
    low_120088: 5,
};
pub const POLICY: Policy = Policy {
    enabled: [true, true],
    over_current: [0xf0f0, 0xffff],
    device_removable: 0,
    ras: false,
    over_current_low: true,
    sparse: true,
    debug_pipe_switch: false,
    // Vendor token 16Eh is FFh; only the four bonded-port bits are used.
    force_gen1: 0xf,
    usb2: None,
    usb3: Some([[PHY; 2]; 2]),
};
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn nfx_usb_policy_is_preflighted_without_firmware_pointers() {
        assert_eq!(POLICY.enabled, [true, true]);
        assert_eq!(POLICY.over_current, [0xf0f0, 0xffff]);
        assert!(POLICY.usb2.is_none());
        assert!(fstart_driver_amd::fch::songshan::usb::ColdBoot::new(&POLICY).is_ok());
    }
}
