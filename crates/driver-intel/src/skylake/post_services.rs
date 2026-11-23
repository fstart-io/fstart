// SPDX-License-Identifier: MIT OR Apache-2.0
//! Native post_services.c cold DDR4 consumers. Measurements are real REUT BER;
//! a retrain request is an outcome, never successful memory initialization.
use super::{
    Error,
    ber_margin::{BerSweep, MarginParameter},
    command_training::CommandContext,
    io_test::{DqPattern, StopOnError},
    memory_check::stopped,
    power_model::{
        LimitKind, MarginKind, TrainingMode, initialize_margin_limits, margin_limit,
        working_margin_limit,
    },
    read_centering::ReadSnapshot,
    registers::MadIntra,
    reut::{Engine, RankLane},
    timings::Clock,
    training::{channels, cold_ranks},
    training_state::CalibrationState,
    write_centering::{GlobalMr6Encoding, WriteSnapshot},
};
use embedded_hal::delay::DelayNs;
use tock_registers::interfaces::{Readable, Writeable};

/// Independent outer images survive nested BER/MR/PDA/reset helpers. Reuses
/// canonical RX/TX rollback; not the much larger optimization workspace.
pub struct PostServicesWorkspace {
    read: ReadSnapshot,
    write: WriteSnapshot,
    measured: [[[[u32; 8]; 8]; 4]; 2],
    /// Literal native diagnostic coordinates, NOT physical BER eye widths.
    /// Only meaningful after a successful rank_margin_tool result.
    pub rmt: [[[RmtEye; 8]; 4]; 2],
}
impl PostServicesWorkspace {
    pub const ZERO: Self = Self {
        read: ReadSnapshot::ZERO,
        write: WriteSnapshot::ZERO,
        measured: [[[[0; 8]; 8]; 4]; 2],
        rmt: [[[RmtEye::EMPTY; 8]; 4]; 2],
    };
}
/// Native absolute scan/report coordinates. Physical DQS fields mask each
/// tested point to six bits, so native_window() is NOT a physical eye width.
/// Native default0/256 values are retained but never marked as measured edges.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RmtEye {
    pub center: u8,
    pub left: u16,
    pub right: u16,
    pub left_valid: bool,
    pub right_valid: bool,
}
impl RmtEye {
    pub const EMPTY: Self = Self {
        center: 0,
        left: 0,
        right: 256,
        left_valid: false,
        right_valid: false,
    };
    pub const fn native_window(self) -> u16 {
        self.right.saturating_sub(self.left)
    }
    pub const fn valid(self) -> bool {
        self.left_valid && self.right_valid && self.native_window() != 0
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetrainOutcome {
    Passed,
    Required { read: bool, write: bool },
}
#[derive(Debug, Clone, Copy)]
pub struct PostServicesPolicy {
    pub training_enables: u32,
    pub training_mode: TrainingMode,
}
const PARAMETERS: [(MarginParameter, MarginKind); 4] = [
    (MarginParameter::ReadTiming, MarginKind::ReadTiming),
    (MarginParameter::WriteTiming, MarginKind::WriteTiming),
    (MarginParameter::ReadVoltage, MarginKind::ReadVoltage),
    (MarginParameter::WriteVoltage, MarginKind::WriteVoltage),
];

impl Engine<'_> {
    fn services_preflight(
        &self,
        s: &CalibrationState,
        ctx: &CommandContext<'_>,
    ) -> Result<[u8; 2], Error> {
        self.services_preflight_encoding(s, ctx, GlobalMr6Encoding::ChannelRange)
    }
    fn services_preflight_encoding(
        &self,
        s: &CalibrationState,
        ctx: &CommandContext<'_>,
        global_encoding: GlobalMr6Encoding,
    ) -> Result<[u8; 2], Error> {
        let ranks = cold_ranks(self, ctx.mode)?;
        self.validate_write_centering_encoding(s, ctx, channels(ranks), global_encoding)?;
        stopped(self)?;
        if self
            .registers
            .mad_intra
            .iter()
            .any(|word| word.read(MadIntra::ECC) != 0)
        {
            return Err(Error::EccRequired);
        }
        for ch in 0..2 {
            for rank in 0..4 {
                if ranks[ch] & (1 << rank) == 0 {
                    continue;
                }
                if s.saved_mr.rank[ch][rank].iter().any(|mr| mr & !0x3fff != 0) {
                    return Err(Error::InvalidTiming);
                }
                for byte in 0..8 {
                    let host = &s.training.ch[ch];
                    let v = i16::from(host.rd_vref[byte])
                        + i16::from(host.rx_vref_rank_offset[rank][byte]);
                    if host.rx_dqs_p[rank][byte] > 63
                        || host.rx_dqs_n[rank][byte] > 63
                        || !(-32..=31).contains(&v)
                    {
                        return Err(Error::InvalidTiming);
                    }
                }
            }
        }
        Ok(ranks)
    }
    /// Every physical rank and all eight data bytes are checked. Unlike the
    /// native rank<2/byte0 restriction, slot1/ranks2/3 cannot silently escape.
    pub fn meminit_retrain_margin_check(
        &self,
        s: &mut CalibrationState,
        ctx: &CommandContext<'_>,
        clock: Clock,
        mode: TrainingMode,
        w: &mut PostServicesWorkspace,
        delay: &mut impl DelayNs,
    ) -> Result<RetrainOutcome, Error> {
        let ranks = self.services_preflight(s, ctx)?;
        super::frequency::qclk_ps(self.registers, clock)?;
        if s.retrain_iteration > 1 {
            return Err(Error::InvalidPolicy);
        }
        // Validate the whole mutable policy, including possible increments,
        // before consuming WDB or changing any training state.
        let mut limits = s.margin_limits;
        if s.retrain_iteration == 0 {
            // Canonical initialization is performed only after all preflight.
            limits = super::power_model::INITIAL_LIMITS;
        }
        for (_, kind) in PARAMETERS {
            let l = limits
                .iter()
                .find(|l| l.param == kind as u8)
                .ok_or(Error::InvalidPolicy)?;
            l.upm_limit.checked_add(80).ok_or(Error::InvalidPolicy)?;
            l.pwr_limit.checked_add(80).ok_or(Error::InvalidPolicy)?;
        }
        self.measure_post_margins(s, ctx, ranks, true, w, delay)?;
        if s.retrain_iteration == 0 {
            initialize_margin_limits(s);
        }
        compare_retrain(s, ranks, clock, mode)
    }
    fn measure_post_margins(
        &self,
        s: &mut CalibrationState,
        ctx: &CommandContext<'_>,
        ranks: [u8; 2],
        all_types: bool,
        w: &mut PostServicesWorkspace,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        self.read_snapshot(s, &mut w.read)?;
        self.write_snapshot(s, &mut w.write)?;
        let mask = channels(ranks);
        let measured = (|| {
            self.setup_wdb(mask)?;
            s.en_dummy_reads = 0;
            self.setup_basic_va_test(s, mask, 17, 8, StopOnError::Never, delay)?;
            for rank in 0..4 {
                let active = self.select_read_rank(mask, rank)?;
                if active == 0 {
                    continue;
                }
                for parameter in [
                    MarginParameter::ReadTiming,
                    MarginParameter::WriteTiming,
                    MarginParameter::ReadVoltage,
                ] {
                    if !all_types && parameter != MarginParameter::ReadTiming {
                        continue;
                    }
                    s.dq_pat = if parameter == MarginParameter::ReadTiming {
                        DqPattern::ReadRead as u8
                    } else {
                        DqPattern::BasicVa as u8
                    };
                    self.get_ber_margin_byte_physical(
                        s,
                        ctx,
                        BerSweep {
                            parameter,
                            channels: active,
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
                    )?;
                }
            }
            if all_types {
                // WRV moves the entire selected rank mask JOINTLY. Canonical
                // BER copies aggregate eyes then rebases each device's PDA
                // center; independently sweeping ranks is not equivalent.
                for ch in 0..2 {
                    if ranks[ch] != 0 {
                        self.program_rank_map(ch, ranks[ch])?;
                    }
                }
                s.dq_pat = DqPattern::BasicVa as u8;
                self.get_ber_margin_byte(
                    s,
                    ctx,
                    BerSweep {
                        parameter: MarginParameter::WriteVoltage,
                        channels: mask,
                        rank_in: ranks[0] | ranks[1],
                        rank_rx: ranks[0] | ranks[1],
                        per_channel: false,
                        interpolate: true,
                        max_margin: 36,
                        interior: false,
                        byte_map: None,
                    },
                    &mut [0; 4],
                    delay,
                )?;
            }
            for ch in 0..2 {
                for rank in 0..4 {
                    for byte in 0..8 {
                        if !all_types
                            && ranks[ch] & (1 << rank) != 0
                            && s.margin_results[ch][rank][byte][2..4].contains(&0)
                        {
                            return Err(Error::TrainingFailed {
                                step: super::TrainingStep::ReadTiming,
                                channel: ch as u8,
                                rank: rank as u8,
                                lane: byte as u8,
                            });
                        }
                        w.measured[ch][rank][byte]
                            .copy_from_slice(&s.margin_results[ch][rank][byte][..8]);
                    }
                }
            }
            Ok(())
        })();
        // Always restore independent RX/TX and host images, including margins.
        // Final RF downloads occur INSIDE these helpers, BEFORE their final
        // unicast DATACONTROL0 restore. No download may follow this cleanup.
        let tx = self.restore_write_centering(s, mask, ranks, &w.write, true, delay);
        let rx = self.read_restore(s, ranks, &w.read, true, delay);
        tx.and(rx).and(measured)?;
        for ch in 0..2 {
            for rank in 0..4 {
                if ranks[ch] & (1 << rank) == 0 {
                    continue;
                }
                for byte in 0..8 {
                    if all_types {
                        s.margin_results[ch][rank][byte][..8]
                            .copy_from_slice(&w.measured[ch][rank][byte]);
                    } else {
                        s.margin_results[ch][rank][byte][2..4]
                            .copy_from_slice(&w.measured[ch][rank][byte][2..4]);
                    }
                }
            }
        }
        Ok(())
    }
    /// Separate relative-BER validation service around independent P/N centers.
    /// This is NOT the native absolute-coordinate rank margin tool/report.
    pub fn bounded_read_margin(
        &self,
        s: &mut CalibrationState,
        ctx: &CommandContext<'_>,
        clock: Clock,
        w: &mut PostServicesWorkspace,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        let ranks = self.services_preflight(s, ctx)?;
        super::frequency::qclk_ps(self.registers, clock)?;
        self.measure_post_margins(s, ctx, ranks, false, w, delay)
    }
    /// Literal native post_services.c RMT: even absolute points upward from0,
    /// odd absolute points downward from255, with the canonical setter masking
    /// to six bits and setting BOTH P/N before physical read-RF download.
    /// Reports preserve unmasked coordinates, never reinterpret them as BER
    /// margins or physical widths. For example physical pass20..44 at center32
    /// yields left20/right235/window215. Invalid/default edges and transport
    /// failures are fatal, unlike native's misleading success-shaped prints.
    /// Independent entry P/N, common centers and transient state are restored;
    /// no measured margins or usable-memory certificate are published.
    pub fn rank_margin_tool(
        &self,
        s: &mut CalibrationState,
        ctx: &CommandContext<'_>,
        clock: Clock,
        w: &mut PostServicesWorkspace,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        let ranks = self.services_preflight(s, ctx)?;
        let qclk = super::frequency::qclk_ps(self.registers, clock)?;
        for ch in 0..2 {
            for rank in 0..4 {
                if ranks[ch] & (1 << rank) != 0
                    && s.training.ch[ch].rank[rank]
                        .rd_dqs_dly
                        .iter()
                        .any(|center| *center > 63)
                {
                    return Err(Error::InvalidTiming);
                }
            }
        }
        self.read_snapshot(s, &mut w.read)?;
        w.rmt = [[[RmtEye::EMPTY; 8]; 4]; 2];
        let result = (|| {
            self.setup_wdb(channels(ranks))?;
            for ch in 0..2 {
                for rank in 0..4 {
                    if ranks[ch] & (1 << rank) == 0 {
                        continue;
                    }
                    // Native basic-VA honors enabled dummy reads using the
                    // actual resolved QCLK; the convenience wrapper defaults0.
                    let setup = super::io_test::TestSetup {
                        qclk_ps: qclk,
                        ..super::io_test::basic_setup(17, 8, StopOnError::Never)?
                    };
                    self.setup_io_test(s, 1 << ch, &setup, delay)?;
                    s.dq_pat = DqPattern::BasicVa as u8;
                    s.dq_pat_lc = u16::from(setup.loop_exponent.saturating_sub(7).max(1));
                    self.program_rank_map(ch, 1 << rank)?;
                    for byte in 0..8 {
                        let center = s.training.ch[ch].rank[rank].rd_dqs_dly[byte];
                        w.rmt[ch][rank][byte].center = center;
                        let target = RankLane {
                            channel: ch,
                            rank,
                            lane: byte,
                        };
                        for point in (0..u16::from(center)).step_by(2) {
                            self.set_rx_dqs_delay(s, target, point as u8)?;
                            delay.delay_us(1);
                            self.run_io(1 << ch, delay)?;
                            if self.read_errors(ch)?.failing_bits_per_byte[byte] == 0 {
                                w.rmt[ch][rank][byte].left = point;
                                w.rmt[ch][rank][byte].left_valid = true;
                                break;
                            }
                        }
                        // Finite odd points avoid native unsigned underflow
                        // when center0 has no passing right edge.
                        for point in (1u16..=255).rev().step_by(2) {
                            if point <= u16::from(center) {
                                break;
                            }
                            self.set_rx_dqs_delay(s, target, point as u8)?;
                            delay.delay_us(1);
                            self.run_io(1 << ch, delay)?;
                            if self.read_errors(ch)?.failing_bits_per_byte[byte] == 0 {
                                w.rmt[ch][rank][byte].right = point;
                                w.rmt[ch][rank][byte].right_valid = true;
                                break;
                            }
                        }
                        if !w.rmt[ch][rank][byte].valid() {
                            return Err(Error::TrainingFailed {
                                step: super::TrainingStep::ReadTiming,
                                channel: ch as u8,
                                rank: rank as u8,
                                lane: byte as u8,
                            });
                        }
                    }
                }
            }
            Ok(())
        })();
        let cleanup = self.read_restore(s, ranks, &w.read, true, delay);
        cleanup.and(result)
    }
    /// Native BIT30/power/first-pass retrain gate and BIT26 RMT gate.
    pub fn meminit_post_services(
        &self,
        s: &mut CalibrationState,
        ctx: &CommandContext<'_>,
        clock: Clock,
        policy: PostServicesPolicy,
        w: &mut PostServicesWorkspace,
        delay: &mut impl DelayNs,
    ) -> Result<RetrainOutcome, Error> {
        self.services_preflight(s, ctx)?;
        let outcome = if policy.training_enables & (1 << 30) != 0
            && policy.training_mode == TrainingMode::Power
            && s.retrain_iteration == 0
        {
            self.meminit_retrain_margin_check(s, ctx, clock, policy.training_mode, w, delay)?
        } else {
            RetrainOutcome::Passed
        };
        if policy.training_enables & (1 << 26) != 0 {
            self.rank_margin_tool(s, ctx, clock, w, delay)?;
        }
        Ok(outcome)
    }
    /// Actual MR0..6/MR7-zero diagnostics, first-populated-rank content,
    /// physical-rank feedback and eight PDA bytes. SA-GV FSM is unsupported.
    pub fn meminit_fill_mr_scratch(
        &self,
        s: &CalibrationState,
        ctx: &CommandContext<'_>,
    ) -> Result<(), Error> {
        // Native final TX-range reconciliation preserves saved global MR6 as
        // a SOURCE coordinate, not the newly encoded live hardware image.
        // Keep every other validation, including current-range PDA/bit codes.
        let ranks = self.services_preflight_encoding(s, ctx, GlobalMr6Encoding::SourceBaseline)?;
        for ch in 0..2 {
            for rank in 0..4 {
                if ranks[ch] & (1 << rank) == 0 {
                    continue;
                }
                let mr = s.saved_mr.rank[ch][rank];
                self.registers.data_lane[rank][ch]
                    .feedback
                    .set(u32::from(mr[0]) | u32::from(mr[1]) << 16);
                for half in 0..2 {
                    let value = (0..4).fold(0, |v, b| {
                        v | u32::from(s.ddr4_pda_mr6[ch][rank][half * 4 + b] & 127) << (8 * b)
                    });
                    self.registers.channel[ch].mr6_vref[rank][half].set(value);
                }
            }
            for d in 0..2 {
                if ranks[ch] & (3 << (2 * d)) == 0 {
                    continue;
                }
                for (index, mr) in [(4 + d, 2), (6 + d, 5)] {
                    self.registers.data_lane[index][ch].feedback.set(
                        u32::from(s.saved_mr.rank[ch][2 * d][mr])
                            | u32::from(s.saved_mr.rank[ch][2 * d + 1][mr]) << 16,
                    );
                }
            }
            if ranks[ch] != 0 {
                let mr = s.saved_mr.rank[ch][ranks[ch].trailing_zeros() as usize];
                for i in 0..4 {
                    self.registers.channel[ch].mr_content[i].set(
                        u32::from(mr[i * 2])
                            | if i == 3 {
                                0
                            } else {
                                u32::from(mr[i * 2 + 1]) << 16
                            },
                    );
                }
            }
        }
        Ok(())
    }
}
fn compare_retrain(
    s: &mut CalibrationState,
    ranks: [u8; 2],
    clock: Clock,
    mode: TrainingMode,
) -> Result<RetrainOutcome, Error> {
    let mut failed = [false; 2];
    for (p, kind) in PARAMETERS {
        let limit = (u32::from(margin_limit(s, kind, LimitKind::Retrain, clock, mode)?) + 5) / 10;
        for ch in 0..2 {
            for rank in 0..4 {
                if ranks[ch] & (1 << rank) == 0 {
                    continue;
                }
                for byte in 0..8 {
                    for edge in 0..2 {
                        if s.margin_results[ch][rank][byte][p.result_type() * 2 + edge] / 10
                            <= limit
                        {
                            failed[usize::from(matches!(
                                p,
                                MarginParameter::WriteTiming | MarginParameter::WriteVoltage
                            ))] = true;
                        }
                    }
                }
            }
        }
    }
    if failed == [false; 2] {
        return Ok(RetrainOutcome::Passed);
    }
    for (p, kind) in PARAMETERS {
        if failed[usize::from(matches!(
            p,
            MarginParameter::WriteTiming | MarginParameter::WriteVoltage
        ))] {
            let l = working_margin_limit(s, kind)?;
            l.upm_limit = l.upm_limit.checked_add(80).ok_or(Error::InvalidPolicy)?;
            l.pwr_limit = l.pwr_limit.checked_add(80).ok_or(Error::InvalidPolicy)?;
        }
    }
    s.upm_pwr_retrain_flag = 1;
    Ok(RetrainOutcome::Required {
        read: failed[0],
        write: failed[1],
    })
}
#[cfg(test)]
#[path = "post_services_tests.rs"]
mod tests;
