// SPDX-License-Identifier: MIT OR Apache-2.0
use super::*;

#[test]
fn meminit_tx_vref_range_native_cumulative_sum_and_span_selection() {
    let mut s = CalibrationState::default();
    // All range1, even several ranks, is a no-op.
    s.ddr4_pda_mr6[1][2].fill(13);
    s.ddr4_pda_mr6[1][3].fill(13);
    assert_eq!(select_ranges(&s, [0, 12]), ([0, 0], false));
    // One all-range2 rank is not mixed. TWO all-range2 ranks IS mixed by
    // native's cumulative sum (16 != 8), and can select range1 in the overlap.
    s.ddr4_pda_mr6[1][2].fill(vref_code(0, 2).unwrap() as u8);
    s.ddr4_pda_mr6[1][3].fill(vref_code(0, 2).unwrap() as u8);
    assert_eq!(select_ranges(&s, [0, 4]), ([0, 0], false));
    assert_eq!(select_ranges(&s, [0, 12]), ([0, 1], true));
    // Source mixed span choice, including surprising larger-positive => 2,
    // ties => 1, and negative-dominant crossing => 1 (source, not a fix).
    for (lo, hi, want) in [
        (-30, 10, 2),
        (-20, 30, 2),
        (-30, 20, 1),
        (-30, 30, 1),
        (-13, 30, 1),
    ] {
        s.ddr4_pda_mr6[1][2].fill(vref_code(lo, 2).unwrap() as u8);
        s.ddr4_pda_mr6[1][2][7] = vref_code(hi, 1).unwrap() as u8;
        assert_eq!(select_ranges(&s, [0, 4]), ([0, want], true), "{lo}..{hi}");
    }
}
