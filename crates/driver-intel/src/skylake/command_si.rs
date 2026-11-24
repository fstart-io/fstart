// SPDX-License-Identifier: MIT OR Apache-2.0
//! Dedicated DDR4 CMD SI consumers of native/si_optimization.c and highspeed.c.
//! Exclusive stopped REUT ownership is required. These routines overwrite the
//! write-only CADB FIFO and WDB patterns. Readable control snapshots are restored,
//! but old pattern contents cannot be restored, so runtime identity is invalidated.
//! A later consumer must populate its patterns and configure its test before
//! running traffic (setup_wdb + test setup, or setup_cadb/test). Not a dispatcher.
use super::{
    Error, TrainingStep,
    ber_margin::{BerSweep, MarginParameter},
    command_training::{CommandContext, CommandIteration, PiSnapshot},
    cpu::Package,
    io_test::{DqPattern, StopOnError},
    opt_param::{OptWorkspace, sweep::TestSnapshot},
    opt_tradeoff::{TradeoffConfig, find_optimal_trade_off, running_average_2d},
    phy::{PhyPolicy, force_rcomp},
    phy_registers::{CommandComp, CommandCompOffset, CompControl0, CompControl1, FubControl},
    power_model::{
        LimitKind, MarginKind, Power, PowerModel, TrainingMode, margin_limit,
        normalize_power_to_margins,
    },
    registers::{ReutControl, ReutSequence},
    reut::{Engine, RankLane},
    reut_registers::CadbControl,
    timings::{Clock, Timings},
    training::channels,
    training_state::CalibrationState,
};
use embedded_hal::delay::DelayNs;
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};

/// Small additional caller-owned snapshot, alongside the shared 42,472-byte
/// OptWorkspace. No full CalibrationState clone or stack-local sweep matrices.
pub struct CommandSiWorkspace {
    pi: [Option<PiSnapshot>; 2],
    tests: [Option<TestSnapshot>; 2],
    controls: [[u32; 2]; 2],
    offsets: [[u32; 2]; 2],
    wdb_seeds: [[u32; 6]; 2],
    wdb_lmn: [u32; 2],
    cadb_seeds: [[u32; 3]; 2],
    cadb_lmn: [u32; 2],
    ctl: [u32; 2],
    vref: u32,
}
impl CommandSiWorkspace {
    pub const ZERO: Self = Self {
        pi: [None, None],
        tests: [None, None],
        controls: [[0; 2]; 2],
        offsets: [[0; 2]; 2],
        wdb_seeds: [[0; 6]; 2],
        wdb_lmn: [0; 2],
        cadb_seeds: [[0; 3]; 2],
        cadb_lmn: [0; 2],
        ctl: [0; 2],
        vref: 0,
    };
    fn load(&mut self, e: &Engine<'_>, s: &CalibrationState) -> Result<(), Error> {
        for ch in 0..2 {
            self.pi[ch] = Some(PiSnapshot::load(e, s, ch));
            self.tests[ch] = Some(TestSnapshot::load(e, ch)?);
            self.controls[ch] = [
                e.registers.command_north[ch].control.get(),
                e.registers.command_south[ch].control.get(),
            ];
            self.offsets[ch] = [
                e.registers.command_north[ch].comp_offset.get(),
                e.registers.command_south[ch].comp_offset.get(),
            ];
            let c = &e.registers.channel[ch];
            for i in 0..3 {
                self.wdb_seeds[ch][i] = c.wdb_read_seed[i].get();
                self.wdb_seeds[ch][i + 3] = c.wdb_write_seed[i].get();
                self.cadb_seeds[ch][i] = c.cadb_seed[i].get();
            }
            self.wdb_lmn[ch] = c.wdb_lmn.get();
            self.cadb_lmn[ch] = c.cadb_lmn.get();
        }
        self.ctl = [
            e.registers.comp_control0.get(),
            e.registers.comp_control1.get(),
        ];
        self.vref = e.registers.data_lane[0][0].vref_adjust.get();
        Ok(())
    }
    fn restore_pi(
        &self,
        e: &Engine<'_>,
        s: &mut CalibrationState,
        ranks: [u8; 2],
        lanes: usize,
        margins: bool,
    ) -> Result<(), Error> {
        let mut result = Ok(());
        for ch in 0..2 {
            if ranks[ch] != 0 {
                let r = self.pi[ch]
                    .as_ref()
                    .ok_or(Error::InvalidPolicy)?
                    .restore(e, s, ch, ranks[ch], lanes, margins);
                result = merge(result, r);
            }
        }
        result
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandSiPoint {
    pub drive_or_stages: i8,
    pub tx_eq: u8,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandSiResult {
    pub range: (i8, i8),
    pub best: [Option<CommandSiPoint>; 2],
    pub global: i8,
    pub residual: [i8; 2],
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Sweep {
    DriveEq,
    Slew,
}
fn merge(a: Result<(), Error>, b: Result<(), Error>) -> Result<(), Error> {
    match (a, b) {
        (Err(Error::TrainingFailed { .. }), Err(e)) => Err(e),
        (Ok(()), b) => b,
        (a, _) => a,
    }
}
fn no_eye(ch: usize, rank: usize) -> Error {
    Error::TrainingFailed {
        step: TrainingStep::CommandTiming,
        channel: ch as u8,
        rank: rank as u8,
        lane: 0,
    }
}
/// `cmd_slew_rate_stages_to_vref`: phase/cycle type and rounded cell count.
fn cmd_slew_rate_stages_to_vref(clock: Clock, stages: i8) -> Result<u8, Error> {
    if clock.tck_fs == 0 || !(2..=8).contains(&stages) {
        return Err(Error::InvalidPolicy);
    }
    let qclk_ps = clock.tck_fs / 2000;
    let min_stages = (qclk_ps + 35) / 70;
    let stages = stages as u32;
    Ok(if stages > 16 || stages > min_stages {
        (stages.div_ceil(2) - 1) as u8
    } else {
        (stages - 1) as u8 | 16
    })
}
impl Engine<'_> {
    fn cmd_si_apply(
        &self,
        saved: &CommandSiWorkspace,
        sweep: Sweep,
        mask: u8,
        value: i8,
        eq: u8,
        clock: Clock,
        delay: &mut impl DelayNs,
    ) -> Result<u8, Error> {
        match sweep {
            Sweep::DriveEq => {
                if !(-6..=7).contains(&value) || eq > 1 {
                    return Err(Error::InvalidPolicy);
                }
                self.registers.comp_control0.set(saved.ctl[0]);
                self.registers
                    .comp_control0
                    .modify(CompControl0::CMD_DRIVE_VREF.val(u32::from(value as u8 & 15)));
            }
            Sweep::Slew => {
                let vref = cmd_slew_rate_stages_to_vref(clock, value)?;
                self.registers.comp_control1.set(saved.ctl[1]);
                self.registers.comp_control1.modify(
                    CompControl1::CMD_CELLS.val(u32::from(vref & 15))
                        + CompControl1::CMD_CYCLE.val(u32::from(vref >> 4)),
                );
            }
        }
        force_rcomp(self.registers, delay)?;
        if sweep == Sweep::DriveEq {
            for ch in 0..2 {
                if mask & (1 << ch) != 0 {
                    for reg in [
                        &self.registers.command_north[ch].control,
                        &self.registers.command_south[ch].control,
                    ] {
                        reg.modify(FubControl::CMD_TX_EQ.val(u32::from(eq)));
                    }
                }
            }
        }
        delay.delay_us(1);
        Ok(match sweep {
            Sweep::DriveEq => self.registers.command_comp.read(CommandComp::DRIVE_UP),
            Sweep::Slew => self.registers.command_comp.read(CommandComp::SLEW),
        } as u8)
    }
    /// `cmd_ds_find_range` / `probe_sr_start/stop`, bounded inward searches.
    fn cmd_si_find_range(
        &self,
        saved: &CommandSiWorkspace,
        sweep: Sweep,
        clock: Clock,
        delay: &mut impl DelayNs,
    ) -> Result<(i8, i8), Error> {
        let (min, max, gb) = if sweep == Sweep::DriveEq {
            (-6, 7, 3)
        } else {
            (2, 8, 2)
        };
        let mut start = max + 1;
        let mut stop = min - 1;
        for value in min..=max {
            let code = self.cmd_si_apply(saved, sweep, 0, value, 0, clock, delay)?;
            if (gb..=63 - gb).contains(&code) {
                start = value;
                break;
            }
        }
        if start <= max {
            for value in (start..=max).rev() {
                let code = self.cmd_si_apply(saved, sweep, 0, value, 0, clock, delay)?;
                if (gb..=63 - gb).contains(&code) {
                    stop = value;
                    break;
                }
            }
        }
        self.registers.comp_control0.set(saved.ctl[0]);
        self.registers.comp_control1.set(saved.ctl[1]);
        force_rcomp(self.registers, delay)?;
        if start > stop {
            Err(no_eye(0, 0))
        } else {
            Ok((start, stop))
        }
    }
    fn cmd_si_measure(
        &self,
        s: &mut CalibrationState,
        ctx: &CommandContext<'_>,
        ranks: [u8; 2],
        voltage: bool,
        max_vref: u8,
        delay: &mut impl DelayNs,
    ) -> Result<[u16; 2], Error> {
        let mask = channels(ranks);
        for ch in 0..2 {
            if ranks[ch] != 0 {
                self.program_rank_map(ch, ranks[ch])?;
            }
        }
        let mut measured = ranks;
        if voltage {
            // All actual physical ranks, with channel-local result slots.
            // This also handles ch1/rank2-only and shared CA Vref correctly.
            self.get_ber_margin_ch(
                s,
                ctx,
                BerSweep {
                    parameter: MarginParameter::CommandVoltage,
                    channels: mask,
                    rank_in: 0xff,
                    rank_rx: 0xff,
                    per_channel: true,
                    interpolate: false,
                    max_margin: max_vref,
                    interior: false,
                    byte_map: None,
                },
                &mut [0; 4],
                delay,
            )?;
        } else {
            // An analog no-eye rejects this channel at this grid point, not
            // the whole grid. Transport and restore errors stay fatal.
            for (ch, active) in measured.iter_mut().enumerate() {
                if *active == 0 {
                    continue;
                }
                match self.cmd_t_margin(s, ctx, 1 << ch, delay) {
                    Ok(()) => {}
                    Err(Error::TrainingFailed {
                        step: TrainingStep::CommandTiming,
                        ..
                    }) => {
                        *active = 0;
                    }
                    Err(error) => return Err(error),
                }
            }
        }
        self.command_reset(s, ctx, delay)?;
        let mut widths = [0; 2];
        for (ch, active) in measured.into_iter().enumerate() {
            if active == 0 {
                continue;
            }
            let rank = active.trailing_zeros() as usize;
            let index = if voltage { 14 } else { 12 };
            let m = &s.margin_results[ch][rank][0];
            if m[index] != 0 && m[index + 1] != 0 {
                let width = if voltage {
                    m[index].checked_add(m[index + 1])
                } else {
                    m[index].min(m[index + 1]).checked_mul(2)
                };
                widths[ch] = u16::try_from(width.ok_or(Error::InvalidPolicy)?)
                    .map_err(|_| Error::InvalidPolicy)?;
            }
        }
        Ok(widths)
    }
    /// Source `meminit_cmd_ds_tx_eq`, dedicated global CMD Vref plus channel EQ.
    /// Borrows actual command/reset resources and the existing optimizer scratch.
    pub fn meminit_cmd_ds_tx_eq(
        &self,
        s: &mut CalibrationState,
        ctx: &CommandContext<'_>,
        t: &Timings,
        package: Package,
        phy: &PhyPolicy,
        mode: TrainingMode,
        work: &mut OptWorkspace,
        saved: &mut CommandSiWorkspace,
        delay: &mut impl DelayNs,
    ) -> Result<CommandSiResult, Error> {
        self.cmd_si_train(
            s,
            ctx,
            t,
            package,
            phy,
            mode,
            Sweep::DriveEq,
            work,
            saved,
            delay,
        )
    }
    /// Source optional DDR4 margin-mode `meminit_cmd_slew_rate`. Not invoked in
    /// power mode; caller still owns enable policy and subsequent normalization.
    pub fn meminit_cmd_slew_rate(
        &self,
        s: &mut CalibrationState,
        ctx: &CommandContext<'_>,
        t: &Timings,
        package: Package,
        phy: &PhyPolicy,
        mode: TrainingMode,
        work: &mut OptWorkspace,
        saved: &mut CommandSiWorkspace,
        delay: &mut impl DelayNs,
    ) -> Result<CommandSiResult, Error> {
        if mode != TrainingMode::Margin {
            return Err(Error::InvalidPolicy);
        }
        self.cmd_si_train(
            s,
            ctx,
            t,
            package,
            phy,
            mode,
            Sweep::Slew,
            work,
            saved,
            delay,
        )
    }
    fn cmd_si_train(
        &self,
        s: &mut CalibrationState,
        ctx: &CommandContext<'_>,
        t: &Timings,
        package: Package,
        phy: &PhyPolicy,
        mode: TrainingMode,
        sweep: Sweep,
        work: &mut OptWorkspace,
        saved: &mut CommandSiWorkspace,
        delay: &mut impl DelayNs,
    ) -> Result<CommandSiResult, Error> {
        let ranks = self.command_ranks(ctx)?;
        let mask = channels(ranks);
        // No flag substitutes for stopped/exclusive hardware ownership.
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
        let upm = [
            margin_limit(s, MarginKind::CommandVoltage, LimitKind::Upm, t.clock, mode)?,
            margin_limit(s, MarginKind::CommandTiming, LimitKind::Upm, t.clock, mode)?,
        ];
        let wrv = margin_limit(s, MarginKind::WriteVoltage, LimitKind::Upm, t.clock, mode)?;
        let max_vref = if sweep == Sweep::DriveEq {
            (margin_limit(
                s,
                MarginKind::CommandVoltage,
                LimitKind::Power,
                t.clock,
                mode,
            )? / 20)
                .min(63) as u8
        } else {
            63
        };
        // Validate the genuine live DQ model even though the source's command
        // tradeoff power weight is zero. Dedicated CMD controls are ignored by
        // native calc_opt_power_byte: model the constant live DQ baseline, NOT
        // fictional CMD coefficients or DQ programming through CMD selectors.
        for ch in 0..2 {
            if ranks[ch] != 0 {
                PowerModel {
                    registers: self.registers,
                    dimms: self.dimms,
                    saved_mr: &s.saved_mr,
                    clock: t.clock,
                    package,
                    phy,
                }
                .calc_opt_power_byte(
                    &mut Power::default(),
                    RankLane {
                        channel: ch,
                        rank: ranks[ch].trailing_zeros() as usize,
                        lane: 0,
                    },
                    &[],
                    10,
                )?;
            }
        }
        saved.load(self, s)?;
        let mids = self.cmd_timing_midpoints();
        let result = (|| {
            let (start, stop) = self.cmd_si_find_range(saved, sweep, t.clock, delay)?;
            let step = if sweep == Sweep::DriveEq { 2 } else { 1 };
            let xdim = ((stop - start) / step + 1) as usize;
            let points = xdim * if sweep == Sweep::DriveEq { 2 } else { 1 };
            let point = |i: usize| CommandSiPoint {
                drive_or_stages: start + (i % xdim) as i8 * step,
                tx_eq: (i / xdim) as u8,
            };
            let loops = if sweep == Sweep::DriveEq { 12 } else { 14 };
            for test in 0..2 {
                self.setup_cadb_test(s, mask, loops, StopOnError::Never, delay)?;
                for i in 0..points {
                    saved.restore_pi(self, s, ranks, ctx.policy.sdram_lanes, false)?;
                    let p = point(i);
                    let code = self.cmd_si_apply(
                        saved,
                        sweep,
                        mask,
                        p.drive_or_stages,
                        p.tx_eq,
                        t.clock,
                        delay,
                    )?;
                    let gb = if sweep == Sweep::DriveEq { 3 } else { 2 };
                    if !(gb..=63 - gb).contains(&code) {
                        for ch in 0..2 {
                            work.saved_widths[test][i][ch][0] = 0;
                        }
                        continue;
                    }
                    let mut valid_ranks = ranks;
                    for ch in 0..2 {
                        if ranks[ch] == 0 {
                            continue;
                        }
                        for (j, iteration) in
                            [CommandIteration::CmdSouth, CommandIteration::CmdNorth]
                                .into_iter()
                                .enumerate()
                        {
                            match self.cmd_timing_center_channel(
                                s,
                                ctx,
                                ch,
                                ranks[ch],
                                iteration,
                                i16::from(mids[ch][j]),
                                delay,
                            ) {
                                Ok(()) => (),
                                Err(Error::TrainingFailed { .. }) => {
                                    valid_ranks[ch] = 0;
                                    break;
                                }
                                Err(e) => return Err(e),
                            }
                        }
                    }
                    let widths = if channels(valid_ranks) == 0 {
                        [0; 2]
                    } else {
                        self.cmd_si_measure(s, ctx, valid_ranks, test == 0, max_vref, delay)?
                    };
                    for ch in 0..2 {
                        work.saved_widths[test][i][ch][0] = widths[ch];
                    }
                }
                self.disable_cadb_deselects(mask)?;
            }
            let mut best = [None; 2];
            let mut codes = [0; 2];
            let mut sum = 0i16;
            let mut count = 0i16;
            for ch in 0..2 {
                if ranks[ch] == 0 {
                    continue;
                }
                work.rows.fill([0; 200]);
                for test in 0..2 {
                    for i in 0..points {
                        work.rows[test][i] = work.saved_widths[test][i][ch][0];
                    }
                    if sweep == Sweep::DriveEq {
                        running_average_2d(
                            &mut work.rows[test][..points],
                            xdim,
                            0,
                            1,
                            &mut work.scorer.scratch,
                        )?;
                    }
                }
                for i in 0..points {
                    let mut power = Power::default();
                    PowerModel {
                        registers: self.registers,
                        dimms: self.dimms,
                        saved_mr: &s.saved_mr,
                        clock: t.clock,
                        package,
                        phy,
                    }
                    .calc_opt_power_byte(
                        &mut power,
                        RankLane {
                            channel: ch,
                            rank: ranks[ch].trailing_zeros() as usize,
                            lane: 0,
                        },
                        &[],
                        10,
                    )?;
                    work.rows[2][i] =
                        u16::try_from(power.write).map_err(|_| Error::InvalidPolicy)?;
                }
                normalize_power_to_margins(&mut work.rows, points, 2)?;
                let chosen = find_optimal_trade_off(
                    &work.rows,
                    points,
                    &TradeoffConfig {
                        scale: [1, 2, 0, 0, 0],
                        upm_optimize: [false; 5],
                        square: false,
                        average: 1,
                        include_ends: true,
                        center_scale: 1,
                        good_power_limit: if sweep == Sweep::DriveEq {
                            [upm[0], upm[1], 0, 0, 0]
                        } else {
                            [1000, 1000, 0, 0, 0]
                        },
                        optimization_mode: 100,
                        guard_band: 0,
                        linear: false,
                    },
                    &mut work.scorer,
                )?
                .best;
                if (0..2).any(|test| work.saved_widths[test][chosen][ch][0] == 0) {
                    return Err(no_eye(ch, ranks[ch].trailing_zeros() as usize));
                }
                let p = point(chosen);
                codes[ch] = self.cmd_si_apply(
                    saved,
                    sweep,
                    1 << ch,
                    p.drive_or_stages,
                    p.tx_eq,
                    t.clock,
                    delay,
                )?;
                best[ch] = Some(p);
                sum += i16::from(p.drive_or_stages);
                count += 1;
            }
            let global = ((if sum >= 0 {
                sum + count / 2
            } else {
                sum - count / 2
            }) / count) as i8;
            let average_code = self.cmd_si_apply(saved, sweep, 0, global, 0, t.clock, delay)?;
            let gb = if sweep == Sweep::DriveEq { 3 } else { 2 };
            if !(gb..=63 - gb).contains(&average_code) {
                return Err(no_eye(0, 0));
            }
            let mut residual = [0; 2];
            if count > 1 {
                for ch in 0..2 {
                    if ranks[ch] == 0 {
                        continue;
                    }
                    let delta = i16::from(codes[ch]) - i16::from(average_code);
                    residual[ch] = if sweep == Sweep::DriveEq {
                        delta.clamp(-8, 7)
                    } else {
                        delta.clamp(-16, 15)
                    } as i8;
                    for reg in [
                        &self.registers.command_north[ch].comp_offset,
                        &self.registers.command_south[ch].comp_offset,
                    ] {
                        if sweep == Sweep::DriveEq {
                            reg.modify(
                                CommandCompOffset::DRIVE_UP.val(u32::from(residual[ch] as u8 & 15))
                                    + CommandCompOffset::DRIVE_DOWN
                                        .val(u32::from(residual[ch] as u8 & 15)),
                            );
                        } else {
                            reg.modify(
                                CommandCompOffset::SLEW.val(u32::from(residual[ch] as u8 & 31)),
                            );
                        }
                    }
                }
                force_rcomp(self.registers, delay)?;
            }
            saved.restore_pi(self, s, ranks, ctx.policy.sdram_lanes, false)?;
            self.cmd_timing_centering(s, ctx, Some(10), Some(mids), delay)?;
            self.cmd_voltage_centering(s, ctx, t, wrv, delay)?;
            // Remeasure BOTH actual final margins after shared compensation and
            // recentering; a nominal intermediate cannot certify the winner.
            self.setup_cadb_test(s, mask, 10, StopOnError::Nth, delay)?;
            for voltage in [true, false] {
                let widths = self.cmd_si_measure(s, ctx, ranks, voltage, max_vref, delay)?;
                for ch in 0..2 {
                    if ranks[ch] != 0 && widths[ch] == 0 {
                        return Err(no_eye(ch, ranks[ch].trailing_zeros() as usize));
                    }
                }
            }
            Ok(CommandSiResult {
                range: (start, stop),
                best,
                global,
                residual,
            })
        })();
        // Taking destructive ownership consumes the old pattern configuration
        // on every exit, even if failure precedes the first WDB overwrite.
        self.registers.reut_global_control.set(0);
        let mut cleanup = self.disable_cadb_deselects(mask);
        if result.is_err() {
            self.registers.comp_control0.set(saved.ctl[0]);
            self.registers.comp_control1.set(saved.ctl[1]);
            for ch in 0..2 {
                self.registers.command_north[ch]
                    .control
                    .set(saved.controls[ch][0]);
                self.registers.command_south[ch]
                    .control
                    .set(saved.controls[ch][1]);
                self.registers.command_north[ch]
                    .comp_offset
                    .set(saved.offsets[ch][0]);
                self.registers.command_south[ch]
                    .comp_offset
                    .set(saved.offsets[ch][1]);
            }
            cleanup = merge(cleanup, force_rcomp(self.registers, delay));
            cleanup = merge(
                cleanup,
                saved.restore_pi(self, s, ranks, ctx.policy.sdram_lanes, true),
            );
            cleanup = merge(cleanup, self.restore_ca_vref(ctx, saved.vref, delay));
            cleanup = merge(cleanup, self.command_reset(s, ctx, delay));
        }
        for ch in 0..2 {
            if let Some(test) = &saved.tests[ch] {
                test.restore(self, ch);
            }
            // These seed/LMN words are readable configuration, unlike the
            // destructive pattern memories and their invalidated ownership.
            let c = &self.registers.channel[ch];
            for i in 0..3 {
                c.wdb_read_seed[i].set(saved.wdb_seeds[ch][i]);
                c.wdb_write_seed[i].set(saved.wdb_seeds[ch][i + 3]);
                c.cadb_seed[i].set(saved.cadb_seeds[ch][i]);
            }
            c.wdb_lmn.set(saved.wdb_lmn[ch]);
            c.cadb_lmn.set(saved.cadb_lmn[ch]);
        }
        // cmd_voltage_centering calls setup_wdb: restoring old SegmentWdb
        // metadata would advertise different unreadable rows as runnable.
        // 0xff is outside every native DqPattern code; run_io_test rejects it
        // before any reset or traffic until real pattern/test setup replaces it.
        s.dq_pat = u8::MAX;
        s.dq_pat_lc = 0;
        match result {
            Ok(r) => cleanup.map(|()| r),
            Err(e) => merge(Err(e), cleanup).map(|()| unreachable!()),
        }
    }
}
