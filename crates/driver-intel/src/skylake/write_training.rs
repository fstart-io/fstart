// SPDX-License-Identifier: MIT OR Apache-2.0
//! DDR4 write timing and voltage feedback from native/write_training.c and
//! training.c. Caller must complete WL cleanup AND full JEDEC reinitialization
//! first. These operations never activate or publish memory.

use super::{
    Error, TrainingStep,
    io_test::StopOnError,
    jedec::vref_code,
    read_training::ReadEye,
    registers::ReutSequence,
    reut::{Engine, RankLane, io_reset},
    reut_registers::ErrorDetail,
    timings::Timings,
    training::{PassingWindow, channels, cold_ranks},
    training_state::CalibrationState,
    vref_training::vref_bounds,
    write_leveling::widen_rdrd,
};
use crate::sunrise_point::pmc::MemoryBootMode;
use embedded_hal::delay::DelayNs;
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};

fn failed(step: TrainingStep, ch: usize, rank: usize, lane: usize) -> Error {
    Error::TrainingFailed {
        step,
        channel: ch as u8,
        rank: rank as u8,
        lane: lane as u8,
    }
}
fn minimum_eye(t: &Timings) -> i16 {
    if t.clock.tck_fs <= 833_333 { 4 } else { 8 }
}

impl Engine<'_> {
    fn quiesce_write_training(&self, delay: &mut impl DelayNs) {
        self.stop(delay);
        for ch in 0..2 {
            self.registers.reut_sequence[ch][0].modify(ReutSequence::GLOBAL_CONTROL::CLEAR);
        }
    }

    /// training.c:byte_wr_time_center_1d. Requires configured VA traffic and
    /// physical rank selection. Returns real eyes, including zero-width eyes
    /// at rejected analog points; no nominal center counts as a passing eye.
    fn byte_wr_time_sweep(
        &self,
        rank: usize,
        mask: u8,
        base: [[u16; 8]; 2],
        delay: &mut impl DelayNs,
    ) -> Result<[[ReadEye; 8]; 2], Error> {
        let mut windows = [[PassingWindow::EMPTY; 8]; 2];
        for ch in 0..2 {
            if mask & (1 << ch) != 0 {
                self.registers.channel[ch].error_counter_control[0].set(0);
            }
        }
        for offset in -31i16..=31 {
            for ch in 0..2 {
                if mask & (1 << ch) != 0 {
                    for lane in 0..8 {
                        let value = (base[ch][lane] as i16 + offset).clamp(0, 511) as u16;
                        self.set_tx_dq_byte_delay(
                            RankLane {
                                channel: ch,
                                rank,
                                lane,
                            },
                            value,
                        )?;
                    }
                }
            }
            delay.delay_us(1);
            self.run_io(mask, delay)?;
            for ch in 0..2 {
                if mask & (1 << ch) != 0 {
                    let errors = self.registers.channel[ch]
                        .error_detail
                        .read(ErrorDetail::BYTE_GROUPS);
                    for lane in 0..8 {
                        // Clipped endpoints are not distinct passing timing points.
                        let in_range = (0..=511).contains(&(base[ch][lane] as i16 + offset));
                        windows[ch][lane].sample(offset, 1, in_range && errors & (1 << lane) == 0);
                    }
                }
            }
        }
        Ok(windows.map(|lanes| lanes.map(ReadEye::from)))
    }

    fn apply_write_eyes(
        &self,
        state: &mut CalibrationState,
        rank: usize,
        mask: u8,
        base: [[u16; 8]; 2],
        eyes: [[ReadEye; 8]; 2],
        minimum: i16,
    ) -> Result<(), Error> {
        // Validate every selected byte before publishing any measured center.
        for ch in 0..2 {
            if mask & (1 << ch) != 0 {
                for lane in 0..8 {
                    let eye = eyes[ch][lane];
                    let value = base[ch][lane] as i16 + eye.center();
                    if eye.width() < minimum.max(1) as u16 || !(0..=511).contains(&value) {
                        return Err(failed(TrainingStep::WriteTiming, ch, rank, lane));
                    }
                }
            }
        }
        for ch in 0..2 {
            if mask & (1 << ch) != 0 {
                for lane in 0..8 {
                    let eye = eyes[ch][lane];
                    let value = (base[ch][lane] as i16 + eye.center()) as u16;
                    self.set_tx_dq_byte_delay(
                        RankLane {
                            channel: ch,
                            rank,
                            lane,
                        },
                        value,
                    )?;
                    state.training.ch[ch].rank[rank].tx_dq_dly[lane] = value;
                    state.margin_results[ch][rank][lane][6] =
                        ((eye.center() - eye.left) * 10) as u32;
                    state.margin_results[ch][rank][lane][7] =
                        ((eye.right - eye.center()) * 10) as u32;
                }
            }
        }
        Ok(())
    }

    /// write_training.c:write_training_ch / meminit_write_training. Real
    /// write/read VA traffic, 63 TX DQ offsets, per-bit reset to 8, largest
    /// contiguous byte eye and frequency-dependent minimum width.
    pub fn write_training(
        &self,
        state: &mut CalibrationState,
        mode: MemoryBootMode,
        t: &Timings,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        let ranks = cold_ranks(self, mode)?;
        for ch in 0..2 {
            for rank in 0..4 {
                if ranks[ch] & (1 << rank) == 0 {
                    continue;
                }
                let r = &self.registers.channel[ch];
                let rdrd = r.tc_rdrd.get();
                let result = (|| {
                    self.setup_wdb(1 << ch)?;
                    self.setup_basic_va_test(state, 1 << ch, 10, 8, StopOnError::Never, delay)?;
                    self.program_rank_map(ch, 1 << rank)?;
                    r.error_counter_control[0].set(0);
                    let mut base = [0u16; 8];
                    for lane in 0..8 {
                        let wl = state.training.ch[ch].rank[rank].wl_dly[lane];
                        if wl > 511 {
                            return Err(Error::InvalidTiming);
                        }
                        base[lane] = (wl + 32).min(480);
                        for bit in lane * 8..lane * 8 + 8 {
                            self.set_tx_dq_delay(ch, bit, rank, 8)?;
                            state.training.ch[ch].rank[rank].wr_dq_dly[bit] = 8;
                        }
                        self.set_tx_dq_byte_delay(
                            RankLane {
                                channel: ch,
                                rank,
                                lane,
                            },
                            base[lane],
                        )?;
                        state.margin_results[ch][rank][lane][6..8].fill(0);
                    }
                    r.tc_rdrd.set(widen_rdrd(rdrd));
                    let mut windows = [PassingWindow::EMPTY; 8];
                    let sweep = (|| {
                        for d in -31i16..=31 {
                            self.write_timing_offset(ch, rank, d)?;
                            self.run_io(1 << ch, delay)?;
                            let errors = r.error_detail.read(ErrorDetail::BYTE_GROUPS);
                            for (lane, window) in windows.iter_mut().enumerate() {
                                window.sample(d, 1, errors & (1 << lane) == 0);
                            }
                        }
                        Ok(())
                    })();
                    let clear = self.write_timing_offset(ch, rank, 0);
                    sweep.and(clear)?;
                    let mut all_base = [[0; 8]; 2];
                    all_base[ch] = base;
                    let mut all_eyes = [[ReadEye::default(); 8]; 2];
                    all_eyes[ch] = windows.map(ReadEye::from);
                    self.apply_write_eyes(state, rank, 1 << ch, all_base, all_eyes, minimum_eye(t))
                })();
                self.quiesce_write_training(delay);
                r.tc_rdrd.set(rdrd);
                result?;
            }
        }
        Ok(())
    }

    /// training.c:early_write_timing_centering_2d / meminit_early_write_2d.
    /// Per physical DIMM, maximize the worst rank/byte eye independently for
    /// each channel over MR6 offsets -12..12, then remeasure the winners.
    pub fn early_write_timing_2d(
        &self,
        state: &mut CalibrationState,
        mode: MemoryBootMode,
        standard_profile: bool,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        let ranks = cold_ranks(self, mode)?;
        for ch in 0..2 {
            if ranks[ch] != 0 {
                vref_bounds(state.ddr4_tx_vref_range[ch])?;
            }
        }
        if state.ddr4_pda_enable {
            return Err(Error::InvalidPolicy);
        }
        let saved_mr6 = core::array::from_fn::<_, 2, _>(|ch| {
            core::array::from_fn::<_, 4, _>(|rank| state.saved_mr.rank[ch][rank][6])
        });
        let rdrd = core::array::from_fn::<_, 2, _>(|ch| self.registers.channel[ch].tc_rdrd.get());
        for ch in 0..2 {
            if ranks[ch] != 0 {
                self.registers.channel[ch].tc_rdrd.set(widen_rdrd(rdrd[ch]));
            }
        }
        let result = (|| {
            self.setup_wdb(channels(ranks))?;
            self.setup_basic_va_test(state, channels(ranks), 10, 8, StopOnError::Never, delay)?;
            for dimm in 0..2 {
                let dimm_mask = 3 << (dimm * 2);
                let local = ranks.map(|v| v & dimm_mask);
                if local == [0; 2] {
                    continue;
                }
                let mut best = [-12i16; 2];
                let mut best_width = [0u16; 2];
                for offset in (-12i16..=12).step_by(if standard_profile { 3 } else { 1 }) {
                    for ch in 0..2 {
                        for rank in dimm * 2..dimm * 2 + 2 {
                            if local[ch] & (1 << rank) != 0 {
                                self.write_vref_offset(state, ch, rank, offset, delay)?;
                            }
                        }
                    }
                    delay.delay_us(1);
                    let mut worst = [u16::MAX; 2];
                    for rank in dimm * 2..dimm * 2 + 2 {
                        let (mask, base) = self.select_write_rank(state, local, rank)?;
                        if mask == 0 {
                            continue;
                        }
                        let eyes = self.byte_wr_time_sweep(rank, mask, base, delay)?;
                        for ch in 0..2 {
                            if mask & (1 << ch) != 0 {
                                for eye in eyes[ch] {
                                    worst[ch] = worst[ch].min(eye.width());
                                }
                            }
                        }
                    }
                    for ch in 0..2 {
                        if local[ch] != 0 && worst[ch] != u16::MAX && worst[ch] > best_width[ch] {
                            best_width[ch] = worst[ch];
                            best[ch] = offset;
                        }
                    }
                }
                for ch in 0..2 {
                    if local[ch] == 0 {
                        continue;
                    }
                    if best_width[ch] == 0 {
                        return Err(failed(
                            TrainingStep::WriteTiming,
                            ch,
                            local[ch].trailing_zeros() as usize,
                            0,
                        ));
                    }
                    for rank in dimm * 2..dimm * 2 + 2 {
                        if local[ch] & (1 << rank) != 0 {
                            self.write_vref_offset(state, ch, rank, best[ch], delay)?;
                        }
                    }
                }
                // Final independent remeasurement is mandatory; an eye seen
                // at some other Vref cannot certify the selected final point.
                for rank in dimm * 2..dimm * 2 + 2 {
                    let (mask, base) = self.select_write_rank(state, local, rank)?;
                    if mask == 0 {
                        continue;
                    }
                    let eyes = self.byte_wr_time_sweep(rank, mask, base, delay)?;
                    self.apply_write_eyes(state, rank, mask, base, eyes, 1)?;
                }
                io_reset(self.registers, delay, 100_000, 100)?;
                for ch in 0..2 {
                    for rank in dimm * 2..dimm * 2 + 2 {
                        if local[ch] & (1 << rank) != 0 {
                            let code =
                                vref_code(i32::from(best[ch]), state.ddr4_tx_vref_range[ch])?;
                            state.saved_mr.rank[ch][rank][6] = (saved_mr6[ch][rank] & !0xff) | code;
                        }
                    }
                }
            }
            Ok(())
        })();
        self.quiesce_write_training(delay);
        let mut cleanup = Ok(());
        for ch in 0..2 {
            if ranks[ch] == 0 {
                continue;
            }
            self.registers.channel[ch].tc_rdrd.set(rdrd[ch]);
            if result.is_err() {
                for rank in 0..4 {
                    if ranks[ch] & (1 << rank) != 0 {
                        let restore = self.mrs_mr6_vref(ch, rank, saved_mr6[ch][rank], delay);
                        cleanup = cleanup.and(restore);
                        state.saved_mr.rank[ch][rank][6] = saved_mr6[ch][rank];
                        for lane in 0..8 {
                            state.margin_results[ch][rank][lane][6..8].fill(0);
                        }
                    }
                }
            }
        }
        result.and(cleanup)
    }

    fn select_write_rank(
        &self,
        state: &CalibrationState,
        ranks: [u8; 2],
        rank: usize,
    ) -> Result<(u8, [[u16; 8]; 2]), Error> {
        let mut mask = 0;
        let mut base = [[0; 8]; 2];
        for ch in 0..2 {
            let present = ranks[ch] & (1 << rank) != 0;
            self.program_rank_map(ch, if present { 1 << rank } else { 0 })?;
            if !present {
                continue;
            }
            mask |= 1 << ch;
            for lane in 0..8 {
                let wl = state.training.ch[ch].rank[rank].wl_dly[lane];
                if wl > 511 {
                    return Err(Error::InvalidTiming);
                }
                base[ch][lane] = (wl + 32).min(511);
            }
        }
        Ok((mask, base))
    }

    /// training.c:meminit_write_voltage_1d. Sweep actual normal traffic at
    /// fixed trained timing, then program measured per-byte PDA centers.
    /// Missing/narrow eyes and every transport/PDA failure are fatal (the C
    /// nominal fallback and success-after-timeout behavior are not retained).
    pub fn write_voltage_1d(
        &self,
        state: &mut CalibrationState,
        mode: MemoryBootMode,
        t: &Timings,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        let ranks = cold_ranks(self, mode)?;
        for ch in 0..2 {
            if ranks[ch] != 0 {
                vref_bounds(state.ddr4_tx_vref_range[ch])?;
            }
        }
        if state.ddr4_pda_enable {
            return Err(Error::InvalidPolicy);
        }
        let saved_mr6 = core::array::from_fn::<_, 2, _>(|ch| {
            core::array::from_fn::<_, 4, _>(|rank| state.saved_mr.rank[ch][rank][6])
        });
        let saved_pda = state.ddr4_pda_mr6;
        let rdrd = core::array::from_fn::<_, 2, _>(|ch| self.registers.channel[ch].tc_rdrd.get());
        for ch in 0..2 {
            if ranks[ch] != 0 {
                self.registers.channel[ch].tc_rdrd.set(widen_rdrd(rdrd[ch]));
            }
        }
        let result = (|| {
            for rank in 0..4 {
                let mask = (0..2).fold(0, |v, ch| {
                    v | if ranks[ch] & (1 << rank) != 0 {
                        1 << ch
                    } else {
                        0
                    }
                });
                if mask == 0 {
                    continue;
                }
                self.setup_wdb(mask)?;
                self.setup_basic_va_test(state, mask, 10, 8, StopOnError::Never, delay)?;
                let mut windows = [[PassingWindow::EMPTY; 8]; 2];
                for ch in 0..2 {
                    if mask & (1 << ch) != 0 {
                        self.program_rank_map(ch, 1 << rank)?;
                        self.registers.channel[ch].error_counter_control[0].set(0);
                        self.write_vref_offset(state, ch, rank, 0, delay)?;
                        for lane in 0..8 {
                            state.margin_results[ch][rank][lane][4..6].fill(0);
                        }
                    }
                }
                for offset in -36i16..=36 {
                    for ch in 0..2 {
                        if mask & (1 << ch) != 0 {
                            self.write_vref_offset(state, ch, rank, offset, delay)?;
                        }
                    }
                    delay.delay_us(1);
                    self.run_io(mask, delay)?;
                    for ch in 0..2 {
                        if mask & (1 << ch) != 0 {
                            let errors = self.registers.channel[ch]
                                .error_detail
                                .read(ErrorDetail::BYTE_GROUPS);
                            let (min, max) = vref_bounds(state.ddr4_tx_vref_range[ch])?;
                            // Range-constrained repeated endpoint commands do not
                            // constitute distinct voltage margin measurements.
                            for (lane, w) in windows[ch].iter_mut().enumerate() {
                                w.sample(
                                    offset,
                                    1,
                                    (min..=max).contains(&offset) && errors & (1 << lane) == 0,
                                );
                            }
                        }
                    }
                }
                for ch in 0..2 {
                    if mask & (1 << ch) != 0 {
                        self.write_vref_offset(state, ch, rank, 0, delay)?;
                    }
                }
                let mut centers = [[0u8; 8]; 2];
                for ch in 0..2 {
                    if mask & (1 << ch) != 0 {
                        for lane in 0..8 {
                            let w = windows[ch][lane];
                            if w.width() < minimum_eye(t) {
                                return Err(failed(TrainingStep::WriteVoltage, ch, rank, lane));
                            }
                            centers[ch][lane] = vref_code(
                                i32::from(w.center().ok_or(failed(
                                    TrainingStep::WriteVoltage,
                                    ch,
                                    rank,
                                    lane,
                                ))?),
                                state.ddr4_tx_vref_range[ch],
                            )? as u8;
                        }
                    }
                }
                for ch in 0..2 {
                    if mask & (1 << ch) != 0 {
                        self.apply_pda_centers(state, ch, rank, centers[ch], delay)?;
                        for lane in 0..8 {
                            let (left, right) = windows[ch][lane].best.ok_or(failed(
                                TrainingStep::WriteVoltage,
                                ch,
                                rank,
                                lane,
                            ))?;
                            let center = left + (right - left) / 2;
                            state.margin_results[ch][rank][lane][4] = ((center - left) * 10) as u32;
                            state.margin_results[ch][rank][lane][5] =
                                ((right - center) * 10) as u32;
                        }
                    }
                }
            }
            Ok(())
        })();
        self.quiesce_write_training(delay);
        let mut cleanup = Ok(());
        for ch in 0..2 {
            if ranks[ch] == 0 {
                continue;
            }
            self.registers.channel[ch].tc_rdrd.set(rdrd[ch]);
            if result.is_err() {
                for rank in 0..4 {
                    if ranks[ch] & (1 << rank) != 0 {
                        let restore = self.mrs_mr6_vref(ch, rank, saved_mr6[ch][rank], delay);
                        cleanup = cleanup.and(restore);
                        state.saved_mr.rank[ch][rank][6] = saved_mr6[ch][rank];
                        for lane in 0..8 {
                            state.margin_results[ch][rank][lane][4..6].fill(0);
                        }
                    }
                }
            }
        }
        if result.is_ok() && cleanup.is_ok() {
            state.ddr4_pda_enable = true;
        } else {
            state.ddr4_pda_enable = false;
            state.ddr4_pda_mr6 = saved_pda;
        }
        result.and(cleanup)
    }
}
