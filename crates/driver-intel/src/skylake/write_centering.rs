// SPDX-License-Identifier: MIT OR Apache-2.0
//! Native centering_2d.c:93–600, 1807–2204, DDR4 write-side closure.
//! Destructive cold-only traffic; not a cold training dispatcher or RAM handoff.
//! Caller owns stopped REUT and allocates scratch directly in CAR. The shared
//! read workspace supplies measured matrices/walkers, NOT write rollback.

use super::{
    Error, TrainingStep,
    ber_margin::{BerSweep, MarginParameter},
    command_training::CommandContext,
    io_test::{DqPattern, StopOnError},
    jedec::vref_code,
    opt_param::sweep::TestSnapshot,
    phy_registers::{DataControl2, DataOffsetTrain, TxTrain},
    power_model::{LimitKind, MarginKind, TrainingMode, margin_limit},
    read_centering::{
        EH_WEIGHTS, EW_WEIGHTS, PerBitParameter, ReadCenteringWorkspace, ReadTiming2d, VREF_POINTS,
        rounded,
    },
    registers::{ReutControl, ReutSequence},
    reut::{DownloadTarget, Engine, RankLane, RegisterFile, io_reset},
    reut_registers::CadbControl,
    timings::{Clock, FrequencyBin},
    training::{channels, cold_ranks},
    training_state::CalibrationState,
    vref_training::{vref_bounds, vref_dq_to_offset},
};
use embedded_hal::delay::DelayNs;
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};

#[derive(Debug, Clone, Copy)]
pub struct WriteTiming2d {
    pub per_bit: bool,
    pub reset_per_bit: bool,
    pub loop_count: u8,
    pub two_dimensional: bool,
}

pub(crate) struct WriteSnapshot {
    tests: [TestSnapshot; 2],
    tx: [[[u32; 9]; 4]; 2],
    bits: [[[u32; 9]; 4]; 2],
    offsets: [[u32; 9]; 2],
    controls: [[u32; 9]; 2],
    control2: [[u32; 9]; 2],
    channel_controls: [u32; 2],
    host_tx: [[[u16; 9]; 4]; 2],
    host_bits: [[[u8; 64]; 4]; 2],
    margins: [[[[u32; 4]; 9]; 4]; 2],
    mr6: [[u16; 4]; 2],
    pda: [[[u8; 9]; 4]; 2],
    pda_enable: bool,
    seeds: [[u32; 11]; 2],
    init: u32,
    global: u32,
    dummy: u8,
}
impl WriteSnapshot {
    pub(crate) const ZERO: Self = Self {
        tests: [TestSnapshot::ZERO; 2],
        tx: [[[0; 9]; 4]; 2],
        bits: [[[0; 9]; 4]; 2],
        offsets: [[0; 9]; 2],
        controls: [[0; 9]; 2],
        control2: [[0; 9]; 2],
        channel_controls: [0; 2],
        host_tx: [[[0; 9]; 4]; 2],
        host_bits: [[[0; 64]; 4]; 2],
        margins: [[[[0; 4]; 9]; 4]; 2],
        mr6: [[0; 4]; 2],
        pda: [[[0; 9]; 4]; 2],
        pda_enable: false,
        seeds: [[0; 11]; 2],
        init: 0,
        global: 0,
        dummy: 0,
    };
}
/// Includes independent TX hardware/host, global MR6 and per-device MR6 images.
/// The embedded read scratch is reused for its real bit walker and matrices;
/// no whole CalibrationState clone, heap, callback or global binding is used.
pub struct WriteCenteringWorkspace {
    saved: WriteSnapshot,
    pub(crate) measured: ReadCenteringWorkspace,
}
impl WriteCenteringWorkspace {
    /// Typed allocation image only; not measured calibration or training success.
    pub const INITIAL: Self = Self {
        saved: WriteSnapshot::ZERO,
        measured: ReadCenteringWorkspace::INITIAL,
    };
}

impl Default for WriteCenteringWorkspace {
    fn default() -> Self {
        Self::INITIAL
    }
}
fn failed(ch: usize, rank: usize, byte: usize) -> Error {
    Error::TrainingFailed {
        step: TrainingStep::WriteTiming,
        channel: ch as u8,
        rank: rank as u8,
        lane: byte as u8,
    }
}
fn minimum_voltage_width(clock: Clock) -> u32 {
    if clock.frequency_bin() >= FrequencyBin::Mts2400 {
        4
    } else {
        8
    }
}
fn timing_scale(eye: [u32; 2]) -> u32 {
    let scale = (eye[0] + eye[1]) / 20;
    if scale == 0 || scale > 12 { 12 } else { scale }
}
/// Source condense_margin_byte, restricted to selected PRESENT physical ranks.
/// Never includes a stale destination-rank eye outside that selection.
fn condense_margin_byte(
    s: &CalibrationState,
    ch: usize,
    ranks: u8,
    byte: usize,
    result: usize,
) -> Result<[u32; 2], Error> {
    if ranks == 0 || ranks & !15 != 0 {
        return Err(Error::InvalidRank);
    }
    let mut eye = [u32::MAX; 2];
    for r in 0..4 {
        if ranks & (1 << r) != 0 {
            for edge in 0..2 {
                eye[edge] = eye[edge].min(s.margin_results[ch][r][byte][result * 2 + edge]);
            }
        }
    }
    if eye.contains(&0) {
        return Err(failed(ch, ranks.trailing_zeros() as usize, byte));
    }
    Ok(eye)
}

/// Training needs a global coordinate in the current channel range. Terminal
/// MR diagnostics instead preserve native's independent saved SOURCE baseline
/// after final TX-range reconciliation; they must not normalize/overwrite it.
#[derive(Clone, Copy)]
pub(crate) enum GlobalMr6Encoding {
    ChannelRange,
    SourceBaseline,
}

impl Engine<'_> {
    fn begin_write_centering(
        &self,
        s: &CalibrationState,
        ctx: &CommandContext<'_>,
        mask: u8,
        w: &mut WriteCenteringWorkspace,
    ) -> Result<[u8; 2], Error> {
        let ranks = self.validate_write_centering(s, ctx, mask)?;
        self.write_snapshot(s, &mut w.saved)?;
        Ok(ranks)
    }
    pub(crate) fn validate_write_centering(
        &self,
        s: &CalibrationState,
        ctx: &CommandContext<'_>,
        mask: u8,
    ) -> Result<[u8; 2], Error> {
        self.validate_write_centering_encoding(s, ctx, mask, GlobalMr6Encoding::ChannelRange)
    }
    pub(crate) fn validate_write_centering_encoding(
        &self,
        s: &CalibrationState,
        ctx: &CommandContext<'_>,
        mask: u8,
        global_encoding: GlobalMr6Encoding,
    ) -> Result<[u8; 2], Error> {
        let ranks = cold_ranks(self, ctx.mode)?;
        if ctx.policy.sdram_lanes != 8
            || !matches!(ctx.policy.ca_vref_config, 0 | 2)
            || mask == 0
            || mask & !channels(ranks) != 0
        {
            return Err(Error::InvalidPolicy);
        }
        if s.dq_pat == DqPattern::Cadb as u8
            || self
                .registers
                .reut_global_control
                .is_set(ReutControl::START)
            || (0..2).any(|ch| {
                self.registers.reut_control[ch].is_set(ReutControl::START)
                    || self.registers.channel[ch]
                        .cadb_control
                        .is_set(CadbControl::DESELECT)
                    || self.registers.reut_sequence[ch][0].read(ReutSequence::INIT_MODE) == 2
            })
        {
            return Err(Error::InvalidReutConfig);
        }
        // Validate saved global/device coordinates and all host per-bit codes
        // before even WDB construction. Unconfigured stopped dq_pat=255 is legal.
        for ch in 0..2 {
            if mask & (1 << ch) == 0 {
                continue;
            }
            let (lo, hi) = vref_bounds(s.ddr4_tx_vref_range[ch])?;
            for r in 0..4 {
                if ranks[ch] & (1 << r) == 0 {
                    continue;
                }
                let mr6 = s.saved_mr.rank[ch][r][6];
                let global_offset = vref_dq_to_offset(mr6 as u8 & 127);
                let global_range = match global_encoding {
                    GlobalMr6Encoding::ChannelRange => s.ddr4_tx_vref_range[ch],
                    GlobalMr6Encoding::SourceBaseline => {
                        if mr6 & 64 == 0 {
                            1
                        } else {
                            2
                        }
                    }
                };
                let (global_lo, global_hi) = vref_bounds(global_range)?;
                if mr6 & 0x80 != 0
                    || !(global_lo..=global_hi).contains(&global_offset)
                    || vref_code(i32::from(global_offset), global_range)? as u8 != mr6 as u8 & 127
                {
                    return Err(Error::InvalidTiming);
                }
                for b in 0..8 {
                    let code = s.ddr4_pda_mr6[ch][r][b];
                    let offset = vref_dq_to_offset(code);
                    if code > 127
                        || !(lo..=hi).contains(&offset)
                        || vref_code(i32::from(offset), s.ddr4_tx_vref_range[ch])? as u8 != code
                        || s.training.ch[ch].rank[r].wr_dq_dly[b * 8..b * 8 + 8]
                            .iter()
                            .any(|&v| v > 15)
                    {
                        return Err(Error::InvalidTiming);
                    }
                }
            }
        }
        Ok(ranks)
    }
    pub(crate) fn write_snapshot(
        &self,
        s: &CalibrationState,
        saved: &mut WriteSnapshot,
    ) -> Result<(), Error> {
        for ch in 0..2 {
            let c = &self.registers.channel[ch];
            saved.tests[ch] = TestSnapshot::load(self, ch)?;
            saved.channel_controls[ch] = self.registers.data_channel[ch].control0.get();
            for i in 0..3 {
                saved.seeds[ch][i] = c.wdb_read_seed[i].get();
                saved.seeds[ch][i + 3] = c.wdb_write_seed[i].get();
                saved.seeds[ch][i + 6] = c.cadb_seed[i].get();
            }
            saved.seeds[ch][9] = c.wdb_lmn.get();
            saved.seeds[ch][10] = c.cadb_lmn.get();
            for b in 0..9 {
                let l = &self.registers.data_lane[b][ch];
                saved.offsets[ch][b] = l.offset_train.get();
                saved.controls[ch][b] = l.control0.get();
                saved.control2[ch][b] = l.control2.get();
                for r in 0..4 {
                    saved.tx[ch][r][b] = l.tx_train[r].get();
                    saved.bits[ch][r][b] = l.tx_per_bit[r].get();
                    saved.host_tx[ch][r][b] = s.training.ch[ch].rank[r].tx_dq_dly[b];
                    saved.margins[ch][r][b].copy_from_slice(&s.margin_results[ch][r][b][4..8]);
                }
            }
            for r in 0..4 {
                saved.host_bits[ch][r] = s.training.ch[ch].rank[r].wr_dq_dly;
                saved.mr6[ch][r] = s.saved_mr.rank[ch][r][6];
            }
        }
        saved.pda = s.ddr4_pda_mr6;
        saved.pda_enable = s.ddr4_pda_enable;
        saved.init = self.registers.init_state.get();
        saved.global = self.registers.reut_global_control.get();
        saved.dummy = s.en_dummy_reads;
        Ok(())
    }
    pub(crate) fn restore_write_centering(
        &self,
        s: &mut CalibrationState,
        mask: u8,
        ranks: [u8; 2],
        saved: &WriteSnapshot,
        rollback: bool,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        self.stop(delay);
        let mut result = io_reset(self.registers, delay, 10_000, 1);
        self.registers.init_state.set(saved.init);
        if rollback {
            s.ddr4_pda_mr6 = saved.pda;
            s.ddr4_pda_enable = saved.pda_enable;
            for ch in 0..2 {
                for r in 0..4 {
                    s.saved_mr.rank[ch][r][6] = saved.mr6[ch][r];
                }
            }
        }
        // Global and PDA are independent images; applying the average alone
        // destroys the original per-device distribution. Attempt every rank.
        for ch in 0..2 {
            if mask & (1 << ch) == 0 {
                continue;
            }
            for r in 0..4 {
                if ranks[ch] & (1 << r) == 0 {
                    continue;
                }
                let global = self.mrs_mr6_vref(ch, r, s.saved_mr.rank[ch][r][6], delay);
                if result.is_ok() {
                    result = global;
                }
                if s.ddr4_pda_enable {
                    let pda = self.ber_pda_offset(s, ch, r, 0xff, 0, delay);
                    if result.is_ok() {
                        result = pda;
                    }
                }
                let mr5 = self.training_mrs(ch, r, 5, s.saved_mr.rank[ch][r][5], delay);
                if result.is_ok() {
                    result = mr5;
                }
            }
        }
        for ch in 0..2 {
            // This aperture broadcasts into each real lane. Restore it BEFORE
            // the independent unicast words, never overwrite them afterwards.
            self.registers.data_channel[ch]
                .control0
                .set(saved.channel_controls[ch]);
            for b in 0..9 {
                let l = &self.registers.data_lane[b][ch];
                l.offset_train
                    .set(if rollback || b == 8 || mask & (1 << ch) == 0 {
                        saved.offsets[ch][b]
                    } else {
                        0
                    });
                if rollback {
                    l.control2.set(saved.control2[ch][b]);
                }
                for r in 0..4 {
                    if rollback {
                        l.tx_train[r].set(saved.tx[ch][r][b]);
                        l.tx_per_bit[r].set(saved.bits[ch][r][b]);
                    }
                    if mask & (1 << ch) != 0 && ranks[ch] & (1 << r) != 0 {
                        let download = self.download(
                            DownloadTarget::Lane {
                                channel: ch,
                                lane: b,
                            },
                            r,
                            RegisterFile::Write,
                        );
                        if result.is_ok() {
                            result = download;
                        }
                    }
                    if rollback {
                        s.training.ch[ch].rank[r].tx_dq_dly[b] = saved.host_tx[ch][r][b];
                        s.margin_results[ch][r][b][4..8].copy_from_slice(&saved.margins[ch][r][b]);
                    }
                }
                l.control0.set(saved.controls[ch][b]);
            }
            if rollback {
                for r in 0..4 {
                    s.training.ch[ch].rank[r].wr_dq_dly = saved.host_bits[ch][r];
                }
            }
            saved.tests[ch].restore(self, ch);
            let c = &self.registers.channel[ch];
            for i in 0..3 {
                c.wdb_read_seed[i].set(saved.seeds[ch][i]);
                c.wdb_write_seed[i].set(saved.seeds[ch][i + 3]);
                c.cadb_seed[i].set(saved.seeds[ch][i + 6]);
            }
            c.wdb_lmn.set(saved.seeds[ch][9]);
            c.cadb_lmn.set(saved.seeds[ch][10]);
        }
        s.en_dummy_reads = saved.dummy;
        s.dq_pat = u8::MAX;
        s.dq_pat_lc = 0;
        self.registers.reut_global_control.set(
            ReutControl::STOP::SET.value
                | (saved.global & ReutControl::STOP_ON_ANY_ERROR::SET.mask()),
        );
        result
    }
    fn finish_write_centering(
        &self,
        s: &mut CalibrationState,
        mask: u8,
        ranks: [u8; 2],
        w: &WriteCenteringWorkspace,
        measured: Result<(), Error>,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        let cleanup =
            self.restore_write_centering(s, mask, ranks, &w.saved, measured.is_err(), delay);
        if cleanup.is_err() && measured.is_ok() {
            let _ = self.restore_write_centering(s, mask, ranks, &w.saved, true, delay);
        }
        cleanup.and(measured)
    }
    fn write_centering_measure(
        &self,
        s: &mut CalibrationState,
        ctx: &CommandContext<'_>,
        mask: u8,
        rank: usize,
        parameter: MarginParameter,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        let voltage = parameter == MarginParameter::WriteVoltage;
        self.get_ber_margin_byte(
            s,
            ctx,
            BerSweep {
                parameter,
                channels: mask,
                rank_in: if voltage { 1 << rank } else { rank as u8 },
                rank_rx: if voltage { 1 << rank } else { rank as u8 },
                per_channel: false,
                interpolate: true,
                max_margin: if voltage { 36 } else { 31 },
                interior: false,
                byte_map: None,
            },
            &mut [0; 4],
            delay,
        )
    }
    fn write_centering_height(
        &self,
        s: &mut CalibrationState,
        ctx: &CommandContext<'_>,
        mask: u8,
        rank: usize,
        w: &mut ReadCenteringWorkspace,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        self.write_centering_measure(s, ctx, mask, rank, MarginParameter::WriteVoltage, delay)?;
        for ch in 0..2 {
            if mask & (1 << ch) != 0 {
                for b in 0..8 {
                    let eye = condense_margin_byte(s, ch, 1 << rank, b, 2)?;
                    w.eh[ch][b] = (eye[0] + eye[1]) / 10;
                    w.scale[ch][b] = (w.eh[ch][b] / 2).min(24);
                }
                // Native dt_dq_time_center_eh uses the weakest byte's voltage scale
                // for ALL devices on this channel in the write timing path.
                let scale = *w.scale[ch].iter().min().ok_or(Error::InvalidTiming)?;
                w.scale[ch].fill(scale);
            }
        }
        Ok(())
    }
    fn write_vref_point(
        &self,
        s: &CalibrationState,
        mask: u8,
        rank: usize,
        point: i16,
        w: &ReadCenteringWorkspace,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        for ch in 0..2 {
            if mask & (1 << ch) != 0 {
                for b in 0..8 {
                    let offset = point * w.scale[ch][b] as i16 / 24;
                    // Fail, rather than silently turning a clipped duplicate into an eye.
                    let center = vref_dq_to_offset(s.ddr4_pda_mr6[ch][rank][b]);
                    let (lo, hi) = vref_bounds(s.ddr4_tx_vref_range[ch])?;
                    if !(lo..=hi).contains(&(center + offset)) {
                        return Err(failed(ch, rank, b));
                    }
                    self.ber_pda_offset(s, ch, rank, 1 << b, offset, delay)?;
                }
            }
        }
        Ok(())
    }
    fn write_centering_timing_offset(
        &self,
        ch: usize,
        rank: usize,
        b: usize,
        offset: i16,
    ) -> Result<(), Error> {
        if !(-32..=31).contains(&offset) {
            return Err(Error::InvalidTiming);
        }
        self.registers.data_lane[b][ch]
            .offset_train
            .write(DataOffsetTrain::TX_DQ.val(offset as u32 & 63));
        self.download(
            DownloadTarget::Lane {
                channel: ch,
                lane: b,
            },
            rank,
            RegisterFile::Write,
        )
    }
    fn final_write_measurement(
        &self,
        s: &mut CalibrationState,
        ctx: &CommandContext<'_>,
        mask: u8,
        rank: usize,
        parameter: MarginParameter,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        for ch in 0..2 {
            if mask & (1 << ch) != 0 {
                self.ber_pda_offset(s, ch, rank, 0xff, 0, delay)?;
                for b in 0..8 {
                    self.write_centering_timing_offset(ch, rank, b, 0)?;
                }
                self.set_soe(ch, StopOnError::Never)?;
            }
        }
        if self.run_io_test(s, mask, delay)?.error_channels != 0 {
            return Err(failed(mask.trailing_zeros() as usize, rank, 0));
        }
        for ch in 0..2 {
            if mask & (1 << ch) != 0 && self.read_errors(ch)?.failing_dq != 0 {
                return Err(failed(ch, rank, 0));
            }
        }
        self.write_centering_measure(s, ctx, mask, rank, parameter, delay)?;
        for ch in 0..2 {
            if mask & (1 << ch) != 0 {
                for b in 0..8 {
                    condense_margin_byte(s, ch, 1 << rank, b, parameter.result_type())?;
                }
            }
        }
        Ok(())
    }
    fn write_timing_2d_body(
        &self,
        s: &mut CalibrationState,
        ctx: &CommandContext<'_>,
        input: u8,
        cfg: WriteTiming2d,
        w: &mut ReadCenteringWorkspace,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        self.setup_basic_va_test(s, input, cfg.loop_count - 1, 8, StopOnError::Never, delay)?;
        s.dq_pat = DqPattern::ReadRead as u8;
        let points = if cfg.two_dimensional { 7 } else { 1 };
        for rank in 0..4 {
            let mask = self.select_read_rank(input, rank)?;
            if mask == 0 {
                continue;
            }
            // Rank selection clears OFFSETTRAIN words but does not latch the
            // write register file. Establish actual nominal timing even for
            // non-reset callers before the first voltage-height measurement.
            for ch in 0..2 {
                if mask & (1 << ch) != 0 {
                    for b in 0..8 {
                        self.write_centering_timing_offset(ch, rank, b, 0)?;
                    }
                }
            }
            if cfg.reset_per_bit {
                self.reset_bits(s, mask, rank, PerBitParameter::WriteTiming)?;
            }
            self.write_centering_height(s, ctx, mask, rank, w, delay)?;
            w.byte_sum = [[0; 8]; 2];
            let mut divisors = [[0i64; 8]; 2];
            for v in 0..points {
                self.write_vref_point(s, mask, rank, VREF_POINTS[v], w, delay)?;
                self.write_centering_measure(
                    s,
                    ctx,
                    mask,
                    rank,
                    MarginParameter::WriteTiming,
                    delay,
                )?;
                for ch in 0..2 {
                    if mask & (1 << ch) != 0 {
                        for b in 0..8 {
                            let m = condense_margin_byte(s, ch, 1 << rank, b, 3)?;
                            w.eyes[v][ch][b] = m;
                            if v == 0 {
                                w.ew[ch][b] = (m[0] + m[1]) / 10;
                            }
                            let weight = i64::from(EH_WEIGHTS[v]) * i64::from(w.eh[ch][b])
                                + i64::from(EW_WEIGHTS[v]) * i64::from(w.ew[ch][b]);
                            w.byte_sum[ch][b] += weight * (i64::from(m[1]) - i64::from(m[0]));
                            divisors[ch][b] += weight;
                        }
                    }
                }
            }
            for ch in 0..2 {
                if mask & (1 << ch) != 0 {
                    for b in 0..8 {
                        let shift = rounded(w.byte_sum[ch][b], 20 * divisors[ch][b])?;
                        let old =
                            self.registers.data_lane[b][ch].tx_train[rank].read(TxTrain::DQ) as i16;
                        let value = old + shift;
                        if !(0..=511).contains(&value) {
                            return Err(failed(ch, rank, b));
                        }
                        self.set_tx_dq_byte_delay(
                            RankLane {
                                channel: ch,
                                rank,
                                lane: b,
                            },
                            value as u16,
                        )?;
                        s.training.ch[ch].rank[rank].tx_dq_dly[b] = value as u16;
                    }
                    self.ber_pda_offset(s, ch, rank, 0xff, 0, delay)?;
                }
            }
            if cfg.per_bit {
                self.write_centering_height(s, ctx, mask, rank, w, delay)?;
                w.bit_sum = [[[0; 8]; 8]; 2];
                divisors = [[0; 8]; 2];
                s.dq_pat_lc = 1;
                for v in 0..points {
                    self.write_vref_point(s, mask, rank, VREF_POINTS[v], w, delay)?;
                    self.write_centering_measure(
                        s,
                        ctx,
                        mask,
                        rank,
                        MarginParameter::WriteTiming,
                        delay,
                    )?;
                    for ch in 0..2 {
                        if mask & (1 << ch) != 0 {
                            self.set_soe(ch, StopOnError::AllLanes)?;
                            for b in 0..8 {
                                w.eyes[v][ch][b] = condense_margin_byte(s, ch, 1 << rank, b, 3)?;
                            }
                        }
                    }
                    self.dt_get_margin_bit(
                        s,
                        mask,
                        rank,
                        PerBitParameter::WriteTiming,
                        v,
                        w,
                        delay,
                    )?;
                    for ch in 0..2 {
                        if mask & (1 << ch) != 0 {
                            for b in 0..8 {
                                let weight = i64::from(EH_WEIGHTS[v]) * i64::from(w.eh[ch][b])
                                    + i64::from(EW_WEIGHTS[v]) * i64::from(w.ew[ch][b]);
                                divisors[ch][b] += weight;
                                for bit in 0..8 {
                                    let m = w.bit_eyes[ch][b][bit];
                                    w.bit_sum[ch][b][bit] +=
                                        weight * (i64::from(m[0]) + i64::from(m[1]) - 16);
                                }
                            }
                        }
                    }
                }
                self.apply_bit_centers(s, mask, rank, PerBitParameter::WriteTiming, divisors, w)?;
            }
            self.final_write_measurement(s, ctx, mask, rank, MarginParameter::WriteTiming, delay)?;
        }
        Ok(())
    }
    pub fn dt_centering_2d_wrt(
        &self,
        s: &mut CalibrationState,
        ctx: &CommandContext<'_>,
        mask: u8,
        cfg: WriteTiming2d,
        w: &mut WriteCenteringWorkspace,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        if !(2..=31).contains(&cfg.loop_count) {
            return Err(Error::InvalidPolicy);
        }
        let ranks = self.begin_write_centering(s, ctx, mask, w)?;
        let measured = (|| {
            self.setup_wdb(mask)?;
            s.en_dummy_reads = 0;
            s.ddr4_pda_enable = true;
            self.write_timing_2d_body(s, ctx, mask, cfg, &mut w.measured, delay)
        })();
        self.finish_write_centering(s, mask, ranks, w, measured, delay)
    }
    /// DDR4 source order: nominal per-bit/reset LC15, then byte 2D LC15.
    /// Native dt_perbit_deskew_wrt is DDR3-only and is NOT inserted here.
    pub fn meminit_write_timing_centering_2d(
        &self,
        s: &mut CalibrationState,
        ctx: &CommandContext<'_>,
        w: &mut WriteCenteringWorkspace,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        let mask = channels(cold_ranks(self, ctx.mode)?);
        let ranks = self.begin_write_centering(s, ctx, mask, w)?;
        let measured = (|| {
            self.setup_wdb(mask)?;
            s.en_dummy_reads = 0;
            s.ddr4_pda_enable = true;
            self.write_timing_2d_body(
                s,
                ctx,
                mask,
                WriteTiming2d {
                    per_bit: true,
                    reset_per_bit: true,
                    loop_count: 15,
                    two_dimensional: false,
                },
                &mut w.measured,
                delay,
            )?;
            self.write_timing_2d_body(
                s,
                ctx,
                mask,
                WriteTiming2d {
                    per_bit: false,
                    reset_per_bit: false,
                    loop_count: 15,
                    two_dimensional: true,
                },
                &mut w.measured,
                delay,
            )
        })();
        self.finish_write_centering(s, mask, ranks, w, measured, delay)
    }
    fn write_voltage_2d_body(
        &self,
        s: &mut CalibrationState,
        ctx: &CommandContext<'_>,
        input: u8,
        clock: Clock,
        w: &mut ReadCenteringWorkspace,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        for rank in 0..4 {
            self.setup_basic_va_test(s, input, 15, 8, StopOnError::AllByteGroups, delay)?;
            let mask = self.select_read_rank(input, rank)?;
            if mask == 0 {
                continue;
            }
            // Use a freshly measured physical-rank timing eye rather than a
            // stale rank-zero destination in native condense_margin_byte.
            self.write_centering_measure(s, ctx, mask, rank, MarginParameter::WriteTiming, delay)?;
            for ch in 0..2 {
                if mask & (1 << ch) != 0 {
                    for b in 0..8 {
                        w.scale[ch][b] =
                            timing_scale(condense_margin_byte(s, ch, 1 << rank, b, 3)?);
                    }
                }
            }
            w.byte_sum = [[0; 8]; 2];
            for (tim, point) in [0i16, -8, 8].into_iter().enumerate() {
                for ch in 0..2 {
                    if mask & (1 << ch) != 0 {
                        for b in 0..8 {
                            self.write_centering_timing_offset(
                                ch,
                                rank,
                                b,
                                point * w.scale[ch][b] as i16 / 12,
                            )?;
                        }
                    }
                }
                delay.delay_us(1);
                self.write_centering_measure(
                    s,
                    ctx,
                    mask,
                    rank,
                    MarginParameter::WriteVoltage,
                    delay,
                )?;
                for ch in 0..2 {
                    if mask & (1 << ch) != 0 {
                        for b in 0..8 {
                            let eye = condense_margin_byte(s, ch, 1 << rank, b, 2)?;
                            if eye[0] + eye[1] < minimum_voltage_width(clock) * 10 {
                                return Err(failed(ch, rank, b));
                            }
                            w.eyes[tim][ch][b] = eye;
                            w.byte_sum[ch][b] += i64::from(eye[1]) - i64::from(eye[0]);
                        }
                    }
                }
            }
            for ch in 0..2 {
                if mask & (1 << ch) != 0 {
                    let mut codes = [0u8; 8];
                    let (lo, hi) = vref_bounds(s.ddr4_tx_vref_range[ch])?;
                    for b in 0..8 {
                        self.write_centering_timing_offset(ch, rank, b, 0)?;
                        let value = vref_dq_to_offset(s.ddr4_pda_mr6[ch][rank][b])
                            + rounded(w.byte_sum[ch][b], 60)?;
                        if !(lo..=hi).contains(&value) {
                            return Err(failed(ch, rank, b));
                        }
                        codes[b] = vref_code(i32::from(value), s.ddr4_tx_vref_range[ch])? as u8;
                    }
                    self.apply_pda_centers(s, ch, rank, codes, delay)?;
                }
            }
            self.final_write_measurement(s, ctx, mask, rank, MarginParameter::WriteVoltage, delay)?;
            for ch in 0..2 {
                if mask & (1 << ch) != 0 {
                    for b in 0..8 {
                        let eye = condense_margin_byte(s, ch, 1 << rank, b, 2)?;
                        if eye[0] + eye[1] < minimum_voltage_width(clock) * 10 {
                            return Err(failed(ch, rank, b));
                        }
                    }
                }
            }
        }
        Ok(())
    }
    pub fn meminit_write_voltage_centering_2d(
        &self,
        s: &mut CalibrationState,
        ctx: &CommandContext<'_>,
        clock: Clock,
        w: &mut WriteCenteringWorkspace,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        self.write_voltage_centering_2d_channels(
            s,
            ctx,
            clock,
            channels(cold_ranks(self, ctx.mode)?),
            w,
            delay,
        )
    }
    /// Reuse the same measured walker for a DIMM optimizer's selected channels.
    pub(crate) fn write_voltage_centering_2d_channels(
        &self,
        s: &mut CalibrationState,
        ctx: &CommandContext<'_>,
        clock: Clock,
        mask: u8,
        w: &mut WriteCenteringWorkspace,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        let ranks = self.begin_write_centering(s, ctx, mask, w)?;
        let measured = (|| {
            self.setup_wdb(mask)?;
            s.en_dummy_reads = 0;
            s.ddr4_pda_enable = true;
            self.write_voltage_2d_body(s, ctx, mask, clock, &mut w.measured, delay)
        })();
        self.finish_write_centering(s, mask, ranks, w, measured, delay)
    }
    /// Native DDR3-only helper, exposed separately for source closure. DDR4's
    /// wrapper deliberately does not disable TX deskew. Changes are measured
    /// at the actual disabled point before committing any host coordinates.
    pub fn dt_perbit_deskew_wrt(
        &self,
        s: &mut CalibrationState,
        ctx: &CommandContext<'_>,
        clock: Clock,
        mode: TrainingMode,
        w: &mut WriteCenteringWorkspace,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        let power = margin_limit(s, MarginKind::WriteTiming, LimitKind::Power, clock, mode)?;
        let upm = margin_limit(s, MarginKind::WriteTiming, LimitKind::Upm, clock, mode)?;
        let mask = channels(cold_ranks(self, ctx.mode)?);
        let ranks = self.begin_write_centering(s, ctx, mask, w)?;
        let measured = (|| {
            self.setup_wdb(mask)?;
            s.en_dummy_reads = 0;
            s.ddr4_pda_enable = true;
            self.setup_basic_va_test(s, mask, 14, 8, StopOnError::Never, delay)?;
            for rank in 0..4 {
                let selected = self.select_read_rank(mask, rank)?;
                if selected != 0 {
                    self.write_centering_measure(
                        s,
                        ctx,
                        selected,
                        rank,
                        MarginParameter::WriteTiming,
                        delay,
                    )?;
                }
            }
            for ch in 0..2 {
                if mask & (1 << ch) != 0 {
                    for b in 0..8 {
                        // Rank eyes are relative to independent TX centers.
                        // Native deskew uses min(left + right), not the
                        // edge-wise intersection needed by other consumers.
                        let mut min_width = u32::MAX;
                        for rank in 0..4 {
                            if ranks[ch] & (1 << rank) != 0 {
                                let eye = &s.margin_results[ch][rank][b][6..8];
                                if eye.contains(&0) {
                                    return Err(failed(ch, rank, b));
                                }
                                let width =
                                    eye[0].checked_add(eye[1]).ok_or(Error::InvalidPolicy)?;
                                min_width = min_width.min(width);
                            }
                        }
                        if min_width <= (u32::from(power) + u32::from(upm)) / 2 {
                            continue;
                        }
                        for rank in 0..4 {
                            if ranks[ch] & (1 << rank) != 0 {
                                self.program_centering_per_bit(
                                    s,
                                    PerBitParameter::WriteTiming,
                                    RankLane {
                                        channel: ch,
                                        rank,
                                        lane: b,
                                    },
                                    0,
                                    true,
                                )?;
                                let value = self.registers.data_lane[b][ch].tx_train[rank]
                                    .read(TxTrain::DQ)
                                    .min(0x1f7) as u16
                                    + 8;
                                self.set_tx_dq_byte_delay(
                                    RankLane {
                                        channel: ch,
                                        rank,
                                        lane: b,
                                    },
                                    value,
                                )?;
                                s.training.ch[ch].rank[rank].tx_dq_dly[b] = value;
                            }
                        }
                        self.registers.data_lane[b][ch]
                            .control2
                            .modify(DataControl2::TX_DESKEW_DISABLE::SET);
                    }
                }
            }
            for rank in 0..4 {
                let selected = self.select_read_rank(mask, rank)?;
                if selected != 0 {
                    self.final_write_measurement(
                        s,
                        ctx,
                        selected,
                        rank,
                        MarginParameter::WriteTiming,
                        delay,
                    )?;
                }
            }
            Ok(())
        })();
        self.finish_write_centering(s, mask, ranks, w, measured, delay)
    }
    /// Combined real read/write route. Unsupported parameters (and write RX
    /// duty-cycle requests) reject before setup, reset, MR or controller writes.
    pub fn meminit_data_time_centering_2d(
        &self,
        s: &mut CalibrationState,
        ctx: &CommandContext<'_>,
        mask: u8,
        parameter: MarginParameter,
        cfg: ReadTiming2d,
        w: &mut WriteCenteringWorkspace,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        match parameter {
            MarginParameter::ReadTiming => {
                self.dt_centering_2d_rdt(s, ctx, mask, cfg, &mut w.measured, delay)
            }
            MarginParameter::WriteTiming if !cfg.duty_cycle => self.dt_centering_2d_wrt(
                s,
                ctx,
                mask,
                WriteTiming2d {
                    per_bit: cfg.per_bit,
                    reset_per_bit: cfg.reset_per_bit,
                    loop_count: cfg.loop_count,
                    two_dimensional: cfg.two_dimensional,
                },
                w,
                delay,
            ),
            _ => Err(Error::InvalidPolicy),
        }
    }
}

#[cfg(test)]
#[path = "write_centering_tests.rs"]
mod tests;
