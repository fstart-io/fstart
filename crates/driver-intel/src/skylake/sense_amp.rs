// SPDX-License-Identifier: MIT OR Apache-2.0
//! DDR4 sense amplifier feedback calibration, directly translated from
//! native/sense_amp.c. Eight DQ bytes are calibrated; this is not ECC training
//! or evidence of a completed cold pipeline.

use super::{
    Error,
    phy::{self, PhyPolicy},
    phy_registers::*,
    reut::{Engine, io_reset},
    timings::Timings,
    training::cold_ranks,
    training_state::CalibrationState,
};
use crate::sunrise_point::pmc::MemoryBootMode;
use embedded_hal::delay::DelayNs;
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};

/// Source warnings retained for later diagnostics. Saturated sense offsets
/// are permitted by the source and do not by themselves establish a valid eye.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SenseAmpFeedback {
    pub vref_flip: [[u8; 8]; 2],
    /// Bit mask of DQ[7:0]/DQS[8] whose offset sweep was saturated.
    pub saturated: [[u16; 8]; 2],
}

#[derive(Clone, Copy)]
struct VrefSearch {
    best: i8,
    first: i8,
    last: i8,
    low: u8,
    high: u8,
}
impl VrefSearch {
    const EMPTY: Self = Self {
        best: -8,
        first: -31,
        last: -31,
        low: 0xff,
        high: 0,
    };
    fn sample(&mut self, vref: i8, plus: u8, minus: u8) {
        self.low &= plus;
        self.high |= minus;
        let score = minus.count_ones() as i8 - plus.count_ones() as i8;
        if score > self.best {
            self.best = score;
            self.first = vref;
            self.last = vref;
        } else if score == self.best {
            self.last = vref;
        }
    }
    fn center(self) -> i8 {
        let v = self.first + (self.last - self.first) / 2;
        (v + if v < 0 { -1 } else { 1 }) / 2
    }
}

fn sample_center(first_zero: u8, last_one: u8) -> u8 {
    if last_one == 0 {
        0
    } else if first_zero == 0 {
        15
    } else {
        (first_zero + last_one) / 2
    }
}

impl Engine<'_> {
    /// sense_amp.c:update_rdodt_target. Retains the saturation/static-leg
    /// retry and final up/down difference. RCOMP exhaustion is fatal.
    fn update_rdodt_target(&self, p: &PhyPolicy, delay: &mut impl DelayNs) -> Result<(), Error> {
        let reference = (i32::from(p.rcomp_resistor[0]) + 1) / 2;
        let target = i32::from(p.rcomp_target[0]);
        if reference == 0 || target == 0 {
            return Err(Error::InvalidPolicy);
        }
        let n = 96 * (reference - target);
        let d = 2 * (reference + target);
        let vref = (if n < 0 {
            -((-n + d / 2) / d)
        } else {
            (n + d / 2) / d
        })
        .clamp(-16, 15);
        let r = self.registers;
        r.comp_control0
            .modify(CompControl0::FIX_ODT_DOWN::CLEAR + CompControl0::DQ_ODT_VREF.val(vref as u32));
        phy::force_rcomp(r, delay)?;
        let mut up = r.data_comp1.read(DataComp1::ODT_UP);
        if !(16..=48).contains(&up) {
            let disable = u32::from(up < 16);
            r.comp_control0
                .modify(CompControl0::DISABLE_STATIC_ODT.val(disable));
            for ch in 0..2 {
                if super::reut::rank_mask(self.dimms, ch)? != 0 {
                    r.data_channel[ch]
                        .control0
                        .modify(DataControl0::DISABLE_STATIC_ODT.val(disable));
                }
            }
            phy::force_rcomp(r, delay)?;
            up = r.data_comp1.read(DataComp1::ODT_UP);
        }
        let down = r.data_comp1.read(DataComp1::ODT_DOWN);
        r.comp_control0.modify(
            CompControl0::ODT_UP_DOWN.val(down.wrapping_sub(up)) + CompControl0::FIX_ODT_DOWN::SET,
        );
        Ok(())
    }

    fn senseamp_test_ctl(&self, ch: usize, rank: usize, enable: bool) {
        let ctl = &self.registers.data_channel[ch].control0;
        if enable {
            ctl.modify(
                DataControl0::SENSE_AMP_TRAIN::SET
                    + DataControl0::READ_RF_READ::SET
                    + DataControl0::FORCE_ODT::SET
                    + DataControl0::READ_RF_WRITE::CLEAR
                    + DataControl0::READ_RF_RANK.val(rank as u32),
            );
        } else {
            ctl.modify(DataControl0::SENSE_AMP_TRAIN::CLEAR + DataControl0::READ_RF_READ::CLEAR);
        }
    }

    /// meminit_sense_amp_offset, DDR4 cold branch. Prerequisites: resolved
    /// electrical policy, initialized PHY/MC, JEDEC MR images and pre-training.
    /// All temporary modes and MR5 are restored even on command/RCOMP failure.
    pub fn sense_amp_offset(
        &self,
        state: &mut CalibrationState,
        mode: MemoryBootMode,
        t: &Timings,
        p: &PhyPolicy,
        delay: &mut impl DelayNs,
    ) -> Result<SenseAmpFeedback, Error> {
        let ranks = cold_ranks(self, mode)?;
        if p.rcomp_resistor[0] == 0 {
            return Err(Error::InvalidPolicy);
        }
        let operational = phy::rx_vref(self.dimms, t, p, Some(&state.saved_mr))?;
        let r = self.registers;
        let mut ctl0 = [0; 2];
        let mut ctl2 = [[0; 8]; 2];
        let mut vdq = [[[0; 4]; 8]; 2];
        let mut first = [0; 2];
        let saved_comp = r.comp_control2.get();
        for ch in 0..2 {
            if ranks[ch] == 0 {
                continue;
            }
            first[ch] = ranks[ch].trailing_zeros() as usize;
            ctl0[ch] = r.data_channel[ch].control0.get()
                & !(DataControl0::READ_RF_READ::SET.value
                    | DataControl0::READ_RF_WRITE::SET.value
                    | DataControl0::READ_RF_RANK.mask << DataControl0::READ_RF_RANK.shift);
            for lane in 0..8 {
                ctl2[ch][lane] = r.data_lane[lane][ch].control2.get();
                for rank in 0..4 {
                    vdq[ch][lane][rank] = r.data_lane[lane][ch].rx_offset_vdq[rank].get();
                }
            }
        }
        let mut result = SenseAmpFeedback {
            vref_flip: [[0; 8]; 2],
            saturated: [[0; 8]; 2],
        };
        let mut mr_attempted = [0u8; 2];
        let mut calibrated = false;
        let run = (|| {
            for ch in 0..2 {
                if ranks[ch] == 0 {
                    continue;
                }
                for rank in 0..4 {
                    if ranks[ch] & (1 << rank) == 0 {
                        continue;
                    }
                    mr_attempted[ch] |= 1 << rank;
                    self.training_mrs(
                        ch,
                        rank,
                        5,
                        state.saved_mr.rank[ch][rank][5] & !0x1c0,
                        delay,
                    )?;
                    for lane in 0..8 {
                        r.data_lane[lane][ch].rx_train[rank].modify(RxTrain::VREF.val(0));
                    }
                }
                for rank in 0..4 {
                    if ranks[ch] & (1 << rank) != 0 {
                        self.pulse_rx(ch, rank, delay)?;
                    }
                }
                for lane in 0..8 {
                    r.data_lane[lane][ch].control2.set(
                        (ctl2[ch][lane]
                            | DataControl2::FORCE_BIAS::SET.value
                            | DataControl2::FORCE_RX::SET.value)
                            & !(DataControl2::VDDQ_ODT::SET.value
                                | DataControl2::VTT_ODT::SET.value
                                | DataControl2::RX_VREF_PMFC::SET.value),
                    );
                }
            }
            r.comp_control2
                .modify(CompControl2::VDDQ_ODT::CLEAR + CompControl2::VTT_ODT::CLEAR);
            self.update_rdodt_target(p, delay)?;
            for ch in 0..2 {
                if ranks[ch] == 0 {
                    continue;
                }
                ctl0[ch] = (ctl0[ch] & !DataControl0::DISABLE_STATIC_ODT::SET.value)
                    | (r.data_channel[ch].control0.get()
                        & DataControl0::DISABLE_STATIC_ODT::SET.value);
            }
            let mut search = [[VrefSearch::EMPTY; 8]; 2];
            for vref in -31i8..=31 {
                r.data_multicast
                    .offset_train
                    .write(DataOffsetTrain::VREF.val((vref as u8 & 0x7f).into()));
                for ch in 0..2 {
                    if ranks[ch] != 0 {
                        self.pulse_rx(ch, first[ch], delay)?;
                    }
                }
                let mut plus = [[0; 8]; 2];
                for (sample, high) in [(15u32, true), (1, false)] {
                    for ch in 0..2 {
                        if ranks[ch] == 0 {
                            continue;
                        }
                        for lane in 0..8 {
                            r.data_lane[lane][ch].rx_offset_vdq[first[ch]].set(sample * 0x11111111);
                        }
                    }
                    delay.delay_us(1);
                    for ch in 0..2 {
                        if ranks[ch] != 0 {
                            self.senseamp_test_ctl(ch, first[ch], true);
                        }
                    }
                    delay.delay_us(1);
                    for ch in 0..2 {
                        if ranks[ch] != 0 {
                            self.senseamp_test_ctl(ch, first[ch], false);
                        }
                    }
                    for ch in 0..2 {
                        if ranks[ch] == 0 {
                            continue;
                        }
                        for lane in 0..8 {
                            let fb = r.data_lane[lane][ch].feedback.get() as u8;
                            if high {
                                plus[ch][lane] = fb;
                            } else {
                                search[ch][lane].sample(vref, plus[ch][lane], fb);
                            }
                        }
                    }
                }
            }
            r.data_multicast.offset_train.set(0);
            for ch in 0..2 {
                if ranks[ch] == 0 {
                    continue;
                }
                self.pulse_rx(ch, first[ch], delay)?;
                for lane in 0..8 {
                    result.vref_flip[ch][lane] = search[ch][lane].high ^ search[ch][lane].low;
                    let vref = search[ch][lane].center();
                    state.training.ch[ch].rd_vref[lane] = vref;
                    for rank in 0..4 {
                        if ranks[ch] & (1 << rank) != 0 {
                            r.data_lane[lane][ch].rx_train[rank]
                                .modify(RxTrain::VREF.val(vref as u32));
                        }
                    }
                }
            }
            let mut first_zero = [[[0u8; 9]; 8]; 2];
            let mut last_one = [[[0u8; 9]; 8]; 2];
            for sample in 1u8..=15 {
                for ch in 0..2 {
                    if ranks[ch] == 0 {
                        continue;
                    }
                    for lane in 0..8 {
                        let l = &r.data_lane[lane][ch];
                        l.rx_offset_vdq[first[ch]].set(u32::from(sample) * 0x11111111);
                        l.control2.set(ctl2[ch][lane]);
                        l.control2.modify(
                            DataControl2::RX_DQS_AMP.val(sample.into())
                                + DataControl2::FORCE_BIAS::SET
                                + DataControl2::FORCE_RX::SET
                                + DataControl2::LEAKER_COMP.val(0)
                                + DataControl2::VDDQ_ODT::CLEAR
                                + DataControl2::VTT_ODT::CLEAR
                                + DataControl2::RX_VREF_PMFC::CLEAR,
                        );
                    }
                }
                for ch in 0..2 {
                    if ranks[ch] != 0 {
                        self.senseamp_test_ctl(ch, first[ch], true);
                    }
                }
                delay.delay_us(1);
                for ch in 0..2 {
                    if ranks[ch] != 0 {
                        self.senseamp_test_ctl(ch, first[ch], false);
                    }
                }
                for ch in 0..2 {
                    if ranks[ch] == 0 {
                        continue;
                    }
                    for lane in 0..8 {
                        let fb = r.data_lane[lane][ch].feedback.get();
                        for bit in 0..9 {
                            if fb & (1 << bit) != 0 {
                                last_one[ch][lane][bit] = sample;
                            } else if first_zero[ch][lane][bit] == 0 {
                                first_zero[ch][lane][bit] = sample;
                            }
                        }
                    }
                }
            }
            for ch in 0..2 {
                if ranks[ch] == 0 {
                    continue;
                }
                for lane in 0..8 {
                    let mut offsets = 0;
                    for bit in 0..9 {
                        let zero = first_zero[ch][lane][bit];
                        let one = last_one[ch][lane][bit];
                        let center = sample_center(zero, one);
                        if zero == 0 || one == 0 {
                            result.saturated[ch][lane] |= 1 << bit;
                        }
                        if bit == 8 {
                            ctl2[ch][lane] = (ctl2[ch][lane]
                                & !(DataControl2::RX_DQS_AMP.mask
                                    << DataControl2::RX_DQS_AMP.shift))
                                | DataControl2::RX_DQS_AMP.val(center.into()).value;
                        } else {
                            offsets |= u32::from(center) << (bit * 4);
                        }
                    }
                    // Apply final DQS amp before downloading DQ offsets.
                    r.data_lane[lane][ch].control2.set(
                        (ctl2[ch][lane]
                            | DataControl2::FORCE_BIAS::SET.value
                            | DataControl2::FORCE_RX::SET.value)
                            & !(DataControl2::VDDQ_ODT::SET.value
                                | DataControl2::VTT_ODT::SET.value
                                | DataControl2::RX_VREF_PMFC::SET.value),
                    );
                    for rank in 0..4 {
                        if ranks[ch] & (1 << rank) != 0 {
                            r.data_lane[lane][ch].rx_offset_vdq[rank].set(offsets);
                        }
                    }
                }
                for rank in 0..4 {
                    if ranks[ch] & (1 << rank) != 0 {
                        self.pulse_rx(ch, rank, delay)?;
                    }
                }
            }
            calibrated = true;
            Ok(())
        })();
        // Never leave CTT/training modes active, including command failures.
        r.data_multicast.offset_train.set(0);
        for ch in 0..2 {
            if ranks[ch] == 0 {
                continue;
            }
            // A failed compensation attempt may already have toggled static legs.
            ctl0[ch] = (ctl0[ch] & !DataControl0::DISABLE_STATIC_ODT::SET.value)
                | (r.data_channel[ch].control0.get() & DataControl0::DISABLE_STATIC_ODT::SET.value);
            r.data_channel[ch].control0.set(ctl0[ch]);
            for lane in 0..8 {
                r.data_lane[lane][ch].control2.set(ctl2[ch][lane]);
                if !calibrated {
                    for rank in 0..4 {
                        if ranks[ch] & (1 << rank) != 0 {
                            r.data_lane[lane][ch].rx_offset_vdq[rank].set(vdq[ch][lane][rank]);
                        }
                    }
                }
            }
        }
        r.comp_control2.set(saved_comp);
        let mut cleanup = self.update_rdodt_target(p, delay);
        for ch in 0..2 {
            for rank in 0..4 {
                if mr_attempted[ch] & (1 << rank) != 0 {
                    let restored =
                        self.training_mrs(ch, rank, 5, state.saved_mr.rank[ch][rank][5], delay);
                    if cleanup.is_ok() {
                        cleanup = restored;
                    }
                }
            }
        }
        phy::program_rx_vref(r, delay, ranks, &mut state.training, operational);
        let reset = io_reset(r, delay, 100_000, 100);
        run.and(cleanup).and(reset)?;
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sense_amp_vref_score_is_channel_local_and_rounds_away_from_zero() {
        let mut negative = VrefSearch::EMPTY;
        let mut positive = VrefSearch::EMPTY;
        for v in -31..=31 {
            negative.sample(
                v,
                if v < -12 { 0xff } else { 0 },
                if v <= -4 { 0xff } else { 0 },
            );
            positive.sample(
                v,
                if v < 8 { 0xff } else { 0 },
                if v <= 16 { 0xff } else { 0 },
            );
        }
        assert_eq!(
            (negative.first, negative.last, negative.center()),
            (-12, -4, -4)
        );
        assert_eq!(
            (positive.first, positive.last, positive.center()),
            (8, 16, 6)
        );
        assert_eq!(sample_center(0, 15), 15);
        assert_eq!(sample_center(1, 0), 0);
        assert_eq!(sample_center(7, 6), 6);
    }
}
