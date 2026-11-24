// SPDX-License-Identifier: MIT OR Apache-2.0
use super::*;
use crate::skylake::{
    power_model::initialize_margin_limits,
    timings::{self, ReferenceClock},
};

#[test]
fn dt_centering_2d_signed_weighted_centers() {
    assert_eq!(rounded(-30, 20), Ok(-2));
    assert_eq!(rounded(30, 20), Ok(2));
    assert_eq!(rounded(-9, 20), Ok(0));
    assert_eq!(rounded(0, 0), Err(Error::InvalidTiming));
}

#[test]
fn read_2d_source_timing_scale_zero_saturation_and_clock_policy_boundaries() {
    assert_eq!(read_voltage_timing_scale(0, 0), 12);
    assert_eq!(read_voltage_timing_scale(5, 5), 12);
    assert_eq!(read_voltage_timing_scale(10, 10), 1);
    assert_eq!(read_voltage_timing_scale(120, 120), 12);
    assert_eq!(read_voltage_timing_scale(130, 130), 12);
    let mut s = CalibrationState::default();
    initialize_margin_limits(&mut s);
    use timings::FrequencyBin::*;
    for (tck, expected) in [
        (1_071_429, Mts1867),
        (1_000_000, Mts1867),
        (938_000, Mts2133),
        (833_333, Mts2400),
    ] {
        let clock = timings::select_clock(tck, ReferenceClock::Mhz133).unwrap();
        assert_eq!(clock.frequency_bin(), expected);
        // Native read 2D has no DDR4 frequency gate (the >=2400 gate in
        // centering_2d.c:166 is write-voltage-only). Do not transplant it.
        assert_eq!(
            margin_limit(
                &s,
                MarginKind::ReadTiming,
                LimitKind::Power,
                clock,
                TrainingMode::Power
            ),
            Ok(320)
        );
        assert_eq!(
            margin_limit(
                &s,
                MarginKind::ReadTiming,
                LimitKind::Upm,
                clock,
                TrainingMode::Power
            ),
            Ok(240)
        );
    }
}
