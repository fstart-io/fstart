// SPDX-License-Identifier: MIT OR Apache-2.0
//! Fixed transmitter coefficients for Gen3/4/5 (DEVICES.md §2.3): a port
//! whose descriptor asks for them (Dell port parameters `0x76`/`0x77`/
//! `0x78`, GIGABYTE one less; [`super::port::Eq::coefficients`]) gets the
//! pre-cursor, cursor and post-cursor of a PCIe preset written to
//! `0x1C44_02E0` (Gen3), `0x037C` (Gen4) or `0x0418` (Gen5), then the
//! enable (`AfterReconfigPerPortProgramming`'s callees, Dell `0xf82470`
//! [D]).
//!
//! [`PRESETS`] is generated from `reports/research/DEVICES/tables.json`
//! (`pcie_preset_coefficients`: Dell SilVenicePei `0xfcf008`, GIGABYTE
//! `0x70a0bd20`, byte-identical [I]). Do not edit.

use super::straps::Emit;
use crate::nbio::wh::pcie::{self, EqCoefficients};

/// `(pre-cursor, cursor, post-cursor)` of presets P0..P9.
pub const PRESETS: [(u8, u8, u8); 10] = [
    (0, 36, 12),
    (0, 40, 8),
    (0, 38, 10),
    (0, 42, 6),
    (0, 48, 0),
    (4, 44, 0),
    (6, 42, 0),
    (5, 33, 10),
    (7, 35, 6),
    (8, 40, 0),
];

/// The coefficients of `preset`; an index of 10 or more is P0, as the
/// vendor takes it.
pub const fn of(preset: u8) -> (u8, u8, u8) {
    if (preset as usize) < PRESETS.len() {
        PRESETS[preset as usize]
    } else {
        PRESETS[0]
    }
}

/// The writes for Gen `3 + rate` (`rate` 0..=2) with `preset`, in the
/// vendor's order: pre-cursor, cursor, post-cursor, then the enable.
pub(super) fn emit(e: &mut Emit, rate: usize, preset: u8) {
    let Some(&reg) = pcie::EQ_COEFFICIENTS.get(rate) else {
        return;
    };
    let (pre, cursor, post) = of(preset);
    e.field(reg, EqCoefficients::PRE_CURSOR.val(pre.into()));
    e.field(reg, EqCoefficients::CURSOR.val(cursor.into()));
    e.field(reg, EqCoefficients::POST_CURSOR.val(post.into()));
    e.field(reg, EqCoefficients::ENABLE::SET);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_sum_to_the_full_swing_and_out_of_range_is_p0() {
        // Every row's |pre| + cursor + |post| is 48 (full swing, 1/48 units).
        assert!(PRESETS.iter().all(|&(a, b, c)| a + b + c == 48));
        assert_eq!(of(4), (0, 48, 0));
        assert_eq!(of(12), of(0));
    }
}
