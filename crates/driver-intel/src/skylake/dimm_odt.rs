// SPDX-License-Identifier: MIT OR Apache-2.0
//! Native opt_param.c:1590–2246, DDR4 DIMM ODT consumers.
//! Candidate capacity is the actual source maximum: 5 NOM x 2 WR x 5 PARK.
use super::*;
use crate::skylake::{
    opt_param::opt_mr_images,
    opt_tradeoff::{TradeoffConfig, find_optimal_trade_off},
};

pub const MAX_DIMM_OPT_POINTS: usize = 50;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DimmOdtTest {
    Write,
    Nominal,
    NominalNonTarget,
    Park,
    ParkNonTarget,
    Combined,
    Combined1R1R,
    ParkSlot1,
}
/// The two nested tests actually consumed by native DDR4 DIMM ODT flows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DimmSubTest {
    ReadOdt,
    TxEqWriteDrive,
}
#[derive(Clone, Copy)]
struct DimmBestOffsets {
    grid: Grid,
    best: [u16; 9],
}
#[derive(Clone, Copy)]
pub struct DimmOptPoint {
    odt: [Option<(OptParameter, u8)>; 3],
    // Each channel retains its own clipped grid and measured byte winners.
    best: [[Option<DimmBestOffsets>; 2]; 2],
    widths: [[u16; 2]; 4],
    tests: usize,
}
impl DimmOptPoint {
    pub const EMPTY: Self = Self {
        odt: [None; 3],
        best: [[None; 2]; 2],
        widths: [[0; 2]; 4],
        tests: 0,
    };
}
pub struct DimmOdtWorkspace {
    points: [DimmOptPoint; MAX_DIMM_OPT_POINTS],
    // Independent of both the outer transaction and nested optimizer scratch.
    candidate: DqSiWorkspace,
    pub tested: [u8; 2],
    pub best: [u8; 2],
}
impl DimmOdtWorkspace {
    /// Typed allocation image only; not measured calibration or training success.
    pub const INITIAL: Self = Self {
        points: [DimmOptPoint::EMPTY; MAX_DIMM_OPT_POINTS],
        candidate: DqSiWorkspace::INITIAL,
        tested: [0; 2],
        best: [0; 2],
    };
}

impl Default for DimmOdtWorkspace {
    fn default() -> Self {
        Self::INITIAL
    }
}
struct OdtGrid {
    parameters: [OptParameter; 3],
    values: [[u8; 8]; 3],
    lengths: [usize; 3],
    dimensions: usize,
    tests: usize,
    set_ranks: u8,
    recenter_read: bool,
    recenter_write: bool,
    upm: [bool; 5],
}
impl OdtGrid {
    fn new(test: DimmOdtTest, ranks: u8, two_dpc: bool, power: bool) -> Self {
        use DimmOdtTest::*;
        use OptParameter::*;
        const ALL: [u8; 8] = [0, 4, 2, 6, 1, 5, 3, 7];
        const HIGH: [u8; 8] = [6, 1, 5, 3, 7, 0, 0, 0];
        const WR: [u8; 8] = [2, 1, 4, 0, 0, 0, 0, 0];
        let mut g = Self {
            parameters: [DimmWriteOdt; 3],
            values: [WR; 3],
            lengths: [1; 3],
            dimensions: 1,
            tests: 4,
            set_ranks: ranks,
            recenter_read: false,
            recenter_write: false,
            upm: [false; 5],
        };
        match test {
            Write => {
                g.parameters[0] = DimmWriteOdt;
                g.lengths[0] = 3;
                g.tests = 2;
                g.recenter_write = true;
            }
            Nominal | NominalNonTarget | Park | ParkNonTarget | ParkSlot1 => {
                g.parameters[0] = match test {
                    Nominal => DimmNominalOdt,
                    NominalNonTarget => DimmNominalOdtNonTarget,
                    ParkNonTarget => DimmParkOdtNonTarget,
                    _ => DimmParkOdt,
                };
                g.values[0] = ALL;
                g.lengths[0] = 8;
                g.recenter_read = test != NominalNonTarget;
                g.recenter_write = test != NominalNonTarget;
                if test == ParkSlot1 {
                    g.set_ranks = 12;
                }
            }
            Combined1R1R => {
                g.parameters = [DimmWriteOdt, DimmNominalOdtNonTarget, DimmWriteOdt];
                g.values = [WR, ALL, WR];
                g.lengths = [3, 8, 1];
                g.dimensions = 2;
            }
            Combined => {
                g.recenter_read = true;
                g.recenter_write = true;
                if !two_dpc {
                    g.parameters = [DimmWriteOdt, DimmParkOdt, DimmWriteOdt];
                    g.values = [WR, ALL, WR];
                    g.lengths = [3, 8, 1];
                    g.dimensions = 2;
                } else if ranks & 3 != 0 {
                    g.parameters = [DimmNominalOdtNonTarget, DimmWriteOdt, DimmParkOdt];
                    g.values = [HIGH, WR, HIGH];
                    g.lengths = [5, 2, 5];
                    g.dimensions = 3;
                    if !power {
                        g.upm[1] = true;
                        g.upm[3] = true;
                    }
                } else {
                    g.parameters = [DimmNominalOdtNonTarget, DimmWriteOdt, DimmWriteOdt];
                    g.values = [ALL, WR, WR];
                    g.lengths = [8, 3, 1];
                    g.dimensions = 2;
                }
            }
        }
        g
    }
    fn count(&self) -> usize {
        self.lengths.iter().product()
    }
    fn point(&self, index: usize) -> DimmOptPoint {
        let mut p = DimmOptPoint::EMPTY;
        let mut n = index;
        for i in 0..self.dimensions {
            p.odt[i] = Some((self.parameters[i], self.values[i][n % self.lengths[i]]));
            n /= self.lengths[i];
        }
        p
    }
}
impl Engine<'_> {
    /// Apply actual DIMM/rank MR controls and the selected nested DQ offsets.
    /// Inputs are a point produced by this module, not arbitrary margin arrays.
    fn update_opt_point_values(
        &self,
        s: &mut CalibrationState,
        p: DqSiPolicy,
        ch: usize,
        ranks: u8,
        point: &DimmOptPoint,
        skip_best: bool,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        let valid = super::super::reut::rank_mask(self.dimms, ch)?;
        if ranks == 0 || ranks & !15 != 0 || ranks & valid == 0 {
            return Err(Error::InvalidRank);
        }
        // Validate every MR transform before the first write.
        for &(param, value) in point.odt.iter().flatten() {
            opt_mr_images(self.dimms, &s.saved_mr, ch, ranks, param, i16::from(value))?;
        }
        let mut host = self.opt_host_sync(s, 8)?;
        for &(param, value) in point.odt.iter().flatten() {
            self.update_opt_param_offset(
                s,
                &mut host,
                p.package,
                ch,
                ranks,
                0,
                param,
                i16::from(value),
                true,
                delay,
            )?;
        }
        if !skip_best {
            for (test, per_channel) in point.best.iter().enumerate() {
                let Some(result) = &per_channel[ch] else {
                    continue;
                };
                let params = if point.tests == 2 || test == 1 {
                    [OptParameter::TxEq, OptParameter::WriteDrive]
                } else {
                    [OptParameter::ReadOdt, OptParameter::Default]
                };
                let dimensions = if params[0] == OptParameter::ReadOdt {
                    1
                } else {
                    2
                };
                for b in 0..8 {
                    let (values, _) = result.grid.get_params_xyz(usize::from(result.best[b]))?;
                    for i in 0..dimensions {
                        self.update_opt_param_offset(
                            s,
                            &mut host,
                            p.package,
                            ch,
                            ranks,
                            b,
                            params[i],
                            i16::from(values[i]),
                            true,
                            delay,
                        )?;
                    }
                }
            }
            force_rcomp(self.registers, delay)?;
        }
        Ok(())
    }
    /// Actual native DDR4 nested read-ODT and combined TXEQ/DS consumers.
    /// Every selected physical rank is independently remeasured after the
    /// optimizer; no rank-zero result is used for an absent/unselected rank.
    fn train_dimm_odt_setting(
        &self,
        s: &mut CalibrationState,
        ctx: &CommandContext<'_>,
        p: DqSiPolicy,
        mask: u8,
        ranks: u8,
        tests: &[DimmSubTest],
        skip: bool,
        point: &mut DimmOptPoint,
        opt: &mut OptWorkspace,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        if tests != [DimmSubTest::TxEqWriteDrive]
            && tests != [DimmSubTest::ReadOdt, DimmSubTest::TxEqWriteDrive]
        {
            return Err(Error::InvalidPolicy);
        }
        point.tests = tests.len() * 2;
        for (i, &test) in tests.iter().enumerate() {
            let tx = test == DimmSubTest::TxEqWriteDrive;
            let params = if tx {
                [OptParameter::TxEq, OptParameter::WriteDrive]
            } else {
                [OptParameter::ReadOdt, OptParameter::Default]
            };
            let params = &params[..if tx { 2 } else { 1 }];
            let g = grid(
                params,
                p,
                if tx {
                    GridMode::ChessEven
                } else {
                    GridMode::Full
                },
            )?;
            let gb = if tx && p.mode == TrainingMode::Power {
                if p.phy.vccio_mv > 900 && p.clock.frequency_bin() > FrequencyBin::Mts1600 {
                    3
                } else {
                    1
                }
            } else {
                0
            };
            let output = self.dq_opt(
                s,
                ctx,
                p,
                params,
                if tx { &WRITE_TESTS } else { &READ_TESTS },
                g,
                [1, 2, u8::from(p.mode == TrainingMode::Power), 0, 0],
                [false; 5],
                if tx { 17 } else { 16 },
                if tx { None } else { Some(DqPattern::ReadRead) },
                gb,
                skip,
                mask,
                ranks,
                opt,
                delay,
            )?;
            for ch in 0..2 {
                if mask & (1 << ch) != 0 {
                    point.best[i][ch] = Some(DimmBestOffsets {
                        grid: output.grid,
                        best: output.best[ch],
                    });
                }
            }
            let widths = self.dq_remeasure(
                s,
                ctx,
                mask,
                ranks,
                if tx { &WRITE_TESTS } else { &READ_TESTS },
                delay,
            )?;
            for j in 0..2 {
                for ch in 0..2 {
                    if mask & (1 << ch) != 0 {
                        point.widths[i * 2 + j][ch] = widths[j][ch];
                    }
                }
            }
        }
        Ok(())
    }
    fn dimm_odt_body(
        &self,
        s: &mut CalibrationState,
        ctx: &CommandContext<'_>,
        p: DqSiPolicy,
        mask: u8,
        ranks: u8,
        skip: bool,
        tests: &[DimmOdtTest],
        opt: &mut OptWorkspace,
        center: &mut WriteCenteringWorkspace,
        dimm: &mut DimmOdtWorkspace,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        let actual = cold_ranks(self, ctx.mode)?;
        let two =
            (0..2).any(|ch| mask & (1 << ch) != 0 && self.dimms[ch].iter().all(Option::is_some));
        let local = core::array::from_fn::<_, 2, _>(|ch| {
            if mask & (1 << ch) != 0 {
                actual[ch] & ranks
            } else {
                0
            }
        });
        let selected = channels(local);
        if selected == 0 {
            return Err(Error::InvalidRank);
        }
        for &test in tests {
            let g = OdtGrid::new(test, ranks, two, p.mode == TrainingMode::Power);
            if g.count() > MAX_DIMM_OPT_POINTS {
                return Err(Error::InvalidPolicy);
            }
            dimm.tested = [0; 2];
            dimm.best = [0; 2];
            let mut pending = selected;
            let sub = if g.tests == 2 {
                &[DimmSubTest::TxEqWriteDrive][..]
            } else {
                &[DimmSubTest::ReadOdt, DimmSubTest::TxEqWriteDrive][..]
            };
            for index in 0..g.count() {
                if pending == 0 {
                    break;
                }
                let point = &mut dimm.points[index];
                *point = g.point(index);
                for ch in 0..2 {
                    if pending & (1 << ch) == 0 {
                        continue;
                    }
                    dimm.tested[ch] = (index + 1) as u8;
                    for slot in 0..2 {
                        let set = actual[ch] & g.set_ranks & (3 << (slot * 2));
                        if set != 0 {
                            self.update_opt_point_values(s, p, ch, set, point, true, delay)?;
                        }
                    }
                }
                point.tests = g.tests;
                for ch in 0..2 {
                    let channel = 1 << ch;
                    if pending & channel == 0 {
                        continue;
                    }
                    // An absent eye is local to this channel. Snapshot after
                    // prior channels finish, so rollback preserves their work.
                    let phase_ranks = self.begin_dq_si(s, ctx, p, &mut dimm.candidate)?;
                    let measured = (|| {
                        self.set_default_rx_vref_ddr4_channels(
                            s, ctx, p.clock, &p.phy, channel, delay,
                        )?;
                        self.dq_time_centering_1d_body(
                            s,
                            ctx,
                            p,
                            channel,
                            MarginParameter::WriteVoltage,
                            10,
                            delay,
                        )?;
                        self.train_dimm_odt_setting(
                            s, ctx, p, channel, ranks, sub, skip, point, opt, delay,
                        )
                    })();
                    match self.finish_dq_si(s, phase_ranks, &dimm.candidate, measured, delay) {
                        Ok(()) => {}
                        Err(Error::TrainingFailed { .. }) => {
                            for widths in &mut point.widths {
                                widths[ch] = 0;
                            }
                            for best in &mut point.best {
                                best[ch] = None;
                            }
                        }
                        // Transport, reset and cleanup failures remain fatal.
                        Err(e) => return Err(e),
                    }
                }
                for ch in 0..2 {
                    if pending & (1 << ch) == 0 {
                        continue;
                    }
                    let mut good = true;
                    for t in 0..g.tests {
                        let margin = if g.tests == 2 {
                            WRITE_TESTS[t]
                        } else if t < 2 {
                            READ_TESTS[t]
                        } else {
                            WRITE_TESTS[t - 2]
                        };
                        let limit = margin_limit(
                            s,
                            MarginKind::try_from(margin as u8)?,
                            if p.mode == TrainingMode::Power {
                                LimitKind::Upm
                            } else {
                                LimitKind::Power
                            },
                            p.clock,
                            p.mode,
                        )?;
                        good &= point.widths[t][ch] >= limit;
                    }
                    if good {
                        pending &= !(1 << ch);
                    }
                }
            }
            for ch in 0..2 {
                if selected & (1 << ch) == 0 {
                    continue;
                }
                opt.rows.fill([0; super::super::opt_param::MAX_OPT_POINTS]);
                let mut limits = [0; 5];
                for t in 0..g.tests {
                    let margin = if g.tests == 2 {
                        WRITE_TESTS[t]
                    } else if t < 2 {
                        READ_TESTS[t]
                    } else {
                        WRITE_TESTS[t - 2]
                    };
                    limits[t] = margin_limit(
                        s,
                        MarginKind::try_from(margin as u8)?,
                        LimitKind::Upm,
                        p.clock,
                        p.mode,
                    )?;
                    for i in 0..usize::from(dimm.tested[ch]) {
                        opt.rows[t][i] = dimm.points[i].widths[t][ch];
                    }
                }
                if !(0..usize::from(dimm.tested[ch]))
                    .any(|i| (0..g.tests).all(|t| opt.rows[t][i] > 0))
                {
                    return Err(no_eye(ch, local[ch].trailing_zeros() as usize, 0));
                }
                let scored = find_optimal_trade_off(
                    &opt.rows,
                    usize::from(dimm.tested[ch]),
                    &TradeoffConfig {
                        scale: if g.tests == 2 {
                            [1, 1, 0, 0, 0]
                        } else {
                            [1, 1, 1, 1, 0]
                        },
                        upm_optimize: g.upm,
                        square: false,
                        average: 1,
                        include_ends: true,
                        center_scale: 1,
                        good_power_limit: limits,
                        optimization_mode: if p.mode == TrainingMode::Power {
                            0
                        } else {
                            100
                        },
                        guard_band: 0,
                        linear: true,
                    },
                    &mut opt.scorer,
                )?;
                dimm.best[ch] = scored.best as u8;
                let point = &dimm.points[scored.best];
                for slot in 0..2 {
                    let set = actual[ch] & g.set_ranks & (3 << (slot * 2));
                    if set != 0 {
                        self.update_opt_point_values(s, p, ch, set, point, skip, delay)?;
                    }
                }
            }
            self.set_default_rx_vref_ddr4_channels(s, ctx, p.clock, &p.phy, selected, delay)?;
            self.dq_time_centering_1d_body(
                s,
                ctx,
                p,
                selected,
                MarginParameter::WriteVoltage,
                10,
                delay,
            )?;
            self.dq_time_centering_1d_body(
                s,
                ctx,
                p,
                selected,
                MarginParameter::ReadVoltage,
                10,
                delay,
            )?;
            if g.recenter_read {
                self.meminit_read_voltage_centering_2d(
                    s,
                    ctx,
                    selected,
                    voltage(15, false),
                    &mut center.measured,
                    delay,
                )?;
                self.meminit_data_time_centering_2d(
                    s,
                    ctx,
                    selected,
                    MarginParameter::ReadTiming,
                    timing(15),
                    center,
                    delay,
                )?;
            }
            if g.recenter_write {
                self.write_voltage_centering_2d_channels(s, ctx, p.clock, selected, center, delay)?;
                self.meminit_data_time_centering_2d(
                    s,
                    ctx,
                    selected,
                    MarginParameter::WriteTiming,
                    timing(15),
                    center,
                    delay,
                )?;
            }
            // Real final measurement at the restored winner and all required ranks.
            self.dq_remeasure(
                s,
                ctx,
                selected,
                ranks,
                if g.tests == 2 {
                    &WRITE_TESTS
                } else {
                    &[READ_TESTS[0], READ_TESTS[1], WRITE_TESTS[0], WRITE_TESTS[1]]
                },
                delay,
            )?;
        }
        Ok(())
    }
    pub fn dimm_odt_training(
        &self,
        s: &mut CalibrationState,
        ctx: &CommandContext<'_>,
        p: DqSiPolicy,
        mask: u8,
        ranks: u8,
        skip: bool,
        tests: &[DimmOdtTest],
        opt: &mut OptWorkspace,
        w: &mut DqSiWorkspace,
        center: &mut WriteCenteringWorkspace,
        dimm: &mut DimmOdtWorkspace,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        let actual = cold_ranks(self, ctx.mode)?;
        if mask == 0
            || mask & !channels(actual) != 0
            || ranks == 0
            || ranks & !15 != 0
            || tests.is_empty()
            || tests.len() > 4
            || !(0..2).any(|ch| mask & (1 << ch) != 0 && actual[ch] & ranks != 0)
            || tests.contains(&DimmOdtTest::ParkSlot1)
                && !(0..2).any(|ch| mask & (1 << ch) != 0 && actual[ch] & 12 != 0)
        {
            return Err(Error::InvalidPolicy);
        }
        let actual = self.begin_dq_si(s, ctx, p, w)?;
        let result = self.dimm_odt_body(
            s, ctx, p, mask, ranks, skip, tests, opt, center, dimm, delay,
        );
        self.finish_dq_si(s, actual, w, result, delay)
    }
    pub fn meminit_dimm_odt_training(
        &self,
        s: &mut CalibrationState,
        ctx: &CommandContext<'_>,
        p: DqSiPolicy,
        opt: &mut OptWorkspace,
        w: &mut DqSiWorkspace,
        center: &mut WriteCenteringWorkspace,
        dimm: &mut DimmOdtWorkspace,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        let ranks = self.begin_dq_si(s, ctx, p, w)?;
        let two = self.dimms.iter().any(|ch| ch.iter().all(Option::is_some));
        let all_single = ranks.iter().all(|&r| r == 0 || r == 1 || r == 4);
        let all_1r1r = ranks.iter().all(|&r| r == 0 || r == 5);
        let power = p.mode == TrainingMode::Power;
        let skip = two || !power;
        let list = if !two {
            [
                if power || all_single {
                    DimmOdtTest::Write
                } else {
                    DimmOdtTest::Combined
                },
                DimmOdtTest::Write,
            ]
        } else if power {
            [DimmOdtTest::NominalNonTarget, DimmOdtTest::Write]
        } else {
            [
                if all_1r1r {
                    DimmOdtTest::Combined1R1R
                } else {
                    DimmOdtTest::Combined
                },
                DimmOdtTest::Write,
            ]
        };
        let length = if two && power { 2 } else { 1 };
        let result = (|| {
            for slot in 0..2 {
                let selected = (ranks[0] | ranks[1]) & (3 << (slot * 2));
                if selected == 0 {
                    continue;
                }
                self.dimm_odt_body(
                    s,
                    ctx,
                    p,
                    channels(ranks),
                    selected,
                    skip,
                    &list[..length],
                    opt,
                    center,
                    dimm,
                    delay,
                )?;
            }
            if two && !all_1r1r {
                self.dimm_odt_body(
                    s,
                    ctx,
                    p,
                    channels(ranks),
                    12,
                    true,
                    &[DimmOdtTest::ParkSlot1],
                    opt,
                    center,
                    dimm,
                    delay,
                )?;
            }
            Ok(())
        })();
        self.finish_dq_si(s, ranks, w, result, delay)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dimm_odt_training_source_1dpc_1r1r_2dpc_slot0_slot1_actual_grid_bounds() {
        for (test, ranks, two, count) in [
            (DimmOdtTest::Write, 3, false, 3),
            (DimmOdtTest::Combined, 3, false, 24),
            (DimmOdtTest::Combined1R1R, 1, true, 24),
            (DimmOdtTest::Combined, 3, true, 50),
            (DimmOdtTest::Combined, 12, true, 24),
            (DimmOdtTest::ParkSlot1, 12, true, 8),
        ] {
            let g = OdtGrid::new(test, ranks, two, false);
            assert_eq!(g.count(), count);
            assert!(count <= MAX_DIMM_OPT_POINTS);
            for i in 0..count {
                let p = g.point(i);
                assert_eq!(p.odt.iter().flatten().count(), g.dimensions);
                for (_, v) in p.odt.iter().flatten() {
                    assert!(*v <= 7);
                }
            }
            if count == 50 {
                assert_eq!(
                    g.point(0).odt,
                    [
                        Some((OptParameter::DimmNominalOdtNonTarget, 6)),
                        Some((OptParameter::DimmWriteOdt, 2)),
                        Some((OptParameter::DimmParkOdt, 6))
                    ]
                );
                assert_eq!(
                    g.point(49).odt,
                    [
                        Some((OptParameter::DimmNominalOdtNonTarget, 7)),
                        Some((OptParameter::DimmWriteOdt, 1)),
                        Some((OptParameter::DimmParkOdt, 7))
                    ]
                );
                assert!(g.upm[1] && g.upm[3]);
                assert!(!OdtGrid::new(test, ranks, two, true).upm.iter().any(|v| *v));
            }
        }
    }
}
