// SPDX-License-Identifier: MIT OR Apache-2.0
//! DDR4 BER margining from coreboot_KabyLakeNative native/ber_margin.c.
//! Real REUT counts, in source integer units; not a training certificate.
//! Caller configures REUT/rank maps and initializes working margin limits once
//! at the training boundary (power_model), not before each measurement.

use super::{
    Error, TrainingStep,
    command_training::{CommandContext, pi, replace_pi},
    jedec::vref_code,
    phy_registers::{DataOffsetTrain, RxTrain},
    registers::ReutSequence,
    reut::{DownloadTarget, Engine, RegisterFile, rank_mask},
    reut_registers::{ErrorCounterControl, TestAddress},
    training::cold_ranks,
    training_state::CalibrationState,
    vref_training::{park_rtt_wr, vref_dq_to_offset},
};
use embedded_hal::delay::DelayNs;
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};

pub const BER_LOG_TARGET: u32 = 7;
pub const BER_ERROR_MASK: u32 = 0xffff_00ff;
pub const MAX_POSSIBLE_TIME: u8 = 31;
pub const MAX_POSSIBLE_VREF: u8 = 63;
pub const MAX_POSSIBLE_DDR4_WRITE_VREF: u8 = 36;
/// Source order: above target, between, below target, nonmonotonic.
pub type BerStats = [u32; 4];

/// Only parameters with a real DDR4 ber_change_margin implementation in the
/// supplied source. RCVENA/FAN2/FAN3 have no dispatch there; do not fake them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum MarginParameter {
    ReadTiming = 1,
    WriteTiming = 2,
    WriteDqsTiming = 3,
    ReadVoltage = 4,
    WriteVoltage = 5,
    WriteLevel = 6,
    ReceiveEnableExtended = 11,
    CommandTiming = 12,
    CommandVoltage = 13,
}
impl MarginParameter {
    pub const fn result_type(self) -> usize {
        match self {
            Self::ReadVoltage => 0,
            Self::ReadTiming => 1,
            Self::WriteVoltage => 2,
            Self::WriteTiming | Self::WriteDqsTiming => 3,
            Self::ReceiveEnableExtended => 4,
            Self::WriteLevel => 5,
            Self::CommandTiming => 6,
            Self::CommandVoltage => 7,
        }
    }
    fn offset_register(self) -> bool {
        matches!(
            self,
            Self::ReadTiming
                | Self::WriteTiming
                | Self::WriteDqsTiming
                | Self::ReadVoltage
                | Self::WriteLevel
        )
    }
    fn write_rf(self) -> bool {
        matches!(
            self,
            Self::WriteTiming | Self::WriteDqsTiming | Self::WriteLevel
        )
    }
}

#[derive(Debug, Clone, Copy)]
pub struct BerSweep {
    pub parameter: MarginParameter,
    pub channels: u8,
    /// Physical rank, 0xff for all; for WriteVoltage this is a physical rank
    /// mask. A mask is never passed directly as a physical rank to MR6.
    pub rank_in: u8,
    /// Physical RX rank (0xff = all); for WriteVoltage a physical rank mask.
    pub rank_rx: u8,
    /// Source byte-entry mode bit 0 (ignored by the channel entry). DIMM
    /// write voltage always walks a common channel point; setting this bit
    /// in the byte entry applies that point separately via PDA.
    pub per_channel: bool,
    pub interpolate: bool,
    pub max_margin: u8,
    pub interior: bool,
    /// Physical-byte counter selectors, not a board DQ/DQS wire map.
    /// Nonidentity selectors require common channel points; independent-byte
    /// sweeps reject them rather than tune a different lane from the feedback.
    pub byte_map: Option<[u8; 9]>,
}

/// native:ber_log8; logarithm quantized to source tenths of log base eight.
pub fn ber_log8(mut value: u32) -> u32 {
    const LOOK: [u8; 18] = [0, 0, 1, 2, 3, 4, 5, 6, 7, 7, 8, 8, 9, 9, 9, 10, 10, 10];
    let mut log = 0;
    let mut remainder = 2 * u64::from(value);
    while value > 8 {
        remainder = u64::from(value >> 2);
        value >>= 3;
        log += 10;
    }
    log + u32::from(LOOK[remainder as usize])
}
/// Source int8_t shift: positive left, negative right, out-of-range zero.
pub fn ber_bit_shift(value: u32, amount: i8) -> u32 {
    if !(-31..=31).contains(&amount) {
        0
    } else if amount > 0 {
        value << amount
    } else {
        value >> -amount
    }
}
/// Source IA-32 start count modulo 32, with unsigned wrapping at bit 31.
pub fn ber_bit_swap(current: u32, new: u32, start: u8, len: u8) -> u32 {
    let mask = if len >= 32 {
        u32::MAX
    } else {
        (1u32 << len) - 1
    };
    let start = start & 31;
    (current & !(mask << start)) | ((new & mask) << start)
}
/// native:ber_interpolate_vref, returns tenths. Inputs are log8 counts, not
/// failing-DQ popcounts. Saturation to a physical sweep limit is caller-owned.
pub fn ber_interpolate_vref(pass: u32, e1: u32, e2: u32, target: u32, stats: &mut BerStats) -> u32 {
    let index = if e2 <= e1 {
        3
    } else if e2 < target {
        2
    } else if e1 <= target {
        1
    } else {
        0
    };
    stats[index] = stats[index].saturating_add(1);
    if e2 <= e1 {
        return pass * 10 + 10;
    }
    let diff = e2 - e1;
    if e2 < target {
        pass * 10 + 20 + 10.min(10 * (target - e2) / diff)
    } else if e1 <= target {
        pass * 10 + 10 + 10 * (target - e1) / diff
    } else {
        pass * 10 + 10 * target / diff.max(e1)
    }
}

/// native:meminit_calc_max_rx_margin, host centers independent of hardware.
fn max_rx_margin_rank(
    state: &CalibrationState,
    param: MarginParameter,
    ch: usize,
    rank_rx: u8,
    byte: usize,
    sign: usize,
    max_margin: u8,
) -> Result<u8, Error> {
    if ch >= 2 || byte >= 9 || sign > 1 || (rank_rx >= 4 && rank_rx != 0xff) {
        return Err(Error::InvalidRank);
    }
    // Rank presence is supplied by Engine::calc_max_rx_margin below. This
    // arithmetic helper handles a single physical rank, or channel-local Vref.
    if rank_rx == 0xff && param != MarginParameter::ReadVoltage {
        return Err(Error::InvalidRank);
    }
    let r = usize::from(rank_rx.min(3));
    let (values, top, scale) = match param {
        MarginParameter::ReadTiming => (
            [
                i16::from(state.training.ch[ch].rx_dqs_p[r][byte]),
                i16::from(state.training.ch[ch].rx_dqs_n[r][byte]),
            ],
            63,
            1,
        ),
        MarginParameter::ReadVoltage => {
            let v = i16::from(state.training.ch[ch].rd_vref[byte])
                + i16::from(state.training.ch[ch].rx_vref_rank_offset[r][byte]);
            if !(-32..=31).contains(&v) {
                return Err(Error::InvalidTiming);
            }
            ([v + 32; 2], 63, 2)
        }
        MarginParameter::ReceiveEnableExtended => (
            [state.training.ch[ch].rank[r].rcven_dly[byte] as i16; 2],
            511,
            -4,
        ),
        _ => return Ok(max_margin),
    };
    let mut limit = i16::from(max_margin);
    if scale == 2 {
        limit /= 2;
    }
    if scale == -4 {
        limit *= 4;
    }
    for value in values {
        if !(0..=top).contains(&value) {
            return Err(Error::InvalidTiming);
        }
        limit = limit.min(if sign == 0 { value } else { top - value });
    }
    if scale == 2 {
        limit *= 2;
    }
    if scale == -4 {
        limit /= 4;
    }
    Ok(limit as u8)
}

pub fn ber_vref_saturation(current: i16, sign: usize, max_margin: u8) -> Result<u8, Error> {
    if sign > 1 || !(-63..=63).contains(&current) {
        return Err(Error::InvalidTiming);
    }
    Ok(i16::from(max_margin).min(if sign == 0 {
        63 + current
    } else {
        63 - current
    }) as u8)
}

/// Hardware-only snapshot, never substitutes live registers for host mirrors.
/// At most 640 bytes on x86-64, plus walkers/results (<1 KiB) during a scan; caller
/// owns CalibrationState in CAR. No full calibration clone or heap allocation.
/// Captured once, then reused for every relative CMDT/CA-Vref point.
pub struct BerBaseline {
    ranks: [u8; 2],
    lanes: usize,
    offsets: [[u32; 9]; 2],
    controls: [[u32; 9]; 2],
    channel_controls: [u32; 2],
    rx: [[[u32; 9]; 4]; 2],
    clock: [u32; 2],
    ca_word: u32,
    ca: [i16; 2],
    cke: [u32; 2],
    odt: [u32; 2],
    init_state: u32,
    cadb: [u32; 2],
}

impl Engine<'_> {
    pub fn ber_baseline(&self, ctx: &CommandContext<'_>) -> Result<BerBaseline, Error> {
        let ranks = cold_ranks(self, ctx.mode)?;
        if !(8..=9).contains(&ctx.policy.sdram_lanes) || !matches!(ctx.policy.ca_vref_config, 0 | 2)
        {
            return Err(Error::InvalidPolicy);
        }
        Ok(BerBaseline {
            ranks,
            lanes: ctx.policy.sdram_lanes,
            offsets: core::array::from_fn(|ch| {
                core::array::from_fn(|b| self.registers.data_lane[b][ch].offset_train.get())
            }),
            controls: core::array::from_fn(|ch| {
                core::array::from_fn(|b| self.registers.data_lane[b][ch].control0.get())
            }),
            channel_controls: core::array::from_fn(|ch| {
                self.registers.data_channel[ch].control0.get()
            }),
            rx: core::array::from_fn(|ch| {
                core::array::from_fn(|r| {
                    core::array::from_fn(|b| self.registers.data_lane[b][ch].rx_train[r].get())
                })
            }),
            clock: core::array::from_fn(|ch| self.registers.clock_channel[ch].rank_pi.get()),
            ca_word: self.registers.data_lane[0][0].vref_adjust.get(),
            ca: self.ca_vref_base(),
            cke: core::array::from_fn(|ch| self.registers.channel[ch].cke.get()),
            odt: core::array::from_fn(|ch| self.registers.channel[ch].odt.get()),
            init_state: self.registers.init_state.get(),
            cadb: core::array::from_fn(|ch| self.registers.channel[ch].cadb_control.get()),
        })
    }
    /// Resolve logical REUT start/current rank through the actual rank map.
    /// Never use a logical rank as a physical CS or silently fall back to rank0.
    pub fn ber_regfile_rank(&self, ch: usize, current: bool) -> Result<usize, Error> {
        self.channel(ch)?;
        let logical = if current {
            self.registers.reut_address_current[ch].read(TestAddress::RANK)
        } else {
            self.registers.reut_address_start[ch].read(TestAddress::RANK)
        };
        let rank = ((self.registers.reut_rank_map[ch].get() >> (logical * 4)) & 3) as usize;
        if rank_mask(self.dimms, ch)? & (1 << rank) == 0 {
            return Err(Error::InvalidRank);
        }
        Ok(rank)
    }
    fn ber_download(
        &self,
        ch: usize,
        byte: Option<usize>,
        current: bool,
        write: bool,
    ) -> Result<(), Error> {
        self.download(
            match byte {
                Some(lane) => DownloadTarget::Lane { channel: ch, lane },
                None => DownloadTarget::Channel(ch),
            },
            self.ber_regfile_rank(ch, current)?,
            if write {
                RegisterFile::Write
            } else {
                RegisterFile::Read
            },
        )
    }
    pub fn calc_max_rx_margin(
        &self,
        state: &CalibrationState,
        param: MarginParameter,
        ch: usize,
        rank_rx: u8,
        byte: usize,
        sign: usize,
        mut max_margin: u8,
    ) -> Result<u8, Error> {
        let ranks = rank_mask(self.dimms, ch)?;
        if ranks == 0
            || byte >= 9
            || (rank_rx != 0xff && (rank_rx >= 4 || ranks & (1 << rank_rx) == 0))
        {
            return Err(Error::InvalidRank);
        }
        for rank in 0..4 {
            if ranks & (1 << rank) != 0 && (rank_rx == 0xff || rank_rx == rank) {
                max_margin = max_rx_margin_rank(state, param, ch, rank, byte, sign, max_margin)?;
            }
        }
        Ok(max_margin)
    }
    /// native:meminit_calc_max_vref_margin. MR6 uses saved rank/device host
    /// centers; CA uses the initial snapshot, not the previous transient point.
    pub fn calc_max_vref_margin(
        &self,
        state: &CalibrationState,
        base: &BerBaseline,
        ch: usize,
        ranks: u8,
        byte: usize,
        param: MarginParameter,
        sign: usize,
        max_margin: u8,
        pda: bool,
        ctx: &CommandContext<'_>,
    ) -> Result<u8, Error> {
        self.channel(ch)?;
        if byte >= base.lanes || sign > 1 {
            return Err(Error::InvalidRank);
        }
        if param == MarginParameter::CommandVoltage {
            return ber_vref_saturation(
                base.ca[if ctx.policy.ca_vref_config == 0 {
                    0
                } else {
                    ch
                }],
                sign,
                max_margin,
            );
        }
        if param != MarginParameter::WriteVoltage {
            return Err(Error::InvalidPolicy);
        }
        let selected = ranks & base.ranks[ch];
        if ranks == 0 || ranks & !15 != 0 || selected == 0 {
            return Err(Error::InvalidRank);
        }
        let mut limit = i16::from(max_margin);
        for rank in 0..4 {
            if selected & (1 << rank) == 0 {
                continue;
            }
            let code = if pda {
                state.ddr4_pda_mr6[ch][rank][byte]
            } else {
                state.saved_mr.rank[ch][rank][6] as u8
            };
            if pda && code > 127 {
                return Err(Error::InvalidTiming);
            }
            let center = vref_dq_to_offset(code & 127);
            // Source +/-36 range and the actual resolved MR6 encoding range.
            let (low, high) = super::vref_training::vref_bounds(state.ddr4_tx_vref_range[ch])?;
            if !(low..=high).contains(&center) {
                return Err(Error::InvalidTiming);
            }
            limit = limit.min(if sign == 0 {
                center - low.max(-36)
            } else {
                high.min(36) - center
            });
        }
        Ok(limit.max(0) as u8)
    }
    pub(crate) fn ber_pda_offset(
        &self,
        state: &CalibrationState,
        ch: usize,
        rank: usize,
        mask: u16,
        offset: i16,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        // Validate all codes before changing termination.
        let mut codes = [0u8; 9];
        for (byte, code) in codes.iter_mut().enumerate() {
            if mask & (1 << byte) != 0 {
                if state.ddr4_pda_mr6[ch][rank][byte] > 127 {
                    return Err(Error::InvalidTiming);
                }
                *code = vref_code(
                    i32::from(vref_dq_to_offset(state.ddr4_pda_mr6[ch][rank][byte] & 127))
                        + i32::from(offset),
                    state.ddr4_tx_vref_range[ch],
                )? as u8;
            }
        }
        let result = (|| {
            self.training_mrs(ch, rank, 5, park_rtt_wr(state, ch, rank), delay)?;
            for (byte, &code) in codes.iter().enumerate() {
                if mask & (1 << byte) == 0 {
                    continue;
                }
                let mut device = Ok(());
                for _ in 0..2 {
                    if device.is_ok() {
                        device = self.mrs_pda_mr6(state, ch, rank, 1 << byte, code, true, delay);
                        delay.delay_us(1);
                    }
                }
                let disable = self.mrs_pda_mr6(state, ch, rank, 1 << byte, code, false, delay);
                delay.delay_us(1);
                device.and(disable)?;
            }
            Ok(())
        })();
        let restore = self.training_mrs(ch, rank, 5, state.saved_mr.rank[ch][rank][5], delay);
        result.and(restore)
    }
    /// native:ber_change_margin. Signed offsets, not absolute pad delays.
    /// Zero byte_mask selects DIMM-wide MR6 for WRV. Other byte masks select
    /// physical lanes. Multicast uses current REUT rank; unicast start rank.
    /// Raw primitive: caller must restore the baseline even after an error;
    /// the two public measurement entry points do this on every exit.
    pub fn ber_change_margin(
        &self,
        state: &CalibrationState,
        base: &BerBaseline,
        ctx: &CommandContext<'_>,
        param: MarginParameter,
        value: i16,
        channel: usize,
        rank: usize,
        byte_mask: u16,
        multicast: bool,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        if channel >= 2
            || rank >= 4
            || byte_mask & !((1 << base.lanes) - 1) != 0
            || !(-127..=127).contains(&value)
        {
            return Err(Error::InvalidRank);
        }
        if !multicast && (base.ranks[channel] & (1 << rank) == 0) {
            return Err(Error::InvalidRank);
        }
        if param.offset_register() {
            let signed6 = (i32::from(value.clamp(-32, 31)) as u32) & 63;
            let fields = match param {
                MarginParameter::ReadTiming => DataOffsetTrain::RX_DQS.val(signed6),
                MarginParameter::WriteTiming => DataOffsetTrain::TX_DQ.val(signed6),
                MarginParameter::WriteDqsTiming => DataOffsetTrain::TX_DQS.val(signed6),
                MarginParameter::WriteLevel => {
                    DataOffsetTrain::TX_DQ.val(signed6) + DataOffsetTrain::TX_DQS.val(signed6)
                }
                MarginParameter::ReadVoltage => {
                    DataOffsetTrain::VREF.val((i32::from(value.clamp(-64, 63)) as u32) & 127)
                }
                _ => unreachable!(),
            };
            if multicast {
                // Validate every physical download before the multicast write.
                for ch in 0..2 {
                    if base.ranks[ch] != 0 {
                        self.ber_regfile_rank(ch, true)?;
                    }
                }
                let orthogonal = match param {
                    MarginParameter::ReadTiming => DataOffsetTrain::VREF::SET.mask(),
                    MarginParameter::ReadVoltage => DataOffsetTrain::RX_DQS::SET.mask(),
                    _ => 0,
                };
                if orthogonal != 0
                    && (0..2).any(|ch| {
                        base.ranks[ch] != 0
                            && base.offsets[ch][..base.lanes]
                                .iter()
                                .any(|word| word & orthogonal != 0)
                    })
                {
                    // Orthogonal read-2D points can differ by byte. Retain
                    // only that entry field plus the selected margin; all
                    // other multicast transients keep native cleared semantics.
                    for ch in 0..2 {
                        if base.ranks[ch] == 0 {
                            continue;
                        }
                        for byte in 0..base.lanes {
                            self.registers.data_lane[byte][ch]
                                .offset_train
                                .set(fields.value | (base.offsets[ch][byte] & orthogonal));
                            self.ber_download(ch, Some(byte), true, false)?;
                        }
                    }
                } else {
                    self.registers.data_multicast.offset_train.set(fields.value);
                    for ch in 0..2 {
                        if base.ranks[ch] != 0 {
                            self.ber_download(ch, None, true, param.write_rf())?;
                        }
                    }
                }
            } else {
                self.ber_regfile_rank(channel, false)?;
                for byte in 0..base.lanes {
                    if byte_mask & (1 << byte) == 0 {
                        continue;
                    }
                    let r = &self.registers.data_lane[byte][channel].offset_train;
                    if param == MarginParameter::ReadTiming {
                        // Late read 2D supplies a baseline voltage point. Keep it
                        // fixed across the timing walk; native's whole-word clear
                        // accidentally measures nominal voltage at every point.
                        r.set(
                            fields.value
                                | (base.offsets[channel][byte] & DataOffsetTrain::VREF::SET.mask()),
                        );
                    } else {
                        r.modify(fields);
                    }
                    self.ber_download(channel, Some(byte), false, param.write_rf())?;
                }
            }
            return Ok(());
        }
        for ch in 0..2 {
            if base.ranks[ch] == 0 || (!multicast && ch != channel) {
                continue;
            }
            match param {
                MarginParameter::ReceiveEnableExtended => {
                    self.ber_regfile_rank(ch, false)?;
                    for byte in 0..base.lanes {
                        if !multicast && byte_mask & (1 << byte) == 0 {
                            continue;
                        }
                        for r in 0..4 {
                            if base.ranks[ch] & (1 << r) == 0 {
                                continue;
                            }
                            let center = i32::from(state.training.ch[ch].rank[r].rcven_dly[byte]);
                            let v = (center + i32::from(value.clamp(-64, 63)) * 4).clamp(0, 511);
                            self.registers.data_lane[byte][ch].rx_train[r]
                                .modify(RxTrain::RCVEN.val(v as u32));
                            self.ber_download(ch, Some(byte), false, false)?;
                        }
                    }
                }
                MarginParameter::WriteVoltage => {
                    for r in 0..4 {
                        if base.ranks[ch] & (1 << r) == 0 || (!multicast && r != rank) {
                            continue;
                        }
                        if byte_mask == 0 {
                            let center =
                                vref_dq_to_offset(state.saved_mr.rank[ch][r][6] as u8 & 127);
                            self.write_vref_offset(state, ch, r, center + value, delay)?;
                        } else {
                            self.ber_pda_offset(state, ch, r, byte_mask, value, delay)?;
                        }
                    }
                }
                MarginParameter::CommandTiming => {
                    let mut word = self.registers.clock_channel[ch].rank_pi.get();
                    for r in 0..4 {
                        if base.ranks[ch] & (1 << r) != 0 && (multicast || r == rank) {
                            word =
                                replace_pi(word, r, (pi(base.clock[ch], r) + value).clamp(0, 127));
                        }
                    }
                    self.registers.clock_channel[ch].rank_pi.set(word);
                }
                MarginParameter::CommandVoltage => {
                    let selector = if ctx.policy.ca_vref_config == 0 {
                        0
                    } else {
                        ch
                    };
                    self.ca_vref_update_for_training(selector, base.ca[selector], value, delay)?;
                }
                _ => unreachable!(),
            }
        }
        Ok(())
    }
}

// The same source two-pass/two-fail packed-count walk drives byte/channel
// searches. Bounds live outside the byte state because per-channel byte mode
// shares min/max while retaining separate measured errors for every byte.
#[derive(Clone, Copy)]
struct Walk {
    last_pass: u8,
    errors: u32,
    pass: bool,
    fail: bool,
    center_failed: bool,
}
impl Walk {
    const NEW: Self = Self {
        last_pass: 127,
        errors: 0,
        pass: false,
        fail: false,
        center_failed: false,
    };
    fn done(self) -> bool {
        self.pass && self.fail
    }
    fn observe(&mut self, mv: u8, ec: u32, min: u8, max: u8, limit: u8, channel_walk: bool) {
        let delta = i16::from(mv) - i16::from(self.last_pass);
        if ec == 0 && mv == max {
            if delta < 0 {
                self.last_pass = mv;
            } else if delta == 1 {
                self.errors = ber_bit_shift(self.errors, -8) & BER_ERROR_MASK;
                self.last_pass = mv;
                self.pass = true;
            } else if delta == 2 {
                self.errors = ber_bit_swap(self.errors, ber_log8(ec), 24, 8);
                self.fail = true;
            }
        } else if ec == 0 && mv == min {
            if self.pass {
                return;
            }
            if delta == -1 {
                self.errors = ber_bit_swap(self.errors, 0, 0, 8);
                self.pass = true;
            } else {
                self.last_pass = mv;
                self.pass = false;
            }
        } else if ec > 0 && mv == max {
            if delta < 2 {
                self.errors = ber_bit_swap(self.errors, ber_log8(ec), 16, 8);
                self.fail = false;
            } else if delta == 2 {
                self.errors = ber_bit_swap(self.errors, ber_log8(ec), 24, 8);
                self.fail = true;
            }
        } else if ec > 0 && mv == min {
            if self.last_pass < 255 && delta <= 0 {
                self.errors = ber_bit_swap(self.errors, ber_log8(ec), (8 * (delta + 1)) as u8, 8);
                self.errors = ber_bit_shift(self.errors, (8 * (1 - delta)) as i8);
                self.last_pass = mv.wrapping_sub(1);
            } else {
                let tmp = if channel_walk {
                    (self.errors & 0xff0000) >> 8
                } else {
                    (self.errors & 0xff0000) << 8
                };
                self.errors = ber_bit_swap(self.errors, tmp.wrapping_add(ber_log8(ec)), 16, 16);
            }
            if min < max {
                self.fail = true;
            }
            if delta <= 0 {
                self.pass = false;
            }
        }
        if mv == limit {
            self.fail = true;
        }
        if ec == 0 && limit == self.last_pass && self.pass {
            self.errors = ber_bit_swap(self.errors, 0xfffe, 16, 16);
        }
        if mv == 0 {
            self.pass = true;
            if ec > 0 {
                self.fail = true;
                self.center_failed = true;
                self.last_pass = 0;
                self.errors =
                    ber_bit_swap(self.errors, (BER_LOG_TARGET << 8) + BER_LOG_TARGET, 16, 16);
            }
        }
    }
    fn margin(self, interpolate: bool, limit: u8, stats: &mut BerStats) -> u32 {
        let measured = if interpolate {
            ber_interpolate_vref(
                u32::from(self.last_pass),
                (self.errors >> 16) & 255,
                self.errors >> 24,
                BER_LOG_TARGET,
                stats,
            )
        } else {
            10 * u32::from(self.last_pass)
        };
        // Interpolation is not permission to exceed a physical saturation edge
        // or an interior-only measurement boundary (including the zero edge).
        // The source forces last_pass=0 even when center itself fails; its
        // nonmonotonic fallback then invents a one-code eye. Preserve measured
        // zero instead (while still collecting the interpolation statistic).
        if self.center_failed {
            0
        } else {
            measured.min(10 * u32::from(limit))
        }
    }
}

impl Engine<'_> {
    fn ber_selected_ranks(
        &self,
        cfg: &BerSweep,
        base: &BerBaseline,
        ch: usize,
    ) -> Result<u8, Error> {
        let mask = if cfg.parameter == MarginParameter::WriteVoltage {
            if cfg.rank_in == 0 || cfg.rank_in & !15 != 0 {
                return Err(Error::InvalidRank);
            }
            cfg.rank_in
        } else if cfg.rank_in == 0xff {
            base.ranks[ch]
        } else if cfg.rank_in < 4 {
            1 << cfg.rank_in
        } else {
            return Err(Error::InvalidRank);
        };
        let selected = mask & base.ranks[ch];
        if selected == 0 {
            Err(Error::InvalidRank)
        } else {
            Ok(selected)
        }
    }
    fn ber_limit(
        &self,
        state: &CalibrationState,
        ctx: &CommandContext<'_>,
        base: &BerBaseline,
        cfg: &BerSweep,
        ch: usize,
        byte: usize,
        sign: usize,
    ) -> Result<u8, Error> {
        let param = cfg.parameter;
        let max = cfg.max_margin.min(match param {
            MarginParameter::ReadTiming
            | MarginParameter::WriteTiming
            | MarginParameter::WriteDqsTiming
            | MarginParameter::WriteLevel => MAX_POSSIBLE_TIME,
            MarginParameter::ReadVoltage
            | MarginParameter::ReceiveEnableExtended
            | MarginParameter::CommandVoltage => MAX_POSSIBLE_VREF,
            MarginParameter::WriteVoltage => MAX_POSSIBLE_DDR4_WRITE_VREF,
            MarginParameter::CommandTiming => 127,
        });
        match param {
            MarginParameter::ReadTiming
            | MarginParameter::ReadVoltage
            | MarginParameter::ReceiveEnableExtended => {
                self.calc_max_rx_margin(state, param, ch, cfg.rank_rx, byte, sign, max)
            }
            MarginParameter::WriteVoltage | MarginParameter::CommandVoltage => self
                .calc_max_vref_margin(
                    state,
                    base,
                    ch,
                    cfg.rank_rx,
                    byte,
                    param,
                    sign,
                    max,
                    cfg.per_channel && param == MarginParameter::WriteVoltage,
                    ctx,
                ),
            MarginParameter::CommandTiming => {
                let selected = self.ber_selected_ranks(cfg, base, ch)?;
                let mut limit = i16::from(max);
                for r in 0..4 {
                    if selected & (1 << r) != 0 {
                        let center = pi(base.clock[ch], r);
                        limit = limit.min(if sign == 0 { center } else { 127 - center });
                    }
                }
                Ok(limit as u8)
            }
            _ => Ok(max),
        }
    }
    fn ber_apply_point(
        &self,
        state: &CalibrationState,
        ctx: &CommandContext<'_>,
        base: &BerBaseline,
        cfg: &BerSweep,
        points: &[[u8; 9]; 2],
        sign: usize,
        channel_walk: bool,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        for ch in 0..2 {
            if cfg.channels & (1 << ch) == 0 {
                continue;
            }
            let selected = self.ber_selected_ranks(cfg, base, ch)?;
            let ranks = if cfg.parameter == MarginParameter::WriteVoltage
                || cfg.parameter == MarginParameter::CommandTiming
            {
                selected
            } else {
                1 << selected.trailing_zeros()
            };
            for rank in 0..4 {
                if ranks & (1 << rank) == 0 {
                    continue;
                }
                for byte in 0..base.lanes {
                    let point = if channel_walk {
                        points[ch][0]
                    } else {
                        points[ch][byte]
                    };
                    let value = i16::from(point) * if sign == 0 { -1 } else { 1 };
                    let mask = if cfg.parameter == MarginParameter::WriteVoltage
                        && (channel_walk || !cfg.per_channel)
                    {
                        0
                    } else {
                        1 << byte
                    };
                    self.ber_change_margin(
                        state,
                        base,
                        ctx,
                        cfg.parameter,
                        value,
                        ch,
                        rank,
                        mask,
                        false,
                        delay,
                    )?;
                    if mask == 0
                        || matches!(
                            cfg.parameter,
                            MarginParameter::CommandTiming | MarginParameter::CommandVoltage
                        )
                    {
                        break;
                    }
                }
            }
        }
        if cfg.parameter == MarginParameter::CommandVoltage {
            // A CA-Vref point requires the real saved-MR/PDA cold reset, not a
            // successful register write masquerading as usable test feedback.
            self.reinitialize_cold(state, ctx.pmc, ctx.mode, base.lanes, delay)?;
        }
        Ok(())
    }
    /// Restore the same baseline/configuration used by raw ber_change_margin.
    /// A raw multicast affects every populated channel; include all of those
    /// channels in cfg. Broadcast aliases are not independent saved storage:
    /// restore real per-lane words after any broadcast control write.
    pub fn restore_ber_margin(
        &self,
        state: &CalibrationState,
        ctx: &CommandContext<'_>,
        base: &BerBaseline,
        cfg: &BerSweep,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        self.stop(delay);
        // A failed IO reset may leave its request asserted. Restore the entry
        // mode rather than returning with a live transient reset request.
        self.registers.init_state.set(base.init_state);
        let mut result = Ok(());
        if cfg.parameter == MarginParameter::WriteVoltage {
            for ch in 0..2 {
                if cfg.channels & (1 << ch) == 0 {
                    continue;
                }
                let ranks = self.ber_selected_ranks(cfg, base, ch)?;
                for rank in 0..4 {
                    if ranks & (1 << rank) == 0 {
                        continue;
                    }
                    let restore =
                        self.mrs_mr6_vref(ch, rank, state.saved_mr.rank[ch][rank][6], delay);
                    if result.is_ok() {
                        result = restore;
                    }
                    if state.ddr4_pda_enable || cfg.per_channel {
                        let restore =
                            self.ber_pda_offset(state, ch, rank, (1 << base.lanes) - 1, 0, delay);
                        if result.is_ok() {
                            result = restore;
                        }
                    }
                }
            }
        }
        if cfg.parameter == MarginParameter::CommandVoltage {
            result = result.and(self.restore_ca_vref(ctx, base.ca_word, delay));
            // Source restores CA center without a final reset. Do not publish
            // the last reset's transient training image as a host baseline.
        }
        if cfg.parameter == MarginParameter::CommandVoltage {
            // Saved-MR reset affects all populated channels, not just selected
            // REUT participants. Restore every affected physical CKE/ODT word.
            self.registers.init_state.set(base.init_state);
            for ch in 0..2 {
                if base.ranks[ch] != 0 {
                    self.registers.channel[ch].cke.set(base.cke[ch]);
                    self.registers.channel[ch].odt.set(base.odt[ch]);
                }
            }
        }
        for ch in 0..2 {
            if cfg.channels & (1 << ch) == 0 {
                continue;
            }
            self.registers.channel[ch].cadb_control.set(base.cadb[ch]);
            self.registers.clock_channel[ch].rank_pi.set(base.clock[ch]);
            // Broadcast before unicast: the alias really overwrites each lane.
            self.registers.data_channel[ch]
                .control0
                .set(base.channel_controls[ch]);
            for byte in 0..base.lanes {
                let lane = &self.registers.data_lane[byte][ch];
                lane.offset_train.set(base.offsets[ch][byte]);
                for rank in 0..4 {
                    if base.ranks[ch] & (1 << rank) == 0 {
                        continue;
                    }
                    if cfg.parameter == MarginParameter::ReceiveEnableExtended {
                        lane.rx_train[rank].set(base.rx[ch][rank][byte]);
                    }
                    let restore = self.download(
                        DownloadTarget::Lane {
                            channel: ch,
                            lane: byte,
                        },
                        rank,
                        if cfg.parameter.write_rf() {
                            RegisterFile::Write
                        } else {
                            RegisterFile::Read
                        },
                    );
                    if result.is_ok() {
                        result = restore;
                    }
                }
                lane.control0.set(base.controls[ch][byte]);
            }
            // Broadcast controls/raw multicast offsets also reach an inactive
            // ninth lane. It is not measured, but must not retain a transient.
            for byte in base.lanes..9 {
                self.registers.data_lane[byte][ch]
                    .offset_train
                    .set(base.offsets[ch][byte]);
                self.registers.data_lane[byte][ch]
                    .control0
                    .set(base.controls[ch][byte]);
            }
        }
        result
    }
    /// native:meminit_get_ber_margin_byte. Measures both edges into
    /// state.margin_results, committing only after successful measurement and
    /// cleanup. WRV rank-mask measurements are conservative combined-rank
    /// edges, copied to each selected physical rank before PDA rebasing.
    /// Scratch is bounded and independent of CalibrationState size.
    pub fn get_ber_margin_byte(
        &self,
        state: &mut CalibrationState,
        ctx: &CommandContext<'_>,
        cfg: BerSweep,
        stats: &mut BerStats,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        self.ber_sweep(state, ctx, cfg, false, false, stats, delay)
    }
    /// native:meminit_get_ber_margin_ch. Channel counter0 is configured to
    /// count all errors (KIND=0), not byte0 or a popcount of byte status.
    pub fn get_ber_margin_ch(
        &self,
        state: &mut CalibrationState,
        ctx: &CommandContext<'_>,
        cfg: BerSweep,
        stats: &mut BerStats,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        self.ber_sweep(state, ctx, cfg, true, false, stats, delay)
    }
    /// Turnaround traffic retains its native dr/dd rank map, whose first
    /// logical rank need not be the physical rank being margined. In addition
    /// to the normal source download, apply each offset to the explicitly
    /// selected physical RF before traffic. Otherwise a later rank can return
    /// the first rank's eye without ever receiving its timing perturbation.
    pub(crate) fn get_ber_margin_byte_physical(
        &self,
        state: &mut CalibrationState,
        ctx: &CommandContext<'_>,
        cfg: BerSweep,
        stats: &mut BerStats,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        self.ber_sweep(state, ctx, cfg, false, true, stats, delay)
    }
    fn ber_sweep(
        &self,
        state: &mut CalibrationState,
        ctx: &CommandContext<'_>,
        cfg: BerSweep,
        channel_walk: bool,
        physical_download: bool,
        stats: &mut BerStats,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        // Source channel walker ignores mode/byte-map; WRV is DIMM-wide there.
        let cfg = if channel_walk {
            BerSweep {
                per_channel: false,
                byte_map: None,
                ..cfg
            }
        } else {
            cfg
        };
        self.validate_channels(cfg.channels)?;
        let base = self.ber_baseline(ctx)?;
        if cfg.max_margin > 127 {
            return Err(Error::InvalidTiming);
        }
        let per_ch = channel_walk
            || cfg.per_channel
            || matches!(
                cfg.parameter,
                MarginParameter::WriteVoltage
                    | MarginParameter::CommandVoltage
                    | MarginParameter::CommandTiming
            );
        if cfg.byte_map.is_some_and(|m| {
            m[..base.lanes].iter().enumerate().any(|(byte, &physical)| {
                usize::from(physical) >= base.lanes || (!per_ch && usize::from(physical) != byte)
            })
        }) {
            return Err(Error::InvalidReutConfig);
        }
        let mut selected = [0u8; 2];
        let mut result_rank = [0usize; 2];
        let mut measured = [[[0u32; 2]; 9]; 2];
        let mut limits = [[[0u8; 9]; 2]; 2];
        for ch in 0..2 {
            if cfg.channels & (1 << ch) == 0 {
                continue;
            }
            selected[ch] = self.ber_selected_ranks(&cfg, &base, ch)?;
            result_rank[ch] = selected[ch].trailing_zeros() as usize;
            // Validate limits/rank selectors before changing any hardware.
            for sign in 0..2 {
                for byte in 0..base.lanes {
                    limits[sign][ch][byte] =
                        self.ber_limit(state, ctx, &base, &cfg, ch, byte, sign)?;
                }
            }
            self.ber_regfile_rank(ch, false)?;
        }
        let saved_counter = core::array::from_fn::<_, 2, _>(|ch| {
            core::array::from_fn::<_, 9, _>(|b| {
                self.registers.channel[ch].error_counter_control[b].get()
            })
        });
        let saved_seq =
            core::array::from_fn::<_, 2, _>(|ch| self.registers.reut_sequence[ch][0].get());
        let mut local_stats = [0; 4];
        let result = (|| {
            for ch in 0..2 {
                self.registers.reut_sequence[ch][0].modify(
                    ReutSequence::GLOBAL_CONTROL.val(u32::from(cfg.channels & (1 << ch) != 0)),
                );
                if cfg.channels & (1 << ch) == 0 {
                    continue;
                }
                if channel_walk {
                    self.registers.channel[ch].error_counter_control[0]
                        .write(ErrorCounterControl::KIND::Channel);
                } else {
                    for byte in 0..base.lanes {
                        self.setup_byte_error_counter(
                            ch,
                            byte,
                            cfg.byte_map.map_or(byte as u8, |m| m[byte]),
                        )?;
                    }
                }
            }
            let shared_ca = cfg.parameter == MarginParameter::CommandVoltage
                && ctx.policy.ca_vref_config == 0
                && cfg.channels == 3;
            for sign in 0..2 {
                let mut walk = [[Walk::NEW; 9]; 2];
                let mut points = [[0u8; 9]; 2];
                let mut min = [[0u8; 9]; 2];
                let mut max = [[0u8; 9]; 2];
                let mut bound = limits[sign];
                let count = if channel_walk { 1 } else { base.lanes };
                for ch in 0..2 {
                    if cfg.channels & (1 << ch) == 0 {
                        continue;
                    }
                    for byte in 0..count {
                        let source = (state.margin_results[ch][result_rank[ch]][byte]
                            [cfg.parameter.result_type() * 2 + sign]
                            / 10)
                            .min(u32::from(bound[ch][byte]))
                            as u8;
                        points[ch][byte] = source;
                    }
                    if per_ch {
                        let limit = *bound[ch][..base.lanes].iter().min().unwrap();
                        let point = *points[ch][..count].iter().min().unwrap();
                        for byte in 0..base.lanes {
                            bound[ch][byte] = limit;
                            points[ch][byte] = point.min(limit);
                        }
                    }
                    for byte in 0..count {
                        if cfg.interior {
                            bound[ch][byte] = bound[ch][byte].min(points[ch][byte]);
                        }
                        min[ch][byte] = points[ch][byte];
                        max[ch][byte] = points[ch][byte];
                    }
                }
                if shared_ca {
                    let point = points[0][0].min(points[1][0]);
                    let limit = bound[0][0].min(bound[1][0]);
                    for ch in 0..2 {
                        for byte in 0..base.lanes {
                            points[ch][byte] = point;
                            min[ch][byte] = point;
                            max[ch][byte] = point;
                            bound[ch][byte] = limit;
                        }
                    }
                }
                let mut complete = false;
                for _ in 0..2 * (usize::from(cfg.max_margin) + 2) {
                    self.ber_apply_point(
                        state,
                        ctx,
                        &base,
                        &cfg,
                        &points,
                        sign,
                        channel_walk,
                        delay,
                    )?;
                    if physical_download && cfg.parameter != MarginParameter::WriteVoltage {
                        for ch in 0..2 {
                            if cfg.channels & (1 << ch) == 0 {
                                continue;
                            }
                            for rank in 0..4 {
                                if selected[ch] & (1 << rank) == 0 {
                                    continue;
                                }
                                for byte in 0..base.lanes {
                                    self.download(
                                        DownloadTarget::Lane {
                                            channel: ch,
                                            lane: byte,
                                        },
                                        rank,
                                        if cfg.parameter.write_rf() {
                                            RegisterFile::Write
                                        } else {
                                            RegisterFile::Read
                                        },
                                    )?;
                                }
                            }
                        }
                    }
                    self.run_io_test(state, cfg.channels, delay)?;
                    for ch in 0..2 {
                        if cfg.channels & (1 << ch) == 0 {
                            continue;
                        }
                        for byte in 0..count {
                            if walk[ch][byte].done() {
                                continue;
                            }
                            let index = if per_ch { 0 } else { byte };
                            let ec = self.read_error_counter(ch, byte)?;
                            walk[ch][byte].observe(
                                points[ch][byte],
                                ec,
                                min[ch][index],
                                max[ch][index],
                                bound[ch][byte],
                                channel_walk,
                            );
                            if !per_ch && !walk[ch][byte].done() {
                                if ec == 0 {
                                    points[ch][byte] = if walk[ch][byte].fail {
                                        min[ch][index] = min[ch][index].saturating_sub(1);
                                        min[ch][index]
                                    } else {
                                        max[ch][index] =
                                            max[ch][index].saturating_add(1).min(bound[ch][byte]);
                                        max[ch][index]
                                    };
                                } else {
                                    points[ch][byte] = if walk[ch][byte].pass {
                                        max[ch][index] =
                                            max[ch][index].saturating_add(1).min(bound[ch][byte]);
                                        max[ch][index]
                                    } else {
                                        min[ch][index] = min[ch][index].saturating_sub(1);
                                        min[ch][index]
                                    };
                                }
                            }
                        }
                        if per_ch && !shared_ca && !walk[ch][..count].iter().all(|w| w.done()) {
                            let next = if walk[ch][..count].iter().all(|w| w.pass) {
                                max[ch][0] = max[ch][0].saturating_add(1).min(bound[ch][0]);
                                max[ch][0]
                            } else {
                                min[ch][0] = min[ch][0].saturating_sub(1);
                                min[ch][0]
                            };
                            for byte in 0..base.lanes {
                                points[ch][byte] = next;
                            }
                        }
                    }
                    if shared_ca && !(0..2).all(|ch| walk[ch][..count].iter().all(|w| w.done())) {
                        let next = if (0..2).all(|ch| walk[ch][..count].iter().all(|w| w.pass)) {
                            max[0][0] = max[0][0].saturating_add(1).min(bound[0][0]);
                            max[0][0]
                        } else {
                            min[0][0] = min[0][0].saturating_sub(1);
                            min[0][0]
                        };
                        for ch in 0..2 {
                            for byte in 0..base.lanes {
                                points[ch][byte] = next;
                                min[ch][byte] = min[0][0];
                                max[ch][byte] = max[0][0];
                            }
                        }
                    }
                    complete = (0..2).all(|ch| {
                        cfg.channels & (1 << ch) == 0 || walk[ch][..count].iter().all(|w| w.done())
                    });
                    if complete {
                        break;
                    }
                }
                if !complete {
                    return Err(Error::TrainingFailed {
                        step: TrainingStep::BerMargin,
                        channel: cfg.channels.trailing_zeros() as u8,
                        rank: result_rank[cfg.channels.trailing_zeros() as usize] as u8,
                        lane: 0,
                    });
                }
                for ch in 0..2 {
                    if cfg.channels & (1 << ch) != 0 {
                        for byte in 0..count {
                            measured[ch][byte][sign] = walk[ch][byte].margin(
                                cfg.interpolate,
                                bound[ch][byte],
                                &mut local_stats,
                            );
                        }
                    }
                }
            }
            Ok(())
        })();
        let mut cleanup = self.restore_ber_margin(state, ctx, &base, &cfg, delay);
        if result.is_ok()
            && cleanup.is_ok()
            && cfg.parameter == MarginParameter::WriteVoltage
            && !channel_walk
            && !state.ddr4_pda_enable
            && !cfg.per_channel
        {
            // Source healthy-eye PDA reapplication, preserving all host MR6
            // images rather than recomputing the global baseline mid-sweep.
            for ch in 0..2 {
                if cfg.channels & (1 << ch) == 0 {
                    continue;
                }
                let mask = (0..base.lanes)
                    .filter(|&byte| measured[ch][byte].iter().all(|&edge| edge >= 40))
                    .fold(0u16, |mask, byte| mask | (1 << byte));
                if mask == 0 {
                    continue;
                }
                for rank in 0..4 {
                    if selected[ch] & (1 << rank) != 0 {
                        let restore = self.ber_pda_offset(state, ch, rank, mask, 0, delay);
                        if cleanup.is_ok() {
                            cleanup = restore;
                        }
                    }
                }
            }
        }
        for ch in 0..2 {
            self.registers.reut_sequence[ch][0].set(saved_seq[ch]);
            if cfg.channels & (1 << ch) != 0 {
                for byte in 0..base.lanes {
                    self.registers.channel[ch].error_counter_control[byte]
                        .set(saved_counter[ch][byte]);
                }
            }
        }
        // Operational cleanup failure overrides an analog nonconvergence.
        match (result, cleanup) {
            (Err(Error::TrainingFailed { .. }), Err(e)) | (Ok(()), Err(e)) => return Err(e),
            (Err(e), _) => return Err(e),
            _ => {}
        }
        for ch in 0..2 {
            if cfg.channels & (1 << ch) == 0 {
                continue;
            }
            // WRV moves a rank mask jointly: its feedback is a measured
            // conservative aggregate, not separately isolated rank eyes. Copy
            // that measured aggregate before PDA rebasing, never rebase stale
            // results in the other selected ranks (the source does the latter).
            let outputs = if cfg.parameter == MarginParameter::WriteVoltage && !channel_walk {
                selected[ch]
            } else {
                1 << result_rank[ch]
            };
            for rank in 0..4 {
                if outputs & (1 << rank) == 0 {
                    continue;
                }
                for byte in 0..if channel_walk { 1 } else { base.lanes } {
                    let slot = cfg.parameter.result_type() * 2;
                    state.margin_results[ch][rank][byte][slot..slot + 2]
                        .copy_from_slice(&measured[ch][byte]);
                }
            }
        }
        // Only global-MR6 sweeps need conversion to independent device
        // centers. PDA test points are already relative to those centers;
        // the source's unconditional adjustment would rebase them twice.
        // Use signed saturation, never unsigned wrap.
        if cfg.parameter == MarginParameter::WriteVoltage && !channel_walk && !cfg.per_channel {
            for ch in 0..2 {
                if cfg.channels & (1 << ch) == 0 {
                    continue;
                }
                for rank in 0..4 {
                    if selected[ch] & (1 << rank) == 0 {
                        continue;
                    }
                    let global = vref_dq_to_offset(state.saved_mr.rank[ch][rank][6] as u8 & 127);
                    for byte in 0..base.lanes {
                        let shift = 10
                            * i64::from(
                                vref_dq_to_offset(state.ddr4_pda_mr6[ch][rank][byte] & 127)
                                    - global,
                            );
                        let slot = cfg.parameter.result_type() * 2;
                        let edges = &mut state.margin_results[ch][rank][byte][slot..slot + 2];
                        edges[0] =
                            (i64::from(edges[0]) + shift).clamp(0, i64::from(u32::MAX)) as u32;
                        edges[1] =
                            (i64::from(edges[1]) - shift).clamp(0, i64::from(u32::MAX)) as u32;
                    }
                }
            }
        }
        for i in 0..4 {
            stats[i] = stats[i].saturating_add(local_stats[i]);
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "ber_margin_tests.rs"]
pub(crate) mod tests;
