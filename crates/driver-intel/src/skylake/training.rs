// SPDX-License-Identifier: MIT OR Apache-2.0
//! Shared cold-training operations from native/training.c. No pipeline or
//! usable-memory result is provided here; callers retain the full CAR workspace.

use super::{
    Error,
    phy_registers::DataControl0,
    reut::{CadbCommand, Command, DownloadTarget, Engine, RankLane, RegisterFile, rank_mask},
    training_state::CalibrationState,
};
use crate::sunrise_point::pmc::MemoryBootMode;
use embedded_hal::delay::DelayNs;
use tock_registers::interfaces::ReadWriteable;

pub(crate) fn cold_ranks(engine: &Engine<'_>, mode: MemoryBootMode) -> Result<[u8; 2], Error> {
    if !matches!(mode, MemoryBootMode::Cold | MemoryBootMode::S3) {
        return Err(Error::RetainedMemoryUnsupported);
    }
    let ranks = [rank_mask(engine.dimms, 0)?, rank_mask(engine.dimms, 1)?];
    if ranks == [0; 2] {
        return Err(Error::NoMemory);
    }
    Ok(ranks)
}

pub(crate) fn channels(ranks: [u8; 2]) -> u8 {
    u8::from(ranks[0] != 0) | (u8::from(ranks[1] != 0) << 1)
}

impl Engine<'_> {
    /// training.c:meminit_send_mrs, with physical CS selection and error propagation.
    pub(crate) fn training_mrs(
        &self,
        ch: usize,
        rank: usize,
        mr: u8,
        address: u16,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        CadbCommand::for_ranks(self.dimms, ch, 1 << rank, Command::Mrs, mr, [address; 2], 0)?.issue(
            self.registers,
            ch,
            delay,
        )
    }

    /// sense_amp.c:update_rxt: separate from the persistent REUT download.
    pub(crate) fn pulse_rx(
        &self,
        ch: usize,
        rank: usize,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        self.download(DownloadTarget::Channel(ch), rank, RegisterFile::Read)?;
        delay.delay_us(2);
        self.registers.data_channel[ch]
            .control0
            .modify(DataControl0::READ_RF_READ::CLEAR);
        Ok(())
    }

    /// training.c:program_vref_all_ranks. Temporary sweep points intentionally
    /// do not change the host operational Vref center.
    pub fn program_read_vref(&self, ch: usize, vref: i8) -> Result<(), Error> {
        let ranks = rank_mask(self.dimms, ch)?;
        self.channel(ch)?;
        if !(-32..=31).contains(&vref) {
            return Err(Error::InvalidPolicy);
        }
        for rank in 0..4 {
            if ranks & (1 << rank) == 0 {
                continue;
            }
            for lane in 0..8 {
                self.set_rx_vref(
                    RankLane {
                        channel: ch,
                        rank,
                        lane,
                    },
                    vref,
                )?;
            }
        }
        Ok(())
    }

    pub(crate) fn all_mpr_dqs(
        &self,
        state: &mut CalibrationState,
        ranks: [u8; 2],
        trained: bool,
    ) -> Result<(), Error> {
        for (ch, mask) in ranks.into_iter().enumerate() {
            for rank in 0..4 {
                if mask & (1 << rank) == 0 {
                    continue;
                }
                for lane in 0..8 {
                    let dqs = if trained {
                        state.training.ch[ch].rank[rank].rd_dqs_dly[lane]
                    } else {
                        32
                    };
                    self.set_rx_dqs_delay(
                        state,
                        RankLane {
                            channel: ch,
                            rank,
                            lane,
                        },
                        dqs,
                    )?;
                }
            }
        }
        for rank in 0..4 {
            for (ch, mask) in ranks.into_iter().enumerate() {
                if mask & (1 << rank) != 0 {
                    self.download(DownloadTarget::Channel(ch), rank, RegisterFile::Read)?;
                }
            }
        }
        Ok(())
    }
}

/// Largest contiguous window with source tie-breaking (first equal-width eye).
/// A single passing point has width zero and cannot certify a usable eye.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PassingWindow {
    current: Option<(i16, i16)>,
    pub best: Option<(i16, i16)>,
}
impl PassingWindow {
    pub const EMPTY: Self = Self {
        current: None,
        best: None,
    };
    pub fn sample(&mut self, value: i16, step: i16, pass: bool) {
        if !pass {
            return;
        }
        let start = match self.current {
            Some((left, right)) if right == value - step => left,
            _ => value,
        };
        self.current = Some((start, value));
        if self
            .best
            .is_none_or(|(left, right)| value - start > right - left)
        {
            self.best = self.current;
        }
    }
    pub fn width(self) -> i16 {
        self.best.map_or(0, |(l, r)| r - l)
    }
    pub fn center(self) -> Option<i16> {
        self.best.map(|(l, r)| l + (r - l) / 2)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn training_largest_contiguous_window_keeps_first_tie_not_disjoint_span() {
        let mut w = PassingWindow::EMPTY;
        for v in 1..=20 {
            w.sample(v, 1, (2..=6).contains(&v) || (11..=15).contains(&v));
        }
        assert_eq!(w.best, Some((2, 6)));
        assert_eq!(w.width(), 4);
        assert_eq!(w.center(), Some(4));
        let mut single = PassingWindow::EMPTY;
        single.sample(8, 8, true);
        assert_eq!(single.width(), 0);
    }
}
