// SPDX-License-Identifier: MIT OR Apache-2.0
use super::*;

#[test]
fn descriptor_regions_decode_to_the_bios_window() {
    let words = [0, 0x0fff0800, 0x07ff0003, 0x00020001, 0x7fff, 0x7fff];
    let observed = Observation::decode(0x0ff0a55a, 0, 5, words[1], words).unwrap();
    assert_eq!(
        observed.image().contiguous_window().unwrap().cpu_base,
        0xff800000
    );
    assert!(Observation::decode(0x0ff0a55a, 0, 5, words[1] | 1 << 31, words).is_err());
    assert!(Observation::decode(0x0ff0a55a, 0, 4, words[1], words).is_err());
    assert_eq!(require_idle_descriptor(1 << 14 | 1 << 5), Err(Error::Busy));
    assert_eq!(require_idle_descriptor(u32::MAX), Err(Error::Descriptor));
}
