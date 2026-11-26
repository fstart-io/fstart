// SPDX-License-Identifier: MIT OR Apache-2.0
use super::*;

#[test]
fn additional_steps_epg_units_integer_order_and_checked_native_u16_bound() {
    assert_eq!(power_overhead(100, 1200, 2), Ok(61)); // native comment's 240mW example
    assert_eq!(power_overhead(26, 1200, 1), Ok(7));
    // Native truncates to integer mW before fixed point, not one combined divide.
    assert_eq!(power_overhead(1, 1999, 2), Ok(0));
    assert_eq!(power_overhead(65535, 1000, 1), Ok(16776));
    for (idd, vdd, count) in [
        (0, 1200, 1),
        (26, 0, 1),
        (26, 1200, 0),
        (26, 1200, 5),
        (32768, 2000, 1),  // 65536 mW would wrap native uint16_t
        (65535, 65535, 4), // u32 product overflow
    ] {
        assert_eq!(power_overhead(idd, vdd, count), Err(Error::InvalidPolicy));
    }
}
