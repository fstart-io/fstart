// SPDX-License-Identifier: MIT OR Apache-2.0
//! DDR4 REUT setup/runner translated from native/io_test_setup.c.
//! No routine here certifies trained memory. Callers must seed timing shadows
//! from actual initial MC programming before using read-read turnaround tests.

use super::{
    Error,
    registers::*,
    reut::{self, Engine, TestResult},
    reut_registers::*,
    training_state::CalibrationState,
};
use embedded_hal::delay::DelayNs;
use tock_registers::{
    LocalRegisterCopy,
    interfaces::{ReadWriteable, Readable, Writeable},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum StopOnError {
    Never = 0,
    Nth = 1,
    AllByteGroups = 2,
    AllLanes = 3,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandPattern {
    WriteRead,
    Write,
    Read,
    ReadWriteTurnaround,
    WriteReadTurnaround,
    OdtTurnaround,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum DqPattern {
    BasicVa = 0,
    SegmentWdb = 1,
    Cadb = 2,
    Turnaround = 3,
    Lmn = 4,
    TurnaroundRw = 5,
    TurnaroundRwSg = 6,
    TurnaroundRwDg = 7,
    TurnaroundWr = 8,
    TurnaroundWrSg = 9,
    TurnaroundWrDg = 10,
    TurnaroundOdt = 11,
    ReadRead = 14,
    ReadReadAll = 15,
}
impl TryFrom<u8> for DqPattern {
    type Error = Error;
    fn try_from(value: u8) -> Result<Self, Error> {
        Ok(match value {
            0 => Self::BasicVa,
            1 => Self::SegmentWdb,
            2 => Self::Cadb,
            3 => Self::Turnaround,
            4 => Self::Lmn,
            5 => Self::TurnaroundRw,
            6 => Self::TurnaroundRwSg,
            7 => Self::TurnaroundRwDg,
            8 => Self::TurnaroundWr,
            9 => Self::TurnaroundWrSg,
            10 => Self::TurnaroundWrDg,
            11 => Self::TurnaroundOdt,
            14 => Self::ReadRead,
            15 => Self::ReadReadAll,
            _ => return Err(Error::InvalidReutConfig),
        })
    }
}
#[derive(Debug, Clone, Copy)]
pub struct WdbPattern {
    pub increment_rate: u32,
    pub start: u8,
    pub end: u8,
    pub pattern: DqPattern,
}

/// Rank is LOGICAL. Column is the source's unshifted register image, not a
/// cacheline index. General IO setup retains its low three bits (e.g. 1023);
/// the separate reut_setup_io helper masks them as in reut.c.
#[derive(Debug, Clone, Copy, Default)]
pub struct Address {
    pub rank: u8,
    pub bank: u8,
    pub row: u32,
    pub column: u16,
}
impl Address {
    fn encoded(self) -> Result<u64, Error> {
        if self.rank > 7 || self.bank > 15 || self.row > 0x1ffff || self.column > 0xfff {
            return Err(Error::InvalidReutConfig);
        }
        Ok((TestAddress::RANK.val(self.rank.into())
            + TestAddress::BANK.val(self.bank.into())
            + TestAddress::ROW.val(self.row.into()))
        .value
            | u64::from(self.column))
    }
}
#[derive(Debug, Clone, Copy)]
pub struct AddressSetup {
    pub start: Address,
    pub end: Address,
    /// Field order is [rank, bank, row, column], as in reut_addr_t.
    pub order: [u8; 4],
    pub increment_rate: [u32; 4],
    pub increment: [u16; 4],
}
impl AddressSetup {
    fn images(&self) -> Result<(u64, u64, u32, u64), Error> {
        if self.order.iter().any(|o| *o > 3)
            || self.increment[0] > 7
            || self.increment[1] > 15
            || self.increment[2] > 4095
            || self.increment[3] > 255
        {
            return Err(Error::InvalidReutConfig);
        }
        let order = (AddressOrder::RANK.val(self.order[0].into())
            + AddressOrder::BANK.val(self.order[1].into())
            + AddressOrder::ROW.val(self.order[2].into())
            + AddressOrder::COLUMN.val(self.order[3].into()))
        .value;
        let (rank, rl) = encode_rate(self.increment_rate[0], 31)?;
        let (bank, bl) = encode_rate(self.increment_rate[1], 31)?;
        let (row, rowl) = encode_rate(self.increment_rate[2], 15)?;
        let (col, cl) = encode_rate(self.increment_rate[3], 31)?;
        let inc = (AddressIncrement::RANK.val(self.increment[0].into())
            + AddressIncrement::RANK_RATE.val(rank.into())
            + AddressIncrement::RANK_LINEAR.val(rl.into())
            + AddressIncrement::BANK.val(self.increment[1].into())
            + AddressIncrement::BANK_RATE.val(bank.into())
            + AddressIncrement::BANK_LINEAR.val(bl.into())
            + AddressIncrement::ROW.val(self.increment[2].into())
            + AddressIncrement::ROW_RATE.val(row.into())
            + AddressIncrement::ROW_LINEAR.val(rowl.into())
            + AddressIncrement::COLUMN.val(self.increment[3].into())
            + AddressIncrement::COLUMN_RATE.val(col.into())
            + AddressIncrement::COLUMN_LINEAR.val(cl.into()))
        .value;
        Ok((self.start.encoded()?, self.end.encoded()?, order, inc))
    }
}
/// Source __fls(n - 1) + 1, defined even for n=1; no undefined clz(0).
pub fn ceil_log2(n: u32) -> Result<u32, Error> {
    if n == 0 {
        return Err(Error::InvalidReutConfig);
    }
    Ok(32 - (n - 1).leading_zeros())
}
/// Hybrid linear/exponential address update encoding. Zero is a valid linear
/// rate; reject exponent overflow instead of truncating to a shorter traversal.
pub fn encode_rate(rate: u32, linear_max: u8) -> Result<(u8, bool), Error> {
    if ![15, 31, 63, 127].contains(&linear_max) {
        return Err(Error::InvalidReutConfig);
    }
    if rate <= u32::from(linear_max) {
        return Ok((rate as u8, true));
    }
    let exp = ceil_log2(rate)?;
    if exp > u32::from(linear_max) {
        return Err(Error::InvalidReutConfig);
    }
    Ok((exp as u8, false))
}
fn cachelines(n: u32) -> Result<u32, Error> {
    if n == 0 {
        return Err(Error::InvalidReutConfig);
    }
    Ok(if n <= 127 { n | 128 } else { ceil_log2(n)? })
}
#[derive(Debug, Clone, Copy)]
pub struct TestSetup {
    pub command: CommandPattern,
    pub cachelines: u32,
    pub loop_exponent: u8,
    pub address: AddressSetup,
    pub stop: StopOnError,
    pub wdb: WdbPattern,
    pub cadb: bool,
    pub cke_wait: bool,
    pub subsequence_wait: u16,
    /// Required for dummy reads, from the resolved PLL (QCLK period in ps).
    pub qclk_ps: u32,
}

impl Engine<'_> {
    pub fn subsequences(
        &self,
        ch: usize,
    ) -> Result<&[fstart_core::mmio::MmioReadWrite<u32, Subsequence::Register>; 8], Error> {
        self.channel(ch)?;
        Ok(if ch == 0 {
            &self.registers.reut_subsequence0
        } else {
            &self.registers.reut_subsequence1
        })
    }
    pub fn program_address(&self, ch: usize, address: &AddressSetup) -> Result<(), Error> {
        self.channel(ch)?;
        let (start, end, order, inc) = address.images()?;
        self.registers.reut_address_start[ch].set(start);
        self.registers.reut_address_wrap[ch].set(end);
        self.registers.reut_address_order[ch].set(order);
        self.registers.reut_address_increment[ch].set(inc);
        Ok(())
    }
    /// Source io_setup_cadb: eight pointer-advancing, single-u64 FIFO writes.
    pub fn setup_cadb(&self, ch: usize, spread: u8, victim: u8, lmn: bool) -> Result<(), Error> {
        let r = self.channel(ch)?;
        if spread == 0 {
            return Err(Error::InvalidReutConfig);
        }
        // The CADB FIFO takes one 64-bit write per entry, which only long
        // mode issues as a single access.
        if !cfg!(target_arch = "x86_64") {
            return Err(Error::InvalidReutConfig);
        }
        r.cadb_write_pointer
            .write(CadbWritePointer::WRITE_ENABLE::SET);
        for row in 0..8 {
            let mut pattern = 0;
            for bit in 0..25 {
                let remap = if bit >= 21 {
                    bit + 19
                } else if bit >= 17 {
                    bit + 7
                } else {
                    bit
                };
                let lfsr = if bit % u32::from(spread) == u32::from(victim) {
                    row & 1
                } else {
                    (row >> 1) & 1
                };
                if lfsr != 0 {
                    pattern |= 1u64 << remap;
                }
            }
            r.cadb_program.set(pattern);
        }
        r.cadb_mux.write(
            CadbMux::MUX0.val(if lmn { 0 } else { 2 })
                + CadbMux::MUX1.val(2)
                + CadbMux::MUX2.val(2)
                + CadbMux::LFSR.val(3),
        );
        r.cadb_lmn
            .write(Lmn::SWEEP::SET + Lmn::L.val(1) + Lmn::M.val(1) + Lmn::N.val(6));
        for (i, seed) in [0x96ea1, 0xcabeef, 0x53dead].iter().enumerate() {
            r.cadb_seed[i].set(*seed);
        }
        Ok(())
    }
    pub fn disable_cadb_deselects(&self, channels: u8) -> Result<(), Error> {
        self.validate_channels(channels)?;
        for ch in 0..2 {
            if channels & (1 << ch) != 0 {
                self.registers.channel[ch]
                    .cadb_control
                    .modify(CadbControl::DESELECT::CLEAR);
            }
        }
        Ok(())
    }
    fn zq_short(&self, channels: u8, delay: &mut impl DelayNs) -> Result<(), Error> {
        for ch in 0..2 {
            if channels & (1 << ch) == 0 {
                continue;
            }
            let gap = if channels & (1 << (ch + 1)) == 0 {
                7
            } else {
                1
            };
            reut::CadbCommand::for_ranks(
                self.dimms,
                ch,
                0xf,
                reut::Command::ZqCalibration,
                0,
                [0; 2],
                gap,
            )?
            .issue(self.registers, ch, delay)?;
        }
        delay.delay_us(1);
        Ok(())
    }
    /// Full meminit_setup_io_test; input encodings are validated before writes.
    /// Does not mutate the caller's WDB rate into its hardware encoding.
    pub fn setup_io_test(
        &self,
        state: &CalibrationState,
        channels: u8,
        setup: &TestSetup,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        self.validate_channels(channels)?;
        let (start, end, order, inc) = setup.address.images()?;
        let cl = cachelines(setup.cachelines)?;
        let cl2 = cachelines(
            setup
                .cachelines
                .checked_mul(2)
                .ok_or(Error::InvalidReutConfig)?,
        )?;
        if setup.wdb.increment_rate == 0
            || setup.wdb.start > setup.wdb.end
            || setup.wdb.end > 63
            || (state.en_dummy_reads != 0 && setup.qclk_ps == 0)
        {
            return Err(Error::InvalidReutConfig);
        }
        let (wdb_rate, wdb_linear) = encode_rate(setup.wdb.increment_rate, 63)?;
        let lc_eff =
            (i32::from(setup.loop_exponent) - ceil_log2(setup.cachelines)? as i32 + 1).max(1);
        let loop_count = if matches!(
            setup.command,
            CommandPattern::WriteReadTurnaround | CommandPattern::OdtTurnaround
        ) {
            1
        } else {
            1u32.checked_shl((lc_eff - 1) as u32)
                .ok_or(Error::InvalidReutConfig)?
        };
        let mut wait = u32::from(setup.subsequence_wait);
        if setup.cke_wait {
            wait = wait.max((self.registers.power_down_config.get() & 0xfff) + 16);
        }
        wait = wait.min(255);
        let (ss_start, ss_end) = match setup.command {
            CommandPattern::WriteRead => (0, 1),
            CommandPattern::Write | CommandPattern::ReadWriteTurnaround => (0, 0),
            CommandPattern::Read => (1, 1),
            CommandPattern::WriteReadTurnaround => (0, 7),
            CommandPattern::OdtTurnaround => (0, 3),
        };
        let common = |count: u32| {
            count | Subsequence::WAIT.val(wait).value | Subsequence::RESET_BASE::SET.value
        };
        let wr = Subsequence::KIND::Write.value;
        let rdwr = Subsequence::KIND::ReadWrite.value;
        let wrrd = Subsequence::KIND::WriteRead.value;
        for ch in 0..2 {
            let cfg = &self.registers.reut_sequence[ch];
            if channels & (1 << ch) == 0 {
                cfg[0].set(0);
                cfg[1].set(0);
                continue;
            }
            let r = &self.registers.channel[ch];
            r.cadb_control
                .write(CadbControl::DESELECT.val(u32::from(setup.cadb)));
            if setup.cadb {
                self.setup_cadb(ch, 7, 8, false)?;
            }
            cfg[0].write(
                ReutSequence::GLOBAL_CONTROL::SET
                    + ReutSequence::INIT_MODE::Testing
                    + ReutSequence::SUBSEQ_START.val(ss_start)
                    + ReutSequence::SUBSEQ_END.val(ss_end)
                    + ReutSequence::DUMMY_READS.val(u32::from(state.en_dummy_reads != 0)),
            );
            cfg[1].set(2);
            self.registers.reut_loop_count[ch].set(loop_count);
            self.registers.reut_control[ch].write(ReutControl::CLEAR_ERRORS::SET);
            let sub = self.subsequences(ch)?;
            match setup.command {
                CommandPattern::ReadWriteTurnaround => sub[0].set(common(cl2) | wrrd),
                CommandPattern::WriteReadTurnaround => {
                    sub[0].set(common(cl) | wr);
                    for s in &sub[1..7] {
                        s.set(common(cl2) | rdwr);
                    }
                    sub[7].set(common(cl));
                }
                CommandPattern::OdtTurnaround => {
                    sub[0].set(common(cl) | wr);
                    sub[1].set(common(cl2) | rdwr);
                    sub[2].set(common(cl));
                    sub[3].set(common(cl2) | wrrd);
                }
                _ => {
                    sub[0].set(common(cl) | wr);
                    if setup.command != CommandPattern::Write {
                        sub[1].set(common(cl));
                    }
                }
            }
            self.registers.reut_address_start[ch].set(start);
            self.registers.reut_address_wrap[ch].set(end);
            self.registers.reut_address_order[ch].set(order);
            self.registers.reut_address_increment[ch].set(inc);
            r.wdb_control.write(
                WdbControl::RATE.val(wdb_rate.into())
                    + WdbControl::LINEAR.val(u32::from(wdb_linear))
                    + WdbControl::START.val(setup.wdb.start.into())
                    + WdbControl::END.val(setup.wdb.end.into()),
            );
            let mut mux = (WdbMux::MUX0.val(
                if matches!(setup.wdb.pattern, DqPattern::Cadb | DqPattern::Lmn) {
                    0
                } else {
                    2
                },
            ) + WdbMux::MUX1.val(2)
                + WdbMux::MUX2.val(2)
                + WdbMux::LFSR.val(6)
                + WdbMux::ECC_SOURCE::SET)
                .value;
            if matches!(
                setup.command,
                CommandPattern::WriteReadTurnaround | CommandPattern::OdtTurnaround
            ) {
                mux |= (WdbMux::RELOAD_RATE.val((ceil_log2(setup.cachelines)? + 1).min(7))
                    + WdbMux::SAVE_RATE.val(1))
                .value;
            }
            r.wdb_mux.set(mux);
            r.wdb_invert.set(0);
            r.error_control.write(
                ErrorControl::NTH.val(1)
                    + ErrorControl::STOP.val(setup.stop as u32)
                    + ErrorControl::CHUNKS.val(255)
                    + ErrorControl::CACHELINES.val(255),
            );
            self.set_error_masks(ch, 0, 0)?;
            self.setup_error_counters(ch)?;
            if state.en_dummy_reads != 0 {
                self.registers.reut_dummy_mask[ch].set(0xfc);
                self.registers.reut_dummy_control[ch].write(
                    Lmn::L.val((40000 / setup.qclk_ps) & 255)
                        + Lmn::M.val((40000 / setup.qclk_ps) & 255)
                        + Lmn::N.val((200000 / setup.qclk_ps) & 255)
                        + Lmn::SWEEP::SET,
                );
            }
        }
        if let Err(error) = self.zq_short(channels, delay) {
            self.stop(delay);
            for ch in 0..2 {
                if channels & (1 << ch) == 0 {
                    continue;
                }
                self.registers.reut_sequence[ch][0].modify(ReutSequence::GLOBAL_CONTROL::CLEAR);
                self.registers.channel[ch]
                    .cadb_control
                    .modify(CadbControl::DESELECT::CLEAR);
            }
            return Err(error);
        }
        Ok(())
    }
    pub fn setup_wl_cleanup_test(
        &self,
        state: &mut CalibrationState,
        channels: u8,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        self.validate_channels(channels)?;
        for ch in 0..2 {
            if channels & (1 << ch) == 0 {
                continue;
            }
            for (i, seed) in [0xaaaaaa, 0xcccccc, 0xf0f0f0].iter().enumerate() {
                self.registers.channel[ch].wdb_read_seed[i].set(*seed);
                self.registers.channel[ch].wdb_write_seed[i].set(*seed);
            }
        }
        let mut setup = basic_setup(4, 4, StopOnError::Never)?;
        setup.cachelines = 2;
        setup.wdb.increment_rate = 1;
        setup.address.increment_rate = [0; 4];
        setup.address.increment[0] = 0;
        self.setup_io_test(state, channels, &setup, delay)?;
        state.dq_pat = DqPattern::BasicVa as u8;
        state.dq_pat_lc = 1;
        Ok(())
    }
    pub fn setup_mpr_test(
        &self,
        state: &mut CalibrationState,
        channels: u8,
        lc: u8,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        let mut setup = basic_setup(lc, 4, StopOnError::Never)?;
        setup.command = CommandPattern::Read;
        setup.wdb.increment_rate = 1;
        setup.address.end = Address {
            bank: 1,
            ..Address::default()
        };
        setup.address.increment_rate = [0; 4];
        setup.address.increment = [0, 1, 0, 0];
        self.setup_io_test(state, channels, &setup, delay)?;
        state.dq_pat = DqPattern::BasicVa as u8;
        state.dq_pat_lc = 1;
        Ok(())
    }
    pub fn setup_basic_va_test(
        &self,
        state: &mut CalibrationState,
        channels: u8,
        lc: u8,
        spread: u8,
        stop: StopOnError,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        self.setup_io_test(state, channels, &basic_setup(lc, spread, stop)?, delay)?;
        state.dq_pat = DqPattern::BasicVa as u8;
        state.dq_pat_lc = u16::from(lc.saturating_sub(7).max(1));
        Ok(())
    }
    pub fn setup_cadb_test(
        &self,
        state: &mut CalibrationState,
        channels: u8,
        lc: u8,
        stop: StopOnError,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        let mut setup = basic_setup(lc, 10, stop)?;
        setup.cadb = true;
        setup.wdb.pattern = DqPattern::Cadb;
        setup.address.end.bank = 15;
        setup.address.end.row = 16383;
        setup.address.increment_rate = [32, 3, 3, 0];
        setup.address.increment = [1, 1, 73, 53];
        self.setup_io_test(state, channels, &setup, delay)?;
        state.dq_pat = DqPattern::Cadb as u8;
        state.dq_pat_lc = u16::from(lc.saturating_sub(4).max(1));
        Ok(())
    }
    fn set_bank_range(&self, ch: usize, start: u8, end: u8) {
        self.registers.reut_address_start[ch].modify(TestAddress::BANK.val(start.into()));
        self.registers.reut_address_wrap[ch].modify(TestAddress::BANK.val(end.into()));
    }
    fn setup_turnaround_test(&self, ch: usize, pattern: DqPattern, t: u32) {
        self.registers.reut_sequence[ch][0]
            .modify(ReutSequence::SUBSEQ_START.val(t) + ReutSequence::SUBSEQ_END.val(t));
        let wr = matches!(
            pattern,
            DqPattern::TurnaroundWr | DqPattern::TurnaroundWrSg | DqPattern::TurnaroundWrDg
        );
        let rate = u64::from(
            !((wr && (t == 0 || t == 7))
                || (pattern == DqPattern::TurnaroundOdt && (t == 0 || t == 2))),
        );
        let same_rank = matches!(
            pattern,
            DqPattern::TurnaroundWrSg | DqPattern::TurnaroundWrDg
        );
        self.registers.reut_address_increment[ch].modify(
            AddressIncrement::RANK_LINEAR.val(u64::from(!same_rank))
                + AddressIncrement::RANK_RATE.val(if same_rank { rate + 5 } else { rate })
                + AddressIncrement::ROW.val(1)
                + AddressIncrement::ROW_RATE.val(7 + rate)
                + AddressIncrement::ROW_LINEAR::CLEAR
                + AddressIncrement::COLUMN_LINEAR::SET
                + AddressIncrement::COLUMN_RATE.val(rate),
        );
        if pattern == DqPattern::TurnaroundWrDg {
            let b = if t == 0 || t == 7 { 2 } else { 3 };
            self.set_bank_range(ch, BANK_START[b], BANK_END[b]);
        }
        if pattern == DqPattern::TurnaroundOdt {
            self.set_bank_range(ch, BANK_START[t as usize], BANK_END[t as usize]);
        }
    }
    pub fn run_io_test(
        &self,
        state: &CalibrationState,
        channels: u8,
        delay: &mut impl DelayNs,
    ) -> Result<TestResult, Error> {
        self.run_io_test_bounded(state, channels, delay, 10000, 10_000_000)
    }
    /// Every transient address/subsequence/timing edit is restored on *all*
    /// exits, including reset failure, timeout and early SOE feedback.
    pub fn run_io_test_bounded(
        &self,
        state: &CalibrationState,
        channels: u8,
        delay: &mut impl DelayNs,
        reset_polls: u32,
        timeout_us: u32,
    ) -> Result<TestResult, Error> {
        self.validate_channels(channels)?;
        let pattern = DqPattern::try_from(state.dq_pat)?;
        let rdrd = matches!(pattern, DqPattern::ReadRead | DqPattern::ReadReadAll);
        let tests = match pattern {
            DqPattern::Cadb => 7,
            DqPattern::TurnaroundWr
            | DqPattern::TurnaroundWrSg
            | DqPattern::TurnaroundWrDg
            | DqPattern::ReadReadAll => 8,
            DqPattern::TurnaroundOdt => 4,
            DqPattern::ReadRead => 2,
            _ => 1,
        };
        // Reject tRDRD overflow before touching hardware; do not silently wrap.
        if rdrd {
            for ch in 0..2 {
                if channels & (1 << ch) == 0 {
                    continue;
                }
                let tc = LocalRegisterCopy::<u32, ReadReadTurnaround::Register>::new(
                    state.mchbar_tc_rdrd[ch],
                );
                let maxoff = if pattern == DqPattern::ReadRead { 1 } else { 3 };
                if tc.read(ReadReadTurnaround::SG) + maxoff > 63
                    || tc.read(ReadReadTurnaround::DG) + maxoff > 63
                    || (pattern == DqPattern::ReadReadAll
                        && tc.read(ReadReadTurnaround::DR) + maxoff > 63)
                {
                    return Err(Error::InvalidReutConfig);
                }
            }
        }
        let saved_cfg =
            core::array::from_fn::<_, 2, _>(|ch| self.registers.reut_sequence[ch][0].get());
        let saved_start =
            core::array::from_fn::<_, 2, _>(|ch| self.registers.reut_address_start[ch].get());
        let saved_wrap =
            core::array::from_fn::<_, 2, _>(|ch| self.registers.reut_address_wrap[ch].get());
        let saved_inc =
            core::array::from_fn::<_, 2, _>(|ch| self.registers.reut_address_increment[ch].get());
        // Host timings define sweep points, not the independent hardware image
        // to restore after success, dead eyes, reset failures or timeouts.
        let saved_rdrd =
            core::array::from_fn::<_, 2, _>(|ch| self.registers.channel[ch].tc_rdrd.get());
        let result = (|| {
            let mut errors = 0;
            for t in 0..tests {
                let mut stop_channels = 0;
                for ch in 0..2 {
                    if channels & (1 << ch) == 0 {
                        continue;
                    }
                    let soe = self.registers.channel[ch]
                        .error_control
                        .read(ErrorControl::STOP);
                    if soe == 1 || soe == 3 {
                        stop_channels |= 1 << ch;
                    }
                    match pattern {
                        DqPattern::Cadb => self.setup_cadb(ch, 7, t as u8, false)?,
                        DqPattern::TurnaroundWr
                        | DqPattern::TurnaroundWrSg
                        | DqPattern::TurnaroundWrDg
                        | DqPattern::TurnaroundOdt => self.setup_turnaround_test(ch, pattern, t),
                        DqPattern::TurnaroundRwSg | DqPattern::TurnaroundRwDg => {
                            self.set_bank_range(ch, BANK_START[3], BANK_END[3])
                        }
                        _ => {}
                    }
                    if rdrd {
                        let off = if pattern == DqPattern::ReadRead {
                            t
                        } else {
                            t % 4
                        };
                        let tc = LocalRegisterCopy::<u32, ReadReadTurnaround::Register>::new(
                            state.mchbar_tc_rdrd[ch],
                        );
                        let mut value = tc.get();
                        value = (value
                            & !(ReadReadTurnaround::SG::SET.mask()
                                | ReadReadTurnaround::DG::SET.mask()))
                            | (ReadReadTurnaround::SG.val(tc.read(ReadReadTurnaround::SG) + off)
                                + ReadReadTurnaround::DG
                                    .val(tc.read(ReadReadTurnaround::DG) + off))
                            .value;
                        if pattern == DqPattern::ReadReadAll {
                            value = (value & !ReadReadTurnaround::DR::SET.mask())
                                | ReadReadTurnaround::DR
                                    .val(tc.read(ReadReadTurnaround::DR) + off)
                                    .value;
                            self.registers.reut_address_increment[ch].modify(
                                AddressIncrement::RANK_LINEAR::SET
                                    + AddressIncrement::RANK_RATE.val(if t > 3 { 0 } else { 31 }),
                            );
                        }
                        self.registers.channel[ch].tc_rdrd.set(value);
                    }
                }
                reut::io_reset(self.registers, delay, reset_polls, 1)?;
                self.start(channels, t == 0)?;
                let feedback = self.poll(channels, timeout_us, delay)?;
                errors |= feedback.error_channels;
                if errors & stop_channels != 0 {
                    break;
                }
            }
            Ok(TestResult {
                error_channels: errors,
            })
        })();
        // poll stops normally; reset/start failure must also quiesce the engine.
        self.stop(delay);
        for ch in 0..2 {
            if channels & (1 << ch) == 0 {
                continue;
            }
            if rdrd {
                self.registers.channel[ch].tc_rdrd.set(saved_rdrd[ch]);
            }
            self.registers.reut_sequence[ch][0].set(saved_cfg[ch]);
            self.registers.reut_address_start[ch].set(saved_start[ch]);
            self.registers.reut_address_wrap[ch].set(saved_wrap[ch]);
            self.registers.reut_address_increment[ch].set(saved_inc[ch]);
        }
        result
    }
    pub fn run_cadb_test(
        &self,
        state: &CalibrationState,
        channels: u8,
        delay: &mut impl DelayNs,
    ) -> Result<[u8; 2], Error> {
        self.run_io_test(state, channels, delay)?;
        Ok(core::array::from_fn(|ch| {
            if channels & (1 << ch) != 0 {
                self.registers.channel[ch]
                    .error_detail
                    .read(ErrorDetail::BYTE_GROUPS) as u8
            } else {
                0
            }
        }))
    }
}
const BANK_START: [u8; 4] = [0, 4, 1, 5];
const BANK_END: [u8; 4] = [3, 11, 4, 12];

pub fn basic_setup(lc: u8, spread: u8, stop: StopOnError) -> Result<TestSetup, Error> {
    if spread == 0 || spread > 64 {
        return Err(Error::InvalidReutConfig);
    }
    Ok(TestSetup {
        command: CommandPattern::WriteRead,
        cachelines: 128,
        loop_exponent: lc,
        address: AddressSetup {
            start: Address::default(),
            end: Address {
                column: 1023,
                ..Address::default()
            },
            order: [0; 4],
            increment_rate: [32, 0, 0, 0],
            increment: [1, 0, 0, 1],
        },
        stop,
        wdb: WdbPattern {
            increment_rate: 4,
            start: 0,
            end: spread - 1,
            pattern: DqPattern::BasicVa,
        },
        cadb: false,
        cke_wait: false,
        subsequence_wait: 0,
        qclk_ps: 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn io_test_setup_linear_exponential_edges_and_nonzero_guards() {
        assert_eq!(ceil_log2(0), Err(Error::InvalidReutConfig));
        assert_eq!(ceil_log2(1), Ok(0));
        assert_eq!(ceil_log2(128), Ok(7));
        assert_eq!(ceil_log2(129), Ok(8));
        assert_eq!(cachelines(127), Ok(255));
        assert_eq!(cachelines(128), Ok(7));
        for (rate, max, expected) in [
            (0, 15, (0, true)),
            (15, 15, (15, true)),
            (16, 15, (4, false)),
            (31, 31, (31, true)),
            (32, 31, (5, false)),
            (33, 31, (6, false)),
            (63, 63, (63, true)),
            (64, 63, (6, false)),
        ] {
            assert_eq!(encode_rate(rate, max), Ok(expected));
        }
        assert_eq!(encode_rate(u32::MAX, 31), Err(Error::InvalidReutConfig));
        assert_eq!(
            basic_setup(14, 0, StopOnError::Never).unwrap_err(),
            Error::InvalidReutConfig
        );
        assert_eq!(
            basic_setup(14, 65, StopOnError::Never).unwrap_err(),
            Error::InvalidReutConfig
        );
    }
}
