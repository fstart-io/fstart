// SPDX-License-Identifier: MIT OR Apache-2.0
use super::*;
use tock_registers::LocalRegisterCopy;

#[test]
fn sku_clamps_time_units_and_full_window_encoding_remain_bounded() {
    let units = LocalRegisterCopy::<u64, Units::Register>::new(
        (Units::POWER.val(3) + Units::TIME.val(8)).value,
    );
    let base_sku = (Sku::TDP.val(35 * 8) + Sku::MIN.val(10 * 8) + Sku::MAX.val(45 * 8)).value;
    let policy = Policy {
        pl1_watts: NonZeroU16::new(100),
        pl2_watts: NonZeroU16::new(20),
        ..Policy::NATIVE
    };
    // PL1 is clamped to SKU maximum; PL2 cannot remain below PL1. A different
    // hardware time unit must not accidentally retain the usual 0x6e encoding.
    assert_eq!(
        compute_limits(units, LocalRegisterCopy::new(base_sku), policy),
        Ok((45 * 8, 45 * 8, 0x6c))
    );
    let max_window_sku = base_sku | Sku::WINDOW.val(0x7f).value;
    assert_eq!(
        compute_limits(units, LocalRegisterCopy::new(max_window_sku), policy),
        Ok((45 * 8, 45 * 8, 0x7f))
    );
}
