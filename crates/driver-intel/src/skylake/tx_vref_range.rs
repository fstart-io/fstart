// SPDX-License-Identifier: MIT OR Apache-2.0
//! Final DDR4 range reconciliation from native/additional_steps.c:977–1059.
//! This is a late cold-training operation, not a dispatcher or RAM certificate.

use super::{
    Error,
    ber_margin::MarginParameter,
    command_training::CommandContext,
    io_test::DqPattern,
    jedec::vref_code,
    opt_param::sweep::TestSnapshot,
    registers::{MrCommand, ReutControl, ReutSequence},
    reut::Engine,
    reut_registers::CadbControl,
    training::cold_ranks,
    training_state::CalibrationState,
    vref_training::vref_dq_to_offset,
};
use embedded_hal::delay::DelayNs;
use tock_registers::interfaces::{Readable, Writeable};

// Validate the encoded range, not the channel's prospective range: final
// trained device centers can legitimately use both overlapping JEDEC ranges.
fn validate_code(code: u8) -> Result<(), Error> {
    let range = if code & 64 == 0 { 1 } else { 2 };
    if code > 127 || vref_code(i32::from(vref_dq_to_offset(code)), range)? as u8 != code {
        return Err(Error::InvalidTiming);
    }
    Ok(())
}

fn select_ranges(s: &CalibrationState, ranks: [u8; 2]) -> ([u8; 2], bool) {
    let mut ranges = s.ddr4_tx_vref_range;
    let mut any_mixed = false;
    for ch in 0..2 {
        let mut min = 36i16;
        let mut max = -36i16;
        let mut mixed = false;
        let mut range_sum = 0;
        for r in 0..4 {
            if ranks[ch] & (1 << r) == 0 {
                continue;
            }
            for &code in &s.ddr4_pda_mr6[ch][r][..8] {
                range_sum += (code >> 6) & 1;
                let offset = vref_dq_to_offset(code);
                min = min.min(offset);
                max = max.max(offset);
            }
            // Deliberately cumulative across ranks, compared to ONE rank's
            // lane count after each rank. Even all-range2 multi-rank enters.
            // Do not reset the sum or replace this with conventional mixing.
            if range_sum != 0 && range_sum != 8 {
                mixed = true;
            }
        }
        if mixed {
            any_mixed = true;
            ranges[ch] = if max > 14 && min > -14 {
                1
            } else if (max <= 14 && min <= -14) || max - 14 > -14 - min {
                2
            } else {
                1
            };
        }
    }
    (ranges, any_mixed)
}

impl Engine<'_> {
    /// Native `meminit_tx_vref_range`, default TE2_TXDQVREF final operation.
    /// Inputs must be the actual final trained global/device centers. Only
    /// resolved standard-profile cold DDR4, eight data lanes, SA-GV disabled
    /// are supported. Caller exclusively owns stopped REUT/CADB/MR engines.
    ///
    /// If ANY channel takes the source mixed branch, re-encode ALL populated
    /// global ranks first, THEN every physical device (including other channels).
    /// Native span selection/clamping and cumulative range_sum are intentional.
    /// Saved MR6 remains the independent SOURCE coordinate baseline, NOT an
    /// exact image/readback of the newly encoded global hardware MR6. PDA and
    /// channel range are published; no device average replaces a global center.
    ///
    /// Errors are fatal. Original host inputs are restored before a best-effort
    /// saved-MR/PDA cold reset; a reset/cleanup error takes precedence. Neither
    /// an error nor successful cleanup proves restoration of unknown hardware.
    /// Readable test controls are restored last; write-only CADB FIFO ownership
    /// is invalidated, never fictitiously replayed. No reset occurs on success.
    pub fn meminit_tx_vref_range(
        &self,
        s: &mut CalibrationState,
        ctx: &CommandContext<'_>,
        standard_profile: bool,
        sa_gv: bool,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        let ranks = cold_ranks(self, ctx.mode)?;
        if !standard_profile
            || sa_gv
            || ctx.policy.sdram_lanes != 8
            || !matches!(ctx.policy.ca_vref_config, 0 | 2)
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
        // The independent PDA MR-command engine must also be idle before
        // changing even a host range or issuing the first global MRS.
        if self
            .registers
            .channel
            .iter()
            .any(|c| c.mr_command.is_set(MrCommand::BUSY))
        {
            return Err(Error::MrCommandTimeout);
        }
        for ch in 0..2 {
            if s.ddr4_tx_vref_range[ch] > 2 {
                return Err(Error::InvalidPolicy);
            }
            for r in 0..4 {
                if ranks[ch] & (1 << r) == 0 {
                    continue;
                }
                let mr6 = s.saved_mr.rank[ch][r][6];
                if mr6 & 128 != 0 {
                    return Err(Error::InvalidTiming);
                }
                validate_code(mr6 as u8 & 127)?;
                for &code in &s.ddr4_pda_mr6[ch][r][..8] {
                    validate_code(code)?;
                }
            }
        }
        let (ranges, any_mixed) = select_ranges(s, ranks);
        if !any_mixed {
            return Ok(());
        }
        let old_ranges = s.ddr4_tx_vref_range;
        let old_pda = s.ddr4_pda_mr6;
        let tests = [TestSnapshot::load(self, 0)?, TestSnapshot::load(self, 1)?];
        let init = self.registers.init_state.get();
        let global_control = self.registers.reut_global_control.get();
        let base = self.ber_baseline(ctx)?;
        s.ddr4_tx_vref_range = ranges;
        let result = (|| {
            self.ber_change_margin(
                s,
                &base,
                ctx,
                MarginParameter::WriteVoltage,
                0,
                0,
                0,
                0,
                true,
                delay,
            )?;
            for ch in 0..2 {
                for r in 0..4 {
                    if ranks[ch] & (1 << r) == 0 {
                        continue;
                    }
                    for b in 0..8 {
                        let code =
                            vref_code(i32::from(vref_dq_to_offset(old_pda[ch][r][b])), ranges[ch])?
                                as u8;
                        // BER consumes the original independent device center;
                        // publish only after its physical-byte write succeeds.
                        self.ber_change_margin(
                            s,
                            &base,
                            ctx,
                            MarginParameter::WriteVoltage,
                            0,
                            ch,
                            r,
                            1 << b,
                            false,
                            delay,
                        )?;
                        s.ddr4_pda_mr6[ch][r][b] = code;
                    }
                }
            }
            Ok(())
        })();
        let cleanup = if result.is_err() {
            s.ddr4_tx_vref_range = old_ranges;
            s.ddr4_pda_mr6 = old_pda;
            // Canonical reset restores exact original encoded PDA bytes, not
            // BER's re-encoding through a formerly mixed channel range.
            self.reinitialize_cold(s, ctx.pmc, ctx.mode, 8, delay)
        } else {
            Ok(())
        };
        self.registers.init_state.set(init);
        for (ch, saved) in tests.iter().enumerate() {
            saved.restore(self, ch);
        }
        self.registers.reut_global_control.set(global_control);
        s.dq_pat = u8::MAX;
        s.dq_pat_lc = 0;
        cleanup.and(result)
    }
}

#[cfg(test)]
#[path = "tx_vref_range_tests.rs"]
mod tests;
