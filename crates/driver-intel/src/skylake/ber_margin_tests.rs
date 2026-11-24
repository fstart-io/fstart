// SPDX-License-Identifier: MIT OR Apache-2.0
extern crate std;
use super::*;

#[test]
fn ber_log8_bit_shift_swap_and_interpolate_vref_source_integer_units() {
    assert_eq!(
        (0..=8).map(ber_log8).collect::<std::vec::Vec<_>>(),
        [0, 1, 3, 5, 7, 8, 9, 9, 10]
    );
    assert_eq!(ber_log8(64), 20);
    assert_eq!(ber_log8(0x7fffff), 76);
    assert_eq!(ber_bit_shift(0x1234, -8), 0x12);
    assert_eq!(ber_bit_shift(1, 31), 0x80000000);
    assert_eq!(ber_bit_shift(1, 32), 0);
    assert_eq!(ber_bit_shift(1, -32), 0);
    assert_eq!(ber_bit_shift(1, i8::MIN), 0);
    assert_eq!(ber_bit_swap(0xffffffff, 0, (-8i8) as u8, 8), 0xffffff);
    let mut stats = [0; 4];
    assert_eq!(ber_interpolate_vref(10, 2, 5, 7, &mut stats), 126);
    assert_eq!(ber_interpolate_vref(10, 5, 9, 7, &mut stats), 115);
    assert_eq!(ber_interpolate_vref(10, 9, 12, 7, &mut stats), 107);
    assert_eq!(ber_interpolate_vref(10, 9, 8, 7, &mut stats), 110);
    assert_eq!(stats, [1, 1, 1, 1]);
    assert_eq!(ber_vref_saturation(-61, 0, 63), Ok(2));
    assert_eq!(ber_vref_saturation(62, 1, 63), Ok(1));
    assert_eq!(ber_vref_saturation(64, 1, 63), Err(Error::InvalidTiming));
}
