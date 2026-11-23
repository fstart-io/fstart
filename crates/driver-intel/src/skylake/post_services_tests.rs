// SPDX-License-Identifier: MIT OR Apache-2.0
use super::*;
use crate::skylake::timings::{ReferenceClock, select_clock};

#[test]
fn post_services_retrain_threshold_rounding_per_byte_physical_ranks_and_compounding_limits() {
    let clock = select_clock(1_000_000, ReferenceClock::Mhz133).unwrap();
    let mut s = CalibrationState::default();
    initialize_margin_limits(&mut s);
    for r in 2..4 {
        for b in 0..8 {
            s.margin_results[1][r][b][..8].fill(1000);
        }
    }
    s.margin_results[1][3][7][2] = 95; // RDT limit9: floor(95/10)<=9.
    assert_eq!(
        compare_retrain(&mut s, [0, 12], clock, TrainingMode::Power).unwrap(),
        RetrainOutcome::Required {
            read: true,
            write: false
        }
    );
    assert_eq!(
        working_margin_limit(&mut s, MarginKind::ReadTiming)
            .unwrap()
            .upm_limit,
        320
    );
    assert_eq!(
        working_margin_limit(&mut s, MarginKind::WriteTiming)
            .unwrap()
            .upm_limit,
        240
    );
    compare_retrain(&mut s, [0, 12], clock, TrainingMode::Power).unwrap();
    assert_eq!(
        working_margin_limit(&mut s, MarginKind::ReadTiming)
            .unwrap()
            .upm_limit,
        400
    );
    s.margin_results[1][3][7][2] = 100;
    assert_eq!(
        compare_retrain(&mut s, [0, 12], clock, TrainingMode::Power).unwrap(),
        RetrainOutcome::Passed
    );
}
