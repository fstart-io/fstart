// SPDX-License-Identifier: MIT OR Apache-2.0
extern crate std;
use super::*;
use crate::skylake::{
    opt_tradeoff::*,
    power_model::*,
    spd::{Dimm, testing::sodimm},
    timings::{ReferenceClock, select_clock},
};

fn clock() -> Clock {
    select_clock(1_071_429, ReferenceClock::Mhz133).unwrap()
}

fn full(start: i8, len: u16) -> Grid {
    Grid {
        start: [start, 0],
        lengths: [len, 1],
        dimensions: 1,
        mode: GridMode::Full,
    }
}

fn trade_cfg() -> TradeoffConfig {
    TradeoffConfig {
        scale: [1, 1, 0, 0, 0],
        upm_optimize: [false; 5],
        square: false,
        average: 1,
        include_ends: true,
        center_scale: 1,
        good_power_limit: [10, 10, 0, 0, 0],
        optimization_mode: 100,
        guard_band: 0,
        linear: false,
    }
}

#[test]
fn get_params_xyz_full_chess_half_reversed_custom_and_200_point_bounds() {
    assert_eq!(full(-100, 200).get_params_xyz(199), Ok(([99, 0], false)));
    assert!(full(-100, 201).point_count().is_err());
    assert!(full(127, 2).point_count().is_err());
    for (mode, values) in [
        (GridMode::Reversed1D, [1, 0, -1]),
        (GridMode::CustomSlew, [-17, 0, 1]),
    ] {
        let g = Grid {
            mode,
            ..full(-1, 3)
        };
        for (i, v) in values.iter().enumerate() {
            assert_eq!(g.get_params_xyz(i).unwrap().0[0], *v);
        }
    }
    assert!(
        Grid {
            mode: GridMode::Half,
            ..full(0, 4)
        }
        .get_params_xyz(1)
        .unwrap()
        .1
    );
    for mode in [GridMode::ChessOdd, GridMode::ChessEven, GridMode::Full] {
        let g = Grid {
            start: [-1, -2],
            lengths: [3, 2],
            dimensions: 2,
            mode,
        };
        for i in 0..6 {
            let (xy, skip) = g.get_params_xyz(i).unwrap();
            assert_eq!(xy, [-1 + (i % 3) as i8, -2 + (i / 3) as i8]);
            assert_eq!(
                skip,
                match mode {
                    GridMode::ChessOdd => (i % 3 + i / 3) % 2 == 0,
                    GridMode::ChessEven => (i % 3 + i / 3) % 2 != 0,
                    _ => false,
                }
            );
        }
    }
}

#[test]
fn opt_param_limit_value_frequency_and_objective_source_precedence() {
    assert_eq!(
        opt_param_limits(OptParameter::TxEq, clock(), TrainingMode::Margin),
        Ok([6, 15])
    );
    let slow = select_clock(1_500_000, ReferenceClock::Mhz133).unwrap();
    assert_eq!(
        opt_param_limits(OptParameter::TxEq, slow, TrainingMode::Margin),
        Ok([4, 8])
    );
    assert!(opt_param_limits(OptParameter::DimmRon, clock(), TrainingMode::Power).is_err());
}

#[test]
fn meminit_calc_power_rsm_get_power_table_exact_last_eligible_signed_vectors() {
    let rx = get_power_table(PowerDirection::Read, 1333, 34);
    assert_eq!(rx.intercept, 105395535);
    assert_eq!(calc_power_rsm(rx, &[300, 300, 1000, 200], 10), Ok(825));
    let tx = get_power_table(PowerDirection::Write, 2133, 120);
    assert_eq!((tx.frequency, tx.termination), (1867, 120));
    assert_eq!(
        get_power_table(PowerDirection::Write, 800, 80).frequency,
        1333
    );
    assert_eq!(
        get_power_table(PowerDirection::Write, 1867, 80).termination,
        0
    );
    assert_eq!(
        get_power_table(PowerDirection::Read, 2400, 48).frequency,
        1867
    );
    assert!(calc_power_rsm(rx, &[], 10).is_err());
    assert!(calc_power_rsm(rx, &[1; 7], 10).is_err());
    assert!(calc_power_rsm(rx, &[1; 4], 0).is_err());
}

#[test]
fn meminit_calc_power_rsm_all_ddr4_coefficients_match_extracted_native_c_vectors() {
    // Independent native C evaluator/tables compiled with UBSan in /tmp;
    // vectors exercise signed cross terms, not just the intercept.
    for (i, expected) in [947, 821, 1014, 860, 1032, 870].iter().enumerate() {
        let frequency = [1333, 1600, 1867][i / 2];
        let ron = [34, 48][i % 2];
        assert_eq!(
            calc_power_rsm(
                get_power_table(PowerDirection::Read, frequency, ron),
                &[250, 180, 1000, 200],
                10
            ),
            Ok(*expected)
        );
    }
    for (i, expected) in [692, 826, 772, 758, 882, 832, 784, 905, 856]
        .iter()
        .enumerate()
    {
        let frequency = [1333, 1600, 1867][i / 3];
        let odt = [0, 120, 240][i % 3];
        assert_eq!(
            calc_power_rsm(
                get_power_table(PowerDirection::Write, frequency, odt),
                &[50, 45, 409, 250, 1000, 200],
                10
            ),
            Ok(*expected)
        );
    }
}

#[test]
fn meminit_effective_margin_linear_approx_trade_off_and_arithmetic_bounds() {
    assert_eq!(effective_margin(40, 60), Ok(90));
    assert_eq!(effective_margin(60, 40), Ok(90));
    assert!(effective_margin(65535, 65535).is_err());
    for (x, score) in [
        (0, 0),
        (250, 25),
        (750, 275),
        (1250, 1275),
        (2000, 1650),
        (4000, 1850),
        (5000, 1860),
    ] {
        assert_eq!(linear_approx_trade_off(x, 1000), Ok(score));
    }
    assert!(linear_approx_trade_off(1, 0).is_err());
}

#[test]
fn meminit_running_average_2d_partial_row_uses_native_preceding_row_fallback() {
    let mut scratch = [0; 200];
    let mut partial = [10, 20, 30, 40, 50];
    running_average_2d(&mut partial, 3, 0, 1, &mut scratch).unwrap();
    assert_eq!(partial, [23, 26, 30, 33, 33]);
    // No preceding row exists here: retain bounded horizontal fallback,
    // rather than reproducing the native C negative-row underflow.
    let mut first_row = [10, 20];
    running_average_2d(&mut first_row, 3, 0, 1, &mut scratch).unwrap();
    assert_eq!(first_row, [13, 16]);
}

#[test]
fn meminit_running_average_2d_fill_2d_average_measured_zero_is_not_a_hole() {
    let mut a = [10, 20, 30, 40];
    let mut scratch = [0; 200];
    running_average_2d(&mut a, 2, 0, 1, &mut scratch).unwrap();
    assert_eq!(a, [20, 23, 26, 30]);
    let mut a = [0, 80, 0, 80, 0, 80, 0, 80, 0];
    let valid = [true, false, true, false, true, false, true, false, true];
    fill_2d_average(&mut a, &valid, 3, 0, 1, &mut scratch).unwrap();
    assert_eq!(a[4], 0);
    assert_eq!(a[0], 0);
    assert!(running_average_2d(&mut a, 0, 0, 1, &mut scratch).is_err());
    let mut a = [20, 0, 40];
    fill_2d_average(&mut a, &[true, false, true], 3, 0, 1, &mut scratch).unwrap();
    assert_eq!(a, [20, 20, 30]);
}

#[test]
fn meminit_find_optimal_trade_off_upm_power_margin_ties_guardbands_and_smoothing() {
    let mut rows = [[0; 200]; 5];
    rows[0][..4].copy_from_slice(&[10, 12, 14, 14]);
    rows[1][..4].copy_from_slice(&[10, 12, 14, 14]);
    rows[2][..4].copy_from_slice(&[20, 30, 10, 10]);
    let mut work = TradeoffWorkspace::ZERO;
    let mut cfg = trade_cfg();
    assert_eq!(
        find_optimal_trade_off(&rows, 4, &cfg, &mut work)
            .unwrap()
            .best,
        2
    );
    cfg.optimization_mode = 2;
    cfg.scale[2] = 1;
    assert_eq!(
        find_optimal_trade_off(&rows, 4, &cfg, &mut work)
            .unwrap()
            .best,
        1
    );
    cfg.scale[2] = 0;
    cfg.optimization_mode = 100;
    cfg.guard_band = -2;
    let r = find_optimal_trade_off(&rows, 4, &cfg, &mut work).unwrap();
    assert_eq!((r.best, r.guard_band), (2, 0));
    cfg.guard_band = 1;
    assert_eq!(
        find_optimal_trade_off(&rows, 4, &cfg, &mut work)
            .unwrap()
            .guard_band,
        1
    );
    cfg.upm_optimize = [true, true, false, false, false];
    cfg.guard_band = 0;
    assert_eq!(
        find_optimal_trade_off(&rows, 4, &cfg, &mut work)
            .unwrap()
            .best,
        0
    );
    cfg.upm_optimize = [false; 5];
    cfg.average = 3;
    cfg.center_scale = 2;
    assert!(find_optimal_trade_off(&rows, 4, &cfg, &mut work).is_ok());
    cfg.linear = true;
    cfg.optimization_mode = 100;
    assert!(find_optimal_trade_off(&rows, 4, &cfg, &mut work).is_ok());
    cfg.scale = [0; 5];
    assert!(find_optimal_trade_off(&rows, 4, &cfg, &mut work).is_err());
    assert!(find_optimal_trade_off(&rows, 201, &cfg, &mut work).is_err());
}

#[test]
fn meminit_find_optimal_trade_off_square_large_inputs_and_rejected_denominators() {
    let mut rows = [[u16::MAX; 200]; 5];
    let mut cfg = trade_cfg();
    cfg.scale = [255; 5];
    cfg.square = true;
    cfg.average = 255;
    let mut work = TradeoffWorkspace::ZERO;
    assert!(find_optimal_trade_off(&rows, 200, &cfg, &mut work).is_ok());
    cfg.average = 1;
    cfg.center_scale = 0;
    assert!(find_optimal_trade_off(&rows, 200, &cfg, &mut work).is_err());
    cfg = trade_cfg();
    cfg.linear = true;
    cfg.good_power_limit[0] = 0;
    assert!(find_optimal_trade_off(&rows, 2, &cfg, &mut work).is_err());
    rows[0][0] = 0;
    cfg = trade_cfg();
    cfg.guard_band = i16::MIN;
    assert!(find_optimal_trade_off(&rows, 200, &cfg, &mut work).is_ok());
}

#[test]
fn meminit_normalize_power_to_margins_source_denominator_and_equal_power() {
    let mut rows = [[0; 200]; 5];
    rows[0][..2].copy_from_slice(&[100, 200]);
    rows[1][..2].copy_from_slice(&[300, 400]);
    rows[2][..2].copy_from_slice(&[100, 200]);
    normalize_power_to_margins(&mut rows, 2, 2).unwrap();
    assert_eq!(&rows[2][..2], &[300, 150]);
    rows[2][..2].fill(10);
    normalize_power_to_margins(&mut rows, 2, 2).unwrap();
    assert_eq!(&rows[2][..2], &[0, 0]);
    assert!(normalize_power_to_margins(&mut rows, 0, 2).is_err());
}

#[test]
fn update_opt_param_offset_all_ddr4_mr_field_images_preserve_neighbors() {
    let raw = sodimm();
    let d = Dimm::decode(&raw).unwrap();
    let dimms = [[Some(d), Some(d)], [None, None]];
    let mr = ModeRegisters {
        rank: [[[0xa5a5; 7]; 4]; 2],
    };
    for (p, off, rank, field, mask, value) in [
        (OptParameter::DimmOdt, 0x42, 0, 2, 7 << 9, 4 << 9),
        (OptParameter::DimmWriteOdt, 1, 0, 2, 7 << 9, 1 << 9),
        (OptParameter::DimmNominalOdt, 3, 0, 1, 7 << 8, 3 << 8),
        (OptParameter::DimmParkOdt, 4, 0, 5, 7 << 6, 4 << 6),
        (OptParameter::DimmParkOdtNonTarget, 2, 2, 5, 7 << 6, 2 << 6),
        (
            OptParameter::DimmNominalOdtNonTarget,
            6,
            2,
            1,
            7 << 8,
            6 << 8,
        ),
    ] {
        let (selected, _, images) = super::opt_mr_images(&dimms, &mr, 0, 1, p, off).unwrap();
        assert_eq!(selected, 1 << rank);
        assert_eq!(images[rank][field] & mask, value);
        assert_eq!(images[rank][field] & !mask, mr.rank[0][rank][field] & !mask);
        assert_eq!(images[rank ^ 2], mr.rank[0][rank ^ 2]);
    }
}
