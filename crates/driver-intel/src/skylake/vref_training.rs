// SPDX-License-Identifier: MIT OR Apache-2.0
//! DDR4 MR6/PDA operations from native/vref_training.c and training.c.
//! Failures invalidate further training even when restoration succeeds.

use super::{
    Error,
    jedec::vref_code,
    registers::MrCommand,
    reut::{Engine, rank_mask},
    training_state::CalibrationState,
};
use embedded_hal::delay::DelayNs;
use tock_registers::interfaces::{Readable, Writeable};

/// training.c:meminit_vref_dq_to_offset (inverse of jedec::vref_code).
pub fn vref_dq_to_offset(code: u8) -> i16 {
    let mut value = ((code << 1) as i8 >> 1) as i16;
    if value < 0 {
        value += 41;
    }
    value.clamp(-23, 50) - 13
}

pub(crate) fn vref_bounds(range: u8) -> Result<(i16, i16), Error> {
    match range {
        0 => Ok((-36, 36)),
        1 => Ok((-13, 36)),
        2 => Ok((-36, 14)),
        _ => Err(Error::InvalidPolicy),
    }
}

/// Shared wl_mr5_park_rtt_wr / mrs_pda_enter termination image.
pub(crate) fn park_rtt_wr(state: &CalibrationState, ch: usize, rank: usize) -> u16 {
    let park = match (state.saved_mr.rank[ch][rank][2] >> 9) & 7 {
        1 => 2,
        2 => 4,
        4 => 6,
        _ => 0,
    };
    (state.saved_mr.rank[ch][rank][5] & !(7 << 6)) | (park << 6)
}

impl Engine<'_> {
    /// vref_training.c:mrs_mr6_vref: enable, enable, disable, preserving
    /// tCCD_L and waiting tVREFDQ_MIN after every command. Always attempt the
    /// disabling write, including after a failed enabling command.
    pub(crate) fn mrs_mr6_vref(
        &self,
        ch: usize,
        rank: usize,
        mr6: u16,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        let mut result = Ok(());
        for _ in 0..2 {
            if result.is_ok() {
                result = self.training_mrs(ch, rank, 6, mr6 | 0x80, delay);
                delay.delay_us(1);
            }
        }
        let disable = self.training_mrs(ch, rank, 6, mr6 & !0x80, delay);
        delay.delay_us(1);
        result.and(disable)
    }

    pub(crate) fn write_vref_offset(
        &self,
        state: &CalibrationState,
        ch: usize,
        rank: usize,
        offset: i16,
        delay: &mut impl DelayNs,
    ) -> Result<u16, Error> {
        let code = vref_code(i32::from(offset), state.ddr4_tx_vref_range[ch])?;
        self.mrs_mr6_vref(
            ch,
            rank,
            (state.saved_mr.rank[ch][rank][6] & !0xff) | code,
            delay,
        )?;
        Ok(code)
    }

    /// training.c:mrs_pda_mr6. Caller brackets this with RTT_PARK entry/exit.
    pub(crate) fn mrs_pda_mr6(
        &self,
        state: &CalibrationState,
        ch: usize,
        rank: usize,
        device_mask: u16,
        code: u8,
        enable: bool,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        let r = self.channel(ch)?;
        if rank >= 4
            || rank_mask(self.dimms, ch)? & (1 << rank) == 0
            || device_mask == 0
            || device_mask & !0x1ff != 0
            || code > 0x7f
        {
            return Err(Error::InvalidRank);
        }
        let wait = |delay: &mut dyn DelayNs| {
            for _ in 0..10_000 {
                if !r.mr_command.is_set(MrCommand::BUSY) {
                    return Ok(());
                }
                delay.delay_us(1);
            }
            Err(Error::MrCommandTimeout)
        };
        wait(delay)?;
        let mr6 =
            (state.saved_mr.rank[ch][rank][6] & !0xff) | u16::from(code) | (u16::from(enable) << 7);
        r.mr_command.write(
            MrCommand::ADDRESS.val(u32::from((mr6 & 0x1f00) >> 5) | 6)
                + MrCommand::DATA.val(u32::from(mr6 & 0xff))
                + MrCommand::RANK.val(rank as u32)
                + MrCommand::COMMAND.val(2)
                + MrCommand::DEVICE_SKIP.val(u32::from(!device_mask & 0x1ff))
                + MrCommand::BUSY::SET,
        );
        wait(delay)
    }

    /// training.c:update_pda_center_data/update_vref_non_pda and PDA sequence.
    /// The source uses physical byte selectors, not a board wiring fallback.
    /// Publish host centers only after every command and MR5 restoration.
    pub(crate) fn apply_pda_centers(
        &self,
        state: &mut CalibrationState,
        ch: usize,
        rank: usize,
        codes: [u8; 8],
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        let old_mr6 = state.saved_mr.rank[ch][rank][6];
        let avg = codes.iter().map(|&v| vref_dq_to_offset(v)).sum::<i16>() / 8;
        let avg_code = vref_code(i32::from(avg), state.ddr4_tx_vref_range[ch])?;
        let mr6 = (old_mr6 & !0xff) | avg_code;
        let result = (|| {
            self.mrs_mr6_vref(ch, rank, mr6, delay)?;
            delay.delay_us(1);
            self.training_mrs(ch, rank, 5, park_rtt_wr(state, ch, rank), delay)?;
            for (byte, &code) in codes.iter().enumerate() {
                let mut device_result = Ok(());
                for _ in 0..2 {
                    if device_result.is_ok() {
                        device_result =
                            self.mrs_pda_mr6(state, ch, rank, 1 << byte, code, true, delay);
                        delay.delay_us(1);
                    }
                }
                let disable = self.mrs_pda_mr6(state, ch, rank, 1 << byte, code, false, delay);
                delay.delay_us(1);
                device_result.and(disable)?;
            }
            Ok(())
        })();
        let restore = self.training_mrs(ch, rank, 5, state.saved_mr.rank[ch][rank][5], delay);
        let result = result.and(restore);
        if result.is_ok() {
            state.saved_mr.rank[ch][rank][6] = mr6;
            state.ddr4_pda_mr6[ch][rank][..8].copy_from_slice(&codes);
        } else {
            // A non-PDA write restores all devices, not just the last selector.
            let _ = self.mrs_mr6_vref(ch, rank, old_mr6, delay);
        }
        result
    }
}
