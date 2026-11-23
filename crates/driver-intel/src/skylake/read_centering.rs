// SPDX-License-Identifier: MIT OR Apache-2.0
//! Late DDR4 read 2D from native/centering_2d.c:39–57, 663–1804,
//! 2213–2462. Destructive cold-only consumers, not a training dispatcher.
//! Caller exclusively owns stopped REUT/WDB/CADB and allocates the workspace
//! directly in CAR. Write-only pattern contents cannot be restored; every later
//! consumer must perform real pattern setup. Eight DQ lanes are supported;
//! this module does not infer ECC feedback from a ninth lane's byte counter.

use super::{
    Error, TrainingStep,
    ber_margin::{BerSweep, MarginParameter},
    command_training::CommandContext,
    io_test::{DqPattern, StopOnError},
    opt_param::sweep::TestSnapshot,
    phy_registers::{DataControl2, DataOffsetTrain, RxTrain},
    power_model::{LimitKind, MarginKind, TrainingMode, margin_limit},
    registers::{ReutControl, ReutSequence},
    reut::{DownloadTarget, Engine, RankLane, RegisterFile, io_reset, rank_mask},
    reut_registers::{CadbControl, ErrorControl},
    timings::Clock,
    training::{channels, cold_ranks},
    training_state::CalibrationState,
};
use embedded_hal::delay::DelayNs;
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};

pub(crate) const VREF_POINTS: [i16; 7] = [0, -6, -12, -18, 6, 12, 18];
pub(crate) const EH_WEIGHTS: [i32; 7] = [6, 2, 1, 0, 2, 1, 0];
pub(crate) const EW_WEIGHTS: [i32; 7] = [0, 1, 2, 3, 1, 2, 3];
const TIME_POINTS: [i16; 3] = [0, -8, 8];
type Bytes = [[u32; 8]; 2];
type ByteEyes = [[[u32; 2]; 8]; 2];
type BitEyes = [[[[u8; 2]; 8]; 8]; 2];

/// Shared read/write per-bit programming, separate from the nine native BER
/// parameters. All three cases use actual register-file downloads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PerBitParameter {
    ReadTiming,
    WriteTiming,
    ReadVoltage,
}
impl PerBitParameter {
    fn reversed(self, edge: usize) -> bool {
        if self == Self::ReadTiming {
            edge == 1
        } else {
            edge == 0
        }
    }
    fn register_file(self) -> RegisterFile {
        if self == Self::WriteTiming {
            RegisterFile::Write
        } else {
            RegisterFile::Read
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ReadTiming2d {
    pub per_bit: bool,
    pub duty_cycle: bool,
    pub reset_per_bit: bool,
    pub loop_count: u8,
    pub two_dimensional: bool,
}
#[derive(Debug, Clone, Copy)]
pub struct ReadVoltage2d {
    pub per_bit: bool,
    pub reset_per_bit: bool,
    pub loop_count: u8,
    pub two_dimensional: bool,
}

#[derive(Clone, Copy, Default)]
struct BitWalk {
    point: u8,
    pass: bool,
    fail: bool,
    best: u8,
}
impl BitWalk {
    const INITIAL: Self = Self {
        point: 0,
        pass: false,
        fail: false,
        best: 0,
    };
    fn done(self) -> bool {
        self.pass && (self.fail || self.best == 15)
    }
    fn observe(&mut self, failed: bool) -> Result<(), Error> {
        if self.done() {
            return Ok(());
        }
        if failed {
            self.fail = true;
            // Unlike native's unsigned c-1 / invented pass at zero, a dead
            // bit is an unusable measured eye, never a wrapping margin.
            if self.point == 0 && !self.pass {
                return Err(Error::ReutError);
            }
            if self.pass {
                return Ok(());
            }
            self.point -= 1;
        } else {
            self.pass = true;
            self.best = self.point;
            if !self.fail && self.point < 15 {
                self.point += 1;
            }
        }
        Ok(())
    }
}

/// Only fields modified by this seam, with independent original hardware and
/// host images. Never a full CalibrationState copy.
#[derive(Clone, Copy)]
pub(crate) struct ReadSnapshot {
    tests: [TestSnapshot; 2],
    rx: [[[u32; 9]; 4]; 2],
    bit_timing: [[[u32; 9]; 4]; 2],
    bit_voltage: [[[u32; 9]; 4]; 2],
    offsets: [[u32; 9]; 2],
    pub(crate) controls: [[u32; 9]; 2],
    control2: [[u32; 9]; 2],
    pub(crate) channel_controls: [u32; 2],
    host_p: [[[u8; 9]; 4]; 2],
    host_n: [[[u8; 9]; 4]; 2],
    host_dqs: [[[u8; 8]; 4]; 2],
    host_bit_timing: [[[u8; 64]; 4]; 2],
    host_bit_voltage: [[[u8; 64]; 4]; 2],
    host_voltage: [[i8; 9]; 2],
    host_rank_voltage: [[[i8; 9]; 4]; 2],
    margins: [[[[u32; 4]; 9]; 4]; 2],
    init: u32,
    wdb_seeds: [[u32; 7]; 2],
    global_control: u32,
    pub(crate) selected_channels: u8,
    dummy: u8,
}

impl ReadSnapshot {
    pub(crate) const ZERO: Self = Self {
        tests: [TestSnapshot::ZERO; 2],
        rx: [[[0; 9]; 4]; 2],
        bit_timing: [[[0; 9]; 4]; 2],
        bit_voltage: [[[0; 9]; 4]; 2],
        offsets: [[0; 9]; 2],
        controls: [[0; 9]; 2],
        control2: [[0; 9]; 2],
        channel_controls: [0; 2],
        host_p: [[[0; 9]; 4]; 2],
        host_n: [[[0; 9]; 4]; 2],
        host_dqs: [[[0; 8]; 4]; 2],
        host_bit_timing: [[[0; 64]; 4]; 2],
        host_bit_voltage: [[[0; 64]; 4]; 2],
        host_voltage: [[0; 9]; 2],
        host_rank_voltage: [[[0; 9]; 4]; 2],
        margins: [[[[0; 4]; 9]; 4]; 2],
        init: 0,
        wdb_seeds: [[0; 7]; 2],
        global_control: 0,
        selected_channels: 0,
        dummy: 0,
    };
}

/// Caller-owned matrices and transaction images. No heap/global context. The
/// same eye and bit buffers are reused across physical ranks, strobes and passes.
/// `Default` is for allocation, not valid training results; avoid stack temporaries
/// when placing this in CAR. Size is asserted and reported by the host test.
pub struct ReadCenteringWorkspace {
    saved: ReadSnapshot,
    pub(crate) eh: Bytes,
    pub(crate) ew: Bytes,
    pub(crate) scale: Bytes,
    pub(crate) eyes: [ByteEyes; 7],
    pub(crate) byte_sum: [[i64; 8]; 2],
    pub(crate) bit_sum: [[[i64; 8]; 8]; 2],
    pub(crate) bit_eyes: BitEyes,
    walk: [[[BitWalk; 8]; 8]; 2],
}
impl ReadCenteringWorkspace {
    /// Typed allocation image only; not measured calibration or training success.
    pub const INITIAL: Self = Self {
        saved: ReadSnapshot::ZERO,
        eh: [[0; 8]; 2],
        ew: [[0; 8]; 2],
        scale: [[0; 8]; 2],
        eyes: [[[[0; 2]; 8]; 2]; 7],
        byte_sum: [[0; 8]; 2],
        bit_sum: [[[0; 8]; 8]; 2],
        bit_eyes: [[[[0; 2]; 8]; 8]; 2],
        walk: [[[BitWalk::INITIAL; 8]; 8]; 2],
    };
}

impl Default for ReadCenteringWorkspace {
    fn default() -> Self {
        Self::INITIAL
    }
}

/// Native max_tscale=12, including its explicit zero-to-maximum fallback.
fn read_voltage_timing_scale(left: u32, right: u32) -> u32 {
    let scale = (left + right) / 20;
    if scale == 0 || scale > 12 { 12 } else { scale }
}

fn failed(ch: usize, rank: usize, byte: usize) -> Error {
    Error::TrainingFailed {
        step: TrainingStep::ReadTiming,
        channel: ch as u8,
        rank: rank as u8,
        lane: byte as u8,
    }
}
/// Native signed rounding, nearest with ties away from zero. Numerator remains
/// signed before subtraction and is widened for weighted measured eye products.
pub(crate) fn rounded(sum: i64, divisor: i64) -> Result<i16, Error> {
    if divisor <= 0 {
        return Err(Error::InvalidTiming);
    }
    i16::try_from((sum + sum.signum() * (divisor / 2)) / divisor).map_err(|_| Error::InvalidTiming)
}

impl Engine<'_> {
    /// Source dt_program_perbit / dt_set_rx_offset_vdq. Explicit physical rank;
    /// consumers needing START/CURRENT resolve through existing ber_regfile_rank.
    pub fn program_centering_per_bit(
        &self,
        state: &mut CalibrationState,
        parameter: PerBitParameter,
        p: RankLane,
        packed: u32,
        update_host: bool,
    ) -> Result<(), Error> {
        if p.channel >= 2
            || p.rank >= 4
            || p.lane >= 8
            || rank_mask(self.dimms, p.channel)? & (1 << p.rank) == 0
        {
            return Err(Error::InvalidRank);
        }
        let l = &self.registers.data_lane[p.lane][p.channel];
        match parameter {
            PerBitParameter::ReadTiming => l.rx_per_bit[p.rank].set(packed),
            PerBitParameter::WriteTiming => l.tx_per_bit[p.rank].set(packed),
            PerBitParameter::ReadVoltage => l.rx_offset_vdq[p.rank].set(packed),
        }
        self.download(
            DownloadTarget::Lane {
                channel: p.channel,
                lane: p.lane,
            },
            p.rank,
            parameter.register_file(),
        )?;
        if update_host {
            let rank = &mut state.training.ch[p.channel].rank[p.rank];
            let host = match parameter {
                PerBitParameter::ReadTiming => &mut rank.rd_dq_dly,
                PerBitParameter::WriteTiming => &mut rank.wr_dq_dly,
                PerBitParameter::ReadVoltage => &mut rank.rx_vref_per_bit,
            };
            for bit in 0..8 {
                host[p.lane * 8 + bit] = ((packed >> (4 * bit)) & 15) as u8;
            }
        }
        Ok(())
    }

    pub(crate) fn read_snapshot(
        &self,
        s: &CalibrationState,
        saved: &mut ReadSnapshot,
    ) -> Result<(), Error> {
        for ch in 0..2 {
            saved.tests[ch] = TestSnapshot::load(self, ch)?;
            saved.channel_controls[ch] = self.registers.data_channel[ch].control0.get();
            saved.host_p[ch] = s.training.ch[ch].rx_dqs_p;
            saved.host_n[ch] = s.training.ch[ch].rx_dqs_n;
            saved.host_voltage[ch] = s.training.ch[ch].rd_vref;
            saved.host_rank_voltage[ch] = s.training.ch[ch].rx_vref_rank_offset;
            for i in 0..3 {
                saved.wdb_seeds[ch][i] = self.registers.channel[ch].wdb_read_seed[i].get();
                saved.wdb_seeds[ch][i + 3] = self.registers.channel[ch].wdb_write_seed[i].get();
            }
            saved.wdb_seeds[ch][6] = self.registers.channel[ch].wdb_lmn.get();
            for b in 0..9 {
                let l = &self.registers.data_lane[b][ch];
                saved.offsets[ch][b] = l.offset_train.get();
                saved.controls[ch][b] = l.control0.get();
                saved.control2[ch][b] = l.control2.get();
                for r in 0..4 {
                    saved.rx[ch][r][b] = l.rx_train[r].get();
                    saved.bit_timing[ch][r][b] = l.rx_per_bit[r].get();
                    saved.bit_voltage[ch][r][b] = l.rx_offset_vdq[r].get();
                    if b < 8 {
                        saved.host_dqs[ch][r][b] = s.training.ch[ch].rank[r].rd_dqs_dly[b];
                    }
                    for i in 0..4 {
                        saved.margins[ch][r][b][i] = s.margin_results[ch][r][b][i];
                    }
                }
            }
            for r in 0..4 {
                saved.host_bit_timing[ch][r] = s.training.ch[ch].rank[r].rd_dq_dly;
                saved.host_bit_voltage[ch][r] = s.training.ch[ch].rank[r].rx_vref_per_bit;
            }
        }
        saved.init = self.registers.init_state.get();
        saved.dummy = s.en_dummy_reads;
        saved.global_control = self.registers.reut_global_control.get();
        Ok(())
    }
    pub(crate) fn read_restore(
        &self,
        s: &mut CalibrationState,
        ranks: [u8; 2],
        saved: &ReadSnapshot,
        rollback: bool,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        self.stop(delay);
        // A final bounded real reset is mandatory; preserve its failure even
        // when MMIO rollback is possible afterwards.
        let mut result = io_reset(self.registers, delay, 10000, 1);
        self.registers.init_state.set(saved.init);
        for ch in 0..2 {
            self.registers.data_channel[ch]
                .control0
                .set(saved.channel_controls[ch]);
            for b in 0..9 {
                let l = &self.registers.data_lane[b][ch];
                l.offset_train.set(
                    if rollback || b >= 8 || saved.selected_channels & (1 << ch) == 0 {
                        saved.offsets[ch][b]
                    } else {
                        0
                    },
                );
                if rollback {
                    l.control2.set(saved.control2[ch][b]);
                }
                for r in 0..4 {
                    if rollback {
                        l.rx_train[r].set(saved.rx[ch][r][b]);
                        l.rx_per_bit[r].set(saved.bit_timing[ch][r][b]);
                        l.rx_offset_vdq[r].set(saved.bit_voltage[ch][r][b]);
                    }
                    if ranks[ch] & (1 << r) != 0 {
                        let restored = self.download(
                            DownloadTarget::Lane {
                                channel: ch,
                                lane: b,
                            },
                            r,
                            RegisterFile::Read,
                        );
                        if result.is_ok() {
                            result = restored;
                        }
                    }
                }
                l.control0.set(saved.controls[ch][b]);
            }
            saved.tests[ch].restore(self, ch);
            for i in 0..3 {
                self.registers.channel[ch].wdb_read_seed[i].set(saved.wdb_seeds[ch][i]);
                self.registers.channel[ch].wdb_write_seed[i].set(saved.wdb_seeds[ch][i + 3]);
            }
            self.registers.channel[ch]
                .wdb_lmn
                .set(saved.wdb_seeds[ch][6]);
            if rollback {
                s.training.ch[ch].rx_dqs_p = saved.host_p[ch];
                s.training.ch[ch].rx_dqs_n = saved.host_n[ch];
                s.training.ch[ch].rd_vref = saved.host_voltage[ch];
                s.training.ch[ch].rx_vref_rank_offset = saved.host_rank_voltage[ch];
                for r in 0..4 {
                    for b in 0..8 {
                        s.training.ch[ch].rank[r].rd_dqs_dly[b] = saved.host_dqs[ch][r][b];
                    }
                    s.training.ch[ch].rank[r].rd_dq_dly = saved.host_bit_timing[ch][r];
                    s.training.ch[ch].rank[r].rx_vref_per_bit = saved.host_bit_voltage[ch][r];
                    for b in 0..9 {
                        for i in 0..4 {
                            s.margin_results[ch][r][b][i] = saved.margins[ch][r][b][i];
                        }
                    }
                }
            }
        }
        // WDB rows and CADB FIFO are write-only, not snapshots. Restored
        // readable selectors do NOT confer runnable pattern ownership.
        s.dq_pat = u8::MAX;
        s.dq_pat_lc = 0;
        s.en_dummy_reads = saved.dummy;
        self.registers.reut_global_control.set(
            ReutControl::STOP::SET.value
                | (saved.global_control & ReutControl::STOP_ON_ANY_ERROR::SET.mask()),
        );
        result
    }
    fn begin_read_centering(
        &self,
        s: &mut CalibrationState,
        ctx: &CommandContext<'_>,
        mask: u8,
        work: &mut ReadCenteringWorkspace,
    ) -> Result<[u8; 2], Error> {
        let ranks = cold_ranks(self, ctx.mode)?;
        // No guessed ninth-bit success: explicit rejection precedes writes.
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
        self.read_snapshot(s, &mut work.saved)?;
        work.saved.selected_channels = mask;
        self.setup_wdb(mask)?;
        s.en_dummy_reads = 0;
        Ok(ranks)
    }
    fn finish_read_centering(
        &self,
        s: &mut CalibrationState,
        ranks: [u8; 2],
        work: &ReadCenteringWorkspace,
        measured: Result<(), Error>,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        let saved = &work.saved;
        let cleanup = self.read_restore(s, ranks, saved, measured.is_err(), delay);
        if cleanup.is_err() && measured.is_ok() {
            // A cleanup failure cannot publish trained host images. Rollback
            // independently, but do not pretend the failed reset was repaired.
            let _ = self.read_restore(s, ranks, saved, true, delay);
        }
        cleanup.and(measured)
    }
    pub(crate) fn select_read_rank(&self, input: u8, rank: usize) -> Result<u8, Error> {
        let mut mask = 0;
        for ch in 0..2 {
            if rank_mask(self.dimms, ch)? == 0 {
                self.registers.reut_sequence[ch][0].modify(ReutSequence::GLOBAL_CONTROL::CLEAR);
                continue;
            }
            let selected = if input & (1 << ch) != 0 { 1 << rank } else { 0 };
            if self.program_rank_map(ch, selected)? != 0 {
                mask |= 1 << ch;
            }
            if mask & (1 << ch) != 0 {
                for b in 0..8 {
                    self.registers.data_lane[b][ch].offset_train.set(0);
                }
            }
        }
        Ok(mask)
    }
    fn centering_offset(
        &self,
        ch: usize,
        rank: usize,
        byte: usize,
        timing: i16,
        voltage: i16,
    ) -> Result<(), Error> {
        if !(-32..=31).contains(&timing) || !(-64..=63).contains(&voltage) {
            return Err(Error::InvalidTiming);
        }
        self.registers.data_lane[byte][ch].offset_train.write(
            DataOffsetTrain::RX_DQS.val(timing as u32 & 63)
                + DataOffsetTrain::VREF.val(voltage as u32 & 127),
        );
        self.download(
            DownloadTarget::Lane {
                channel: ch,
                lane: byte,
            },
            rank,
            RegisterFile::Read,
        )
    }
    fn centering_measure(
        &self,
        s: &mut CalibrationState,
        ctx: &CommandContext<'_>,
        mask: u8,
        rank: usize,
        parameter: MarginParameter,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        self.get_ber_margin_byte(
            s,
            ctx,
            BerSweep {
                parameter,
                channels: mask,
                rank_in: rank as u8,
                rank_rx: rank as u8,
                per_channel: false,
                interpolate: true,
                max_margin: if parameter == MarginParameter::ReadVoltage {
                    63
                } else {
                    31
                },
                interior: false,
                byte_map: None,
            },
            &mut [0; 4],
            delay,
        )
    }
    fn dt_dq_time_center_eh(
        &self,
        s: &mut CalibrationState,
        ctx: &CommandContext<'_>,
        mask: u8,
        rank: usize,
        w: &mut ReadCenteringWorkspace,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        self.centering_measure(s, ctx, mask, rank, MarginParameter::ReadVoltage, delay)?;
        for ch in 0..2 {
            if mask & (1 << ch) == 0 {
                continue;
            }
            for b in 0..8 {
                let m = &s.margin_results[ch][rank][b];
                if m[0] == 0 || m[1] == 0 {
                    return Err(failed(ch, rank, b));
                }
                w.eh[ch][b] = (m[0] + m[1]) / 10;
                w.scale[ch][b] = (w.eh[ch][b] / 2).min(24);
            }
        }
        Ok(())
    }
    /// Real failing-DQ per-bit walker (native dt_get_margin_bit). Each edge
    /// starts at code 8, moves toward first pass/fail, and terminates within 32
    /// measurements. Byte push comes from measured byte eyes, never presets.
    /// Raw shared substrate: caller owns setup, measured `eyes[shape]`, a
    /// stopped engine and an outer rollback transaction for any failure.
    pub(crate) fn dt_get_margin_bit(
        &self,
        s: &mut CalibrationState,
        mask: u8,
        rank: usize,
        parameter: PerBitParameter,
        shape: usize,
        w: &mut ReadCenteringWorkspace,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        self.validate_channels(mask)?;
        if rank >= 4 || shape >= w.eyes.len() {
            return Err(Error::InvalidRank);
        }
        for ch in 0..2 {
            if mask & (1 << ch) != 0 && rank_mask(self.dimms, ch)? & (1 << rank) == 0 {
                return Err(Error::InvalidRank);
            }
        }
        let voltage = parameter == PerBitParameter::ReadVoltage;
        let outer = core::array::from_fn::<_, 2, _>(|ch| {
            core::array::from_fn::<_, 8, _>(|b| self.registers.data_lane[b][ch].offset_train.get())
        });
        for edge in 0..2 {
            for ch in 0..2 {
                if mask & (1 << ch) == 0 {
                    continue;
                }
                for b in 0..8 {
                    w.walk[ch][b] = [BitWalk {
                        point: 8,
                        ..BitWalk::default()
                    }; 8];
                    let eye = w.eyes[shape][ch][b];
                    if eye[0] == 0 || eye[1] == 0 {
                        return Err(failed(ch, rank, b));
                    }
                    // Source's ab_margin-2 is signed here (no zero-eye underflow).
                    let push = ((eye[0] + eye[1]) / 20) as i16 - 2 + if voltage { 4 } else { 0 };
                    let push = push * if edge == 0 { -1 } else { 1 };
                    let l = &self.registers.data_lane[b][ch];
                    l.offset_train.set(outer[ch][b]);
                    if voltage {
                        l.offset_train
                            .modify(DataOffsetTrain::VREF.val(push.clamp(-64, 63) as u32 & 127));
                    } else if parameter == PerBitParameter::WriteTiming {
                        l.offset_train
                            .modify(DataOffsetTrain::TX_DQ.val(push.clamp(-32, 31) as u32 & 63));
                    } else {
                        l.offset_train
                            .modify(DataOffsetTrain::RX_DQS.val(push.clamp(-32, 31) as u32 & 63));
                    }
                }
            }
            let mut complete = false;
            for _ in 0..32 {
                for ch in 0..2 {
                    if mask & (1 << ch) == 0 {
                        continue;
                    }
                    for b in 0..8 {
                        let mut packed = 0;
                        for bit in 0..8 {
                            let code = w.walk[ch][b][bit].point;
                            packed |= u32::from(if parameter.reversed(edge) {
                                15 - code
                            } else {
                                code
                            }) << (4 * bit);
                        }
                        let mapped = self.ber_regfile_rank(ch, false)?;
                        if mapped != rank {
                            return Err(Error::InvalidRank);
                        }
                        self.program_centering_per_bit(
                            s,
                            parameter,
                            RankLane {
                                channel: ch,
                                rank,
                                lane: b,
                            },
                            packed,
                            false,
                        )?;
                    }
                }
                self.run_io_test(s, mask, delay)?;
                complete = true;
                for ch in 0..2 {
                    if mask & (1 << ch) == 0 {
                        continue;
                    }
                    let errors = self.read_errors(ch)?.failing_dq;
                    for b in 0..8 {
                        for bit in 0..8 {
                            let walk = &mut w.walk[ch][b][bit];
                            walk.observe(errors & (1u64 << (8 * b + bit)) != 0)
                                .map_err(|_| failed(ch, rank, b))?;
                            complete &= walk.done();
                        }
                    }
                }
                if complete {
                    break;
                }
            }
            if !complete {
                return Err(Error::ReutError);
            }
            for ch in 0..2 {
                if mask & (1 << ch) == 0 {
                    continue;
                }
                for b in 0..8 {
                    for bit in 0..8 {
                        let value = w.walk[ch][b][bit].best;
                        w.bit_eyes[ch][b][bit][edge] = if parameter.reversed(edge) {
                            15 - value
                        } else {
                            value
                        };
                    }
                }
            }
        }
        for ch in 0..2 {
            if mask & (1 << ch) == 0 {
                continue;
            }
            for b in 0..8 {
                self.registers.data_lane[b][ch]
                    .offset_train
                    .set(outer[ch][b]);
                self.program_centering_per_bit(
                    s,
                    parameter,
                    RankLane {
                        channel: ch,
                        rank,
                        lane: b,
                    },
                    0x8888_8888,
                    false,
                )?;
            }
        }
        Ok(())
    }
    fn final_read_measurement(
        &self,
        s: &mut CalibrationState,
        ctx: &CommandContext<'_>,
        mask: u8,
        rank: usize,
        parameter: MarginParameter,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        for ch in 0..2 {
            if mask & (1 << ch) == 0 {
                continue;
            }
            for b in 0..8 {
                self.centering_offset(ch, rank, b, 0, 0)?;
            }
            self.set_soe(ch, StopOnError::Never)?;
        }
        // Explicit point at the selected bit/byte settings, then genuine BER
        // edges. A final point failure is not replaced with arithmetic rebasing.
        if self.run_io_test(s, mask, delay)?.error_channels != 0 {
            return Err(failed(mask.trailing_zeros() as usize, rank, 0));
        }
        for ch in 0..2 {
            if mask & (1 << ch) != 0 && self.read_errors(ch)?.failing_dq != 0 {
                return Err(failed(ch, rank, 0));
            }
        }
        self.centering_measure(s, ctx, mask, rank, parameter, delay)?;
        let i = parameter.result_type() * 2;
        for ch in 0..2 {
            if mask & (1 << ch) == 0 {
                continue;
            }
            for b in 0..8 {
                if s.margin_results[ch][rank][b][i] == 0
                    || s.margin_results[ch][rank][b][i + 1] == 0
                {
                    return Err(failed(ch, rank, b));
                }
            }
        }
        Ok(())
    }
    pub(crate) fn reset_bits(
        &self,
        s: &mut CalibrationState,
        mask: u8,
        rank: usize,
        parameter: PerBitParameter,
    ) -> Result<(), Error> {
        for ch in 0..2 {
            if mask & (1 << ch) != 0 {
                for b in 0..8 {
                    self.program_centering_per_bit(
                        s,
                        parameter,
                        RankLane {
                            channel: ch,
                            rank,
                            lane: b,
                        },
                        0x8888_8888,
                        true,
                    )?;
                }
            }
        }
        Ok(())
    }
    pub(crate) fn apply_bit_centers(
        &self,
        s: &mut CalibrationState,
        mask: u8,
        rank: usize,
        parameter: PerBitParameter,
        divisor: [[i64; 8]; 2],
        w: &ReadCenteringWorkspace,
    ) -> Result<(), Error> {
        for ch in 0..2 {
            if mask & (1 << ch) == 0 {
                continue;
            }
            for b in 0..8 {
                let mut packed = 0;
                for bit in 0..8 {
                    // The walker programs absolute nibbles around 8, not
                    // offsets from an arbitrary old host center. Mandatory
                    // native reset passes already start at 8; non-reset callers
                    // must not add that old center a second time.
                    let code =
                        (8 + rounded(w.bit_sum[ch][b][bit], 2 * divisor[ch][b])?).clamp(0, 15);
                    packed |= (code as u32) << (4 * bit);
                }
                self.program_centering_per_bit(
                    s,
                    parameter,
                    RankLane {
                        channel: ch,
                        rank,
                        lane: b,
                    },
                    packed,
                    true,
                )?;
            }
        }
        Ok(())
    }
    fn read_timing_2d_body(
        &self,
        s: &mut CalibrationState,
        ctx: &CommandContext<'_>,
        input: u8,
        cfg: ReadTiming2d,
        w: &mut ReadCenteringWorkspace,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        if !(2..=31).contains(&cfg.loop_count) {
            return Err(Error::InvalidPolicy);
        }
        self.setup_basic_va_test(s, input, cfg.loop_count - 1, 8, StopOnError::Never, delay)?;
        s.dq_pat = DqPattern::ReadRead as u8;
        let points = if cfg.two_dimensional { 7 } else { 1 };
        for rank in 0..4 {
            let mask = self.select_read_rank(input, rank)?;
            if mask == 0 {
                continue;
            }
            if cfg.reset_per_bit {
                self.reset_bits(s, mask, rank, PerBitParameter::ReadTiming)?;
            }
            // Hardware DQS P/N are independent of the entry host images. Use
            // actual hardware as source working baseline, retaining originals
            // independently in the outer transaction for exact error rollback.
            for ch in 0..2 {
                if mask & (1 << ch) != 0 {
                    for b in 0..8 {
                        let rx = &self.registers.data_lane[b][ch].rx_train[rank];
                        s.training.ch[ch].rx_dqs_p[rank][b] = rx.read(RxTrain::DQS_P) as u8;
                        s.training.ch[ch].rx_dqs_n[rank][b] = rx.read(RxTrain::DQS_N) as u8;
                        let raw = rx.read(RxTrain::VREF) as i16;
                        let value = if raw & 32 != 0 { raw - 64 } else { raw };
                        s.training.ch[ch].rx_vref_rank_offset[rank][b] =
                            (value - i16::from(s.training.ch[ch].rd_vref[b])) as i8;
                    }
                }
            }
            // Even nominal weights are based on measured height, not a
            // preset eye. Nominal's common factor cancels in the centroid.
            self.dt_dq_time_center_eh(s, ctx, mask, rank, w, delay)?;
            let mut divisors = [[0i64; 8]; 2];
            for strobe in 0..if cfg.duty_cycle { 2 } else { 1 } {
                for ch in 0..2 {
                    if mask & (1 << ch) != 0 {
                        self.registers.channel[ch].error_control.modify(
                            ErrorControl::CHUNKS.val(if cfg.duty_cycle {
                                0x55 << strobe
                            } else {
                                0xff
                            }),
                        );
                    }
                }
                w.byte_sum = [[0; 8]; 2];
                for v in 0..points {
                    for ch in 0..2 {
                        if mask & (1 << ch) != 0 {
                            for b in 0..8 {
                                self.centering_offset(
                                    ch,
                                    rank,
                                    b,
                                    0,
                                    if cfg.two_dimensional {
                                        VREF_POINTS[v] * w.scale[ch][b] as i16 / 24
                                    } else {
                                        0
                                    },
                                )?;
                            }
                        }
                    }
                    self.centering_measure(s, ctx, mask, rank, MarginParameter::ReadTiming, delay)?;
                    for ch in 0..2 {
                        if mask & (1 << ch) == 0 {
                            continue;
                        }
                        for b in 0..8 {
                            let m = &s.margin_results[ch][rank][b];
                            if m[2] == 0 || m[3] == 0 {
                                return Err(failed(ch, rank, b));
                            }
                            w.eyes[v][ch][b] = [m[2], m[3]];
                            if v == 0 {
                                w.ew[ch][b] = (m[2] + m[3]) / 10;
                                divisors[ch][b] = 0;
                            }
                            let weight = i64::from(EH_WEIGHTS[v]) * i64::from(w.eh[ch][b])
                                + i64::from(EW_WEIGHTS[v]) * i64::from(w.ew[ch][b]);
                            w.byte_sum[ch][b] += weight * (i64::from(m[3]) - i64::from(m[2]));
                            divisors[ch][b] += weight;
                        }
                    }
                }
                for ch in 0..2 {
                    if mask & (1 << ch) == 0 {
                        continue;
                    }
                    for b in 0..8 {
                        let shift = rounded(w.byte_sum[ch][b], 20 * divisors[ch][b])?;
                        let h = &mut s.training.ch[ch];
                        let p =
                            i16::from(h.rx_dqs_p[rank][b]) + if strobe == 0 { shift } else { 0 };
                        let n = i16::from(h.rx_dqs_n[rank][b])
                            + if !cfg.duty_cycle || strobe == 1 {
                                shift
                            } else {
                                0
                            };
                        if !(0..=63).contains(&p) || !(0..=63).contains(&n) {
                            return Err(failed(ch, rank, b));
                        }
                        h.rx_dqs_p[rank][b] = p as u8;
                        h.rx_dqs_n[rank][b] = n as u8;
                        h.rank[rank].rd_dqs_dly[b] = p as u8;
                        self.registers.data_lane[b][ch].rx_train[rank]
                            .modify(RxTrain::DQS_P.val(p as u32) + RxTrain::DQS_N.val(n as u32));
                        self.centering_offset(ch, rank, b, 0, 0)?;
                    }
                }
            }
            if cfg.per_bit {
                for ch in 0..2 {
                    if mask & (1 << ch) != 0 {
                        self.registers.channel[ch]
                            .error_control
                            .modify(ErrorControl::CHUNKS.val(0xff));
                    }
                }
                self.dt_dq_time_center_eh(s, ctx, mask, rank, w, delay)?;
                w.bit_sum = [[[0; 8]; 8]; 2];
                for ch in 0..2 {
                    if mask & (1 << ch) != 0 {
                        self.set_soe(ch, StopOnError::AllLanes)?;
                    }
                }
                s.dq_pat_lc = 1;
                for v in 0..points {
                    for ch in 0..2 {
                        if mask & (1 << ch) != 0 {
                            for b in 0..8 {
                                self.centering_offset(
                                    ch,
                                    rank,
                                    b,
                                    0,
                                    if cfg.two_dimensional {
                                        VREF_POINTS[v] * w.scale[ch][b] as i16 / 24
                                    } else {
                                        0
                                    },
                                )?;
                            }
                        }
                    }
                    // Remeasure the byte shape at the newly centered byte
                    // setting before deriving the real per-bit byte push.
                    self.centering_measure(s, ctx, mask, rank, MarginParameter::ReadTiming, delay)?;
                    for ch in 0..2 {
                        for b in 0..8 {
                            w.eyes[v][ch][b] = [
                                s.margin_results[ch][rank][b][2],
                                s.margin_results[ch][rank][b][3],
                            ];
                        }
                    }
                    self.dt_get_margin_bit(
                        s,
                        mask,
                        rank,
                        PerBitParameter::ReadTiming,
                        v,
                        w,
                        delay,
                    )?;
                    for ch in 0..2 {
                        if mask & (1 << ch) != 0 {
                            for b in 0..8 {
                                let weight = i64::from(EH_WEIGHTS[v]) * i64::from(w.eh[ch][b])
                                    + i64::from(EW_WEIGHTS[v]) * i64::from(w.ew[ch][b]);
                                if v == 0 {
                                    divisors[ch][b] = 0;
                                }
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
                self.apply_bit_centers(s, mask, rank, PerBitParameter::ReadTiming, divisors, w)?;
            }
            for ch in 0..2 {
                if mask & (1 << ch) != 0 {
                    self.registers.channel[ch]
                        .error_control
                        .modify(ErrorControl::CHUNKS.val(0xff));
                }
            }
            self.final_read_measurement(s, ctx, mask, rank, MarginParameter::ReadTiming, delay)?;
        }
        Ok(())
    }
    fn read_voltage_2d_body(
        &self,
        s: &mut CalibrationState,
        ctx: &CommandContext<'_>,
        input: u8,
        cfg: ReadVoltage2d,
        w: &mut ReadCenteringWorkspace,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        if !(1..=31).contains(&cfg.loop_count) {
            return Err(Error::InvalidPolicy);
        }
        self.setup_basic_va_test(
            s,
            input,
            cfg.loop_count,
            8,
            StopOnError::AllByteGroups,
            delay,
        )?;
        let points = if cfg.two_dimensional { 3 } else { 1 };
        // Physical rank-by-rank measurements preserve rank-specific RX Vref
        // offsets instead of the native rank0 aggregation/fallback. The shared
        // host rd_vref remains the common baseline; offsets are live per rank.
        for rank in 0..4 {
            let mask = self.select_read_rank(input, rank)?;
            if mask == 0 {
                continue;
            }
            if cfg.reset_per_bit {
                self.reset_bits(s, mask, rank, PerBitParameter::ReadVoltage)?;
            }
            for ch in 0..2 {
                if mask & (1 << ch) != 0 {
                    for b in 0..8 {
                        let rx = &self.registers.data_lane[b][ch].rx_train[rank];
                        s.training.ch[ch].rx_dqs_p[rank][b] = rx.read(RxTrain::DQS_P) as u8;
                        s.training.ch[ch].rx_dqs_n[rank][b] = rx.read(RxTrain::DQS_N) as u8;
                        let raw = rx.read(RxTrain::VREF) as i16;
                        let value = if raw & 32 != 0 { raw - 64 } else { raw };
                        s.training.ch[ch].rx_vref_rank_offset[rank][b] =
                            (value - i16::from(s.training.ch[ch].rd_vref[b])) as i8;
                    }
                }
            }
            // Derive scaling from real current timing margins, not zero/stale
            // fixtures. All timing sweeps restore the entry point afterwards.
            self.centering_measure(s, ctx, mask, rank, MarginParameter::ReadTiming, delay)?;
            for ch in 0..2 {
                if mask & (1 << ch) != 0 {
                    for b in 0..8 {
                        let m = &s.margin_results[ch][rank][b];
                        if m[2] == 0 || m[3] == 0 {
                            return Err(failed(ch, rank, b));
                        }
                        w.scale[ch][b] = if cfg.two_dimensional {
                            read_voltage_timing_scale(m[2], m[3])
                        } else {
                            1
                        };
                    }
                }
            }
            w.byte_sum = [[0; 8]; 2];
            for t in 0..points {
                for ch in 0..2 {
                    if mask & (1 << ch) != 0 {
                        for b in 0..8 {
                            self.centering_offset(
                                ch,
                                rank,
                                b,
                                TIME_POINTS[t] * w.scale[ch][b] as i16 / 12,
                                0,
                            )?;
                        }
                    }
                }
                self.centering_measure(s, ctx, mask, rank, MarginParameter::ReadVoltage, delay)?;
                for ch in 0..2 {
                    if mask & (1 << ch) != 0 {
                        for b in 0..8 {
                            let m = &s.margin_results[ch][rank][b];
                            if m[0] == 0 || m[1] == 0 {
                                return Err(failed(ch, rank, b));
                            }
                            w.eyes[t][ch][b] = [m[0], m[1]];
                            w.byte_sum[ch][b] += i64::from(m[1]) - i64::from(m[0]);
                        }
                    }
                }
            }
            for ch in 0..2 {
                if mask & (1 << ch) != 0 {
                    for b in 0..8 {
                        let shift = rounded(w.byte_sum[ch][b], 20 * points as i64)?;
                        let l = &self.registers.data_lane[b][ch];
                        let raw = l.rx_train[rank].read(RxTrain::VREF) as i16;
                        let baseline = if raw & 32 != 0 { raw - 64 } else { raw };
                        let value = (baseline + shift / 2).clamp(-31, 31);
                        l.rx_train[rank].modify(RxTrain::VREF.val(value as u32 & 63));
                        s.training.ch[ch].rx_vref_rank_offset[rank][b] =
                            (value - i16::from(s.training.ch[ch].rd_vref[b])) as i8;
                        self.centering_offset(ch, rank, b, 0, 0)?;
                    }
                }
            }
            if cfg.per_bit {
                w.bit_sum = [[[0; 8]; 8]; 2];
                s.dq_pat_lc = 1;
                for ch in 0..2 {
                    if mask & (1 << ch) != 0 {
                        self.set_soe(ch, StopOnError::AllLanes)?;
                    }
                }
                for t in 0..points {
                    for ch in 0..2 {
                        if mask & (1 << ch) != 0 {
                            for b in 0..8 {
                                self.centering_offset(
                                    ch,
                                    rank,
                                    b,
                                    TIME_POINTS[t] * w.scale[ch][b] as i16 / 12,
                                    0,
                                )?;
                            }
                        }
                    }
                    self.centering_measure(
                        s,
                        ctx,
                        mask,
                        rank,
                        MarginParameter::ReadVoltage,
                        delay,
                    )?;
                    for ch in 0..2 {
                        for b in 0..8 {
                            w.eyes[t][ch][b] = [
                                s.margin_results[ch][rank][b][0],
                                s.margin_results[ch][rank][b][1],
                            ];
                        }
                    }
                    self.dt_get_margin_bit(
                        s,
                        mask,
                        rank,
                        PerBitParameter::ReadVoltage,
                        t,
                        w,
                        delay,
                    )?;
                    for ch in 0..2 {
                        if mask & (1 << ch) != 0 {
                            for b in 0..8 {
                                for bit in 0..8 {
                                    let m = w.bit_eyes[ch][b][bit];
                                    w.bit_sum[ch][b][bit] += i64::from(m[0]) + i64::from(m[1]) - 16;
                                }
                            }
                        }
                    }
                }
                self.apply_bit_centers(
                    s,
                    mask,
                    rank,
                    PerBitParameter::ReadVoltage,
                    [[points as i64; 8]; 2],
                    w,
                )?;
            }
            self.final_read_measurement(s, ctx, mask, rank, MarginParameter::ReadVoltage, delay)?;
        }
        Ok(())
    }
    /// Bounded read-only timing seam; no generic write-2D dispatch branch.
    pub fn dt_centering_2d_rdt(
        &self,
        s: &mut CalibrationState,
        ctx: &CommandContext<'_>,
        mask: u8,
        cfg: ReadTiming2d,
        work: &mut ReadCenteringWorkspace,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        if !(2..=31).contains(&cfg.loop_count) {
            return Err(Error::InvalidPolicy);
        }
        let ranks = self.begin_read_centering(s, ctx, mask, work)?;
        let measured = self.read_timing_2d_body(s, ctx, mask, cfg, work, delay);
        self.finish_read_centering(s, ranks, work, measured, delay)
    }
    pub fn meminit_read_voltage_centering_2d(
        &self,
        s: &mut CalibrationState,
        ctx: &CommandContext<'_>,
        mask: u8,
        cfg: ReadVoltage2d,
        work: &mut ReadCenteringWorkspace,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        if !(1..=31).contains(&cfg.loop_count) {
            return Err(Error::InvalidPolicy);
        }
        let ranks = self.begin_read_centering(s, ctx, mask, work)?;
        let measured = self.read_voltage_2d_body(s, ctx, mask, cfg, work, delay);
        self.finish_read_centering(s, ranks, work, measured, delay)
    }
    fn dt_perbit_deskew(
        &self,
        s: &mut CalibrationState,
        ctx: &CommandContext<'_>,
        mask: u8,
        clock: Clock,
        mode: TrainingMode,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        let limit = (u32::from(margin_limit(
            s,
            MarginKind::ReadTiming,
            LimitKind::Power,
            clock,
            mode,
        )?) + u32::from(margin_limit(
            s,
            MarginKind::ReadTiming,
            LimitKind::Upm,
            clock,
            mode,
        )?)) / 2;
        for ch in 0..2 {
            if mask & (1 << ch) == 0 {
                continue;
            }
            let ranks = rank_mask(self.dimms, ch)?;
            for b in 0..8 {
                let mut min = u32::MAX;
                for r in 0..4 {
                    if ranks & (1 << r) != 0 {
                        min =
                            min.min(s.margin_results[ch][r][b][2] + s.margin_results[ch][r][b][3]);
                    }
                }
                if min <= limit {
                    continue;
                }
                for r in 0..4 {
                    if ranks & (1 << r) != 0 {
                        let h = &mut s.training.ch[ch];
                        if h.rx_dqs_p[r][b] < 8 || h.rx_dqs_n[r][b] < 8 {
                            return Err(failed(ch, r, b));
                        }
                        h.rx_dqs_p[r][b] -= 8;
                        h.rx_dqs_n[r][b] -= 8;
                        h.rank[r].rd_dqs_dly[b] = h.rx_dqs_p[r][b];
                        self.registers.data_lane[b][ch].rx_train[r].modify(
                            RxTrain::DQS_P.val(u32::from(h.rx_dqs_p[r][b]))
                                + RxTrain::DQS_N.val(u32::from(h.rx_dqs_n[r][b])),
                        );
                        self.program_centering_per_bit(
                            s,
                            PerBitParameter::ReadTiming,
                            RankLane {
                                channel: ch,
                                rank: r,
                                lane: b,
                            },
                            0,
                            true,
                        )?;
                    }
                }
                self.registers.data_lane[b][ch]
                    .control2
                    .modify(DataControl2::RX_DESKEW_DISABLE::SET);
            }
        }
        // Deskew disable is a real changed hardware point and must be measured.
        for rank in 0..4 {
            let selected = self.select_read_rank(mask, rank)?;
            if selected != 0 {
                self.final_read_measurement(
                    s,
                    ctx,
                    selected,
                    rank,
                    MarginParameter::ReadTiming,
                    delay,
                )?;
            }
        }
        Ok(())
    }
    /// Native pass order: nominal per-bit/reset, measured deskew decision,
    /// byte 2D with independently selected positive/negative duty-cycle eyes.
    pub fn meminit_read_timing_centering_2d(
        &self,
        s: &mut CalibrationState,
        ctx: &CommandContext<'_>,
        clock: Clock,
        mode: TrainingMode,
        work: &mut ReadCenteringWorkspace,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        let mask = channels(cold_ranks(self, ctx.mode)?);
        // Resolve mutable policy before destructive writes. Its frequency gates
        // already use Clock::frequency_bin, never rounded SPD inventory MT/s.
        margin_limit(s, MarginKind::ReadTiming, LimitKind::Upm, clock, mode)?;
        margin_limit(s, MarginKind::ReadTiming, LimitKind::Power, clock, mode)?;
        let ranks = self.begin_read_centering(s, ctx, mask, work)?;
        let measured = (|| {
            self.read_timing_2d_body(
                s,
                ctx,
                mask,
                ReadTiming2d {
                    per_bit: true,
                    duty_cycle: false,
                    reset_per_bit: true,
                    loop_count: 15,
                    two_dimensional: false,
                },
                work,
                delay,
            )?;
            self.dt_perbit_deskew(s, ctx, mask, clock, mode, delay)?;
            self.read_timing_2d_body(
                s,
                ctx,
                mask,
                ReadTiming2d {
                    per_bit: false,
                    duty_cycle: true,
                    reset_per_bit: false,
                    loop_count: 15,
                    two_dimensional: true,
                },
                work,
                delay,
            )
        })();
        self.finish_read_centering(s, ranks, work, measured, delay)
    }
    /// Native voltage wrapper: nominal per-bit LC15, optional true physical
    /// 2DPC byte2D LC10, then byte2D LC15. Not a whole-training dispatcher.
    pub fn meminit_centering_2d(
        &self,
        s: &mut CalibrationState,
        ctx: &CommandContext<'_>,
        work: &mut ReadCenteringWorkspace,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        let mask = channels(cold_ranks(self, ctx.mode)?);
        let ranks = self.begin_read_centering(s, ctx, mask, work)?;
        let two_dpc = self.dimms.iter().any(|ch| ch.iter().all(Option::is_some));
        let measured = (|| {
            self.read_voltage_2d_body(
                s,
                ctx,
                mask,
                ReadVoltage2d {
                    per_bit: true,
                    reset_per_bit: true,
                    loop_count: 15,
                    two_dimensional: false,
                },
                work,
                delay,
            )?;
            if two_dpc {
                self.read_voltage_2d_body(
                    s,
                    ctx,
                    mask,
                    ReadVoltage2d {
                        per_bit: false,
                        reset_per_bit: false,
                        loop_count: 10,
                        two_dimensional: true,
                    },
                    work,
                    delay,
                )?;
            }
            self.read_voltage_2d_body(
                s,
                ctx,
                mask,
                ReadVoltage2d {
                    per_bit: false,
                    reset_per_bit: false,
                    loop_count: 15,
                    two_dimensional: true,
                },
                work,
                delay,
            )
        })();
        self.finish_read_centering(s, ranks, work, measured, delay)
    }
}

#[cfg(test)]
#[path = "read_centering_tests.rs"]
mod tests;
