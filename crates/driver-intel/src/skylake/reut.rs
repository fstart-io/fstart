// SPDX-License-Identifier: MIT OR Apache-2.0
//! REUT hardware primitives translated from native/reut.c and reut.h.
//! Uses x86-64 single-u64 MMIO writes: the source's 32-bit MMX workaround is
//! unnecessary here, but splitting CADB_PROG into two dword writes is unsafe
//! because writing it advances the controller's pattern pointer.

use super::{
    Error,
    registers::*,
    spd::{Dimm, Dimms},
};
use super::{phy_registers::*, reut_registers::*, training_state::CalibrationState};
use embedded_hal::delay::DelayNs;
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Mrs,
    ZqCalibration,
}
impl Command {
    fn encoding(self) -> u64 {
        match self {
            Self::Mrs => 8,
            Self::ZqCalibration => 14,
        }
    }
}

pub fn rank_mask(dimms: &Dimms<'_>, channel: usize) -> Result<u8, Error> {
    let channel = dimms.get(channel).ok_or(Error::InvalidRank)?;
    let mut mask = 0;
    for (slot, dimm) in channel.iter().enumerate() {
        if let Some(dimm) = dimm {
            if !(1..=2).contains(&dimm.ranks) {
                return Err(Error::InvalidRank);
            }
            mask |= ((1u8 << dimm.ranks) - 1) << (slot * 2);
        }
    }
    Ok(mask)
}

fn mirror(dimm: &Dimm<'_>, rank: usize, mut bank: u8, mut address: u16) -> (u8, u16) {
    if rank & 1 != 0 && dimm.rank_one_mirrored {
        let ma357 = address & 0xa8;
        let ma468 = address & 0x150;
        address = (address & 0xfe07) | (ma357 << 1) | (ma468 >> 1);
        bank = (bank & 0xc) | ((bank & 2) >> 1) | ((bank & 1) << 1);
        address = (address & 0xd7ff) | ((address & (1 << 13)) >> 2) | ((address & (1 << 11)) << 2);
        if dimm.device_width == 8 {
            bank = (bank & 3) | ((bank & 8) >> 1) | ((bank & 4) << 1);
        }
    }
    (bank, address)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CadbCommand {
    pub patterns: [u64; 4],
    pub count: usize,
    pub mrs: u32,
}
impl CadbCommand {
    pub fn for_ranks(
        dimms: &Dimms<'_>,
        channel: usize,
        selected: u8,
        command: Command,
        bank: u8,
        values: [u16; 2],
        gap: u8,
    ) -> Result<Self, Error> {
        if selected & !0xf != 0 || bank > 15 || gap > 7 {
            return Err(Error::InvalidRank);
        }
        let selected = selected & rank_mask(dimms, channel)?;
        if selected == 0 {
            return Err(Error::InvalidRank);
        }
        let mut result = Self {
            patterns: [0; 4],
            count: 0,
            mrs: 0,
        };
        for rank in 0..4 {
            if selected & (1 << rank) == 0 {
                continue;
            }
            let dimm = dimms[channel][rank / 2]
                .as_ref()
                .ok_or(Error::InvalidRank)?;
            let (bank, address) = mirror(dimm, rank, bank, values[rank / 2]);
            result.patterns[result.count] = (CadbProgram::ADDRESS.val(address.into())
                + CadbProgram::BANK.val(bank.into())
                + CadbProgram::CHIP_SELECT.val(u64::from(!(1u8 << rank) & 0xf))
                + CadbProgram::COMMAND.val(command.encoding())
                + CadbProgram::CKE.val(0xf))
            .value;
            result.count += 1;
        }
        result.mrs = (CadbMrs::GAP.val(u32::from(if gap == 0 { 3 } else { gap }))
            + CadbMrs::GAP_SCALE::SET
            + CadbMrs::END.val((result.count - 1) as u32)
            + CadbMrs::WAIT_SCALE::SET
            + CadbMrs::WAIT.val(1))
        .value;
        Ok(result)
    }

    /// Prerequisites: PHY/MC initialization, clock and JEDEC reset completed
    /// sufficiently to issue commands. Does not make DRAM generally usable.
    /// Restores the original low sequence-config dword on success *and* error;
    /// the high dword is never touched.
    pub fn issue(
        &self,
        registers: &MchRegisters,
        channel: usize,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        self.issue_bounded(registers, channel, delay, 100_000)
    }
    fn issue_bounded(
        &self,
        registers: &MchRegisters,
        channel: usize,
        delay: &mut impl DelayNs,
        polls: u32,
    ) -> Result<(), Error> {
        let r = registers.channel.get(channel).ok_or(Error::InvalidRank)?;
        if self.count == 0 || self.count > 4 {
            return Err(Error::InvalidRank);
        }
        // x86-64 lowers a volatile u64 write to a single MOVQ, not two MOVLs.
        // This hardware operation must not be called from a 32-bit target.
        // The CADB FIFO takes one 64-bit write per entry, which only long
        // mode issues as a single access.
        if !cfg!(target_arch = "x86_64") {
            return Err(Error::InvalidRank);
        }
        r.cadb_write_pointer
            .write(CadbWritePointer::WRITE_ENABLE::SET);
        for pattern in &self.patterns[..self.count] {
            r.cadb_program.set(*pattern);
        }
        r.cadb_mrs.set(self.mrs);
        let cfg = &registers.reut_sequence[channel][0];
        let ctl = &registers.reut_control[channel];
        let saved = cfg.get();
        let testing = (saved & !((1 << 11) | (3 << 12))) | ReutSequence::INIT_MODE::Testing.value;
        cfg.set(testing);
        ctl.write(ReutControl::CLEAR_ERRORS::SET);
        cfg.set((testing & !(3 << 12)) | ReutSequence::INIT_MODE::Mrs.value);
        r.cadb_loop_count.set(1);
        ctl.write(ReutControl::CLEAR_ERRORS::SET + ReutControl::START::SET);
        let mut result = Err(Error::ReutTimeout);
        for _ in 0..polls {
            let status = registers.reut_global_error.extract();
            if status.read(ReutGlobalError::ERRORS) & (1 << channel) != 0 {
                result = Err(Error::ReutError);
                break;
            }
            if status.read(ReutGlobalError::DONE) & (1 << channel) != 0 {
                result = Ok(());
                break;
            }
            delay.delay_us(1);
        }
        cfg.set(saved);
        ctl.write(ReutControl::CLEAR_ERRORS::SET);
        result
    }
}

/// Hardware engine with explicit topology, never a global bound context.
/// Channel, rank and byte indices are physical; address rank fields are logical.
pub struct Engine<'a> {
    pub registers: &'a MchRegisters,
    pub dimms: &'a Dimms<'a>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RankLane {
    pub channel: usize,
    pub rank: usize,
    pub lane: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DownloadTarget {
    Channel(usize),
    Lane { channel: usize, lane: usize },
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegisterFile {
    Read,
    Write,
    Both,
}

/// Unlike a failed completion poll, analog mismatches are sweep feedback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TestResult {
    pub error_channels: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ErrorSnapshot {
    pub failing_dq: u64,
    pub failing_bits_per_byte: [u8; 8],
    pub context: u64,
    /// BER magnitudes, NOT popcount of failing DQ status bits.
    pub counters: [u32; 8],
}

impl Engine<'_> {
    pub(crate) fn channel(&self, ch: usize) -> Result<&ChannelRegisters, Error> {
        if rank_mask(self.dimms, ch)? == 0 {
            return Err(Error::InvalidRank);
        }
        Ok(&self.registers.channel[ch])
    }
    pub(crate) fn validate_channels(&self, mask: u8) -> Result<(), Error> {
        if mask == 0 || mask & !3 != 0 {
            return Err(Error::InvalidReutConfig);
        }
        for ch in 0..2 {
            if mask & (1 << ch) != 0 {
                self.channel(ch)?;
            }
        }
        Ok(())
    }
    fn lane(&self, p: RankLane) -> Result<&DataLaneRegisters, Error> {
        if p.rank >= 4 || p.lane >= 9 || rank_mask(self.dimms, p.channel)? & (1 << p.rank) == 0 {
            return Err(Error::InvalidRank);
        }
        Ok(&self.registers.data_lane[p.lane][p.channel])
    }
    /// Persistent READ/WRITE/RANK replacement, not the Vref pulse-and-clear.
    pub fn download(
        &self,
        target: DownloadTarget,
        rank: usize,
        rf: RegisterFile,
    ) -> Result<(), Error> {
        let (ch, lane) = match target {
            DownloadTarget::Channel(ch) => (ch, None),
            DownloadTarget::Lane { channel, lane } => (channel, Some(lane)),
        };
        self.lane(RankLane {
            channel: ch,
            rank,
            lane: lane.unwrap_or(0),
        })?;
        let ctl = match lane {
            Some(lane) => &self.registers.data_lane[lane][ch].control0,
            None => &self.registers.data_channel[ch].control0,
        };
        ctl.modify(
            DataControl0::READ_RF_READ.val(u32::from(rf != RegisterFile::Write))
                + DataControl0::READ_RF_WRITE.val(u32::from(rf != RegisterFile::Read))
                + DataControl0::READ_RF_RANK.val(rank as u32),
        );
        Ok(())
    }
    fn download_lane(&self, p: RankLane, rf: RegisterFile) -> Result<(), Error> {
        self.download(
            DownloadTarget::Lane {
                channel: p.channel,
                lane: p.lane,
            },
            p.rank,
            rf,
        )
    }
    pub fn set_tx_delay(&self, p: RankLane, delay: u16) -> Result<(), Error> {
        self.lane(p)?.tx_train[p.rank].modify(TxTrain::DQS.val(u32::from(delay & 0x1ff)));
        self.download_lane(p, RegisterFile::Write)
    }
    pub fn set_rx_dqs_delay(
        &self,
        state: &mut CalibrationState,
        p: RankLane,
        delay: u8,
    ) -> Result<(), Error> {
        let delay = delay & 0x3f;
        self.lane(p)?.rx_train[p.rank]
            .modify(RxTrain::DQS_P.val(delay.into()) + RxTrain::DQS_N.val(delay.into()));
        state.training.ch[p.channel].rx_dqs_p[p.rank][p.lane] = delay;
        state.training.ch[p.channel].rx_dqs_n[p.rank][p.lane] = delay;
        self.download_lane(p, RegisterFile::Read)
    }
    pub fn set_rx_vref(&self, p: RankLane, vref: i8) -> Result<(), Error> {
        if !(-32..=31).contains(&vref) {
            return Err(Error::InvalidReutConfig);
        }
        self.lane(p)?.rx_train[p.rank].modify(RxTrain::VREF.val((vref as u8 & 0x3f).into()));
        self.download_lane(p, RegisterFile::Read)
    }
    /// Margining write only; does not overwrite the host receive-enable center.
    pub fn write_rcven_pi(&self, p: RankLane, delay: u16) -> Result<(), Error> {
        self.lane(p)?.rx_train[p.rank].modify(RxTrain::RCVEN.val(u32::from(delay & 0x1ff)));
        self.download_lane(p, RegisterFile::Read)
    }
    pub fn set_rcven_delay(
        &self,
        state: &mut CalibrationState,
        p: RankLane,
        delay: u16,
    ) -> Result<(), Error> {
        self.write_rcven_pi(p, delay)?;
        state.training.ch[p.channel].rank[p.rank].rcven_dly[p.lane] = delay & 0x1ff;
        Ok(())
    }
    pub fn set_rx_eq(&self, p: RankLane, eq: u8) -> Result<(), Error> {
        self.lane(p)?.rx_train[p.rank].modify(RxTrain::EQ.val(u32::from(eq & 0x1f)));
        self.download_lane(p, RegisterFile::Read)
    }
    pub fn set_tx_eq(&self, p: RankLane, eq: u8) -> Result<(), Error> {
        self.lane(p)?.tx_train[p.rank].modify(TxTrain::EQ.val(u32::from(eq & 0x3f)));
        self.download_lane(p, RegisterFile::Write)
    }
    pub fn set_tx_dq_delay(
        &self,
        channel: usize,
        bit: usize,
        rank: usize,
        delay: u8,
    ) -> Result<(), Error> {
        if bit >= 64 {
            return Err(Error::InvalidReutConfig);
        }
        let p = RankLane {
            channel,
            rank,
            lane: bit / 8,
        };
        let reg = &self.lane(p)?.tx_per_bit[rank];
        let shift = (bit % 8) * 4;
        reg.set((reg.get() & !(0xf << shift)) | (u32::from(delay & 0xf) << shift));
        self.download_lane(p, RegisterFile::Write)
    }
    pub fn set_tx_dq_byte_delay(&self, p: RankLane, delay: u16) -> Result<(), Error> {
        self.lane(p)?.tx_train[p.rank].modify(TxTrain::DQ.val(u32::from(delay & 0x1ff)));
        self.download_lane(p, RegisterFile::Write)
    }
    pub fn feedback(&self, p: RankLane) -> Result<u16, Error> {
        Ok(self.lane(p)?.feedback.read(TrainingFeedback::VALUE) as u16)
    }
    /// All eight LDAT chunks, two DATAIN halves, SDAT then PDAT trigger.
    /// This is an upload, not a read/modify/write of existing WDB bits.
    pub fn write_wdb_fixed_pattern(
        &self,
        ch: usize,
        pattern: u64,
        mask: u64,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        let r = self.channel(ch)?;
        r.wdb_mux.set(0);
        r.wdb_control.set(0);
        for chunk in 0..8 {
            let data = pattern & mask;
            r.ldat_data[0].set(data as u32);
            r.ldat_data[1].set((data >> 32) as u32);
            r.ldat_sdat
                .write(LdatSdat::MODE::SET + LdatSdat::BANK.val(chunk));
            r.ldat_pdat.write(LdatPdat::COMMAND::Write);
            delay.delay_us(1);
        }
        r.ldat_sdat.set(0);
        Ok(())
    }
    pub fn write_wdb(
        &self,
        ch: usize,
        pattern: u32,
        mask: u32,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        self.write_wdb_fixed_pattern(
            ch,
            u64::from(pattern) * 0x100000001,
            u64::from(mask) * 0x100000001,
            delay,
        )
    }
    pub fn write_wdb_data(
        &self,
        ch: usize,
        low: u32,
        high: u32,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        self.write_wdb_fixed_pattern(
            ch,
            u64::from(low) | (u64::from(high) << 32),
            u64::MAX,
            delay,
        )
    }
    pub fn write_wdb_spread(
        &self,
        ch: usize,
        patterns: &[u8],
        selectors: &[u8],
        start: u8,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        if selectors.is_empty() || selectors.iter().any(|s| usize::from(*s) >= patterns.len()) {
            return Err(Error::InvalidReutConfig);
        }
        let r = self.channel(ch)?;
        r.wdb_mux.set(0);
        r.wdb_control.set(0);
        for chunk in 0..8 {
            for half in 0..2 {
                let mut data = 0;
                for b in 0..32 {
                    if patterns[usize::from(selectors[(b + 32 * half) % selectors.len()])]
                        & (1 << chunk)
                        != 0
                    {
                        data |= 1 << b;
                    }
                }
                r.ldat_data[half].set(data);
            }
            r.ldat_sdat
                .write(LdatSdat::MODE::SET + LdatSdat::BANK.val(chunk));
            r.ldat_pdat
                .write(LdatPdat::COMMAND::Write + LdatPdat::ADDRESS.val(u32::from(start.min(63))));
            delay.delay_us(1);
        }
        r.ldat_sdat.set(0);
        Ok(())
    }
    fn write_wdb_va_pattern(
        &self,
        channels: u8,
        mut aggressors: u32,
        mut victims: u32,
        rotations: u8,
        start: u8,
    ) {
        let compressed = [0xaa, 0xc0, 0xcc, 0xf0];
        for v in 0..rotations {
            for chunk in 0..8 {
                let mut data = 0;
                for bit in 0..32 {
                    let index = match (victims & (1 << bit) != 0, aggressors & (1 << bit) != 0) {
                        (true, false) => 0,
                        (true, true) => 1,
                        (false, true) => 2,
                        (false, false) => 3,
                    };
                    if compressed[index] & (1 << chunk) != 0 {
                        data |= 1 << bit;
                    }
                }
                for ch in 0..2 {
                    if channels & (1 << ch) == 0 {
                        continue;
                    }
                    let r = &self.registers.channel[ch];
                    r.ldat_data[0].set(data);
                    r.ldat_data[1].set(data);
                    r.ldat_sdat
                        .write(LdatSdat::MODE::SET + LdatSdat::BANK.val(chunk));
                    r.ldat_pdat.write(
                        LdatPdat::COMMAND::Write
                            + LdatPdat::ADDRESS.val(u32::from((start + v).min(63))),
                    );
                }
            }
            victims = victims.rotate_left(1);
            aggressors = aggressors.rotate_left(1);
        }
        for ch in 0..2 {
            if channels & (1 << ch) != 0 {
                self.registers.channel[ch].ldat_sdat.set(0);
            }
        }
    }
    pub fn setup_wdb(&self, channels: u8) -> Result<(), Error> {
        self.validate_channels(channels)?;
        self.write_wdb_va_pattern(channels, 0, 0x01010101, 8, 0);
        for (a, mask) in [
            0x86186186, 0x18618618, 0x30c30c30, 0xa28a28a2, 0x8a28a28a, 0x14514514, 0x28a28a28,
            0x92492492, 0x24924924,
        ]
        .iter()
        .enumerate()
        {
            self.write_wdb_va_pattern(channels, *mask, 0x41041041, 6, 8 + a as u8 * 6);
        }
        for ch in 0..2 {
            if channels & (1 << ch) == 0 {
                continue;
            }
            let r = &self.registers.channel[ch];
            for (i, seed) in [0xa10ca1, 0xef0d08, 0xad0a1e].iter().enumerate() {
                r.wdb_read_seed[i].set(*seed);
                r.wdb_write_seed[i].set(*seed);
            }
            r.wdb_lmn
                .write(Lmn::N.val(10) + Lmn::M.val(1) + Lmn::L.val(1) + Lmn::SWEEP::SET);
        }
        Ok(())
    }
    pub fn set_loop_count(&self, ch: usize, count: u32) -> Result<(), Error> {
        self.channel(ch)?;
        if count == 0 {
            return Err(Error::InvalidReutConfig);
        }
        self.registers.reut_loop_count[ch].set(count);
        Ok(())
    }
    /// Empty intersection disables this channel. Nibble packing retains rank holes.
    /// Native reut.c:1135–1184 also disables an EMPTY physical channel.
    /// No selected rank is a valid control operation, not a traffic request.
    /// In that case preserve the entire wrap word, clear map/global-control.
    pub fn program_rank_map(&self, ch: usize, selected: u8) -> Result<u8, Error> {
        let populated = rank_mask(self.dimms, ch)?;
        if selected & !0xf != 0 {
            return Err(Error::InvalidRank);
        }
        let selected = selected & populated;
        if selected == 0 {
            self.registers.reut_rank_map[ch].set(0);
            self.registers.reut_sequence[ch][0].modify(ReutSequence::GLOBAL_CONTROL::CLEAR);
            return Ok(0);
        }
        let mut count: u32 = 0;
        let mut map = 0;
        for rank in 0..4 {
            if selected & (1 << rank) != 0 {
                map |= rank << (count * 4);
                count += 1;
            }
        }
        self.registers.reut_rank_map[ch].set(map);
        self.registers.reut_address_wrap[ch]
            .modify(TestAddress::RANK.val(u64::from(count.saturating_sub(1))));
        self.registers.reut_sequence[ch][0]
            .modify(ReutSequence::GLOBAL_CONTROL.val(u32::from(count != 0)));
        Ok(selected)
    }
    pub fn setup_error_counters(&self, ch: usize) -> Result<(), Error> {
        let r = self.channel(ch)?;
        for byte in 0..8 {
            r.error_counter_control[byte].write(
                ErrorCounterControl::POINTER.val(byte as u32) + ErrorCounterControl::KIND::Byte,
            );
        }
        Ok(())
    }
    pub fn read_errors(&self, ch: usize) -> Result<ErrorSnapshot, Error> {
        let r = self.channel(ch)?;
        let dq = r.error_data.get();
        Ok(ErrorSnapshot {
            failing_dq: dq,
            failing_bits_per_byte: core::array::from_fn(|lane| {
                ((dq >> (lane * 8)) as u8).count_ones() as u8
            }),
            context: r.error_detail.get(),
            counters: core::array::from_fn(|byte| {
                r.error_count[byte].read(ErrorCounterStatus::COUNT)
            }),
        })
    }
    pub fn set_soe(&self, ch: usize, mode: super::io_test::StopOnError) -> Result<(), Error> {
        self.channel(ch)?
            .error_control
            .modify(ErrorControl::STOP.val(mode as u32));
        Ok(())
    }
    /// Lane/byte selection is a DQ bit mask, not a permutation of board wiring.
    pub fn set_error_masks(&self, ch: usize, dq_ignore: u64, ecc_ignore: u8) -> Result<(), Error> {
        let r = self.channel(ch)?;
        r.error_data_mask.set(dq_ignore);
        r.error_ecc_mask.set(ecc_ignore.into());
        Ok(())
    }
    pub fn save_and_set_pcit(&self, ch: usize) -> Result<u32, Error> {
        let r = self.channel(ch)?;
        let saved = r.pcit.get();
        r.pcit.modify(Pcit::CPU.val(255));
        Ok(saved)
    }
    pub fn restore_pcit(&self, ch: usize, saved: u32) -> Result<(), Error> {
        self.channel(ch)?.pcit.set(saved);
        Ok(())
    }
    /// Normal mode is a primitive only, not a training/activation certificate.
    pub fn set_normal_mode(&self, ch: usize) -> Result<(), Error> {
        self.channel(ch)?;
        self.registers.reut_sequence[ch][0].write(ReutSequence::INIT_MODE::Normal);
        self.registers.reut_sequence[ch][1].set(0);
        Ok(())
    }
    pub fn start(&self, channels: u8, clear_errors: bool) -> Result<(), Error> {
        self.validate_channels(channels)?;
        // Global start must not run a stale enabled channel outside the selection.
        for ch in 0..2 {
            let enabled = self.registers.reut_sequence[ch][0].is_set(ReutSequence::GLOBAL_CONTROL);
            if enabled != (channels & (1 << ch) != 0) {
                return Err(Error::InvalidReutConfig);
            }
        }
        let ctl = &self.registers.reut_global_control;
        let policy = ctl.get() & ReutControl::STOP_ON_ANY_ERROR::SET.value;
        ctl.set(
            policy
                | ReutControl::START::SET.value
                | if clear_errors {
                    ReutControl::CLEAR_ERRORS::SET.value
                } else {
                    0
                },
        );
        Ok(())
    }
    pub fn stop(&self, delay: &mut impl DelayNs) {
        self.registers
            .reut_global_control
            .write(ReutControl::STOP::SET);
        delay.delay_us(1);
        self.registers.reut_global_control.set(0);
    }
    /// Waits for ALL selected done bits; error bits never stand in for completion.
    /// Always stops, including timeout. Mismatch feedback is returned separately.
    pub fn poll(
        &self,
        channels: u8,
        timeout_us: u32,
        delay: &mut impl DelayNs,
    ) -> Result<TestResult, Error> {
        self.validate_channels(channels)?;
        let mut done = 0;
        for _ in 0..timeout_us.div_ceil(10).max(1) {
            let status = self.registers.reut_global_error.extract();
            done = status.read(ReutGlobalError::DONE) as u8 & channels;
            if done == channels {
                let result = TestResult {
                    error_channels: status.read(ReutGlobalError::ERRORS) as u8 & channels,
                };
                self.stop(delay);
                return Ok(result);
            }
            delay.delay_us(10);
        }
        self.stop(delay);
        Err(Error::ReutTestTimeout { channels, done })
    }
    pub fn run_io_no_reset(
        &self,
        channels: u8,
        delay: &mut impl DelayNs,
    ) -> Result<TestResult, Error> {
        self.start(channels, true)?;
        delay.delay_us(1);
        self.poll(channels, 10_000_000, delay)
    }
    pub fn run_io(&self, channels: u8, delay: &mut impl DelayNs) -> Result<TestResult, Error> {
        self.validate_channels(channels)?;
        if let Err(error) = io_reset(self.registers, delay, 100_000, 100) {
            self.stop(delay);
            return Err(error);
        }
        self.run_io_no_reset(channels, delay)
    }
}

/// Shared bounded IO reset; interval/count preserve each source caller's budget.
/// A timeout is fatal, not a successful reset followed by a 60us delay.
pub fn io_reset(
    registers: &MchRegisters,
    delay: &mut impl DelayNs,
    polls: u32,
    interval_us: u32,
) -> Result<(), Error> {
    registers.init_state.modify(InitState::IO_RESET::SET);
    for _ in 0..polls {
        if !registers.init_state.is_set(InitState::IO_RESET) {
            delay.delay_us(60);
            return Ok(());
        }
        delay.delay_us(interval_us);
    }
    Err(Error::IoResetTimeout)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BasicIoMode {
    WriteRead,
    ReadOnly,
}

impl Engine<'_> {
    /// reut_setup_io: select the lowest *present physical* rank in cs_mask,
    /// placed in logical slot zero. General multi-rank setup uses program_rank_map.
    pub fn setup_io(&self, ch: usize, cs_mask: u8, mode: BasicIoMode) -> Result<(), Error> {
        use super::io_test::{Address, AddressSetup, StopOnError};
        self.channel(ch)?;
        if cs_mask & !0xf != 0 {
            return Err(Error::InvalidRank);
        }
        let selected = cs_mask & rank_mask(self.dimms, ch)?;
        if selected == 0 {
            return Err(Error::InvalidRank);
        }
        let rank = selected.trailing_zeros();
        let sub = self.subsequences(ch)?;
        let common = Subsequence::COUNT.val(1) + Subsequence::RESET_BASE::SET;
        if mode == BasicIoMode::ReadOnly {
            sub[0].write(common + Subsequence::KIND::Read);
        } else {
            sub[0].write(common + Subsequence::KIND::Write);
            sub[1].write(common + Subsequence::KIND::Read);
        }
        self.registers.reut_sequence[ch][0].write(
            ReutSequence::GLOBAL_CONTROL::SET
                + ReutSequence::INIT_MODE::Testing
                + ReutSequence::SUBSEQ_END.val(u32::from(mode == BasicIoMode::WriteRead)),
        );
        self.registers.reut_sequence[ch][1].set(2);
        self.program_address(
            ch,
            &AddressSetup {
                start: Address::default(),
                end: Address {
                    column: 1023 & 0xff8,
                    ..Address::default()
                },
                order: [0; 4],
                increment_rate: [28, 24, 8, 0],
                increment: [0, 1, 1, 1],
            },
        )?;
        self.registers.reut_rank_map[ch].set(rank);
        let r = &self.registers.channel[ch];
        r.error_control.write(
            ErrorControl::NTH.val(1)
                + ErrorControl::STOP.val(StopOnError::Never as u32)
                + ErrorControl::CHUNKS.val(255)
                + ErrorControl::CACHELINES.val(255),
        );
        self.set_error_masks(ch, 0, 0)?;
        self.setup_error_counters(ch)?;
        self.registers.reut_control[ch].write(ReutControl::CLEAR_ERRORS::SET);
        self.set_loop_count(ch, 64)
    }
    pub fn set_rcven_address(&self, ch: usize) -> Result<(), Error> {
        use super::io_test::{Address, AddressSetup};
        self.channel(ch)?;
        let order = self.registers.reut_address_order[ch].get();
        self.program_address(
            ch,
            &AddressSetup {
                start: Address::default(),
                end: Address {
                    bank: 1,
                    column: 1023 & 0xff8,
                    ..Address::default()
                },
                order: [0; 4],
                increment_rate: [0; 4],
                increment: [0, 1, 0, 1],
            },
        )?;
        self.registers.reut_address_order[ch].set(order);
        Ok(())
    }
    pub fn set_rcven_read_burst(&self, ch: usize) -> Result<(), Error> {
        self.subsequences(ch)?[0].modify(
            Subsequence::COUNT.val(8) + Subsequence::LINEAR::SET + Subsequence::WAIT.val(4),
        );
        Ok(())
    }
    /// A BER byte-map selects which physical byte's errors feed a counter;
    /// this does NOT remap the board's DQ/DQS wires or physical rank identity.
    pub fn setup_byte_error_counter(
        &self,
        ch: usize,
        counter: usize,
        physical_byte: u8,
    ) -> Result<(), Error> {
        let r = self.channel(ch)?;
        if counter >= 9 || physical_byte >= 9 {
            return Err(Error::InvalidReutConfig);
        }
        r.error_counter_control[counter].write(
            ErrorCounterControl::POINTER.val(physical_byte.into())
                + ErrorCounterControl::KIND::Byte,
        );
        Ok(())
    }
    pub fn read_error_counter(&self, ch: usize, counter: usize) -> Result<u32, Error> {
        let r = self.channel(ch)?;
        let reg = r.error_count.get(counter).ok_or(Error::InvalidReutConfig)?;
        Ok(reg.read(ErrorCounterStatus::COUNT))
    }
    pub fn set_scrub_sequence(&self, ch: usize) -> Result<(), Error> {
        self.channel(ch)?;
        self.registers.reut_sequence[ch][0]
            .write(ReutSequence::GLOBAL_CONTROL::SET + ReutSequence::INIT_MODE::Testing);
        self.registers.reut_sequence[ch][1].set(2);
        Ok(())
    }
    /// Scrub count is an EXPONENT already, unlike setup_io_test's CL count.
    pub fn set_scrub_subsequence(&self, ch: usize, cacheline_exponent: u8) -> Result<(), Error> {
        if cacheline_exponent > 127 {
            return Err(Error::InvalidReutConfig);
        }
        self.subsequences(ch)?[0]
            .write(Subsequence::COUNT.val(cacheline_exponent.into()) + Subsequence::KIND::Write);
        Ok(())
    }
    pub fn set_scrub_address_range(
        &self,
        ch: usize,
        column_end: u16,
        row_end: u32,
        bank_end: u8,
        row_rate: u32,
        bank_rate: u32,
    ) -> Result<(), Error> {
        use super::io_test::{Address, AddressSetup, encode_rate};
        self.channel(ch)?;
        // Preserve the logical rank-count field installed by program_rank_map.
        let ranks = self.registers.reut_address_wrap[ch].read(TestAddress::RANK);
        let address = AddressSetup {
            start: Address::default(),
            end: Address {
                rank: ranks as u8,
                bank: bank_end,
                row: row_end,
                column: column_end,
            },
            order: [0; 4],
            increment_rate: [0, bank_rate, row_rate, 0],
            increment: [0, 1, 1, 1],
        };
        // Scrub leaves column/rank scale zero, unlike general IO setup.
        encode_rate(row_rate, 15)?;
        encode_rate(bank_rate, 31)?;
        self.program_address(ch, &address)?;
        self.registers.reut_address_increment[ch].modify(
            AddressIncrement::COLUMN_RATE.val(0)
                + AddressIncrement::COLUMN_LINEAR::CLEAR
                + AddressIncrement::RANK_RATE.val(0)
                + AddressIncrement::RANK_LINEAR::CLEAR,
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::spd::testing::{seal, sodimm};
    use super::*;
    use tock_registers::LocalRegisterCopy;

    #[test]
    fn reut_cadb_rank_one_mirror_and_single_rank_chip_select() {
        let mut raw = sodimm();
        raw[12] = 9;
        seal(&mut raw);
        let d = Dimm::decode(&raw).unwrap();
        let dimms = [[Some(d), None], [None, None]];
        let c = CadbCommand::for_ranks(&dimms, 0, 0xf, Command::Mrs, 1, [1 << 3, 0], 0).unwrap();
        assert_eq!(c.count, 2);
        let first = LocalRegisterCopy::<u64, CadbProgram::Register>::new(c.patterns[0]);
        let second = LocalRegisterCopy::<u64, CadbProgram::Register>::new(c.patterns[1]);
        assert_eq!(
            (
                first.read(CadbProgram::BANK),
                first.read(CadbProgram::ADDRESS),
                first.read(CadbProgram::CHIP_SELECT)
            ),
            (1, 8, 14)
        );
        assert_eq!(
            (
                second.read(CadbProgram::BANK),
                second.read(CadbProgram::ADDRESS),
                second.read(CadbProgram::CHIP_SELECT)
            ),
            (2, 16, 13)
        );
        assert_eq!(first.read(CadbProgram::COMMAND), 8);
        assert_eq!(first.read(CadbProgram::CKE), 15);
        assert_eq!(c.mrs, 0x30010083);
        let bg = CadbCommand::for_ranks(&dimms, 0, 2, Command::Mrs, 4, [1 << 11, 0], 0).unwrap();
        let p = LocalRegisterCopy::<u64, CadbProgram::Register>::new(bg.patterns[0]);
        assert_eq!(
            (p.read(CadbProgram::BANK), p.read(CadbProgram::ADDRESS)),
            (8, 1 << 13)
        );
    }

    #[test]
    fn reut_cadb_rejects_empty_or_invalid_rank_selection() {
        assert_eq!(
            CadbCommand::for_ranks(&[[None; 2]; 2], 0, 15, Command::Mrs, 0, [0; 2], 0),
            Err(Error::InvalidRank)
        );
        assert_eq!(rank_mask(&[[None; 2]; 2], 2), Err(Error::InvalidRank));
    }
}
