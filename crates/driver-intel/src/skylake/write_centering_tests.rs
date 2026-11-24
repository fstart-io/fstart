// SPDX-License-Identifier: MIT OR Apache-2.0
use super::*;
use crate::skylake::timings::{ReferenceClock, select_clock};

fn clock() -> Clock {
    select_clock(1_000_000, ReferenceClock::Mhz100).unwrap()
}

#[test]
fn condense_margin_byte_all_selected_rank_intersection_and_zero_nonfirst_rank() {
    let mut s = CalibrationState::default();
    s.margin_results[1][2][3][4..6].copy_from_slice(&[90, 130]);
    s.margin_results[1][3][3][4..6].copy_from_slice(&[120, 70]);
    assert_eq!(condense_margin_byte(&s, 1, 12, 3, 2), Ok([90, 70]));
    s.margin_results[1][3][3][4] = 0;
    assert!(condense_margin_byte(&s, 1, 12, 3, 2).is_err());
}

#[test]
fn write_voltage_centering_2d_source_frequency_threshold_and_signed_weights() {
    let mut c = clock();
    for (tck, width) in [
        (1_000_000, 8),
        (938_000, 8),
        (833_334, 8),
        (833_333, 4),
        (750_000, 4),
    ] {
        c.tck_fs = tck;
        assert_eq!(minimum_voltage_width(c), width);
    }
    assert_eq!(clock().frequency_bin(), FrequencyBin::Mts1867);
    assert_eq!(timing_scale([10, 10]), 1);
    assert_eq!(timing_scale([1, 1]), 12);
    assert_eq!(timing_scale([130, 130]), 12);
    assert_eq!(rounded(-90, 60), Ok(-2));
    assert_eq!(rounded(90, 60), Ok(2));
}
