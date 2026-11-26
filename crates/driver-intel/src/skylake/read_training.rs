// SPDX-License-Identifier: MIT OR Apache-2.0
//! DDR4 feedback training translated from native/read_training.c,
//! read_mpr.c and the MPR/read-timing functions in training.c. These are cold
//! pipeline steps, not an orchestrator, trained-memory certificate or restore.

use super::{
    Error, TrainingStep,
    io_test::StopOnError,
    phy_registers::*,
    registers::*,
    reut::{BasicIoMode, DownloadTarget, Engine, RankLane, RegisterFile, io_reset},
    reut_registers::ErrorDetail,
    timings::Timings,
    training::{PassingWindow, channels, cold_ranks},
    training_state::CalibrationState,
};
use crate::sunrise_point::pmc::MemoryBootMode;
use embedded_hal::delay::DelayNs;
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};

const PREAMBLE_MAP: u64 = 0x7654321076543240;

fn failed(step: TrainingStep, ch: usize, rank: usize, lane: usize) -> Error {
    Error::TrainingFailed {
        step,
        channel: ch as u8,
        rank: rank as u8,
        lane: lane as u8,
    }
}

/// Measured largest contiguous timing eye, in PI ticks relative to nominal
/// DQS=32 for MPR and absolute DQS codes for normal reads. Width zero is not
/// a passing eye. Consumers of later 2D/BER training must evaluate it.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct ReadEye {
    pub left: i16,
    pub right: i16,
}
impl ReadEye {
    pub fn width(self) -> u16 {
        (self.right - self.left).max(0) as u16
    }
    pub fn center(self) -> i16 {
        self.left + (self.right - self.left) / 2
    }
}
impl From<PassingWindow> for ReadEye {
    fn from(w: PassingWindow) -> Self {
        w.best
            .map_or(Self::default(), |(left, right)| Self { left, right })
    }
}

impl Engine<'_> {
    fn reset_read_io(&self, delay: &mut impl DelayNs) -> Result<(), Error> {
        io_reset(self.registers, delay, 100_000, 100)
    }

    fn mpr_dqs_sweep(
        &self,
        rank: usize,
        mask: u8,
        delay: &mut impl DelayNs,
    ) -> Result<[[ReadEye; 8]; 2], Error> {
        let r = self.registers;
        let mut ctl0 = [0; 2];
        let mut ctl2 = [[0; 8]; 2];
        let mut windows = [[PassingWindow::EMPTY; 8]; 2];
        for ch in 0..2 {
            if mask & (1 << ch) == 0 {
                continue;
            }
            // Source captures lane zero, not a stale broadcast readback.
            ctl0[ch] = r.data_lane[0][ch].control0.get()
                & !(DataControl0::READ_RF_READ::SET.value
                    | DataControl0::READ_RF_WRITE::SET.value
                    | (DataControl0::READ_RF_RANK.mask << DataControl0::READ_RF_RANK.shift));
            for lane in 0..8 {
                ctl2[ch][lane] = r.data_lane[lane][ch].control2.get();
            }
        }
        let restore = || {
            for ch in 0..2 {
                if mask & (1 << ch) == 0 {
                    continue;
                }
                for lane in 0..8 {
                    r.data_lane[lane][ch].control2.set(ctl2[ch][lane]);
                }
                r.data_channel[ch].control0.set(ctl0[ch]);
            }
        };
        let result: Result<_, Error> = (|| {
            for dqs in -31i16..32 {
                r.data_multicast
                    .offset_train
                    .write(DataOffsetTrain::RX_DQS.val((dqs as u32) & 0x3f));
                for ch in 0..2 {
                    if mask & (1 << ch) == 0 {
                        continue;
                    }
                    self.download(DownloadTarget::Channel(ch), rank, RegisterFile::Read)?;
                    for lane in 0..8 {
                        let ctl = &r.data_lane[lane][ch].control2;
                        ctl.set(ctl2[ch][lane]);
                        ctl.modify(
                            DataControl2::FORCE_BIAS::SET
                                + DataControl2::FORCE_RX::SET
                                + DataControl2::LEAKER_COMP.val(0),
                        );
                    }
                    r.data_channel[ch].control0.set(
                        (ctl0[ch]
                            | DataControl0::RX_TRAIN::SET.value
                            | DataControl0::FORCE_ODT::SET.value)
                            & !DataControl0::READ_PREAMBLE::SET.value,
                    );
                }
                self.reset_read_io(delay)?;
                // Continuous MPR sampling is deliberately NOT a finite run/poll:
                // loop count zero, reset around a 1us pulse, then stop and sample.
                self.start(mask, false)?;
                delay.delay_us(1);
                self.reset_read_io(delay)?;
                delay.delay_us(1);
                self.stop(delay);
                for ch in 0..2 {
                    if mask & (1 << ch) == 0 {
                        continue;
                    }
                    for lane in 0..8 {
                        windows[ch][lane].sample(dqs, 1, r.data_lane[lane][ch].feedback.get() == 1);
                    }
                }
                restore();
                self.reset_read_io(delay)?;
            }
            Ok(())
        })();
        self.stop(delay);
        restore();
        r.data_multicast.offset_train.set(0);
        // Clear the transient offset at the pads even on the last/reset-failed point.
        for ch in 0..2 {
            if mask & (1 << ch) != 0 {
                self.download(DownloadTarget::Channel(ch), rank, RegisterFile::Read)?;
                r.data_channel[ch].control0.set(ctl0[ch]);
            }
        }
        result?;
        Ok(windows.map(|w| w.map(ReadEye::from)))
    }

    fn read_mpr_rank(
        &self,
        state: &mut CalibrationState,
        ranks: [u8; 2],
        rank: usize,
        delay: &mut impl DelayNs,
    ) -> Result<Option<Error>, Error> {
        let r = self.registers;
        let mut mask = 0;
        let mut attempted = 0;
        let mut saved_odt = [0; 2];
        // Disable populated channels lacking this physical rank as well.
        for ch in 0..2 {
            if ranks[ch] == 0 {
                continue;
            }
            self.program_rank_map(ch, 1 << rank)?;
            if ranks[ch] & (1 << rank) != 0 {
                mask |= 1 << ch;
                saved_odt[ch] = r.channel[ch].odt.get();
            }
        }
        let result: Result<_, Error> = (|| {
            for ch in 0..2 {
                if mask & (1 << ch) == 0 {
                    continue;
                }
                attempted |= 1 << ch;
                self.training_mrs(ch, rank, 3, state.saved_mr.rank[ch][rank][3] | 4, delay)?;
                r.channel[ch].odt.modify(OdtControl::MPR_TRAIN::SET);
            }
            self.mpr_dqs_sweep(rank, mask, delay)
        })();
        let mut cleanup = Ok(());
        for ch in 0..2 {
            if attempted & (1 << ch) == 0 {
                continue;
            }
            r.channel[ch].odt.set(saved_odt[ch]);
            let restored = self.training_mrs(ch, rank, 3, state.saved_mr.rank[ch][rank][3], delay);
            if cleanup.is_ok() {
                cleanup = restored;
            }
        }
        let eyes = result?;
        cleanup?;
        let mut failure = None;
        for ch in 0..2 {
            if mask & (1 << ch) == 0 {
                continue;
            }
            for lane in 0..8 {
                let eye = eyes[ch][lane];
                if eye.width() == 0 {
                    failure.get_or_insert(failed(TrainingStep::Mpr, ch, rank, lane));
                    state.margin_results[ch][rank][lane][2] = 0;
                    state.margin_results[ch][rank][lane][3] = 0;
                    // Source nominal cleanup for an invalid point; the zero
                    // margin and explicit failure must never certify it.
                    state.training.ch[ch].rank[rank].rd_dqs_dly[lane] = 32;
                    continue;
                }
                let c = eye.center();
                state.margin_results[ch][rank][lane][2] = ((c - eye.left) * 10) as u32;
                state.margin_results[ch][rank][lane][3] = ((eye.right - c) * 10) as u32;
                state.training.ch[ch].rank[rank].rd_dqs_dly[lane] = (32 + c) as u8;
            }
        }
        Ok(failure)
    }

    // Eye failures are returned separately so the early 2D sweep can evaluate
    // another Vref. Transport/reset failures must never become a bad-eye point.
    fn mpr_once(
        &self,
        state: &mut CalibrationState,
        ranks: [u8; 2],
        delay: &mut impl DelayNs,
    ) -> Result<Option<Error>, Error> {
        let r = self.registers;
        let mask = channels(ranks);
        let mut bank = [[0; 2]; 2];
        let mut sched = [0; 2];
        self.all_mpr_dqs(state, ranks, false)?;
        for ch in 0..2 {
            if ranks[ch] == 0 {
                continue;
            }
            bank[ch] = r.reut_bank_map[ch].each_ref().map(|v| v.get());
            r.reut_bank_map[ch][0].set(PREAMBLE_MAP as u32);
            r.reut_bank_map[ch][1].set((PREAMBLE_MAP >> 32) as u32);
            sched[ch] = r.channel[ch].sched_cbit.get();
            r.channel[ch]
                .sched_cbit
                .modify(SchedulerCbit::DIS_PT_IT::SET);
            for rank in 0..4 {
                for lane in 0..8 {
                    state.margin_results[ch][rank][lane][2] = 0;
                    state.margin_results[ch][rank][lane][3] = 0;
                }
            }
        }
        r.data_multicast.offset_train.set(0);
        let result: Result<_, Error> = (|| {
            self.setup_mpr_test(state, mask, 10, delay)?;
            for ch in 0..2 {
                if ranks[ch] != 0 {
                    // training.c intentionally uses hardware's continuous mode,
                    // unlike the finite nonzero Engine::set_loop_count contract.
                    r.reut_loop_count[ch].set(0);
                }
            }
            let mut failure = None;
            for rank in 0..4 {
                if (ranks[0] | ranks[1]) & (1 << rank) == 0 {
                    continue;
                }
                let rank_failure = self.read_mpr_rank(state, ranks, rank, delay)?;
                if failure.is_none() {
                    failure = rank_failure;
                }
            }
            Ok(failure)
        })();
        self.stop(delay);
        for ch in 0..2 {
            if ranks[ch] == 0 {
                continue;
            }
            r.channel[ch].sched_cbit.set(sched[ch]);
            for i in 0..2 {
                r.reut_bank_map[ch][i].set(bank[ch][i]);
            }
            r.reut_sequence[ch][0].modify(ReutSequence::GLOBAL_CONTROL::CLEAR);
        }
        let apply = self.all_mpr_dqs(state, ranks, true);
        r.data_multicast.offset_train.set(0);
        let reset = self.reset_read_io(delay);
        let failure = result?;
        apply.and(reset)?;
        Ok(failure)
    }

    /// training.c:meminit_read_mpr_training_once and read_mpr.c entry. Requires
    /// JEDEC/pre-training and the receive-enable step at its pipeline position.
    pub fn read_mpr_training_once(
        &self,
        state: &mut CalibrationState,
        mode: MemoryBootMode,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        let ranks = cold_ranks(self, mode)?;
        match self.mpr_once(state, ranks, delay)? {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    /// training.c:meminit_early_read_mpr_2d (DDR4). Uses actual per-rank MPR
    /// eyes at every Vref, selecting the maximum worst-byte/rank width. Standard
    /// profile is step 4; nonstandard is step 1. No failed transport is ignored.
    pub fn early_read_mpr_2d(
        &self,
        state: &mut CalibrationState,
        mode: MemoryBootMode,
        vtt_odt: bool,
        standard_profile: bool,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        let ranks = cold_ranks(self, mode)?;
        let (start, end) = if vtt_odt { (-12i8, 12i8) } else { (-32, 24) };
        let step = if standard_profile { 4 } else { 1 };
        let mut best = [start; 2];
        let mut best_width = [0; 2];
        let mut nonzero = [[0u8; 4]; 2];
        for vref in (i16::from(start)..=i16::from(end)).step_by(step) {
            for ch in 0..2 {
                if ranks[ch] != 0 {
                    self.program_read_vref(ch, vref as i8)?;
                }
            }
            let _eye_failure = self.mpr_once(state, ranks, delay)?;
            for ch in 0..2 {
                if ranks[ch] == 0 {
                    continue;
                }
                let mut min_eye = u32::MAX;
                for rank in 0..4 {
                    if ranks[ch] & (1 << rank) == 0 {
                        continue;
                    }
                    for lane in 0..8 {
                        let eye = state.margin_results[ch][rank][lane][2]
                            + state.margin_results[ch][rank][lane][3];
                        if eye != 0 {
                            nonzero[ch][rank] |= 1 << lane;
                        }
                        min_eye = min_eye.min(eye);
                    }
                }
                if min_eye > best_width[ch] {
                    best_width[ch] = min_eye;
                    best[ch] = vref as i8;
                }
            }
        }
        for ch in 0..2 {
            if ranks[ch] == 0 {
                continue;
            }
            for rank in 0..4 {
                if ranks[ch] & (1 << rank) == 0 {
                    continue;
                }
                for lane in 0..8 {
                    if nonzero[ch][rank] & (1 << lane) == 0 {
                        return Err(failed(TrainingStep::Mpr, ch, rank, lane));
                    }
                }
            }
            // All bytes must have an eye at the SAME Vref point; per-byte
            // nonzero sightings at mutually exclusive points are insufficient.
            if best_width[ch] == 0 {
                return Err(failed(TrainingStep::Mpr, ch, 0, 0));
            }
            self.program_read_vref(ch, best[ch])?;
            for lane in 0..8 {
                state.training.ch[ch].rd_vref[lane] = best[ch];
            }
        }
        match self.mpr_once(state, ranks, delay)? {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
}

/// read_training.c:change_rcven_timing arithmetic. Keep the total receive
/// delay (PI - 64*IOL) invariant while reserving both PI edges. Unlike the C
/// fallback/clamp, impossible constraints fail rather than shorten a delay.
fn synchronized_iol(
    rcven: [u16; 8],
    iol: u8,
    rtl: u8,
    comp: u8,
    reserve: i16,
) -> Option<(u8, [u16; 8])> {
    if !(0..=255).contains(&reserve)
        || iol > 15
        || rtl > 127
        || comp > 127
        || rcven.iter().any(|v| *v > 511)
    {
        return None;
    }
    let totals = rcven.map(|v| v as i16 - 64 * i16::from(iol));
    let min = *totals.iter().min()?;
    let max = *totals.iter().max()?;
    let ideal_pi = 320 - (min + max) / 2;
    let mut ideal = if ideal_pi > 0 {
        (ideal_pi + 32) / 64
    } else {
        (ideal_pi - 32) / 64
    };
    let limit = i16::from(rtl) - 4 - i16::from(comp) - 4;
    ideal = ideal.min(limit);
    if 64 * ideal + min < reserve {
        ideal += (reserve - (64 * ideal + min) + 63) / 64;
    }
    if 64 * ideal + max > 511 - reserve {
        ideal -= (64 * ideal + max - (511 - reserve) + 63) / 64;
    }
    if !(0..=15).contains(&ideal) || ideal > limit {
        return None;
    }
    let adjusted = totals.map(|v| v + 64 * ideal);
    if adjusted
        .iter()
        .any(|v| !(reserve..=511 - reserve).contains(v))
    {
        return None;
    }
    Some((ideal as u8, adjusted.map(|v| v as u16)))
}

fn minimum_read_eye(t: &Timings) -> u16 {
    // Use the source PLL band boundary rather than an inventory data rate.
    if t.clock.tck_fs <= 833_333 { 4 } else { 8 }
}

fn backwards(value: u16, ticks: u16) -> Option<u16> {
    if value >= ticks {
        Some(value - ticks)
    } else if value >= ticks - 28 {
        Some(0)
    } else {
        None
    }
}

impl Engine<'_> {
    fn set_read_latencies(
        &self,
        state: &CalibrationState,
        ch: usize,
        rank: usize,
    ) -> Result<(), Error> {
        let channel = &state.training.ch[ch];
        let rt = &channel.rank[rank];
        if rt.rt_latency > 127 || rt.io_latency > 15 || channel.rt_iocomp > 127 {
            return Err(Error::InvalidTiming);
        }
        let r = &self.registers.channel[ch];
        let rtl = [
            RoundtripLatency::RANK0,
            RoundtripLatency::RANK1,
            RoundtripLatency::RANK2,
            RoundtripLatency::RANK3,
        ][rank];
        let iol = [
            IoLatency::RANK0,
            IoLatency::RANK1,
            IoLatency::RANK2,
            IoLatency::RANK3,
        ][rank];
        r.roundtrip_latency
            .modify(rtl.val(u32::from(rt.rt_latency)));
        // Preserve all other rank fields, program SKL RFR=14 and io_comp.
        r.io_latency.modify(
            iol.val(u32::from(rt.io_latency))
                + IoLatency::IO_COMP.val(u32::from(channel.rt_iocomp))
                + IoLatency::RFR.val(14),
        );
        Ok(())
    }

    fn rre_io_reset(&self, ch: usize, delay: &mut impl DelayNs) -> Result<(), Error> {
        let ctl = &self.registers.data_channel[ch].control0;
        let saved = ctl.get();
        ctl.modify(DataControl0::FORCE_ODT::CLEAR);
        let result = self.reset_read_io(delay);
        ctl.set(saved);
        result
    }

    fn rre_run_io(&self, ch: usize, rank: usize, delay: &mut impl DelayNs) -> Result<u8, Error> {
        if let Err(e) = self.rre_io_reset(ch, delay) {
            self.stop(delay);
            return Err(e);
        }
        // Data errors are expected analog sweep feedback, not transport failure.
        self.run_io_no_reset(1 << ch, delay)?;
        let mut high = 0;
        for lane in 0..8 {
            if self.feedback(RankLane {
                channel: ch,
                rank,
                lane,
            })? >= 32
            {
                high |= 1 << lane;
            }
        }
        Ok(high)
    }

    fn rcven_adjust(
        &self,
        state: &mut CalibrationState,
        ch: usize,
        rank: usize,
        mask: u8,
        delta: i16,
        tolerate_undershoot: bool,
    ) -> Result<(), Error> {
        for lane in 0..8 {
            if mask & (1 << lane) == 0 {
                continue;
            }
            let old = state.training.ch[ch].rank[rank].rcven_dly[lane];
            let new = if delta < 0 && tolerate_undershoot {
                backwards(old, (-delta) as u16).map(i32::from)
            } else {
                Some(i32::from(old) + i32::from(delta))
            };
            let Some(new) = new.filter(|n| (0..=511).contains(n)) else {
                return Err(failed(TrainingStep::ReceiveEnable, ch, rank, lane));
            };
            self.set_rcven_delay(
                state,
                RankLane {
                    channel: ch,
                    rank,
                    lane,
                },
                new as u16,
            )?;
        }
        Ok(())
    }

    fn read_training_rank(
        &self,
        state: &mut CalibrationState,
        ch: usize,
        rank: usize,
        t: &Timings,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        let r = self.registers;
        let comp = state.training.ch[ch].rt_iocomp;
        let iol = if t.clock.tck_fs == 1_875_000 { 3u8 } else { 4 };
        let rtl = u16::from(comp) + u16::from(iol) + 2 * t.cas + 10;
        if rtl > 127 {
            return Err(Error::InvalidTiming);
        }
        state.training.ch[ch].rank[rank].io_latency = iol;
        state.training.ch[ch].rank[rank].rt_latency = rtl as u8;
        self.set_read_latencies(state, ch, rank)?;
        let saved_ctl0 = r.data_channel[ch].control0.get();
        let saved_ctl2 =
            core::array::from_fn::<_, 8, _>(|lane| r.data_lane[lane][ch].control2.get());
        let saved_odt = r.channel[ch].odt.get();
        let mut mr3_attempted = false;
        let mut mr4_attempted = false;
        r.data_channel[ch]
            .control0
            .modify(DataControl0::RL_TRAIN::SET + DataControl0::FORCE_ODT::SET);
        for lane in 0..8 {
            r.data_lane[lane][ch]
                .control2
                .modify(DataControl2::FORCE_RX::SET);
        }
        let result: Result<_, Error> = (|| {
            mr3_attempted = true;
            self.training_mrs(ch, rank, 3, state.saved_mr.rank[ch][rank][3] | 4, delay)?;
            mr4_attempted = true;
            self.training_mrs(
                ch,
                rank,
                4,
                state.saved_mr.rank[ch][rank][4] | (1 << 10),
                delay,
            )?;
            r.channel[ch].odt.modify(OdtControl::MPR_TRAIN::SET);
            // The native FREQ_1867 band's rounded tCK produces 1866 MT/s
            // through integer division. Compare the actual PLL band boundary.
            let start = if t.clock.tck_fs <= 1_071_429 {
                0i16
            } else {
                128
            };
            let mut windows = [PassingWindow::EMPTY; 8];
            for value in (start..start + 192).step_by(8) {
                for lane in 0..8 {
                    self.set_rcven_delay(
                        state,
                        RankLane {
                            channel: ch,
                            rank,
                            lane,
                        },
                        value as u16,
                    )?;
                }
                let high = self.rre_run_io(ch, rank, delay)?;
                for (lane, w) in windows.iter_mut().enumerate() {
                    w.sample(value, 8, high & (1 << lane) != 0);
                }
            }
            for (lane, w) in windows.iter().enumerate() {
                if !(32..=96).contains(&w.width()) {
                    return Err(failed(TrainingStep::ReceiveEnable, ch, rank, lane));
                }
                self.set_rcven_delay(
                    state,
                    RankLane {
                        channel: ch,
                        rank,
                        lane,
                    },
                    w.center().unwrap() as u16,
                )?;
            }
            let mut high = self.rre_run_io(ch, rank, delay)?;
            let mut iterations = 0;
            while high != 0 {
                if iterations == 256 {
                    return Err(failed(
                        TrainingStep::ReceiveEnable,
                        ch,
                        rank,
                        high.trailing_zeros() as usize,
                    ));
                }
                let rt = &state.training.ch[ch].rank[rank];
                let constraint =
                    i16::from(rt.rt_latency) - 4 - i16::from(rt.io_latency) - i16::from(comp);
                if high == 0xff {
                    if constraint - 2 >= 4 {
                        state.training.ch[ch].rank[rank].rt_latency -= 2;
                        self.set_read_latencies(state, ch, rank)?;
                    } else if constraint > 4 {
                        state.training.ch[ch].rank[rank].rt_latency -= 1;
                        self.set_read_latencies(state, ch, rank)?;
                        self.rcven_adjust(state, ch, rank, 0xff, -64, true)?;
                    } else {
                        self.rcven_adjust(state, ch, rank, 0xff, -128, true)?;
                    }
                } else if constraint - 2 >= 4 {
                    let rt = &mut state.training.ch[ch].rank[rank];
                    if rt.io_latency > 13 {
                        return Err(failed(TrainingStep::ReceiveEnable, ch, rank, 0));
                    }
                    rt.io_latency += 2;
                    self.set_read_latencies(state, ch, rank)?;
                    self.rcven_adjust(state, ch, rank, !high, 128, false)?;
                } else if constraint > 4 {
                    let rt = &mut state.training.ch[ch].rank[rank];
                    if rt.io_latency > 14 {
                        return Err(failed(TrainingStep::ReceiveEnable, ch, rank, 0));
                    }
                    rt.io_latency += 1;
                    self.set_read_latencies(state, ch, rank)?;
                    self.rcven_adjust(state, ch, rank, !high, 64, false)?;
                    self.rcven_adjust(state, ch, rank, high, -64, true)?;
                } else {
                    self.rcven_adjust(state, ch, rank, high, -128, true)?;
                }
                high = self.rre_run_io(ch, rank, delay)?;
                iterations += 1;
            }
            let rt = &mut state.training.ch[ch].rank[rank];
            // Combine guardband and the mandatory IOL-1 before writing: the C
            // code can transiently write 16 into a four-bit IOL field (zero).
            let guard = 5u8.min(16 - rt.io_latency);
            rt.rt_latency = rt
                .rt_latency
                .checked_add(guard)
                .filter(|v| *v <= 127)
                .ok_or(failed(TrainingStep::ReceiveEnable, ch, rank, 0))?;
            rt.io_latency = rt.io_latency + guard - 1;
            if rt.rcven_dly[..8].iter().any(|v| *v + 64 > 511) {
                if rt.io_latency == 0 {
                    return Err(failed(TrainingStep::ReceiveEnable, ch, rank, 0));
                }
                rt.io_latency -= 1;
                self.rcven_adjust(state, ch, rank, 0xff, -64, false)?;
            }
            self.set_read_latencies(state, ch, rank)?;
            let mut done = 0u8;
            for _ in 0..64 {
                let high = self.rre_run_io(ch, rank, delay)?;
                done |= high;
                if done == 0xff {
                    break;
                }
                self.rcven_adjust(state, ch, rank, !done, 1, false)?;
            }
            if done != 0xff {
                return Err(failed(
                    TrainingStep::ReceiveEnable,
                    ch,
                    rank,
                    (!done).trailing_zeros() as usize,
                ));
            }
            // Source's final drift subtraction saturates at zero.
            for lane in 0..8 {
                let value = state.training.ch[ch].rank[rank].rcven_dly[lane].saturating_sub(64);
                self.set_rcven_delay(
                    state,
                    RankLane {
                        channel: ch,
                        rank,
                        lane,
                    },
                    value,
                )?;
            }
            Ok(())
        })();
        self.stop(delay);
        r.channel[ch].odt.set(saved_odt);
        let restore4 = if mr4_attempted {
            self.training_mrs(ch, rank, 4, state.saved_mr.rank[ch][rank][4], delay)
        } else {
            Ok(())
        };
        let restore3 = if mr3_attempted {
            self.training_mrs(ch, rank, 3, state.saved_mr.rank[ch][rank][3], delay)
        } else {
            Ok(())
        };
        r.data_channel[ch].control0.set(saved_ctl0);
        for lane in 0..8 {
            r.data_lane[lane][ch].control2.set(saved_ctl2[lane]);
        }
        let reset = self.rre_io_reset(ch, delay);
        result.and(restore4).and(restore3).and(reset)
    }

    /// read_training.c:meminit_read_training. Initial RTL/IOL, coarse feedback,
    /// bounded preamble walkback, guardband, rising-edge search and rank sync.
    /// Called after early MPR, before normal MPR/write leveling. Every selected
    /// physical rank/byte must pass; no missing edge or transport error is ignored.
    pub fn read_training(
        &self,
        state: &mut CalibrationState,
        mode: MemoryBootMode,
        t: &Timings,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        let ranks = cold_ranks(self, mode)?;
        if t.cas == 0 || t.cas > 48 {
            return Err(Error::InvalidTiming);
        }
        let r = self.registers;
        // A single-channel REUT run must not include the previous channel.
        for ch in 0..2 {
            r.reut_sequence[ch][0].modify(ReutSequence::GLOBAL_CONTROL::CLEAR);
        }
        for ch in 0..2 {
            if ranks[ch] == 0 {
                continue;
            }
            state.training.ch[ch].rt_iocomp = 21;
            let bank = r.reut_bank_map[ch].each_ref().map(|v| v.get());
            r.reut_bank_map[ch][0].set(PREAMBLE_MAP as u32);
            r.reut_bank_map[ch][1].set((PREAMBLE_MAP >> 32) as u32);
            let result: Result<_, Error> = (|| {
                self.write_wdb(ch, 0x00ff00ff, 0xffffffff, delay)?;
                self.set_loop_count(ch, 64)?;
                for rank in 0..4 {
                    if ranks[ch] & (1 << rank) == 0 {
                        continue;
                    }
                    self.setup_io(ch, 1 << rank, BasicIoMode::ReadOnly)?;
                    self.set_rcven_address(ch)?;
                    self.set_rcven_read_burst(ch)?;
                    self.read_training_rank(state, ch, rank, t, delay)?;
                }
                Ok(())
            })();
            self.stop(delay);
            for i in 0..2 {
                r.reut_bank_map[ch][i].set(bank[i]);
            }
            r.reut_sequence[ch][0].modify(ReutSequence::GLOBAL_CONTROL::CLEAR);
            result?;
        }
        for ch in 0..2 {
            if ranks[ch] == 0 {
                continue;
            }
            self.change_rcven_timing(state, ch, ranks[ch], 64)?;
        }
        Ok(())
    }

    /// read_training.c:change_rcven_timing; also needed by later RTL/BER steps.
    /// The selected ranks must be real and nonempty; unrelated ranks are kept.
    pub fn change_rcven_timing(
        &self,
        state: &mut CalibrationState,
        ch: usize,
        selected: u8,
        reserve: i16,
    ) -> Result<(), Error> {
        let present = super::reut::rank_mask(self.dimms, ch)?;
        if selected == 0 || selected & !present != 0 {
            return Err(Error::InvalidRank);
        }
        for rank in 0..4 {
            if selected & (1 << rank) == 0 {
                continue;
            }
            let rt = &state.training.ch[ch].rank[rank];
            let values = core::array::from_fn(|lane| rt.rcven_dly[lane]);
            let (iol, values) = synchronized_iol(
                values,
                rt.io_latency,
                rt.rt_latency,
                state.training.ch[ch].rt_iocomp,
                reserve,
            )
            .ok_or(failed(TrainingStep::ReceiveEnable, ch, rank, 0))?;
            for (lane, value) in values.into_iter().enumerate() {
                self.set_rcven_delay(
                    state,
                    RankLane {
                        channel: ch,
                        rank,
                        lane,
                    },
                    value,
                )?;
            }
            state.training.ch[ch].rank[rank].io_latency = iol;
            self.set_read_latencies(state, ch, rank)?;
        }
        Ok(())
    }

    fn normal_read_sweep(
        &self,
        state: &mut CalibrationState,
        ch: usize,
        rank: usize,
        delay: &mut impl DelayNs,
    ) -> Result<[ReadEye; 8], Error> {
        self.program_rank_map(ch, 1 << rank)?;
        let mut windows = [PassingWindow::EMPTY; 8];
        for dqs in 1u8..64 {
            for lane in 0..8 {
                self.set_rx_dqs_delay(
                    state,
                    RankLane {
                        channel: ch,
                        rank,
                        lane,
                    },
                    dqs,
                )?;
            }
            delay.delay_us(1);
            self.run_io(1 << ch, delay)?;
            let errors = self.registers.channel[ch]
                .error_detail
                .read(ErrorDetail::BYTE_GROUPS);
            for (lane, w) in windows.iter_mut().enumerate() {
                w.sample(i16::from(dqs), 1, errors & (1 << lane) == 0);
            }
        }
        Ok(windows.map(ReadEye::from))
    }

    /// Actual normal-traffic DQS sweep for later training.c early-read 2D and
    /// centering_2d/BER consumers. It writes rank-local VA data before each read,
    /// returns all measured eyes (including zero), and does not invent a center.
    /// Write leveling/write centering and WDB initialization must already be
    /// complete. Leaves the last DQS point programmed: callers must apply or
    /// restore a measured center, and must abort on transport/reset failure.
    pub fn read_timing_sweep(
        &self,
        state: &mut CalibrationState,
        mode: MemoryBootMode,
        ch: usize,
        rank: usize,
        delay: &mut impl DelayNs,
    ) -> Result<[ReadEye; 8], Error> {
        let ranks = cold_ranks(self, mode)?;
        if ch >= 2 || rank >= 4 || ranks[ch] & (1 << rank) == 0 {
            return Err(Error::InvalidRank);
        }
        self.setup_basic_va_test(state, 1 << ch, 10, 8, StopOnError::Never, delay)?;
        let result = self.normal_read_sweep(state, ch, rank, delay);
        self.stop(delay);
        self.registers.reut_sequence[ch][0].modify(ReutSequence::GLOBAL_CONTROL::CLEAR);
        result
    }

    /// training.c:meminit_early_read_timing_2d, normal-traffic DDR4 path.
    /// Requires completed write leveling/early write centering. This is not the
    /// later BER/centering_2d.c step: it maximizes the worst rank/byte eye using
    /// actual VA write/read tests, then requires a final eye at the chosen Vref.
    pub fn early_read_timing_2d(
        &self,
        state: &mut CalibrationState,
        mode: MemoryBootMode,
        t: &Timings,
        vtt_odt: bool,
        standard_profile: bool,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        let ranks = cold_ranks(self, mode)?;
        let (start, end, standard_step) = if vtt_odt {
            (-12i8, 12i8, 4)
        } else {
            (-32, 24, 8)
        };
        let step = if standard_profile { standard_step } else { 1 };
        let mut best = [start; 2];
        let mut best_width = [0; 2];
        for vref in (i16::from(start)..=i16::from(end)).step_by(step) {
            for ch in 0..2 {
                if ranks[ch] == 0 {
                    continue;
                }
                self.program_read_vref(ch, vref as i8)?;
                let mut min_eye = u16::MAX;
                for rank in 0..4 {
                    if ranks[ch] & (1 << rank) == 0 {
                        continue;
                    }
                    let eyes = self.read_timing_sweep(state, mode, ch, rank, delay)?;
                    for (lane, eye) in eyes.into_iter().enumerate() {
                        min_eye = min_eye.min(eye.width());
                        // This nominal code is only an intermediate sweep
                        // point, never a passing substitute for a missing eye.
                        let center = if eye.width() == 0 {
                            32
                        } else {
                            eye.center() as u8
                        };
                        self.set_rx_dqs_delay(
                            state,
                            RankLane {
                                channel: ch,
                                rank,
                                lane,
                            },
                            center,
                        )?;
                    }
                }
                if min_eye > best_width[ch] {
                    best_width[ch] = min_eye;
                    best[ch] = vref as i8;
                }
            }
        }
        let min_eye = if standard_profile {
            minimum_read_eye(t)
        } else {
            1
        };
        for ch in 0..2 {
            if ranks[ch] == 0 {
                continue;
            }
            if best_width[ch] == 0 {
                return Err(failed(
                    TrainingStep::ReadTiming,
                    ch,
                    ranks[ch].trailing_zeros() as usize,
                    0,
                ));
            }
            self.program_read_vref(ch, best[ch])?;
            for lane in 0..8 {
                state.training.ch[ch].rd_vref[lane] = best[ch];
            }
            for rank in 0..4 {
                if ranks[ch] & (1 << rank) == 0 {
                    continue;
                }
                let eyes = self.read_timing_sweep(state, mode, ch, rank, delay)?;
                for (lane, eye) in eyes.into_iter().enumerate() {
                    if eye.width() < min_eye {
                        return Err(failed(TrainingStep::ReadTiming, ch, rank, lane));
                    }
                    self.set_rx_dqs_delay(
                        state,
                        RankLane {
                            channel: ch,
                            rank,
                            lane,
                        },
                        eye.center() as u8,
                    )?;
                }
            }
        }
        self.reset_read_io(delay)
    }

    /// training.c:meminit_read_timing_centering_1d. Requires completed write
    /// training and the later early-read-2D step at its proper pipeline position.
    pub fn read_timing_centering_1d(
        &self,
        state: &mut CalibrationState,
        mode: MemoryBootMode,
        t: &Timings,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        let ranks = cold_ranks(self, mode)?;
        let min_eye = minimum_read_eye(t);
        self.registers.data_multicast.rx_per_bit[0].set(0x88888888);
        for ch in 0..2 {
            if ranks[ch] == 0 {
                continue;
            }
            // Source resets the rank-0 multicast per-bit regfile; download it
            // even when a physical rank hole means rank 0 is not installed.
            self.registers.data_channel[ch].control0.modify(
                DataControl0::READ_RF_READ::SET
                    + DataControl0::READ_RF_WRITE::CLEAR
                    + DataControl0::READ_RF_RANK.val(0),
            );
        }
        for ch in 0..2 {
            if ranks[ch] == 0 {
                continue;
            }
            for rank in 0..4 {
                if ranks[ch] & (1 << rank) == 0 {
                    continue;
                }
                let eyes = self.read_timing_sweep(state, mode, ch, rank, delay)?;
                for (lane, eye) in eyes.into_iter().enumerate() {
                    if eye.width() < min_eye {
                        return Err(failed(TrainingStep::ReadTiming, ch, rank, lane));
                    }
                    self.set_rx_dqs_delay(
                        state,
                        RankLane {
                            channel: ch,
                            rank,
                            lane,
                        },
                        eye.center() as u8,
                    )?;
                }
            }
        }
        self.reset_read_io(delay)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_training_change_rcven_timing_preserves_total_delay_and_constraints() {
        let values = [128, 144, 160, 176, 192, 208, 224, 240];
        let (iol, new) = synchronized_iol(values, 4, 55, 21, 64).unwrap();
        for lane in 0..8 {
            assert_eq!(
                i32::from(new[lane]) - 64 * i32::from(iol),
                i32::from(values[lane]) - 256
            );
        }
        assert!(new.iter().all(|v| (64..=447).contains(v)));
        assert!(synchronized_iol([512; 8], 0, 30, 21, 64).is_none());
        assert!(synchronized_iol([0, 511, 0, 0, 0, 0, 0, 0], 4, 55, 21, 64).is_none());
        assert!(synchronized_iol(values, 16, 55, 21, 64).is_none());
        assert_eq!(backwards(36, 64), Some(0));
        assert_eq!(backwards(35, 64), None);
        assert_eq!(backwards(100, 128), Some(0));
        assert_eq!(backwards(99, 128), None);
    }
}
