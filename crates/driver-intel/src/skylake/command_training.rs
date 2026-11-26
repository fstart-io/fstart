// SPDX-License-Identifier: MIT OR Apache-2.0
//! DDR4 late command training from native/cmd_training.c and highspeed.c.
//! Destructive cold-training operations; not a MemoryInit implementation.

use super::{
    Error, TrainingStep,
    io_test::{DqPattern, StopOnError},
    phy_registers::{RxTrain, TxTrain, VrefAdjust},
    registers::ReutSequence,
    reut::{DownloadTarget, Engine, RegisterFile, TestResult, rank_mask},
    reut_registers::ErrorDetail,
    timings::Timings,
    training::{PassingWindow, channels, cold_ranks},
    training_state::CalibrationState,
};
use crate::sunrise_point::pmc::{MemoryBootMode, PmcRegisters};
use embedded_hal::delay::DelayNs;
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};

/// Resolved silicon/topology policy, not M900 defaults. ECC participation in
/// clock/DQ coupling and PDA reset is explicit (8 or 9 SDRAM devices).
#[derive(Debug, Clone, Copy)]
pub struct CommandPolicy {
    pub sdram_lanes: usize,
    pub ddp_shared_clock: bool,
    /// DDR4 source configurations: 0 shares CA Vref; 2 has channel-local Vref.
    pub ca_vref_config: u8,
}

/// Caller-owned reset resources. Saved MR/PDA images in CalibrationState must
/// already have been initialized by successful cold JEDEC setup. Timing/PHY
/// host shadows and preceding read/write training must be complete. The native
/// command-rate update must precede late training; this module neither invokes
/// that prerequisite nor establishes a complete training/activation certificate.
pub struct CommandContext<'a> {
    pub pmc: &'a PmcRegisters,
    pub mode: MemoryBootMode,
    pub policy: CommandPolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandIteration {
    Clock,
    CmdNorth,
    CmdSouth,
    Cke,
    Control,
}

fn failure(step: TrainingStep, ch: usize, rank: usize, lane: usize) -> Error {
    Error::TrainingFailed {
        step,
        channel: ch as u8,
        rank: rank as u8,
        lane: lane as u8,
    }
}
// Cleanup transport/readiness failures must not be demoted to a rejected
// analog candidate by a simultaneous narrow-eye result.
fn finish(result: Result<(), Error>, cleanup: Result<(), Error>) -> Result<(), Error> {
    match (result, cleanup) {
        (Err(Error::TrainingFailed { .. }), Err(e)) => Err(e),
        (Ok(()), other) => other,
        (other, _) => other,
    }
}
pub(crate) fn pi(word: u32, rank: usize) -> i16 {
    ((word >> (7 * rank)) & 127) as i16
}
pub(crate) fn replace_pi(word: u32, rank: usize, value: i16) -> u32 {
    (word & !(127 << (7 * rank))) | (u32::from(value as u16 & 127) << (7 * rank))
}
fn check_ca_vref_readback(observed: u32, expected: u32, mask: u32) -> Result<(), Error> {
    if observed == u32::MAX || (observed ^ expected) & mask != 0 {
        return Err(Error::CaVrefReadback {
            expected,
            observed,
            mask,
        });
    }
    Ok(())
}

fn signed7(value: u32) -> i16 {
    ((value as u8) << 1) as i8 as i16 >> 1
}

// Full hardware words and independent host mirrors: hardware is not a reliable
// replacement for the caller's host baseline. No whole CalibrationState clone.
pub(crate) struct PiSnapshot {
    pub(crate) clk: u32,
    pub(crate) ctl: u32,
    pub(crate) cke_ctl: u32,
    pub(crate) cmds: u32,
    pub(crate) cmdn: u32,
    pub(crate) cke: u32,
    pub(crate) tx: [[u32; 9]; 4],
    pub(crate) rx: [[u32; 9]; 4],
    host_dq: [[u16; 9]; 4],
    host_dqs: [[u16; 9]; 4],
    pub(crate) host_rcven: [[u16; 9]; 4],
    margins: [[[u32; 4]; 9]; 4],
}
impl PiSnapshot {
    pub(crate) fn load(e: &Engine<'_>, s: &CalibrationState, ch: usize) -> Self {
        Self {
            clk: e.registers.clock_channel[ch].rank_pi.get(),
            ctl: e.registers.control_channel[ch].rank_pi.get(),
            cke_ctl: e.registers.cke_channel[ch].control_pi.get(),
            cmds: e.registers.command_south[ch].cmd_pi.get(),
            cmdn: e.registers.command_north[ch].cmd_pi.get(),
            cke: e.registers.cke_channel[ch].cmd_pi.get(),
            tx: core::array::from_fn(|r| {
                core::array::from_fn(|b| e.registers.data_lane[b][ch].tx_train[r].get())
            }),
            rx: core::array::from_fn(|r| {
                core::array::from_fn(|b| e.registers.data_lane[b][ch].rx_train[r].get())
            }),
            host_dq: core::array::from_fn(|r| s.training.ch[ch].rank[r].tx_dq_dly),
            host_dqs: core::array::from_fn(|r| s.training.ch[ch].rank[r].wl_dly),
            host_rcven: core::array::from_fn(|r| s.training.ch[ch].rank[r].rcven_dly),
            margins: core::array::from_fn(|r| {
                core::array::from_fn(|b| {
                    core::array::from_fn(|i| s.margin_results[ch][r][b][12 + i])
                })
            }),
        }
    }
    pub(crate) fn restore(
        &self,
        e: &Engine<'_>,
        s: &mut CalibrationState,
        ch: usize,
        ranks: u8,
        lanes: usize,
        restore_margins: bool,
    ) -> Result<(), Error> {
        e.registers.clock_channel[ch].rank_pi.set(self.clk);
        e.registers.control_channel[ch].rank_pi.set(self.ctl);
        e.registers.cke_channel[ch].control_pi.set(self.cke_ctl);
        e.registers.command_south[ch].cmd_pi.set(self.cmds);
        e.registers.command_north[ch].cmd_pi.set(self.cmdn);
        e.registers.cke_channel[ch].cmd_pi.set(self.cke);
        for rank in 0..4 {
            if ranks & (1 << rank) == 0 {
                continue;
            }
            for byte in 0..lanes {
                let lane = &e.registers.data_lane[byte][ch];
                lane.tx_train[rank].set(self.tx[rank][byte]);
                lane.rx_train[rank].set(self.rx[rank][byte]);
                let host = &mut s.training.ch[ch].rank[rank];
                host.tx_dq_dly[byte] = self.host_dq[rank][byte];
                host.wl_dly[byte] = self.host_dqs[rank][byte];
                host.rcven_dly[byte] = self.host_rcven[rank][byte];
                let target = DownloadTarget::Lane {
                    channel: ch,
                    lane: byte,
                };
                e.download(target, rank, RegisterFile::Read)?;
                e.download(target, rank, RegisterFile::Write)?;
                if restore_margins {
                    s.margin_results[ch][rank][byte][12..16]
                        .copy_from_slice(&self.margins[rank][byte]);
                }
            }
        }
        Ok(())
    }
}

impl Engine<'_> {
    pub(crate) fn command_ranks(&self, ctx: &CommandContext<'_>) -> Result<[u8; 2], Error> {
        let ranks = cold_ranks(self, ctx.mode)?;
        if !(8..=9).contains(&ctx.policy.sdram_lanes) || !matches!(ctx.policy.ca_vref_config, 0 | 2)
        {
            return Err(Error::InvalidPolicy);
        }
        Ok(ranks)
    }
    fn command_feedback(
        &self,
        state: &CalibrationState,
        selected: u8,
        delay: &mut impl DelayNs,
    ) -> Result<(TestResult, [u8; 2]), Error> {
        self.validate_channels(selected)?;
        if state.dq_pat != DqPattern::Cadb as u8 {
            return Err(Error::InvalidReutConfig);
        }
        let saved = core::array::from_fn::<_, 2, _>(|ch| self.registers.reut_sequence[ch][0].get());
        for ch in 0..2 {
            self.registers.reut_sequence[ch][0]
                .modify(ReutSequence::GLOBAL_CONTROL.val(u32::from(selected & (1 << ch) != 0)));
        }
        let result = self.run_io_test(state, selected, delay).map(|r| {
            (
                r,
                core::array::from_fn(|ch| {
                    if selected & (1 << ch) != 0 {
                        self.registers.channel[ch]
                            .error_detail
                            .read(ErrorDetail::BYTE_GROUPS) as u8
                    } else {
                        0
                    }
                }),
            )
        });
        for ch in 0..2 {
            self.registers.reut_sequence[ch][0].set(saved[ch]);
        }
        result
    }
    pub(crate) fn command_reset(
        &self,
        state: &CalibrationState,
        ctx: &CommandContext<'_>,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        self.reinitialize_cold(state, ctx.pmc, ctx.mode, ctx.policy.sdram_lanes, delay)
    }
    fn clock_ranks(&self, ch: usize, ranks: u8, policy: CommandPolicy) -> Result<u8, Error> {
        let present = rank_mask(self.dimms, ch)?;
        let mut selected = ranks & present;
        if policy.ddp_shared_clock && selected & 3 != 0 {
            selected |= present & 3;
        }
        Ok(selected)
    }
    /// highspeed.c:meminit_ca_vref_base. Signed seven-bit, not RX-Vref's six bits.
    pub fn ca_vref_base(&self) -> [i16; 2] {
        let r = self.registers.data_lane[0][0].vref_adjust.extract();
        [
            signed7(r.read(VrefAdjust::CA)),
            signed7(r.read(VrefAdjust::CH1)),
        ]
    }
    /// highspeed.c:meminit_ca_vref_update. Keep the settle indication distinct
    /// from programming failure; the caller decides how to use it.
    pub fn ca_vref_update(
        &self,
        channel: usize,
        base: i16,
        offset: i16,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        if channel > 1 || !(-63..=63).contains(&base) || !(-127..=127).contains(&offset) {
            return Err(Error::InvalidTiming);
        }
        let value = ((i32::from(base) + i32::from(offset)).clamp(-63, 63) as u32) & 127;
        let r = &self.registers.data_lane[0][0].vref_adjust;
        let (fields, status) = if channel == 0 {
            (
                VrefAdjust::CA.val(value) + VrefAdjust::ENABLE_CA::SET,
                VrefAdjust::CA_SLOW_BW,
            )
        } else {
            (
                VrefAdjust::CH1.val(value) + VrefAdjust::ENABLE_CH1::SET,
                VrefAdjust::CH1_SLOW_BW,
            )
        };
        let mask = fields.mask();
        let expected = fields.value;
        r.modify(fields);
        let mut result = Err(Error::CaVrefTimeout);
        for _ in 0..50 {
            if r.is_set(status) {
                result = Ok(());
                break;
            }
            delay.delay_us(1);
        }
        delay.delay_us(5);
        check_ca_vref_readback(r.get(), expected, mask)?;
        result
    }

    /// Native command training still tests the programmed setting when only
    /// the settle indication is missing. A failed write or subsequent JEDEC,
    /// CADB or margin error is not made advisory by this helper.
    pub(crate) fn ca_vref_update_for_training(
        &self,
        channel: usize,
        base: i16,
        offset: i16,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        match self.ca_vref_update(channel, base, offset, delay) {
            Err(Error::CaVrefTimeout) => Ok(()),
            result => result,
        }
    }
    fn command_vref_offset(
        &self,
        ctx: &CommandContext<'_>,
        base: [i16; 2],
        offset: i16,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        self.ca_vref_update_for_training(0, base[0], offset, delay)?;
        if ctx.policy.ca_vref_config == 2 {
            self.ca_vref_update_for_training(1, base[1], offset, delay)?;
        }
        Ok(())
    }
    pub(crate) fn restore_ca_vref(
        &self,
        ctx: &CommandContext<'_>,
        saved: u32,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        // Restore both programmed centers even if the first write failed.
        let a = self.ca_vref_update_for_training(0, signed7(saved & 127), 0, delay);
        let b = if ctx.policy.ca_vref_config == 2 {
            self.ca_vref_update_for_training(1, signed7((saved >> 7) & 127), 0, delay)
        } else {
            Ok(())
        };
        let r = &self.registers.data_lane[0][0].vref_adjust;
        r.set(saved);
        // Slow-bandwidth status is live; compare the writable controls only.
        let mask = (VrefAdjust::CA.val(0)
            + VrefAdjust::CH1.val(0)
            + VrefAdjust::ENABLE_CA::SET
            + VrefAdjust::ENABLE_CH1::SET)
            .mask();
        a.and(b).and(check_ca_vref_readback(r.get(), saved, mask))
    }
    pub(crate) fn shift_command_pi(
        &self,
        state: &mut CalibrationState,
        base: &PiSnapshot,
        ch: usize,
        ranks: u8,
        iteration: CommandIteration,
        value: i16,
        update_host: bool,
        policy: CommandPolicy,
    ) -> Result<(), Error> {
        let value_clamped = value.clamp(0, 127);
        match iteration {
            CommandIteration::Clock => {
                let ranks = self.clock_ranks(ch, ranks, policy)?;
                let mut clk = self.registers.clock_channel[ch].rank_pi.get();
                for rank in 0..4 {
                    if ranks & (1 << rank) == 0 {
                        continue;
                    }
                    clk = replace_pi(clk, rank, (pi(base.clk, rank) + value).rem_euclid(128));
                    for byte in 0..policy.sdram_lanes {
                        let lane = &self.registers.data_lane[byte][ch];
                        let dq =
                            (((base.tx[rank][byte] & 511) as i16 + value).clamp(0, 511)) as u32;
                        let dqs = ((((base.tx[rank][byte] >> 9) & 511) as i16 + value)
                            .clamp(0, 511)) as u32;
                        let rcven =
                            (((base.rx[rank][byte] & 511) as i16 + value).clamp(0, 511)) as u32;
                        lane.tx_train[rank].modify(TxTrain::DQ.val(dq) + TxTrain::DQS.val(dqs));
                        lane.rx_train[rank].modify(RxTrain::RCVEN.val(rcven));
                        let target = DownloadTarget::Lane {
                            channel: ch,
                            lane: byte,
                        };
                        self.download(target, rank, RegisterFile::Read)?;
                        self.download(target, rank, RegisterFile::Write)?;
                        if update_host {
                            let host = &mut state.training.ch[ch].rank[rank];
                            host.tx_dq_dly[byte] = dq as u16;
                            host.wl_dly[byte] = dqs as u16;
                            host.rcven_dly[byte] = rcven as u16;
                        }
                    }
                }
                self.registers.clock_channel[ch].rank_pi.set(clk);
            }
            CommandIteration::CmdNorth | CommandIteration::CmdSouth | CommandIteration::Cke => {
                let r = match iteration {
                    CommandIteration::CmdNorth => &self.registers.command_north[ch].cmd_pi,
                    CommandIteration::CmdSouth => &self.registers.command_south[ch].cmd_pi,
                    _ => &self.registers.cke_channel[ch].cmd_pi,
                };
                r.set(replace_pi(
                    replace_pi(r.get(), 0, value_clamped),
                    1,
                    value_clamped,
                ));
                if iteration == CommandIteration::CmdSouth {
                    let cke = &self.registers.cke_channel[ch].cmd_pi;
                    cke.set(replace_pi(
                        replace_pi(cke.get(), 0, value_clamped),
                        1,
                        value_clamped,
                    ));
                }
            }
            CommandIteration::Control => {
                let r = &self.registers.control_channel[ch].rank_pi;
                let cke = &self.registers.cke_channel[ch].control_pi;
                let mut ctl = r.get();
                let mut cke_ctl = cke.get() & !(127 << 21);
                for rank in 0..4 {
                    if ranks & (1 << rank) == 0 {
                        continue;
                    }
                    ctl = replace_pi(ctl, rank, value_clamped);
                    if rank < 3 {
                        cke_ctl = replace_pi(cke_ctl, rank, value_clamped);
                    }
                }
                r.set(ctl);
                cke.set(cke_ctl);
            }
        }
        Ok(())
    }
    /// highspeed.c:meminit_shift_pi_for_cmd_training. Clock shifts include all
    /// physically coupled DDP ranks, with one shift per dependent DQ lane.
    pub fn shift_pi_for_cmd_training(
        &self,
        state: &mut CalibrationState,
        ch: usize,
        ranks: u8,
        iteration: CommandIteration,
        value: i16,
        ctx: &CommandContext<'_>,
    ) -> Result<(), Error> {
        let present = self.command_ranks(ctx)?;
        if ch > 1 || ranks == 0 || ranks & !present[ch] != 0 || !(-127..=127).contains(&value) {
            return Err(Error::InvalidRank);
        }
        let base = PiSnapshot::load(self, state, ch);
        self.shift_command_pi(state, &base, ch, ranks, iteration, value, true, ctx.policy)
    }
    /// highspeed.c:meminit_cmd_dq_pi_limits, including TX DQ/DQS and RX enable.
    pub fn cmd_dq_pi_limits(
        &self,
        ch: usize,
        ranks: u8,
        policy: CommandPolicy,
    ) -> Result<(i16, i16), Error> {
        if !(8..=9).contains(&policy.sdram_lanes) {
            return Err(Error::InvalidPolicy);
        }
        let ranks = self.clock_ranks(ch, ranks, policy)?;
        if ranks == 0 {
            return Err(Error::InvalidRank);
        }
        let mut min = 511;
        let mut max = 0;
        for rank in 0..4 {
            if ranks & (1 << rank) == 0 {
                continue;
            }
            for byte in 0..policy.sdram_lanes {
                let lane = &self.registers.data_lane[byte][ch];
                for v in [
                    lane.tx_train[rank].read(TxTrain::DQ),
                    lane.tx_train[rank].read(TxTrain::DQS),
                    lane.rx_train[rank].read(RxTrain::RCVEN),
                ] {
                    min = min.min(v as i16);
                    max = max.max(v as i16);
                }
            }
        }
        Ok((min, max))
    }
}

impl Engine<'_> {
    /// highspeed.c:meminit_cmd_clock_scan/cmd_linear_find_edges. Bounds and
    /// winning center are RELATIVE shifts from live hardware, not absolute CLK
    /// codes. Rejected scans restore hardware AND independently different hosts.
    /// Caller sets up CADB and rank maps before invoking this measurement.
    #[allow(clippy::too_many_arguments)]
    pub fn cmd_clock_scan(
        &self,
        state: &mut CalibrationState,
        ctx: &CommandContext<'_>,
        selected: u8,
        ranks: u8,
        low: [i16; 2],
        high: [i16; 2],
        step: i16,
        update_center: bool,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        let present = self.command_ranks(ctx)?;
        self.validate_channels(selected)?;
        if state.dq_pat != DqPattern::Cadb as u8 {
            return Err(Error::InvalidReutConfig);
        }
        if !(1..=127).contains(&step) || ranks == 0 || ranks & !15 != 0 {
            return Err(Error::InvalidTiming);
        }
        let mut masks = [0; 2];
        for ch in 0..2 {
            if selected & (1 << ch) == 0 {
                continue;
            }
            masks[ch] = ranks & present[ch];
            if masks[ch] == 0 || low[ch] < -127 || high[ch] > 127 || low[ch] > high[ch] {
                return Err(Error::InvalidTiming);
            }
            let (min, max) = self.cmd_dq_pi_limits(ch, masks[ch], ctx.policy)?;
            if min + low[ch] < 0 || max + high[ch] > 511 {
                return Err(Error::InvalidTiming);
            }
        }
        let saved = core::array::from_fn::<_, 2, _>(|ch| PiSnapshot::load(self, state, ch));
        let mut windows = [PassingWindow::EMPTY; 2];
        let mut initial_end = [None; 2];
        let mut trailing = [None; 2];
        let result = (|| {
            let first = (0..2)
                .filter(|&ch| selected & (1 << ch) != 0)
                .map(|ch| low[ch])
                .min()
                .ok_or(Error::InvalidRank)?;
            let last = (0..2)
                .filter(|&ch| selected & (1 << ch) != 0)
                .map(|ch| high[ch])
                .max()
                .ok_or(Error::InvalidRank)?;
            let mut d = first;
            while d <= last {
                let mut local = 0;
                for ch in 0..2 {
                    if masks[ch] != 0 && d >= low[ch] && d <= high[ch] {
                        local |= 1 << ch;
                        self.shift_command_pi(
                            state,
                            &saved[ch],
                            ch,
                            masks[ch],
                            CommandIteration::Clock,
                            d,
                            false,
                            ctx.policy,
                        )?;
                    }
                }
                if local != 0 {
                    self.command_reset(state, ctx, delay)?;
                    let errors = self.command_feedback(state, local, delay)?.0.error_channels;
                    for ch in 0..2 {
                        if local & (1 << ch) == 0 {
                            continue;
                        }
                        let pass = errors & (1 << ch) == 0;
                        windows[ch].sample(d, step, pass);
                        if pass && (d == 0 || initial_end[ch] == Some(d - step)) {
                            initial_end[ch] = Some(d);
                        }
                        trailing[ch] = if pass {
                            Some(match trailing[ch] {
                                Some((l, r)) if r == d - step => (l, d),
                                _ => (d, d),
                            })
                        } else {
                            None
                        };
                        // The source allows the initial and trailing eyes to
                        // join only across the actual 128-tick circular seam.
                        if pass
                            && low[ch] == 0
                            && d >= 127 - step
                            && let (Some(end), Some((left, right))) =
                                (initial_end[ch], trailing[ch])
                            && left != 0
                            && right == d
                        {
                            // PI codes wrap at 128, even when the scan
                            // step does not divide 128 exactly.
                            let joined = 128 + end;
                            if joined - left > windows[ch].width() {
                                windows[ch].best = Some((left, joined));
                            }
                        }
                    }
                }
                d += step;
            }
            for ch in 0..2 {
                if masks[ch] != 0 && windows[ch].width() < 3 * step {
                    return Err(failure(
                        TrainingStep::CommandTiming,
                        ch,
                        masks[ch].trailing_zeros() as usize,
                        0,
                    ));
                }
            }
            Ok(())
        })();
        // Restore complete baseline even for a successful measurement-only scan.
        let mut cleanup = Ok(());
        for ch in 0..2 {
            if masks[ch] != 0 {
                cleanup = cleanup.and(saved[ch].restore(
                    self,
                    state,
                    ch,
                    self.clock_ranks(ch, masks[ch], ctx.policy)?,
                    ctx.policy.sdram_lanes,
                    true,
                ));
            }
        }
        let mut result = result.and(cleanup);
        if result.is_ok() {
            for ch in 0..2 {
                if masks[ch] == 0 {
                    continue;
                }
                let (left, right) = windows[ch].best.ok_or(Error::InvalidTiming)?;
                if update_center {
                    let center = ((left + right) / 2).rem_euclid(128);
                    // Negative scan centers are real signed shifts, not modulo
                    // positive shifts. Only the circular [0,127] scan wraps.
                    let center = if low[ch] < 0 {
                        (left + right) / 2
                    } else {
                        center
                    };
                    result = result.and(self.shift_command_pi(
                        state,
                        &saved[ch],
                        ch,
                        masks[ch],
                        CommandIteration::Clock,
                        center,
                        true,
                        ctx.policy,
                    ));
                }
                for rank in 0..4 {
                    if masks[ch] & (1 << rank) != 0 {
                        state.margin_results[ch][rank][0][12] = u32::from(left.unsigned_abs()) * 10;
                        state.margin_results[ch][rank][0][13] =
                            u32::from(right.unsigned_abs()) * 10;
                    }
                }
            }
        }
        let reset = self.command_reset(state, ctx, delay);
        result = finish(result, reset);
        if result.is_err() {
            for ch in 0..2 {
                if masks[ch] != 0 {
                    saved[ch].restore(
                        self,
                        state,
                        ch,
                        self.clock_ranks(ch, masks[ch], ctx.policy)?,
                        ctx.policy.sdram_lanes,
                        true,
                    )?;
                }
            }
        }
        result
    }

    /// highspeed.c:cmd_binary_find_edge. Every candidate is actually tested at
    /// CA Vref -8 AND +8 unless all selected channels have already failed.
    fn cmd_binary_find_edge(
        &self,
        state: &mut CalibrationState,
        ctx: &CommandContext<'_>,
        base: &PiSnapshot,
        ch: usize,
        ranks: u8,
        iteration: CommandIteration,
        mut low: i16,
        mut high: i16,
        count_up: bool,
        vref_base: [i16; 2],
        delay: &mut impl DelayNs,
    ) -> Result<i16, Error> {
        while low < high {
            let target = (high + low + i16::from(count_up)) / 2;
            self.shift_command_pi(state, base, ch, ranks, iteration, target, false, ctx.policy)?;
            let mut failed = false;
            for offset in [-8, 8] {
                self.command_vref_offset(ctx, vref_base, offset, delay)?;
                failed |= self
                    .command_feedback(state, 1 << ch, delay)?
                    .0
                    .error_channels
                    & (1 << ch)
                    != 0;
                if failed {
                    break;
                }
            }
            if count_up {
                if failed {
                    high = target - 1;
                } else {
                    low = target;
                }
            } else if failed {
                low = target + 1;
            } else {
                high = target;
            }
            if failed {
                self.command_reset(state, ctx, delay)?;
            }
        }
        self.command_vref_offset(ctx, vref_base, 0, delay)?;
        self.command_reset(state, ctx, delay)?;
        Ok(if count_up { low } else { high })
    }
    pub(crate) fn cmd_timing_center_channel(
        &self,
        state: &mut CalibrationState,
        ctx: &CommandContext<'_>,
        ch: usize,
        ranks: u8,
        iteration: CommandIteration,
        mid: i16,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        if !(0..=127).contains(&mid) {
            return Err(Error::InvalidTiming);
        }
        let saved = PiSnapshot::load(self, state, ch);
        self.program_rank_map(ch, ranks)?;
        let vref = self.registers.data_lane[0][0].vref_adjust.get();
        let base = self.ca_vref_base();
        let result = (|| {
            // Verify the supplied midpoint rather than assuming it passed.
            self.shift_command_pi(state, &saved, ch, ranks, iteration, mid, false, ctx.policy)?;
            for offset in [-8, 8] {
                self.command_vref_offset(ctx, base, offset, delay)?;
                if self
                    .command_feedback(state, 1 << ch, delay)?
                    .0
                    .error_channels
                    & (1 << ch)
                    != 0
                {
                    return Err(failure(
                        TrainingStep::CommandTiming,
                        ch,
                        ranks.trailing_zeros() as usize,
                        0,
                    ));
                }
            }
            let left = self.cmd_binary_find_edge(
                state, ctx, &saved, ch, ranks, iteration, 0, mid, false, base, delay,
            )?;
            let right = self.cmd_binary_find_edge(
                state, ctx, &saved, ch, ranks, iteration, mid, 127, true, base, delay,
            )?;
            if right - left < 18 {
                return Err(failure(
                    TrainingStep::CommandTiming,
                    ch,
                    ranks.trailing_zeros() as usize,
                    0,
                ));
            }
            let center = if left == 0 && right == 127 {
                mid
            } else {
                (left + right + 1) / 2
            };
            self.shift_command_pi(
                state, &saved, ch, ranks, iteration, center, true, ctx.policy,
            )
        })();
        let restore = self.restore_ca_vref(ctx, vref, delay);
        let reset = self.command_reset(state, ctx, delay);
        let result = finish(finish(result, restore), reset);
        if result.is_err() {
            saved.restore(
                self,
                state,
                ch,
                rank_mask(self.dimms, ch)?,
                ctx.policy.sdram_lanes,
                true,
            )?;
        }
        result
    }

    pub fn cmd_timing_midpoints(&self) -> [[u8; 2]; 2] {
        core::array::from_fn(|ch| {
            [
                (self.registers.command_south[ch].cmd_pi.get() & 127) as u8,
                (self.registers.command_north[ch].cmd_pi.get() & 127) as u8,
            ]
        })
    }

    /// highspeed.c:meminit_cmd_timing_centering. Optional host midpoints are
    /// the pre-sweep CMDS/CMDN codes needed by subsequent SI optimization.
    pub fn cmd_timing_centering(
        &self,
        state: &mut CalibrationState,
        ctx: &CommandContext<'_>,
        setup_cadb: Option<u8>,
        host_midpoints: Option<[[u8; 2]; 2]>,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        let ranks = self.command_ranks(ctx)?;
        let mask = channels(ranks);
        let saved = core::array::from_fn::<_, 2, _>(|ch| PiSnapshot::load(self, state, ch));
        let mids = host_midpoints.unwrap_or_else(|| self.cmd_timing_midpoints());
        let result = (|| {
            if let Some(lc) = setup_cadb {
                self.setup_cadb_test(state, mask, lc, StopOnError::Nth, delay)?;
            }
            for (i, iteration) in [CommandIteration::CmdSouth, CommandIteration::CmdNorth]
                .into_iter()
                .enumerate()
            {
                for ch in 0..2 {
                    if ranks[ch] != 0 {
                        self.cmd_timing_center_channel(
                            state,
                            ctx,
                            ch,
                            ranks[ch],
                            iteration,
                            i16::from(mids[ch][i]),
                            delay,
                        )?;
                    }
                }
            }
            Ok(())
        })();
        let cleanup = if setup_cadb.is_some() || result.is_err() {
            self.disable_cadb_deselects(mask)
        } else {
            Ok(())
        };
        let result = result.and(cleanup);
        if result.is_err() {
            for ch in 0..2 {
                if ranks[ch] != 0 {
                    saved[ch].restore(self, state, ch, ranks[ch], ctx.policy.sdram_lanes, true)?;
                }
            }
            let _ = self.command_reset(state, ctx, delay);
        }
        result
    }

    /// highspeed.c:meminit_cmd_ctl_centering. CTL2/3 are a single CKE2 group;
    /// an isolated rank3 never moves the CKE2 timing of an absent rank2.
    pub fn cmd_ctl_centering(
        &self,
        state: &mut CalibrationState,
        ctx: &CommandContext<'_>,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        let ranks = self.command_ranks(ctx)?;
        let saved = core::array::from_fn::<_, 2, _>(|ch| PiSnapshot::load(self, state, ch));
        let result = (|| {
            for group in [1, 2, 12] {
                for ch in 0..2 {
                    let selected = ranks[ch] & group;
                    if selected != 0 {
                        self.cmd_timing_center_channel(
                            state,
                            ctx,
                            ch,
                            selected,
                            CommandIteration::Control,
                            pi(saved[ch].ctl, selected.trailing_zeros() as usize),
                            delay,
                        )?;
                    }
                }
            }
            Ok(())
        })();
        if result.is_err() {
            for ch in 0..2 {
                if ranks[ch] != 0 {
                    saved[ch].restore(self, state, ch, ranks[ch], ctx.policy.sdram_lanes, true)?;
                }
            }
            self.disable_cadb_deselects(channels(ranks))?;
            let _ = self.command_reset(state, ctx, delay);
        }
        result
    }
}

impl Engine<'_> {
    fn ctl_clock_center_channel(
        &self,
        state: &mut CalibrationState,
        ctx: &CommandContext<'_>,
        ch: usize,
        ranks: u8,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        // Move CLK/DQ left without clipping; DDP0/1 share one physical clock.
        for rank in 0..4 {
            if ranks & (1 << rank) == 0
                || (ctx.policy.ddp_shared_clock && rank == 1 && ranks & 1 != 0)
            {
                continue;
            }
            let selected = self.clock_ranks(ch, 1 << rank, ctx.policy)?;
            let (min, _) = self.cmd_dq_pi_limits(ch, selected, ctx.policy)?;
            let clk = pi(self.registers.clock_channel[ch].rank_pi.get(), rank);
            self.shift_pi_for_cmd_training(
                state,
                ch,
                selected,
                CommandIteration::Clock,
                -min.min(clk),
                ctx,
            )?;
        }
        let (_, max) = self.cmd_dq_pi_limits(ch, ranks, ctx.policy)?;
        // cmd_clock_scan takes relative CLK/DQ shifts. After the initial
        // left move, do not add an absolute residual clock code to the range:
        // e.g. CLK24 with dependent PIs [0,440] permits shifts [0,71], not
        // [24,95]. Clock codes may wrap; dependent 9-bit PIs must not clip.
        let low = [0; 2];
        let mut high = [0; 2];
        high[ch] = 127.min(511 - max);
        if low[ch] > high[ch] {
            return Err(Error::InvalidTiming);
        }
        self.setup_cadb_test(state, 1 << ch, 14, StopOnError::Nth, delay)?;
        self.program_rank_map(ch, ranks)?;
        let mut best = 0;
        let mut best_last = 0;
        let mut width = 0;
        let result_rank = ranks.trailing_zeros() as usize;
        for candidate in (32..=96).step_by(16) {
            for iteration in [
                CommandIteration::Control,
                CommandIteration::CmdSouth,
                CommandIteration::CmdNorth,
            ] {
                self.shift_pi_for_cmd_training(state, ch, ranks, iteration, candidate, ctx)?;
            }
            match self.cmd_clock_scan(state, ctx, 1 << ch, ranks, low, high, 6, false, delay) {
                Ok(()) => {
                    let m = &state.margin_results[ch][result_rank][0];
                    let measured = (m[13] - m[12]) / 10;
                    if measured > width {
                        width = measured;
                        best = candidate;
                        best_last = candidate;
                    } else if measured == width {
                        best_last = candidate;
                    }
                }
                // A narrow measured eye is a rejected analog candidate, not a
                // reason to reuse old margins from an earlier CTL candidate.
                Err(Error::TrainingFailed {
                    step: TrainingStep::CommandTiming,
                    ..
                }) => {}
                Err(e) => return Err(e),
            }
        }
        // If the coarse sweep finds no candidate, try the last programmed
        // CTL value with the finer native scan, not an untested zero.
        let center = if width == 0 {
            96
        } else {
            (best + best_last) / 2
        };
        for iteration in [
            CommandIteration::Control,
            CommandIteration::CmdSouth,
            CommandIteration::CmdNorth,
        ] {
            self.shift_pi_for_cmd_training(state, ch, ranks, iteration, center, ctx)?;
        }
        self.cmd_clock_scan(state, ctx, 1 << ch, ranks, low, high, 4, true, delay)?;
        let (min, max) = self.cmd_dq_pi_limits(ch, ranks, ctx.policy)?;
        if min < 75 {
            let shift = (75 - min).min(511 - max);
            self.shift_pi_for_cmd_training(state, ch, ranks, CommandIteration::Clock, shift, ctx)?;
            for rank in 0..4 {
                if ranks & (1 << rank) != 0 {
                    let ctl = pi(self.registers.control_channel[ch].rank_pi.get(), rank);
                    self.shift_pi_for_cmd_training(
                        state,
                        ch,
                        1 << rank,
                        CommandIteration::Control,
                        (ctl + shift).min(127),
                        ctx,
                    )?;
                }
            }
            for (iteration, value) in [
                (
                    CommandIteration::CmdSouth,
                    pi(self.registers.command_south[ch].cmd_pi.get(), 0),
                ),
                (
                    CommandIteration::CmdNorth,
                    pi(self.registers.command_north[ch].cmd_pi.get(), 0),
                ),
            ] {
                self.shift_pi_for_cmd_training(
                    state,
                    ch,
                    ranks,
                    iteration,
                    (value + shift).min(127),
                    ctx,
                )?;
            }
        }
        Ok(())
    }

    /// cmd_training.c:meminit_cmd_training and ctl_clock_center_channel.
    /// Joint CTL/CLK, CMDS/CMDN binary centering, then grouped CTL centering.
    /// A failure rolls back ALL channels, including already-centered ones.
    pub fn cmd_training(
        &self,
        state: &mut CalibrationState,
        ctx: &CommandContext<'_>,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        let ranks = self.command_ranks(ctx)?;
        let mask = channels(ranks);
        let saved = core::array::from_fn::<_, 2, _>(|ch| PiSnapshot::load(self, state, ch));
        let result = (|| {
            // CADB consumes original VA rows 0..9, never a fixed-row substitute.
            self.setup_wdb(mask)?;
            for ch in 0..2 {
                if ranks[ch] != 0 {
                    self.ctl_clock_center_channel(state, ctx, ch, ranks[ch], delay)?;
                }
            }
            // Per-channel joint setup disabled the other channel's sequence.
            self.setup_cadb_test(state, mask, 14, StopOnError::Nth, delay)?;
            self.cmd_timing_centering(state, ctx, None, None, delay)?;
            self.cmd_ctl_centering(state, ctx, delay)
        })();
        let cleanup = self.disable_cadb_deselects(mask);
        let mut result = result.and(cleanup);
        if result.is_err() {
            for ch in 0..2 {
                if ranks[ch] != 0 {
                    saved[ch].restore(self, state, ch, ranks[ch], ctx.policy.sdram_lanes, true)?;
                }
            }
        }
        let reset = self.command_reset(state, ctx, delay);
        result = finish(result, reset);
        if result.is_err() {
            for ch in 0..2 {
                if ranks[ch] != 0 {
                    saved[ch].restore(self, state, ch, ranks[ch], ctx.policy.sdram_lanes, true)?;
                }
            }
        }
        result
    }

    /// highspeed.c:meminit_cmd_t_margin. Measured timing margins for later BER/
    /// SI consumers, with CLK and all dependent DQ/host mirrors restored.
    pub fn cmd_t_margin(
        &self,
        state: &mut CalibrationState,
        ctx: &CommandContext<'_>,
        selected: u8,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        let ranks = self.command_ranks(ctx)?;
        self.validate_channels(selected)?;
        let mut low = [0; 2];
        let mut high = [0; 2];
        for ch in 0..2 {
            if selected & (1 << ch) != 0 {
                let (min, max) = self.cmd_dq_pi_limits(ch, ranks[ch], ctx.policy)?;
                low[ch] = -min.min(64);
                high[ch] = (511 - max).min(64);
            }
        }
        self.cmd_clock_scan(state, ctx, selected, 15, low, high, 1, false, delay)
    }

    /// highspeed.c:meminit_cmd_normalization. Preserve at least 128 TX-DQ
    /// ticks for the Tx FIFO; shared DDP clocks/data move exactly once.
    pub fn cmd_normalization(
        &self,
        state: &mut CalibrationState,
        ctx: &CommandContext<'_>,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        let ranks = self.command_ranks(ctx)?;
        let saved = core::array::from_fn::<_, 2, _>(|ch| PiSnapshot::load(self, state, ch));
        let result = (|| {
            for ch in 0..2 {
                if ranks[ch] == 0 {
                    continue;
                }
                let s = &saved[ch];
                let mut min = pi(s.cke, 0).min(pi(s.cmds, 0)).min(pi(s.cmdn, 0));
                let mut min_dq = 511;
                for rank in 0..4 {
                    if ranks[ch] & (1 << rank) == 0 {
                        continue;
                    }
                    min = min.min(pi(s.ctl, rank));
                    if rank < 3 {
                        min = min.min(pi(s.cke_ctl, rank));
                    }
                    for byte in 0..ctx.policy.sdram_lanes {
                        let dq = (s.tx[rank][byte] & 511) as i16;
                        min_dq = min_dq.min(dq);
                        min = min
                            .min(dq)
                            .min(((s.tx[rank][byte] >> 9) & 511) as i16)
                            .min((s.rx[rank][byte] & 511) as i16);
                    }
                }
                min = if min_dq >= 128 {
                    min.min(min_dq - 128)
                } else {
                    0
                };
                self.shift_command_pi(
                    state,
                    s,
                    ch,
                    ranks[ch],
                    CommandIteration::Clock,
                    -min,
                    true,
                    ctx.policy,
                )?;
                for rank in 0..4 {
                    if ranks[ch] & (1 << rank) != 0 {
                        self.shift_command_pi(
                            state,
                            s,
                            ch,
                            1 << rank,
                            CommandIteration::Control,
                            pi(s.ctl, rank) - min,
                            true,
                            ctx.policy,
                        )?;
                    }
                }
                self.shift_command_pi(
                    state,
                    s,
                    ch,
                    ranks[ch],
                    CommandIteration::CmdSouth,
                    pi(s.cmds, 0) - min,
                    true,
                    ctx.policy,
                )?;
                self.shift_command_pi(
                    state,
                    s,
                    ch,
                    ranks[ch],
                    CommandIteration::CmdNorth,
                    pi(s.cmdn, 0) - min,
                    true,
                    ctx.policy,
                )?;
            }
            self.command_reset(state, ctx, delay)
        })();
        if result.is_err() {
            for ch in 0..2 {
                if ranks[ch] != 0 {
                    saved[ch].restore(self, state, ch, ranks[ch], ctx.policy.sdram_lanes, true)?;
                }
            }
        }
        result
    }
}

impl Engine<'_> {
    /// highspeed.c:meminit_cmd_voltage_centering, DDR4 path. `wrv_upm_limit`
    /// is the caller's resolved MEMINIT_PM_WRV UPM limit in tenths (including
    /// power_model.c frequency/retrain policy); no zero/default bypass exists.
    /// All seven CADB victims contribute byte errors at EACH Vref point.
    pub fn cmd_voltage_centering(
        &self,
        state: &mut CalibrationState,
        ctx: &CommandContext<'_>,
        t: &Timings,
        wrv_upm_limit: u16,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        let ranks = self.command_ranks(ctx)?;
        if wrv_upm_limit == 0 {
            return Err(Error::InvalidPolicy);
        }
        let mask = channels(ranks);
        let saved = core::array::from_fn::<_, 2, _>(|ch| PiSnapshot::load(self, state, ch));
        let offsets = core::array::from_fn::<_, 2, _>(|ch| {
            core::array::from_fn::<_, 9, _>(|b| self.registers.data_lane[b][ch].offset_train.get())
        });
        let vref = self.registers.data_lane[0][0].vref_adjust.get();
        let base = self.ca_vref_base();
        // Retain signed endpoints until the center has been selected. Taking
        // ABS first (as the C source does) destroys a same-side asymmetric eye.
        let mut endpoints = [[[(0i16, 0i16); 8]; 4]; 2];
        let mut common = [(-127i16, 127i16); 2];
        let mut centers = [0i16; 2];
        let result = (|| {
            self.setup_wdb(mask)?;
            self.setup_cadb_test(state, mask, 10, StopOnError::Never, delay)?;
            for ch in 0..2 {
                if ranks[ch] == 0 {
                    continue;
                }
                for b in 0..8 {
                    self.registers.data_lane[b][ch].offset_train.set(0);
                }
            }
            for rank in 0..4 {
                let mut rank_channels = 0;
                for ch in 0..2 {
                    if ranks[ch] & (1 << rank) != 0 {
                        rank_channels |= 1 << ch;
                    }
                    if ranks[ch] != 0 {
                        self.program_rank_map(ch, ranks[ch] & (1 << rank))?;
                    }
                }
                if rank_channels == 0 {
                    continue;
                }
                let mut eyes = [[PassingWindow::EMPTY; 8]; 2];
                for offset in (-63..=63).step_by(2) {
                    self.command_vref_offset(ctx, base, offset, delay)?;
                    self.command_reset(state, ctx, delay)?;
                    let errors = self.command_feedback(state, rank_channels, delay)?.1;
                    for ch in 0..2 {
                        if rank_channels & (1 << ch) == 0 {
                            continue;
                        }
                        let index = if ctx.policy.ca_vref_config == 0 {
                            0
                        } else {
                            ch
                        };
                        let representable = (-63..=63).contains(&(base[index] + offset));
                        for byte in 0..8 {
                            eyes[ch][byte].sample(
                                offset,
                                2,
                                representable && errors[ch] & (1 << byte) == 0,
                            );
                        }
                    }
                }
                self.command_vref_offset(ctx, base, 0, delay)?;
                self.command_reset(state, ctx, delay)?;
                for ch in 0..2 {
                    if rank_channels & (1 << ch) == 0 {
                        continue;
                    }
                    for byte in 0..8 {
                        let eye = eyes[ch][byte];
                        let min_width = if t.clock.tck_fs <= 833_333 { 4 } else { 8 };
                        if eye.width() < min_width {
                            return Err(failure(TrainingStep::CommandVoltage, ch, rank, byte));
                        }
                        let (left, right) = eye.best.ok_or(Error::InvalidTiming)?;
                        endpoints[ch][rank][byte] = (left, right);
                        common[ch].0 = common[ch].0.max(left);
                        common[ch].1 = common[ch].1.min(right);
                    }
                }
            }
            if ctx.policy.ca_vref_config == 0 {
                let mut shared = (-127, 127);
                for ch in 0..2 {
                    if ranks[ch] != 0 {
                        shared.0 = shared.0.max(common[ch].0);
                        shared.1 = shared.1.min(common[ch].1);
                    }
                }
                common = [shared; 2];
            }
            for ch in 0..2 {
                if ranks[ch] == 0 {
                    continue;
                }
                let (left, right) = common[ch];
                if right <= left || i32::from(right - left) * 10 < i32::from(wrv_upm_limit / 2) {
                    return Err(failure(TrainingStep::CommandVoltage, ch, 0, 0));
                }
                centers[ch] = (left + right) / 2;
            }
            if ctx.policy.ca_vref_config == 0 {
                self.ca_vref_update_for_training(
                    0,
                    base[0],
                    centers[mask.trailing_zeros() as usize],
                    delay,
                )?;
            } else {
                for ch in 0..2 {
                    if ranks[ch] != 0 {
                        self.ca_vref_update_for_training(ch, base[ch], centers[ch], delay)?;
                    }
                }
            }
            self.command_reset(state, ctx, delay)?;
            // Validate the actual selected centers, not an intermediate point.
            for rank in 0..4 {
                let mut selected = 0;
                for ch in 0..2 {
                    if ranks[ch] == 0 {
                        continue;
                    }
                    let rm = ranks[ch] & (1 << rank);
                    self.program_rank_map(ch, rm)?;
                    if rm != 0 {
                        selected |= 1 << ch;
                    }
                }
                if selected != 0 {
                    let errors = self.command_feedback(state, selected, delay)?.1;
                    for ch in 0..2 {
                        if selected & (1 << ch) != 0 && errors[ch] != 0 {
                            return Err(failure(
                                TrainingStep::CommandVoltage,
                                ch,
                                rank,
                                errors[ch].trailing_zeros() as usize,
                            ));
                        }
                    }
                }
            }
            for ch in 0..2 {
                for rank in 0..4 {
                    if ranks[ch] & (1 << rank) == 0 {
                        continue;
                    }
                    for byte in 0..8 {
                        let (left, right) = endpoints[ch][rank][byte];
                        state.margin_results[ch][rank][byte][14] =
                            u32::from(left.unsigned_abs()) * 10;
                        state.margin_results[ch][rank][byte][15] =
                            u32::from(right.unsigned_abs()) * 10;
                    }
                    // Native byte-0 command margins around the new common
                    // center, only for physically present channel/rank pairs.
                    state.margin_results[ch][rank][0][14] =
                        (centers[ch] - common[ch].0) as u32 * 10;
                    state.margin_results[ch][rank][0][15] =
                        (common[ch].1 - centers[ch]) as u32 * 10;
                }
            }
            Ok(())
        })();
        for ch in 0..2 {
            if ranks[ch] != 0 {
                for b in 0..ctx.policy.sdram_lanes {
                    self.registers.data_lane[b][ch]
                        .offset_train
                        .set(offsets[ch][b]);
                }
            }
        }
        let cleanup = self.disable_cadb_deselects(mask);
        let result = result.and(cleanup);
        if result.is_err() {
            let restore = self.restore_ca_vref(ctx, vref, delay);
            for ch in 0..2 {
                if ranks[ch] != 0 {
                    saved[ch].restore(self, state, ch, ranks[ch], ctx.policy.sdram_lanes, true)?;
                }
            }
            let reset = self.command_reset(state, ctx, delay);
            // Preserve the first operational failure; cleanup was still tried.
            return finish(finish(result, restore), reset);
        }
        result
    }
}
