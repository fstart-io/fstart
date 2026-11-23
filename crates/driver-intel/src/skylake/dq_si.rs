// SPDX-License-Identifier: MIT OR Apache-2.0
//! Native opt_param.c DDR4 DIMM/DQ SI consumers. Callable seams, not a dispatcher.
//! All traffic is destructive: readable setup is restored but FIFO ownership is
//! consumed. Caller allocates workspaces directly in CAR, not on a small stack.
use super::{
    Error, TrainingStep,
    ber_margin::{BerSweep, MarginParameter},
    command_training::CommandContext,
    cpu::Package,
    io_test::{DqPattern, StopOnError},
    jedec::vref_code,
    opt_param::{
        Grid, GridMode, OptHost, OptParameter, OptResult, OptSweep, OptWorkspace, opt_param_limits,
    },
    phy::{PhyPolicy, force_rcomp, rx_vref_for_clock},
    phy_registers::DataComp1,
    power_model::{LimitKind, MarginKind, TrainingMode, margin_limit},
    read_centering::{ReadSnapshot, ReadTiming2d, ReadVoltage2d},
    registers::{ReutControl, ReutSequence},
    reut::{Engine, RankLane},
    reut_registers::CadbControl,
    timings::{Clock, FrequencyBin},
    training::{PassingWindow, channels, cold_ranks},
    training_state::CalibrationState,
    vref_training::vref_bounds,
    write_centering::{WriteCenteringWorkspace, WriteSnapshot},
};
use embedded_hal::delay::DelayNs;
use tock_registers::interfaces::{Readable, Writeable};

#[derive(Debug, Clone, Copy)]
pub struct DqSiPolicy {
    pub clock: Clock,
    pub package: Package,
    pub phy: PhyPolicy,
    pub mode: TrainingMode,
}
/// Independent outer snapshots survive every nested optimizer/centering call.
pub struct DqSiWorkspace {
    read: ReadSnapshot,
    write: WriteSnapshot,
    host: Option<OptHost>,
    host_comp: [[u32; 9]; 2],
    receive_margins: [[[[u32; 2]; 9]; 4]; 2],
    comp2: u32,
    misc: u32,
}
impl DqSiWorkspace {
    /// Typed allocation image only; not measured calibration or training success.
    pub const INITIAL: Self = Self {
        read: ReadSnapshot::ZERO,
        write: WriteSnapshot::ZERO,
        host: None,
        host_comp: [[0; 9]; 2],
        receive_margins: [[[[0; 2]; 9]; 4]; 2],
        comp2: 0,
        misc: 0,
    };
}

impl Default for DqSiWorkspace {
    fn default() -> Self {
        Self::INITIAL
    }
}
pub(crate) fn stopped(e: &Engine<'_>, s: &CalibrationState) -> Result<(), Error> {
    if s.dq_pat == DqPattern::Cadb as u8
        || e.registers.reut_global_control.is_set(ReutControl::START)
        || (0..2).any(|ch| {
            e.registers.reut_control[ch].is_set(ReutControl::START)
                || e.registers.channel[ch]
                    .cadb_control
                    .is_set(CadbControl::DESELECT)
                || e.registers.reut_sequence[ch][0].read(ReutSequence::INIT_MODE) == 2
        })
    {
        return Err(Error::InvalidReutConfig);
    }
    Ok(())
}
fn no_eye(ch: usize, r: usize, b: usize) -> Error {
    Error::TrainingFailed {
        step: TrainingStep::BerMargin,
        channel: ch as u8,
        rank: r as u8,
        lane: b as u8,
    }
}
fn timing(lc: u8) -> ReadTiming2d {
    ReadTiming2d {
        per_bit: false,
        duty_cycle: false,
        reset_per_bit: false,
        loop_count: lc,
        two_dimensional: false,
    }
}
fn voltage(lc: u8, two: bool) -> ReadVoltage2d {
    ReadVoltage2d {
        per_bit: false,
        reset_per_bit: false,
        loop_count: lc,
        two_dimensional: two,
    }
}
fn grid(params: &[OptParameter], p: DqSiPolicy, mode: GridMode) -> Result<Grid, Error> {
    if !(1..=2).contains(&params.len()) {
        return Err(Error::InvalidPolicy);
    }
    let mut g = Grid {
        start: [0; 2],
        lengths: [1; 2],
        dimensions: params.len(),
        mode,
    };
    for (i, &param) in params.iter().enumerate() {
        let [lo, hi] = opt_param_limits(param, p.clock, p.mode)?;
        g.start[i] = lo;
        g.lengths[i] = (i16::from(hi) - i16::from(lo) + 1) as u16;
    }
    g.validate_axes()?;
    Ok(g)
}
impl Engine<'_> {
    fn begin_dq_si(
        &self,
        s: &CalibrationState,
        ctx: &CommandContext<'_>,
        p: DqSiPolicy,
        w: &mut DqSiWorkspace,
    ) -> Result<[u8; 2], Error> {
        let ranks = cold_ranks(self, ctx.mode)?;
        self.validate_write_centering(s, ctx, channels(ranks))?;
        if p.clock.tck_fs == 0 {
            return Err(Error::InvalidPolicy);
        }
        // Validate the actual electrical model before any destructive operation.
        rx_vref_for_clock(self.dimms, p.clock, &p.phy, Some(&s.saved_mr))?;
        w.host = Some(self.opt_host_sync(s, 8)?);
        self.read_snapshot(s, &mut w.read)?;
        w.read.selected_channels = channels(ranks);
        self.write_snapshot(s, &mut w.write)?;
        for ch in 0..2 {
            w.host_comp[ch] = s.training.ch[ch].data_comp_offset;
            for r in 0..4 {
                for b in 0..9 {
                    w.receive_margins[ch][r][b].copy_from_slice(&s.margin_results[ch][r][b][8..10]);
                }
            }
        }
        w.comp2 = self.registers.comp_control2.get();
        w.misc = self.registers.ddr_misc.get();
        Ok(ranks)
    }
    fn restore_dq_si(
        &self,
        s: &mut CalibrationState,
        ranks: [u8; 2],
        w: &DqSiWorkspace,
        rollback: bool,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        let mut result = Ok(());
        if rollback {
            let h = w.host.as_ref().ok_or(Error::InvalidPolicy)?;
            s.saved_mr = h.mr;
            self.registers.comp_control0.set(h.comp_control0);
            self.registers.comp_control1.set(h.comp_control1);
            self.registers.comp_control2.set(w.comp2);
            for ch in 0..2 {
                s.training.ch[ch].data_comp_offset = w.host_comp[ch];
                for b in 0..9 {
                    let l = &self.registers.data_lane[b][ch];
                    l.offset_comp.set(h.comp[ch][b]);
                    l.control1.set(h.control1[ch][b]);
                    l.control2.set(h.control2[ch][b]);
                    for r in 0..4 {
                        s.margin_results[ch][r][b][8..10]
                            .copy_from_slice(&w.receive_margins[ch][r][b]);
                    }
                }
                for r in 0..4 {
                    if ranks[ch] & (1 << r) == 0 {
                        continue;
                    }
                    for mr in [1, 2, 5] {
                        let v = self.training_mrs(ch, r, mr, h.mr.rank[ch][r][mr as usize], delay);
                        if result.is_ok() {
                            result = v;
                        }
                    }
                }
            }
            let v = force_rcomp(self.registers, delay);
            if result.is_ok() {
                result = v;
            }
        }
        let read = self.read_restore(s, ranks, &w.read, rollback, delay);
        if result.is_ok() {
            result = read;
        }
        let write =
            self.restore_write_centering(s, channels(ranks), ranks, &w.write, rollback, delay);
        if result.is_ok() {
            result = write;
        }
        self.registers.ddr_misc.set(w.misc);
        result
    }
    fn finish_dq_si<T>(
        &self,
        s: &mut CalibrationState,
        ranks: [u8; 2],
        w: &DqSiWorkspace,
        measured: Result<T, Error>,
        delay: &mut impl DelayNs,
    ) -> Result<T, Error> {
        let cleanup = self.restore_dq_si(s, ranks, w, measured.is_err(), delay);
        if let Err(e) = cleanup {
            if measured.is_ok() {
                let _ = self.restore_dq_si(s, ranks, w, true, delay);
            }
            return Err(e);
        }
        measured
    }
    fn dq_opt(
        &self,
        s: &mut CalibrationState,
        ctx: &CommandContext<'_>,
        p: DqSiPolicy,
        params: &[OptParameter],
        tests: &[MarginParameter],
        g: Grid,
        scale: [u8; 5],
        upm: [bool; 5],
        lc: u8,
        rr: Option<DqPattern>,
        gb: i16,
        skip: bool,
        mask: u8,
        ranks: u8,
        opt: &mut OptWorkspace,
        delay: &mut impl DelayNs,
    ) -> Result<OptResult, Error> {
        self.train_ddr_opt_param(
            s,
            ctx,
            p.clock,
            p.package,
            &p.phy,
            &OptSweep {
                channels: mask,
                ranks,
                parameters: params,
                grid: g,
                tests,
                scale,
                upm_optimize: upm,
                loop_count: lc,
                repeats: 1,
                skip_update: skip,
                read_read: rr,
                guard_band: gb,
                mode: p.mode,
            },
            opt,
            delay,
        )
    }
    /// sense_amp.c:375, saved-MR branch; not a measurement or update-host flag.
    /// The source's `false` selects live saved MRs instead of pre-JEDEC tables.
    pub fn meminit_set_default_rx_vref_ddr4(
        &self,
        s: &mut CalibrationState,
        ctx: &CommandContext<'_>,
        clock: Clock,
        phy: &PhyPolicy,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        self.set_default_rx_vref_ddr4_channels(
            s,
            ctx,
            clock,
            phy,
            channels(cold_ranks(self, ctx.mode)?),
            delay,
        )
    }
    fn set_default_rx_vref_ddr4_channels(
        &self,
        s: &mut CalibrationState,
        ctx: &CommandContext<'_>,
        clock: Clock,
        phy: &PhyPolicy,
        mask: u8,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        let ranks = cold_ranks(self, ctx.mode)?;
        stopped(self, s)?;
        if ctx.policy.sdram_lanes != 8 || mask == 0 || mask & !channels(ranks) != 0 {
            return Err(Error::InvalidPolicy);
        }
        let values = rx_vref_for_clock(self.dimms, clock, phy, Some(&s.saved_mr))?;
        for ch in 0..2 {
            if mask & (1 << ch) == 0 {
                continue;
            }
            for b in 0..8 {
                s.training.ch[ch].rd_vref[b] = values[ch];
                for r in 0..4 {
                    if ranks[ch] & (1 << r) != 0 {
                        s.training.ch[ch].rx_vref_rank_offset[r][b] = 0;
                        self.set_rx_vref(
                            RankLane {
                                channel: ch,
                                rank: r,
                                lane: b,
                            },
                            values[ch],
                        )?;
                    }
                }
            }
            for r in 0..4 {
                if ranks[ch] & (1 << r) != 0 {
                    self.pulse_rx(ch, r, delay)?;
                }
            }
        }
        Ok(())
    }
    /// Native contiguous 1D read/write voltage walker. Actual failing-byte
    /// feedback, per-physical-rank WRV and joint all-rank RDV; final BER remeasure.
    fn dq_time_centering_1d_body(
        &self,
        s: &mut CalibrationState,
        ctx: &CommandContext<'_>,
        p: DqSiPolicy,
        mask: u8,
        param: MarginParameter,
        lc: u8,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        let ranks = cold_ranks(self, ctx.mode)?;
        if !matches!(
            param,
            MarginParameter::ReadVoltage | MarginParameter::WriteVoltage
        ) || mask == 0
            || mask & !channels(ranks) != 0
            || !(1..=31).contains(&lc)
        {
            return Err(Error::InvalidPolicy);
        }
        self.setup_wdb(mask)?;
        self.setup_basic_va_test(s, mask, lc, 8, StopOnError::Never, delay)?;
        let write = param == MarginParameter::WriteVoltage;
        let min = if p.clock.frequency_bin() >= FrequencyBin::Mts2400 {
            4
        } else {
            8
        };
        for rank in 0..4 {
            let mut selected = [0; 2];
            for ch in 0..2 {
                if mask & (1 << ch) == 0 {
                    continue;
                }
                selected[ch] = if write {
                    ranks[ch] & (1 << rank)
                } else {
                    ranks[ch]
                };
                if selected[ch] != 0 {
                    self.program_rank_map(ch, selected[ch])?;
                    if write {
                        let code = self.write_vref_offset(s, ch, rank, 0, delay)?;
                        s.saved_mr.rank[ch][rank][6] = (s.saved_mr.rank[ch][rank][6] & !255) | code;
                        s.ddr4_pda_mr6[ch][rank].fill(code as u8);
                    } else {
                        s.training.ch[ch].rd_vref.fill(0);
                        s.training.ch[ch].rx_vref_rank_offset = [[0; 9]; 4];
                        self.program_read_vref(ch, 0)?;
                    }
                }
            }
            for ch in 0..2 {
                if ranks[ch] != 0 {
                    self.program_rank_map(ch, selected[ch])?;
                }
            }
            let active = channels(selected);
            if active == 0 {
                continue;
            }
            let baseline = self.ber_baseline(ctx)?;
            let mut windows = [[PassingWindow::EMPTY; 8]; 2];
            let bound = if write { 36 } else { 63 };
            for d in -bound..=bound {
                for ch in 0..2 {
                    if selected[ch] == 0 {
                        continue;
                    }
                    if write {
                        let (lo, hi) = vref_bounds(s.ddr4_tx_vref_range[ch])?;
                        if !(lo..=hi).contains(&d) {
                            continue;
                        }
                        self.write_vref_offset(s, ch, rank, d, delay)?;
                    } else {
                        // Keep the native seven-bit half-code sweep resolution;
                        // RXTRAIN itself is only six bits and would quantize it.
                        self.ber_change_margin(
                            s,
                            &baseline,
                            ctx,
                            param,
                            d,
                            ch,
                            selected[ch].trailing_zeros() as usize,
                            0xff,
                            false,
                            delay,
                        )?;
                    }
                }
                self.run_io_test(s, active, delay)?;
                for ch in 0..2 {
                    if selected[ch] == 0 {
                        continue;
                    }
                    if write {
                        let (lo, hi) = vref_bounds(s.ddr4_tx_vref_range[ch])?;
                        if !(lo..=hi).contains(&d) {
                            continue;
                        }
                    }
                    let errors = self.read_errors(ch)?;
                    for b in 0..8 {
                        let pass = errors.failing_bits_per_byte[b] == 0
                            && errors.counters[b] == 0
                            && (errors.context >> (32 + b) & 1) == 0;
                        windows[ch][b].sample(d, 1, pass);
                    }
                }
            }
            if !write {
                for ch in 0..2 {
                    if selected[ch] != 0 {
                        self.ber_change_margin(
                            s,
                            &baseline,
                            ctx,
                            param,
                            0,
                            ch,
                            selected[ch].trailing_zeros() as usize,
                            0xff,
                            false,
                            delay,
                        )?;
                    }
                }
            }
            for ch in 0..2 {
                if selected[ch] == 0 {
                    continue;
                }
                let mut codes = [0; 8];
                for b in 0..8 {
                    let win = windows[ch][b];
                    if win.width() < min {
                        return Err(no_eye(ch, rank, b));
                    }
                    let center = win.center().ok_or(no_eye(ch, rank, b))?;
                    if write {
                        codes[b] = vref_code(i32::from(center), s.ddr4_tx_vref_range[ch])? as u8;
                    } else {
                        let v = (center / 2) as i8;
                        s.training.ch[ch].rd_vref[b] = v;
                        for r in 0..4 {
                            if selected[ch] & (1 << r) != 0 {
                                self.set_rx_vref(
                                    RankLane {
                                        channel: ch,
                                        rank: r,
                                        lane: b,
                                    },
                                    v,
                                )?;
                            }
                        }
                    }
                }
                if write {
                    self.apply_pda_centers(s, ch, rank, codes, delay)?;
                } else {
                    for r in 0..4 {
                        if selected[ch] & (1 << r) != 0 {
                            self.pulse_rx(ch, r, delay)?;
                        }
                    }
                }
            }
            if !write {
                break;
            }
        }
        if write {
            s.ddr4_pda_enable = true;
        }
        self.dq_remeasure(s, ctx, mask, 15, &[param], delay)?;
        Ok(())
    }
    pub fn dq_time_centering_1d(
        &self,
        s: &mut CalibrationState,
        ctx: &CommandContext<'_>,
        p: DqSiPolicy,
        mask: u8,
        param: MarginParameter,
        lc: u8,
        w: &mut DqSiWorkspace,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        if !matches!(
            param,
            MarginParameter::ReadVoltage | MarginParameter::WriteVoltage
        ) || !(1..=31).contains(&lc)
        {
            return Err(Error::InvalidPolicy);
        }
        let actual = cold_ranks(self, ctx.mode)?;
        if mask == 0 || mask & !channels(actual) != 0 {
            return Err(Error::InvalidPolicy);
        }
        let ranks = self.begin_dq_si(s, ctx, p, w)?;
        let result = self.dq_time_centering_1d_body(s, ctx, p, mask, param, lc, delay);
        self.finish_dq_si(s, ranks, w, result, delay)
    }
    fn dq_remeasure(
        &self,
        s: &mut CalibrationState,
        ctx: &CommandContext<'_>,
        mask: u8,
        rank_mask: u8,
        tests: &[MarginParameter],
        delay: &mut impl DelayNs,
    ) -> Result<[[u16; 2]; 4], Error> {
        let actual = cold_ranks(self, ctx.mode)?;
        self.setup_wdb(mask)?;
        self.setup_basic_va_test(s, mask, 15, 8, StopOnError::Never, delay)?;
        let mut widths = [[u16::MAX; 2]; 4];
        for ch in 0..2 {
            let ranks = actual[ch] & rank_mask;
            if mask & (1 << ch) == 0 || ranks == 0 {
                continue;
            }
            for rank in 0..4 {
                if ranks & (1 << rank) == 0 {
                    continue;
                }
                self.program_rank_map(ch, 1 << rank)?;
                for (i, &test) in tests.iter().enumerate() {
                    self.get_ber_margin_byte(
                        s,
                        ctx,
                        BerSweep {
                            parameter: test,
                            channels: 1 << ch,
                            rank_in: if test == MarginParameter::WriteVoltage {
                                1 << rank
                            } else {
                                rank as u8
                            },
                            rank_rx: if test == MarginParameter::WriteVoltage {
                                1 << rank
                            } else {
                                rank as u8
                            },
                            per_channel: false,
                            interpolate: true,
                            max_margin: if test == MarginParameter::WriteVoltage {
                                36
                            } else if test == MarginParameter::ReadVoltage {
                                63
                            } else {
                                31
                            },
                            interior: false,
                            byte_map: None,
                        },
                        &mut [0; 4],
                        delay,
                    )?;
                    for b in 0..8 {
                        let edges = &s.margin_results[ch][rank][b]
                            [test.result_type() * 2..test.result_type() * 2 + 2];
                        if edges.contains(&0) {
                            return Err(no_eye(ch, rank, b));
                        }
                        let width = u16::try_from(
                            edges[0].checked_add(edges[1]).ok_or(Error::InvalidPolicy)?,
                        )
                        .map_err(|_| Error::InvalidPolicy)?;
                        widths[i][ch] = widths[i][ch].min(width);
                    }
                }
            }
        }
        Ok(widths)
    }
    /// Native write_eq_ds_training / DDR4 late entry (early=false).
    pub fn meminit_write_eq_ds_training(
        &self,
        s: &mut CalibrationState,
        ctx: &CommandContext<'_>,
        p: DqSiPolicy,
        opt: &mut OptWorkspace,
        w: &mut DqSiWorkspace,
        delay: &mut impl DelayNs,
    ) -> Result<OptResult, Error> {
        let params = [OptParameter::TxEq, OptParameter::WriteDrive];
        let g = grid(&params, p, GridMode::ChessOdd)?;
        let ranks = self.begin_dq_si(s, ctx, p, w)?;
        let result = self.dq_opt(
            s,
            ctx,
            p,
            &params,
            &WRITE_TESTS,
            g,
            [1, 2, u8::from(p.mode == TrainingMode::Power), 0, 0],
            [false; 5],
            16,
            None,
            0,
            false,
            channels(ranks),
            15,
            opt,
            delay,
        );
        self.finish_dq_si(s, ranks, w, result, delay)
    }
    pub fn meminit_write_slew_rate(
        &self,
        s: &mut CalibrationState,
        ctx: &CommandContext<'_>,
        p: DqSiPolicy,
        opt: &mut OptWorkspace,
        w: &mut DqSiWorkspace,
        delay: &mut impl DelayNs,
    ) -> Result<OptResult, Error> {
        let params = [OptParameter::Slew];
        let g = grid(&params, p, GridMode::CustomSlew)?;
        let ranks = self.begin_dq_si(s, ctx, p, w)?;
        let result = self.dq_opt(
            s,
            ctx,
            p,
            &params,
            &WRITE_TESTS,
            g,
            [1, 2, 0, 0, 0],
            [false; 5],
            15,
            None,
            0,
            false,
            channels(ranks),
            15,
            opt,
            delay,
        );
        self.finish_dq_si(s, ranks, w, result, delay)
    }
    pub fn meminit_write_ds_up_down_training(
        &self,
        s: &mut CalibrationState,
        ctx: &CommandContext<'_>,
        p: DqSiPolicy,
        opt: &mut OptWorkspace,
        w: &mut DqSiWorkspace,
        center: &mut WriteCenteringWorkspace,
        delay: &mut impl DelayNs,
    ) -> Result<OptResult, Error> {
        let params = [
            OptParameter::WriteDriveUpCoarse,
            OptParameter::WriteDriveDownCoarse,
        ];
        let g = grid(&params, p, GridMode::ChessOdd)?;
        let ranks = self.begin_dq_si(s, ctx, p, w)?;
        let result = (|| {
            let mut out = self.dq_opt(
                s,
                ctx,
                p,
                &params,
                &WRITE_TESTS,
                g,
                [1, 1, 0, 0, 0],
                [false, true, false, false, false],
                16,
                None,
                0,
                false,
                channels(ranks),
                15,
                opt,
                delay,
            )?;
            self.meminit_write_voltage_centering_2d(s, ctx, p.clock, center, delay)?;
            out.worst_width =
                self.dq_remeasure(s, ctx, channels(ranks), 15, &WRITE_TESTS, delay)?;
            Ok(out)
        })();
        self.finish_dq_si(s, ranks, w, result, delay)
    }
    pub fn meminit_read_eq_training(
        &self,
        s: &mut CalibrationState,
        ctx: &CommandContext<'_>,
        p: DqSiPolicy,
        opt: &mut OptWorkspace,
        w: &mut DqSiWorkspace,
        delay: &mut impl DelayNs,
    ) -> Result<[Option<OptResult>; 4], Error> {
        let params = [OptParameter::RxEq];
        let g = grid(&params, p, GridMode::Full)?;
        let ranks = self.begin_dq_si(s, ctx, p, w)?;
        let result = (|| {
            let mut out = [None; 4];
            for rank in 0..4 {
                if (ranks[0] | ranks[1]) & (1 << rank) == 0 {
                    continue;
                }
                out[rank] = Some(self.dq_opt(
                    s,
                    ctx,
                    p,
                    &params,
                    &READ_TESTS,
                    g,
                    [1, 2, 0, 0, 0],
                    [false; 5],
                    15,
                    None,
                    0,
                    false,
                    channels(ranks),
                    1 << rank,
                    opt,
                    delay,
                )?);
            }
            Ok(out)
        })();
        self.finish_dq_si(s, ranks, w, result, delay)
    }
    pub fn meminit_read_odt_training(
        &self,
        s: &mut CalibrationState,
        ctx: &CommandContext<'_>,
        p: DqSiPolicy,
        opt: &mut OptWorkspace,
        w: &mut DqSiWorkspace,
        center: &mut WriteCenteringWorkspace,
        delay: &mut impl DelayNs,
    ) -> Result<OptResult, Error> {
        let params = [OptParameter::ReadOdt];
        let mut g = grid(&params, p, GridMode::Full)?;
        let up = self.registers.data_comp1.read(DataComp1::ODT_UP) as i16;
        let dn = self.registers.data_comp1.read(DataComp1::ODT_DOWN) as i16;
        let lo = i16::from(g.start[0]).max(-up.min(dn));
        let hi = (i16::from(g.start[0]) + g.lengths[0] as i16 - 1).min(63 - up.max(dn));
        if hi < lo {
            return Err(Error::InvalidPolicy);
        }
        g.start[0] = lo as i8;
        g.lengths[0] = (hi - lo + 1) as u16;
        let ranks = self.begin_dq_si(s, ctx, p, w)?;
        let result = (|| {
            let mut out = self.dq_opt(
                s,
                ctx,
                p,
                &params,
                &READ_TESTS,
                g,
                [
                    1,
                    2,
                    u8::from(p.mode == TrainingMode::Power && !p.phy.vtt_odt),
                    0,
                    0,
                ],
                [false; 5],
                17,
                Some(DqPattern::ReadRead),
                0,
                false,
                channels(ranks),
                15,
                opt,
                delay,
            )?;
            self.meminit_read_voltage_centering_2d(
                s,
                ctx,
                channels(ranks),
                voltage(15, true),
                &mut center.measured,
                delay,
            )?;
            self.meminit_data_time_centering_2d(
                s,
                ctx,
                channels(ranks),
                MarginParameter::ReadTiming,
                timing(15),
                center,
                delay,
            )?;
            out.worst_width = self.dq_remeasure(s, ctx, channels(ranks), 15, &READ_TESTS, delay)?;
            Ok(out)
        })();
        self.finish_dq_si(s, ranks, w, result, delay)
    }
    pub fn meminit_read_amplifier_power(
        &self,
        s: &mut CalibrationState,
        ctx: &CommandContext<'_>,
        p: DqSiPolicy,
        opt: &mut OptWorkspace,
        w: &mut DqSiWorkspace,
        center: &mut WriteCenteringWorkspace,
        delay: &mut impl DelayNs,
    ) -> Result<OptResult, Error> {
        let params = [OptParameter::RxBias, OptParameter::RxCb];
        let g = grid(&params, p, GridMode::Full)?;
        let ranks = self.begin_dq_si(s, ctx, p, w)?;
        let tests = [
            MarginParameter::ReadVoltage,
            MarginParameter::ReadTiming,
            MarginParameter::ReceiveEnableExtended,
        ];
        let result = (|| {
            let mut out = self.dq_opt(
                s,
                ctx,
                p,
                &params,
                &tests,
                g,
                [1, 2, 1, u8::from(p.mode == TrainingMode::Power), 0],
                [false; 5],
                15,
                None,
                1,
                false,
                channels(ranks),
                15,
                opt,
                delay,
            )?;
            self.meminit_data_time_centering_2d(
                s,
                ctx,
                channels(ranks),
                MarginParameter::ReadTiming,
                timing(15),
                center,
                delay,
            )?;
            out.worst_width = self.dq_remeasure(s, ctx, channels(ranks), 15, &tests, delay)?;
            Ok(out)
        })();
        self.finish_dq_si(s, ranks, w, result, delay)
    }
}
const READ_TESTS: [MarginParameter; 2] =
    [MarginParameter::ReadVoltage, MarginParameter::ReadTiming];
const WRITE_TESTS: [MarginParameter; 2] =
    [MarginParameter::WriteVoltage, MarginParameter::WriteTiming];

#[path = "dimm_odt.rs"]
mod dimm_odt;
pub use dimm_odt::{DimmOdtTest, DimmOdtWorkspace, MAX_DIMM_OPT_POINTS};
