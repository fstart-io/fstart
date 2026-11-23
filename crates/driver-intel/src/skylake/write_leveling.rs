// SPDX-License-Identifier: MIT OR Apache-2.0
//! JEDEC DQS feedback, whole-cycle write/read cleanup and post-processing
//! translated from native/write_leveling.c. `write_leveling_cleanup` includes
//! the mandatory saved-MR/PDA JEDEC reset before early write centering. The
//! lower-level phases alone are not sufficient; this is not usable DRAM.

use super::{
    Error, TrainingStep,
    phy_registers::*,
    registers::*,
    reut::{DownloadTarget, Engine, RankLane, RegisterFile, io_reset},
    reut_registers::{ErrorDetail, WdbMux},
    timings::Timings,
    training::{channels, cold_ranks},
    training_state::CalibrationState,
    vref_training::park_rtt_wr,
};
use crate::sunrise_point::pmc::MemoryBootMode;
use embedded_hal::delay::DelayNs;
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};

fn failure(step: TrainingStep, ch: usize, rank: usize, lane: usize) -> Error {
    Error::TrainingFailed {
        step,
        channel: ch as u8,
        rank: rank as u8,
        lane: lane as u8,
    }
}
fn cycle_pi(cycle: i16) -> i16 {
    if cycle < 0 {
        cycle * 128
    } else {
        (cycle - 2).max(0) * 128
    }
}
fn reserves(t: &Timings, standard: bool) -> (i16, i16) {
    if !standard {
        return (0, 0);
    }
    (
        if t.clock.tck_fs >= 833_333 { 64 } else { 96 },
        if t.clock.tck_fs >= 937_500 { 32 } else { 16 },
    )
}
fn target_dqs(base: u16, first: i16, target: i16) -> i16 {
    base as i16 + cycle_pi(target) + 128 * (first - target)
}

/// Circular 128-PI JEDEC sweep window. Joins only the runs touching both
/// sweep boundaries, never disjoint interior windows. First widest run wins.
#[derive(Clone, Copy)]
struct LevelWindow {
    current: Option<i16>,
    first_end: Option<i16>,
    end: i16,
    best: (i16, i16),
}
impl LevelWindow {
    const EMPTY: Self = Self {
        current: None,
        first_end: None,
        end: -1,
        best: (0, 0),
    };
    fn sample(&mut self, d: i16, pass: bool) {
        if pass {
            if self.current.is_none() {
                self.current = Some(d);
            }
            self.end = d;
            if d == 128 || self.first_end == Some(d - 2) {
                self.first_end = Some(d);
            }
        } else if let Some(start) = self.current.take()
            && self.end - start > self.best.1 - self.best.0
        {
            self.best = (start, self.end);
        }
    }
    fn finish(mut self) -> Option<u16> {
        if let Some(start) = self.current {
            let mut end = self.end;
            if end == 254
                && let Some(first) = self.first_end
            {
                end += 2 + first - 128;
            }
            if end - start > self.best.1 - self.best.0 {
                self.best = (start, end);
            }
        }
        if self.best.1 == 0
            && let Some(first) = self.first_end
        {
            self.best = (128, first);
        }
        let width = self.best.1 - self.best.0;
        if width <= 32 || width >= 96 {
            None
        } else {
            Some((self.best.0 + 64) as u16)
        }
    }
}

impl Engine<'_> {
    /// BER_PARAM_WRT subset used by WL cleanup and write timing. Preserve
    /// unrelated offset fields and download the physical write register file.
    pub(crate) fn write_timing_offset(
        &self,
        ch: usize,
        rank: usize,
        offset: i16,
    ) -> Result<(), Error> {
        if !(-32..=31).contains(&offset) {
            return Err(Error::InvalidPolicy);
        }
        for lane in 0..8 {
            self.registers.data_lane[lane][ch]
                .offset_train
                .modify(DataOffsetTrain::TX_DQ.val((offset as u32) & 0x3f));
            self.download(
                DownloadTarget::Lane { channel: ch, lane },
                rank,
                RegisterFile::Write,
            )?;
        }
        Ok(())
    }

    fn wl_channel(
        &self,
        state: &mut CalibrationState,
        ch: usize,
        rank: usize,
        t: &Timings,
        mask: u8,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        let r = self.registers;
        let misc = r.ddr_misc.get();
        let dc0 = r.data_channel[ch].control0.get();
        let dc2 = core::array::from_fn::<_, 8, _>(|lane| r.data_lane[lane][ch].control2.get());
        let high = t.clock.tck_fs < 1_500_000;
        let wait_us = ((u64::from(t.clock.tck_fs) / 2000) * if high { 2048 } else { 1024 })
            .div_ceil(1_000_000) as u32;
        let mut windows = [LevelWindow::EMPTY; 8];
        let result = (|| {
            r.ddr_misc.modify(DdrMisc::DATA_CLOCK_GATE_DISABLE::SET);
            for other in 0..4 {
                if other == rank || mask & (1 << other) == 0 {
                    continue;
                }
                self.training_mrs(ch, other, 5, state.saved_mr.rank[ch][other][5], delay)?;
                self.training_mrs(
                    ch,
                    other,
                    1,
                    state.saved_mr.rank[ch][other][1] | (1 << 12),
                    delay,
                )?;
            }
            self.training_mrs(ch, rank, 5, park_rtt_wr(state, ch, rank), delay)?;
            self.training_mrs(
                ch,
                rank,
                1,
                (state.saved_mr.rank[ch][rank][1] & !(1 << 12)) | (1 << 7),
                delay,
            )?;
            for lane in 0..8 {
                let ctl = &r.data_lane[lane][ch].control2;
                ctl.modify(DataControl2::FORCE_BIAS::SET + DataControl2::FORCE_RX::CLEAR);
                if high {
                    ctl.modify(DataControl2::WL_LONG_DELAY::SET);
                }
            }
            let wl_dc0 = (dc0 & !((1 << 20) | (3 << 22)))
                | DataControl0::WL_TRAIN::SET.value
                | DataControl0::TX_PI_ON::SET.value
                | DataControl0::READ_RF_WRITE::SET.value
                | ((rank as u32) << 22);
            for d in (128u16..256).step_by(2) {
                for lane in 0..8 {
                    self.set_tx_delay(
                        RankLane {
                            channel: ch,
                            rank,
                            lane,
                        },
                        d,
                    )?;
                }
                r.data_channel[ch].control0.set(wl_dc0);
                if d == 128 {
                    delay.delay_us(wait_us);
                }
                io_reset(r, delay, 1000, 1)?;
                delay.delay_us(wait_us);
                for (lane, w) in windows.iter_mut().enumerate() {
                    w.sample(
                        d as i16,
                        self.feedback(RankLane {
                            channel: ch,
                            rank,
                            lane,
                        })? >= 16,
                    );
                }
            }
            let mut values = [0u16; 8];
            for lane in 0..8 {
                values[lane] = windows[lane].finish().ok_or(failure(
                    TrainingStep::WriteLeveling,
                    ch,
                    rank,
                    lane,
                ))?;
            }
            if self.dimms[ch][rank / 2]
                .as_ref()
                .ok_or(Error::InvalidRank)?
                .device_width
                == 16
            {
                for lane in (1..8).step_by(2) {
                    let mut v = values[lane] as i16;
                    let reference = values[lane - 1] as i16;
                    if v - reference > 64 {
                        v -= 128;
                    } else if reference - v > 64 {
                        v += 128;
                    }
                    if !(0..=479).contains(&v) {
                        return Err(failure(TrainingStep::WriteLeveling, ch, rank, lane));
                    }
                    values[lane] = v as u16;
                }
            }
            for (lane, &dqs) in values.iter().enumerate() {
                let p = RankLane {
                    channel: ch,
                    rank,
                    lane,
                };
                self.set_tx_delay(p, dqs)?;
                self.set_tx_dq_byte_delay(p, dqs + 32)?;
                state.training.ch[ch].rank[rank].wl_dly[lane] = dqs;
                state.training.ch[ch].rank[rank].tx_dq_dly[lane] = dqs + 32;
            }
            Ok(())
        })();
        // Restore all ranks even if an earlier command/eye/reset failed.
        let mut restore = Ok(());
        for other in 0..4 {
            if mask & (1 << other) == 0 {
                continue;
            }
            let m1 = self.training_mrs(ch, other, 1, state.saved_mr.rank[ch][other][1], delay);
            let m5 = self.training_mrs(ch, other, 5, state.saved_mr.rank[ch][other][5], delay);
            restore = restore.and(m1).and(m5);
        }
        r.data_channel[ch].control0.set(dc0);
        let reset = io_reset(r, delay, 1000, 1);
        r.ddr_misc.set(misc);
        for lane in 0..8 {
            r.data_lane[lane][ch].control2.set(dc2[lane]);
        }
        result.and(restore).and(reset)
    }

    /// write_leveling.c:wl_channel for every populated physical rank. This
    /// phase alone does NOT resolve the 128-PI alias: cleanup is mandatory.
    pub fn write_leveling_feedback(
        &self,
        state: &mut CalibrationState,
        mode: MemoryBootMode,
        t: &Timings,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        let ranks = cold_ranks(self, mode)?;
        if t.clock.tck_fs == 0 {
            return Err(Error::InvalidTiming);
        }
        for ch in 0..2 {
            for rank in 0..4 {
                if ranks[ch] & (1 << rank) != 0 {
                    self.wl_channel(state, ch, rank, t, ranks[ch], delay)?;
                }
            }
        }
        Ok(())
    }

    fn wl_set_cycle(
        &self,
        ch: usize,
        rank: usize,
        cycle: i16,
        base: &[u16; 8],
    ) -> Result<(), Error> {
        let pi = cycle_pi(cycle);
        for (lane, &v) in base.iter().enumerate() {
            if !(0..=479).contains(&(v as i16 + pi)) {
                return Err(failure(TrainingStep::WriteLevelingCleanup, ch, rank, lane));
            }
        }
        let reg = &self.registers.channel[ch].write_delay;
        let shift = rank * 2;
        reg.set((reg.get() & !(3 << shift)) | ((cycle.clamp(0, 2) as u32) << shift));
        for (lane, &v) in base.iter().enumerate() {
            let p = RankLane {
                channel: ch,
                rank,
                lane,
            };
            let dqs = (v as i16 + pi) as u16;
            self.set_tx_delay(p, dqs)?;
            self.set_tx_dq_byte_delay(p, dqs + 32)?;
        }
        Ok(())
    }

    fn wl_cleanup_rank(
        &self,
        state: &mut CalibrationState,
        ch: usize,
        rank: usize,
        t: &Timings,
        standard: bool,
        delay: &mut impl DelayNs,
    ) -> Result<bool, Error> {
        let r = &self.registers.channel[ch];
        let base =
            core::array::from_fn::<_, 8, _>(|lane| state.training.ch[ch].rank[rank].wl_dly[lane]);
        if base.iter().any(|&v| v > 479) {
            return Err(Error::InvalidTiming);
        }
        let start = -(*base.iter().min().unwrap() as i16) / 128;
        let end = 2 + (479 - *base.iter().max().unwrap() as i16) / 128;
        let wdb = r.wdb_control.get();
        let mut pattern = [[0x00, 0xff], [0xff, 0x00], [0xc3, 0x3c], [0x3c, 0xc3]];
        let mut first = [0i16; 8];
        let mut pass = 0u8;
        self.program_rank_map(ch, 1 << rank)?;
        let result = (|| {
            for cycle in start..=end {
                if pass == 0xff {
                    break;
                }
                self.wl_set_cycle(ch, rank, cycle, &base)?;
                io_reset(self.registers, delay, 1000, 1)?;
                self.training_mrs(ch, rank, 0, state.saved_mr.rank[ch][rank][0], delay)?;
                delay.delay_us(1);
                for offset in [-15, -10, -5, 0, 5, 10, 15] {
                    if pass == 0xff {
                        break;
                    }
                    self.write_timing_offset(ch, rank, offset)?;
                    for (entry, p) in pattern.iter_mut().enumerate() {
                        self.write_wdb_spread(
                            ch,
                            p,
                            &[0, 0, 1, 1, 1, 1, 0, 0],
                            entry as u8,
                            delay,
                        )?;
                        p[0] = !p[0];
                        p[1] = !p[1];
                    }
                    r.wdb_control.set(wdb);
                    r.wdb_mux.write(
                        WdbMux::MUX0.val(1)
                            + WdbMux::MUX1.val(1)
                            + WdbMux::MUX2.val(1)
                            + WdbMux::ECC_SOURCE::SET,
                    );
                    self.run_io(1 << ch, delay)?;
                    let good = !(r.error_detail.read(ErrorDetail::BYTE_GROUPS) as u8);
                    for lane in 0..8 {
                        if good & (1 << lane) != 0 && pass & (1 << lane) == 0 {
                            first[lane] = cycle;
                            pass |= 1 << lane;
                        }
                    }
                }
            }
            Ok(())
        })();
        let clear = self.write_timing_offset(ch, rank, 0);
        result.and(clear)?;
        if pass != 0xff {
            if !r.write_delay.is_set(WriteDelay::DEC_WRD) {
                return Err(failure(
                    TrainingStep::WriteLevelingCleanup,
                    ch,
                    rank,
                    (!pass).trailing_zeros() as usize,
                ));
            }
            r.write_delay.modify(WriteDelay::DEC_WRD::CLEAR);
            return Ok(true);
        }
        let sum: i16 = first.iter().sum();
        let mut target = if sum > 0 {
            (sum + 4) / 8
        } else {
            (sum - 4) / 8
        };
        let (left, right) = reserves(t, standard);
        let mut pi_delta = 0;
        for _ in 0..2 {
            let mut delta = 0;
            let mut overflow = false;
            for lane in 0..8 {
                let dqs = target_dqs(base[lane], first[lane], target);
                if dqs + 32 > 511 - right {
                    delta = delta.max(dqs + 32 - (511 - right));
                    overflow = true;
                }
                if dqs < left {
                    delta = delta.max(left - dqs);
                    overflow = false;
                }
            }
            if delta == 0 {
                break;
            }
            if pi_delta == 0 {
                pi_delta = delta;
            } else if pi_delta > delta {
                break;
            }
            target += if overflow { 1 } else { -1 };
        }
        let mut final_dqs = [0u16; 8];
        for lane in 0..8 {
            let v = target_dqs(base[lane], first[lane], target);
            if !(0..=479).contains(&v) {
                return Err(failure(TrainingStep::WriteLevelingCleanup, ch, rank, lane));
            }
            final_dqs[lane] = v as u16;
        }
        // Do not write the source's transient out-of-range base+cycle PI:
        // only the final per-byte values are representable and meaningful.
        let shift = rank * 2;
        r.write_delay
            .set((r.write_delay.get() & !(3 << shift)) | ((target.clamp(0, 2) as u32) << shift));
        for (lane, &dqs) in final_dqs.iter().enumerate() {
            let p = RankLane {
                channel: ch,
                rank,
                lane,
            };
            self.set_tx_delay(p, dqs)?;
            self.set_tx_dq_byte_delay(p, dqs + 32)?;
            state.training.ch[ch].rank[rank].wl_dly[lane] = dqs;
            state.training.ch[ch].rank[rank].tx_dq_dly[lane] = dqs + 32;
        }
        self.training_mrs(ch, rank, 0, state.saved_mr.rank[ch][rank][0], delay)?;
        delay.delay_us(1);
        Ok(false)
    }

    fn wl_cleanup_min_code(
        &self,
        state: &mut CalibrationState,
        ch: usize,
        rank: usize,
        t: &Timings,
        standard: bool,
    ) -> Result<(), Error> {
        let rt = &mut state.training.ch[ch].rank[rank];
        let reg = &self.registers.channel[ch].write_delay;
        let shift = rank * 2;
        let add = (reg.get() >> shift) & 3;
        let (_, right) = reserves(t, standard);
        if add == 0
            || rt.wl_dly[..8].iter().all(|&v| v + 32 >= 128)
            || rt.wl_dly[..8]
                .iter()
                .any(|&v| i32::from(v) + 160 > 511 - i32::from(right))
        {
            return Ok(());
        }
        reg.set((reg.get() & !(3 << shift)) | ((add - 1) << shift));
        for lane in 0..8 {
            rt.wl_dly[lane] += 128;
            rt.tx_dq_dly[lane] = rt.wl_dly[lane] + 32;
            let p = RankLane {
                channel: ch,
                rank,
                lane,
            };
            self.set_tx_delay(p, rt.wl_dly[lane])?;
            self.set_tx_dq_byte_delay(p, rt.tx_dq_dly[lane])?;
        }
        Ok(())
    }

    /// Whole-cycle cleanup followed by the mandatory saved-MR/PDA JEDEC reset.
    /// Unlike first initialization, this preserves measured calibration state.
    #[allow(clippy::too_many_arguments)]
    pub fn write_leveling_cleanup(
        &self,
        state: &mut CalibrationState,
        pmc: &crate::sunrise_point::pmc::PmcRegisters,
        mode: MemoryBootMode,
        t: &Timings,
        standard_profile: bool,
        shared_clock: bool,
        sdram_lanes: usize,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        if !(8..=9).contains(&sdram_lanes) {
            return Err(Error::InvalidPolicy);
        }
        self.write_leveling_cleanup_before_jedec_reinit(
            state,
            mode,
            t,
            standard_profile,
            shared_clock,
            delay,
        )?;
        self.reinitialize_cold(state, pmc, mode, sdram_lanes, delay)
    }

    /// write_leveling.c:whole-cycle cleanup and post-processing, following
    /// successful write_leveling_feedback. The caller MUST then invoke the
    /// saved-MR/PDA `reinitialize_cold` with caller-owned PMC
    /// and lane count, before early_write_timing_2d. No reset substitute is performed.
    pub fn write_leveling_cleanup_before_jedec_reinit(
        &self,
        state: &mut CalibrationState,
        mode: MemoryBootMode,
        t: &Timings,
        standard_profile: bool,
        shared_clock: bool,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        let ranks = cold_ranks(self, mode)?;
        for ch in 0..2 {
            if ranks[ch] == 0 {
                continue;
            }
            let r = &self.registers.channel[ch];
            let wrwr = r.tc_wrwr.get();
            let rdrd = r.tc_rdrd.get();
            let mux = r.wdb_mux.get();
            let wdb = r.wdb_control.get();
            let result = (|| {
                self.setup_wl_cleanup_test(state, 1 << ch, delay)?;
                r.tc_wrwr.set((wrwr & !0x3f3f) | 30 | (30 << 8));
                r.tc_rdrd.set(widen_rdrd(rdrd));
                // DEC_WRD can only clear once, bounding the all-rank rerun.
                for attempt in 0..2 {
                    let mut rerun = false;
                    for rank in 0..4 {
                        if ranks[ch] & (1 << rank) == 0 {
                            continue;
                        }
                        if self.wl_cleanup_rank(state, ch, rank, t, standard_profile, delay)? {
                            rerun = true;
                            break;
                        }
                    }
                    if !rerun {
                        break;
                    }
                    if attempt == 1 {
                        return Err(failure(TrainingStep::WriteLevelingCleanup, ch, 0, 0));
                    }
                }
                for rank in 0..4 {
                    if ranks[ch] & (1 << rank) != 0 {
                        self.wl_cleanup_min_code(state, ch, rank, t, standard_profile)?;
                    }
                }
                Ok(())
            })();
            self.stop(delay);
            self.registers.reut_sequence[ch][0].modify(ReutSequence::GLOBAL_CONTROL::CLEAR);
            r.tc_wrwr.set(wrwr);
            r.tc_rdrd.set(rdrd);
            r.wdb_mux.set(mux);
            r.wdb_control.set(wdb);
            for rank in 0..4 {
                if ranks[ch] & (1 << rank) != 0 {
                    self.write_timing_offset(ch, rank, 0)?;
                }
            }
            result?;
        }
        self.setup_wdb(channels(ranks))?;
        self.wl_post_processing(state, ranks, t, standard_profile, shared_clock)?;
        self.registers.data_multicast.offset_train.set(0);
        Ok(())
    }

    fn wl_post_processing(
        &self,
        state: &mut CalibrationState,
        ranks: [u8; 2],
        t: &Timings,
        standard: bool,
        shared: bool,
    ) -> Result<(), Error> {
        let (left_res, right_res) = reserves(t, standard);
        for ch in 0..2 {
            let mask = ranks[ch];
            if mask == 0 {
                continue;
            }
            let r = &self.registers.channel[ch];
            let mut add_reg = r.write_delay.get();
            let add = core::array::from_fn::<_, 4, _>(|rank| ((add_reg >> (2 * rank)) & 3) as i16);
            let mut shifts = [0i16; 4];
            let run_dd = mask & 3 != 0 && mask & 12 != 0;
            let run_dr = mask & 3 == 3 || mask & 12 == 12;
            let mut stretch_dd = 0u32;
            let mut stretch_dr = 0u32;
            if run_dd && t.clock.tck_fs <= 750_000 {
                let sum = (0..4)
                    .filter(|&r| mask & (1 << r) != 0)
                    .map(|r| add[r])
                    .sum::<i16>();
                let n = mask.count_ones() as i16;
                let avg = (sum + n / 2) / n;
                for rank in 0..4 {
                    if mask & (1 << rank) != 0 {
                        shifts[rank] = avg - add[rank];
                    }
                }
            } else {
                for rank in 0..4 {
                    if mask & (1 << rank) == 0 {
                        continue;
                    }
                    if run_dd && rank < 2 {
                        for other in 2..4 {
                            if mask & (1 << other) != 0 {
                                stretch_dd =
                                    stretch_dd.max((add[rank] - add[other]).unsigned_abs().into());
                            }
                        }
                    }
                    if !run_dr
                        || rank % 2 != 0
                        || mask & (1 << (rank + 1)) == 0
                        || add[rank] == add[rank + 1]
                    {
                        continue;
                    }
                    let low = if add[rank] < add[rank + 1] {
                        rank
                    } else {
                        rank + 1
                    };
                    let high = if low == rank { rank + 1 } else { rank };
                    let diff = add[high] - add[low];
                    let mut underflow = false;
                    let mut overflow = false;
                    for lane in 0..8 {
                        let lo = &self.registers.data_lane[lane][ch].tx_train[low];
                        let hi = &self.registers.data_lane[lane][ch].tx_train[high];
                        if (lo.read(TxTrain::DQS) as i16) - 128 * diff < left_res {
                            underflow = true;
                        } else if hi.read(TxTrain::DQ) as i16 + 128 * diff + right_res > 511 {
                            overflow = true;
                        }
                    }
                    if !overflow {
                        shifts[high] = -diff;
                    } else if !underflow {
                        shifts[low] = diff;
                    } else {
                        stretch_dr = stretch_dr.max(diff as u32);
                    }
                }
            }
            for rank in 0..4 {
                if mask & (1 << rank) == 0 || shifts[rank] == 0 {
                    continue;
                }
                let new_add = add[rank] + shifts[rank];
                if !(0..=3).contains(&new_add) {
                    return Err(Error::InvalidTiming);
                }
                for lane in 0..8 {
                    let tx = &self.registers.data_lane[lane][ch].tx_train[rank];
                    let dq = tx.read(TxTrain::DQ) as i16 - 128 * shifts[rank];
                    let dqs = tx.read(TxTrain::DQS) as i16 - 128 * shifts[rank];
                    if !(0..=511).contains(&dq) || !(0..=511).contains(&dqs) {
                        return Err(failure(TrainingStep::WriteLevelingCleanup, ch, rank, lane));
                    }
                }
                add_reg = (add_reg & !(3 << (2 * rank))) | ((new_add as u32) << (2 * rank));
                r.write_delay.set(add_reg);
                for lane in 0..8 {
                    let tx = &self.registers.data_lane[lane][ch].tx_train[rank];
                    let dq = (tx.read(TxTrain::DQ) as i16 - 128 * shifts[rank]) as u16;
                    let dqs = (tx.read(TxTrain::DQS) as i16 - 128 * shifts[rank]) as u16;
                    self.set_tx_delay(
                        RankLane {
                            channel: ch,
                            rank,
                            lane,
                        },
                        dqs,
                    )?;
                    self.set_tx_dq_byte_delay(
                        RankLane {
                            channel: ch,
                            rank,
                            lane,
                        },
                        dq,
                    )?;
                    state.training.ch[ch].rank[rank].wl_dly[lane] = dqs;
                    state.training.ch[ch].rank[rank].tx_dq_dly[lane] = dq;
                }
            }
            let mut sum_shift = 0i16;
            for rank in 0..4 {
                if mask & (1 << rank) == 0 {
                    continue;
                }
                let selected = if shared { mask } else { 1 << rank };
                let (min_pi, max_pi) = self.wl_pi_limits(ch, selected);
                let left = min_pi;
                let right = 511 - max_pi;
                if left >= left_res && right >= right_res {
                    continue;
                }
                let shift = if left < left_res && right >= right_res {
                    (left_res - left).min(right - right_res)
                } else if left >= left_res {
                    -(left - left_res).min(right_res - right)
                } else {
                    ((left_res - left) - (right_res - right)) / 2
                };
                // Unlike the C rank-zero special case, sparse shared-clock
                // populations shift exactly once at the first physical rank.
                if !shared || rank == mask.trailing_zeros() as usize {
                    self.shift_clock_and_data(state, ch, selected, shift)?;
                }
                let ctl = &self.registers.control_channel[ch].rank_pi;
                let value = (((ctl.get() >> (rank * 7)) & 127) as i16 + shift).clamp(0, 127) as u32;
                ctl.set((ctl.get() & !(127 << (rank * 7))) | (value << (rank * 7)));
                let cke = &self.registers.cke_channel[ch].control_pi;
                let mut value_cke = cke.get() & !(127 << 21);
                if rank < 3 {
                    value_cke = (value_cke & !(127 << (rank * 7))) | (value << (rank * 7));
                }
                cke.set(value_cke);
                sum_shift += shift;
            }
            if sum_shift != 0 {
                let shift = sum_shift / mask.count_ones() as i16;
                for cmd in [
                    &self.registers.command_south[ch].cmd_pi,
                    &self.registers.command_north[ch].cmd_pi,
                ] {
                    let value = ((cmd.get() & 127) as i16 + shift).clamp(0, 127) as u32;
                    cmd.set(value | (value << 7));
                    if core::ptr::eq(cmd, &self.registers.command_south[ch].cmd_pi) {
                        self.registers.cke_channel[ch]
                            .cmd_pi
                            .set(value | (value << 7));
                    }
                }
            }
            let mut rank_diff = 0;
            let mut dimm_diff = 0;
            for rank in 0..4 {
                if mask & (1 << rank) == 0 {
                    continue;
                }
                if run_dd && rank < 2 {
                    for other in 2..4 {
                        if mask & (1 << other) != 0 {
                            dimm_diff = dimm_diff.max(self.wl_rank_difference(ch, rank, other));
                        }
                    }
                }
                if run_dr && rank % 2 == 0 && mask & (1 << (rank + 1)) != 0 {
                    rank_diff = rank_diff.max(self.wl_rank_difference(ch, rank, rank + 1));
                }
            }
            stretch_dr += u32::from(rank_diff > 64);
            stretch_dd += u32::from(dimm_diff > 64);
            for tc in [&r.tc_wrrd, &r.tc_wrwr] {
                let dr = tc.read(Turnaround::DR) + stretch_dr;
                let dd = tc.read(Turnaround::DD) + stretch_dd;
                if dr > 63 || dd > 63 {
                    return Err(Error::InvalidTiming);
                }
                tc.modify(Turnaround::DR.val(dr) + Turnaround::DD.val(dd));
            }
        }
        Ok(())
    }

    fn wl_pi_limits(&self, ch: usize, mask: u8) -> (i16, i16) {
        let mut min = 511;
        let mut max = 0;
        for rank in 0..4 {
            if mask & (1 << rank) != 0 {
                for lane in 0..8 {
                    let l = &self.registers.data_lane[lane][ch];
                    for v in [
                        l.tx_train[rank].read(TxTrain::DQ),
                        l.tx_train[rank].read(TxTrain::DQS),
                        l.rx_train[rank].read(RxTrain::RCVEN),
                    ] {
                        min = min.min(v as i16);
                        max = max.max(v as i16);
                    }
                }
            }
        }
        (min, max)
    }
    fn wl_rank_difference(&self, ch: usize, a: usize, b: usize) -> u32 {
        let mut diff = 0;
        for lane in 0..8 {
            let l = &self.registers.data_lane[lane][ch];
            diff = diff.max(
                l.tx_train[a]
                    .read(TxTrain::DQ)
                    .abs_diff(l.tx_train[b].read(TxTrain::DQ)),
            );
            diff = diff.max(
                l.tx_train[a]
                    .read(TxTrain::DQS)
                    .abs_diff(l.tx_train[b].read(TxTrain::DQS)),
            );
        }
        diff
    }

    /// Required highspeed.c:shift_pi_for_cmd_training(CLOCK)/shift_dq_pis
    /// subset. Live per-rank images are the source host snapshot. Reject
    /// saturation rather than silently breaking the CLK/DQ/RCVEN relationship.
    fn shift_clock_and_data(
        &self,
        state: &mut CalibrationState,
        ch: usize,
        mask: u8,
        shift: i16,
    ) -> Result<(), Error> {
        for rank in 0..4 {
            if mask & (1 << rank) != 0 {
                for lane in 0..8 {
                    let l = &self.registers.data_lane[lane][ch];
                    for v in [
                        l.tx_train[rank].read(TxTrain::DQ),
                        l.tx_train[rank].read(TxTrain::DQS),
                        l.rx_train[rank].read(RxTrain::RCVEN),
                    ] {
                        if !(0..=511).contains(&(v as i16 + shift)) {
                            return Err(failure(
                                TrainingStep::WriteLevelingCleanup,
                                ch,
                                rank,
                                lane,
                            ));
                        }
                    }
                }
            }
        }
        let clk = &self.registers.clock_channel[ch].rank_pi;
        let mut value = clk.get();
        for rank in 0..4 {
            if mask & (1 << rank) == 0 {
                continue;
            }
            let code = ((((value >> (rank * 7)) & 127) as i16 + shift).rem_euclid(128)) as u32;
            value = (value & !(127 << (rank * 7))) | (code << (rank * 7));
            for lane in 0..8 {
                let l = &self.registers.data_lane[lane][ch];
                let p = RankLane {
                    channel: ch,
                    rank,
                    lane,
                };
                let dq = (l.tx_train[rank].read(TxTrain::DQ) as i16 + shift) as u16;
                let dqs = (l.tx_train[rank].read(TxTrain::DQS) as i16 + shift) as u16;
                let rcven = (l.rx_train[rank].read(RxTrain::RCVEN) as i16 + shift) as u16;
                self.set_tx_delay(p, dqs)?;
                self.set_tx_dq_byte_delay(p, dq)?;
                self.set_rcven_delay(state, p, rcven)?;
                state.training.ch[ch].rank[rank].wl_dly[lane] = dqs;
                state.training.ch[ch].rank[rank].tx_dq_dly[lane] = dq;
            }
        }
        clk.set(value);
        Ok(())
    }
}

pub(crate) fn widen_rdrd(reg: u32) -> u32 {
    (reg & !0x3f3f) | ((reg & 63) + 8).min(63) | ((((reg >> 8) & 63) + 8).min(63) << 8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wl_feedback_circular_windows_reject_thresholds_all_high_all_low_and_keep_first_tie() {
        let sweep = |pass: fn(i16) -> bool| {
            let mut w = LevelWindow::EMPTY;
            for d in (128..256).step_by(2) {
                w.sample(d, pass(d));
            }
            w.finish()
        };
        assert_eq!(sweep(|d| (140..=188).contains(&d)), Some(204));
        assert_eq!(sweep(|d| d <= 150 || d >= 230), Some(294));
        assert_eq!(sweep(|d| (140..=172).contains(&d)), None); // width ==32
        assert_eq!(sweep(|d| (140..=236).contains(&d)), None); // width ==96
        assert_eq!(sweep(|_| true), None);
        assert_eq!(sweep(|_| false), None);
        assert_eq!(
            sweep(|d| (130..=164).contains(&d) || (200..=234).contains(&d)),
            Some(194)
        );
        assert_eq!(cycle_pi(-1), -128);
        assert_eq!(cycle_pi(0), 0);
        assert_eq!(cycle_pi(2), 0);
        assert_eq!(cycle_pi(3), 128);
        assert_eq!(target_dqs(204, 0, 1), 76);
        assert_eq!(target_dqs(204, 1, 1), 204);
    }
}
