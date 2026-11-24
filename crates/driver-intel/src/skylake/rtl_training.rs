// SPDX-License-Identifier: MIT OR Apache-2.0
//! Native si_optimization.c:660–1448, 2038–2607, DDR4 cold RTL.
//! Real REUT/BER feedback and clock/data guard phases; not usable-memory proof.
use super::{
    Error,
    ber_margin::{BerSweep, MarginParameter},
    command_training::CommandContext,
    opt_tradeoff::effective_margin,
    registers::{IoLatency, Turnaround},
    reut::Engine,
    training::channels,
    training_state::CalibrationState,
    turnaround_training::{LatencyWorkspace, merge, no_eye},
};
use embedded_hal::delay::DelayNs;
use tock_registers::interfaces::{Readable, Writeable};

const MIN_MARGIN: u16 = 20;
const READ_TIMING_RESULT: usize = MarginParameter::ReadTiming.result_type() * 2;

fn rtl_margin_passes(score: u16, baseline: u16) -> bool {
    u32::from(score.max(MIN_MARGIN)) >= u32::from(baseline) * 90 / 100
}

fn rtl_margin_error(
    phase: &str,
    s: &CalibrationState,
    ch: usize,
    rank: usize,
    lane: usize,
    score: u16,
    baseline: u16,
) -> Error {
    let timing = &s.training.ch[ch].rank[rank];
    let eye = &s.margin_results[ch][rank][lane];
    fstart_log::error!(
        "RTL {}: CH{} R{} byte{} RTL={} IOL={} edges={}/{} score={} baseline={}",
        phase,
        ch,
        rank,
        lane,
        timing.rt_latency,
        timing.io_latency,
        eye[READ_TIMING_RESULT],
        eye[READ_TIMING_RESULT + 1],
        score,
        baseline
    );
    no_eye(ch, rank, lane)
}

impl Engine<'_> {
    /// Native rtl_update_ta_param_offset. Validate the COMPLETE rank update
    /// before changing either host or hardware; signed IOL never wraps/clips.
    fn rtl_set(
        &self,
        s: &mut CalibrationState,
        ch: usize,
        ranks: u8,
        rtl: i16,
    ) -> Result<(), Error> {
        if !(0..=127).contains(&rtl) || ranks == 0 || ranks & !15 != 0 {
            return Err(Error::InvalidTiming);
        }
        for r in 0..4 {
            if ranks & (1 << r) != 0 {
                let t = &s.training.ch[ch].rank[r];
                let io = i16::from(t.io_latency) + rtl - i16::from(t.rt_latency);
                if !(0..=15).contains(&io) {
                    return Err(Error::InvalidTiming);
                }
            }
        }
        for r in 0..4 {
            if ranks & (1 << r) != 0 {
                let t = &mut s.training.ch[ch].rank[r];
                t.io_latency = (i16::from(t.io_latency) + rtl - i16::from(t.rt_latency)) as u8;
                t.rt_latency = rtl as u8;
            }
        }
        let t = &s.training.ch[ch];
        let mut rt = 0;
        let mut io =
            IoLatency::IO_COMP.val(u32::from(t.rt_iocomp)).value | IoLatency::RFR.val(14).value;
        for r in 0..4 {
            rt |= u32::from(t.rank[r].rt_latency.min(127)) << (r * 8);
            io |= u32::from(t.rank[r].io_latency.min(15)) << (r * 4);
        }
        self.registers.channel[ch].io_latency.set(io);
        self.registers.channel[ch].roundtrip_latency.set(rt);
        Ok(())
    }
    fn rtl_measure_phase(
        &self,
        s: &mut CalibrationState,
        ctx: &CommandContext<'_>,
        masks: [u8; 2],
        w: &mut LatencyWorkspace,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        let selected = channels(masks);
        let rank = (masks[0] | masks[1]).trailing_zeros() as usize;
        self.latency_select(masks, selected, None)?;
        let measured = (|| {
            self.latency_shift_channel(s, ctx, masks, w, 25, delay)?;
            self.get_ber_margin_byte_physical(
                s,
                ctx,
                BerSweep {
                    parameter: MarginParameter::ReadTiming,
                    channels: selected,
                    rank_in: rank as u8,
                    // Test the selected rank, but bound the PI sweep by all
                    // populated ranks' read DQS centers, as native RTL does.
                    rank_rx: 0xff,
                    per_channel: false,
                    interpolate: true,
                    max_margin: 31,
                    interior: false,
                    byte_map: None,
                },
                &mut [0; 4],
                delay,
            )?;
            for ch in 0..2 {
                if masks[ch] == 0 {
                    continue;
                }
                for b in 0..8 {
                    let eye = &s.margin_results[ch][rank][b];
                    // A one-sided eye still contributes to the native score.
                    let edge = |i| u16::try_from(eye[i]).map_err(|_| Error::InvalidTiming);
                    w.measured[ch][b] =
                        effective_margin(edge(READ_TIMING_RESULT)?, edge(READ_TIMING_RESULT + 1)?)?;
                }
            }
            Ok(())
        })();
        let cleanup = self.latency_unshift(s, ctx, masks, w, delay);
        merge(measured, cleanup)
    }
    fn rtl_cliff_search(
        &self,
        s: &mut CalibrationState,
        ctx: &CommandContext<'_>,
        ranks: [u8; 2],
        rank: usize,
        start: i16,
        stop: i16,
        w: &mut LatencyWorkspace,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        let masks = core::array::from_fn(|ch| ranks[ch] & (1 << rank));
        let selected = channels(masks);
        if selected == 0 || start < 0 || stop > 127 || start >= stop {
            return Err(Error::InvalidTiming);
        }
        self.latency_setup(s, selected, None, 10, delay)?;
        w.baseline = [[1280; 8]; 2];
        w.last = [[stop; 8]; 2];
        w.passed = [0; 2];
        // Native initial point plus four repeats, then reverse the sweep and
        // search upward from start for each byte's first 90%-baseline pass.
        for _ in 0..5 {
            self.rtl_measure_phase(s, ctx, masks, w, delay)?;
            for ch in 0..2 {
                if masks[ch] == 0 {
                    continue;
                }
                for b in 0..8 {
                    let m = w.measured[ch][b];
                    if m <= MIN_MARGIN {
                        return Err(rtl_margin_error("baseline", s, ch, rank, b, m, 0));
                    }
                    w.baseline[ch][b] = w.baseline[ch][b].min(m);
                }
            }
        }
        for off in start..stop {
            // The global source range can exceed a channel's representable
            // IOL interval. Never truncate it or treat it as a measured pass.
            let mut candidate = masks;
            for ch in 0..2 {
                if masks[ch] == 0 {
                    continue;
                }
                let original = w.host_latency[ch][rank];
                if !(0..=15).contains(&(i16::from(original[1]) + off - i16::from(original[0]))) {
                    candidate[ch] = 0;
                    continue;
                }
                self.rtl_set(s, ch, masks[ch], off)?;
            }
            if channels(candidate) == 0 {
                continue;
            }
            self.rtl_measure_phase(s, ctx, candidate, w, delay)?;
            for ch in 0..2 {
                if candidate[ch] == 0 {
                    continue;
                }
                for b in 0..8 {
                    if w.passed[ch] & (1 << b) == 0
                        && rtl_margin_passes(w.measured[ch][b], w.baseline[ch][b])
                    {
                        w.last[ch][b] = off;
                        w.passed[ch] |= 1 << b;
                    }
                }
            }
            if (0..2).all(|ch| masks[ch] == 0 || w.passed[ch] == 255) {
                break;
            }
        }
        for ch in 0..2 {
            if masks[ch] == 0 {
                continue;
            }
            // Native leaves the conservative endpoint as the choice for
            // any byte that never passed the sweep. If that shared endpoint
            // cannot be represented on this channel, use its tested baseline.
            let mut winner = *w.last[ch].iter().max().ok_or(Error::InvalidPolicy)?;
            if w.passed[ch] != 255 {
                let [rtl, iol] = w.host_latency[ch][rank];
                if !(0..=15).contains(&(i16::from(iol) + winner - i16::from(rtl))) {
                    winner = i16::from(rtl);
                }
                fstart_log::debug!("RTL: CH{} R{} using conservative RTL {}", ch, rank, winner);
            }
            self.rtl_set(s, ch, masks[ch], winner)?;
            fstart_log::debug!("RTL: CH{} R{} selected RTL {}", ch, rank, winner);
        }
        // Native TrainDDROptParamCliff programs the selection and returns; it
        // does not measure the chosen point again.
        Ok(())
    }
    /// Native meminit_round_trip_latency. Ranks remain physical (including
    /// channel1/slot1 ranks2/3). All selected bytes constrain each channel's RTL.
    /// Final normalization is reprogrammed; like native, it is not measured again.
    pub fn meminit_round_trip_latency(
        &self,
        s: &mut CalibrationState,
        ctx: &CommandContext<'_>,
        w: &mut LatencyWorkspace,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        let ranks = self.latency_preflight(s, ctx)?;
        // Preflight every rank's actual source fields and ReadRead runner
        // headroom before the first destructive pattern/controller write.
        for ch in 0..2 {
            if ranks[ch] == 0 {
                continue;
            }
            if s.training.ch[ch].rt_iocomp > 127
                || (s.mchbar_tc_rdrd[ch] & 63) == 63
                || ((s.mchbar_tc_rdrd[ch] >> 8) & 63) == 63
            {
                return Err(Error::InvalidTiming);
            }
            for r in 0..4 {
                if ranks[ch] & (1 << r) != 0 {
                    let t = &s.training.ch[ch].rank[r];
                    if t.rt_latency > 127 || t.io_latency == 0 || t.io_latency > 15 {
                        return Err(Error::InvalidTiming);
                    }
                }
            }
        }
        // Plan EVERY physical rank before consuming the write-only WDB.
        let mut ranges = [None; 4];
        for (rank, range) in ranges.iter_mut().enumerate() {
            let mut stop = 0i16;
            let mut min_io = 15i16;
            let mut found = false;
            for ch in 0..2 {
                if ranks[ch] & (1 << rank) == 0 {
                    continue;
                }
                found = true;
                let t = &s.training.ch[ch].rank[rank];
                stop = stop.max(i16::from(t.rt_latency));
                min_io = min_io.min(i16::from(t.io_latency));
            }
            if found {
                let start = (stop - min_io).max(0);
                if stop <= start {
                    return Err(Error::InvalidTiming);
                }
                *range = Some((start, stop));
            }
        }
        self.latency_begin(s, w)?;
        let measured = (|| {
            self.setup_wdb(channels(ranks))?;
            s.en_dummy_reads = 0;
            self.latency_stress(ranks);
            for (rank, range) in ranges.into_iter().enumerate() {
                if let Some((start, stop)) = range {
                    self.rtl_cliff_search(s, ctx, ranks, rank, start, stop, w, delay)?;
                }
            }
            // Source delta bound uses actual live tRDRD, not a host shadow.
            for ch in 0..2 {
                if ranks[ch] == 0 {
                    continue;
                }
                let reg = self.registers.channel[ch].tc_rdrd.extract();
                let limit = (2 * (reg.read(Turnaround::DR).min(reg.read(Turnaround::DD)) as i16)
                    - 8)
                .max(0);
                let max = (0..4)
                    .filter(|r| ranks[ch] & (1 << r) != 0)
                    .map(|r| s.training.ch[ch].rank[r].rt_latency)
                    .max()
                    .ok_or(Error::InvalidRank)?;
                let mut restore = false;
                for r in 0..4 {
                    if ranks[ch] & (1 << r) == 0 {
                        continue;
                    }
                    if i16::from(max) - i16::from(s.training.ch[ch].rank[r].rt_latency) > limit
                        && self.rtl_set(s, ch, 1 << r, i16::from(max) - limit).is_err()
                    {
                        restore = true;
                        break;
                    }
                }
                if restore {
                    // Source fallback restores every rank of this channel to
                    // its original host values.
                    for r in 0..4 {
                        s.training.ch[ch].rank[r].rt_latency = w.host_latency[ch][r][0];
                        s.training.ch[ch].rank[r].io_latency = w.host_latency[ch][r][1];
                    }
                    let r = ranks[ch].trailing_zeros() as usize;
                    self.rtl_set(s, ch, 1 << r, i16::from(w.host_latency[ch][r][0]))?;
                }
            }
            Ok(())
        })();
        self.latency_finish(s, ctx, ranks, w, measured, delay)
    }
}
