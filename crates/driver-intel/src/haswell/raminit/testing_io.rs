//! REUT profiles used by Haswell DDR3 training (B0 exponential and C0
//! linear loop counts). The test address is in DRAM coordinates, not bytes.
use super::{
    reut::{self, read_qword, write_qword},
    spd::Topology,
    *,
};
#[derive(Clone, Copy, Default)]
pub struct Axis {
    pub start: u32,
    pub stop: u32,
    pub rate: u32,
    pub inc: u32,
    pub wrap: bool,
}
#[derive(Clone, Copy, Default)]
pub struct Address {
    pub col: Axis,
    pub row: Axis,
    pub bank: Axis,
    pub rank: Axis,
}
fn log2ceil(v: u32) -> u32 {
    u32::BITS - v.saturating_sub(1).leading_zeros()
}
fn rate(v: u32, limit: u32, shift: u32) -> u64 {
    u64::from(if v > limit {
        log2ceil(v - 1)
    } else {
        (1 << shift) | v
    })
}
impl Address {
    pub fn program(self, m: MchBar, ch: usize) {
        use REUT_SEQ_ADDR_INC_CTL as i;
        use REUT_SEQ_BASE_ADDR as a;
        use REUT_SEQ_MISC_CTL as w;
        let off = 8 * ch as u32;
        for (reg, select) in [(0x48d8, false), (0x48e8, true)] {
            let v = |x: Axis| u64::from(if select { x.stop } else { x.start });
            write_qword(
                m,
                reg + off,
                (a::COL_ADDR.val(v(self.col) / 8)
                    + a::ROW_ADDR.val(v(self.row))
                    + a::BANK_ADDR.val(v(self.bank))
                    + a::RANK_ADDR.val(v(self.rank)))
                .value,
            );
        }
        m.write32(
            0x4908 + 4 * ch as u32,
            (w::COL_WRAP_TRIGGER_EN.val(self.col.wrap.into())
                + w::ROW_WRAP_TRIGGER_EN.val(self.row.wrap.into())
                + w::BANK_WRAP_TRIGGER_EN.val(self.bank.wrap.into())
                + w::RANK_WRAP_TRIGGER_EN.val(self.rank.wrap.into()))
            .value,
        );
        write_qword(
            m,
            0x4910 + off,
            (i::COL_ADDR_UPDATE.val(rate(self.col.rate, 31, 7))
                + i::ROW_ADDR_UPDATE.val(rate(self.row.rate, 15, 5))
                + i::BANK_ADDR_UPDATE.val(rate(self.bank.rate, 31, 7))
                + i::RANK_ADDR_UPDATE.val(rate(self.rank.rate, 31, 7))
                + i::COL_ADDR_INCREMENT.val(self.col.inc.into())
                + i::ROW_ADDR_INCREMENT.val(self.row.inc.into())
                + i::BANK_ADDR_INCREMENT.val(self.bank.inc.into())
                + i::RANK_ADDR_INCREMENT.val(self.rank.inc.into()))
            .value,
        );
    }
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Profile {
    Receive,
    Mpr,
    WrLevel,
    Basic,
    Cadb,
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Stepping {
    B0,
    C0,
}
impl Stepping {
    pub fn from_cpuid(id: u32) -> Option<Self> {
        match id {
            0x306c2 | 0x40650 | 0x40660 => Some(Self::B0),
            0x306c3 | 0x40651 | 0x40661 => Some(Self::C0),
            _ => None,
        }
    }
    pub fn loop_count(self, m: MchBar, ch: usize, exp: u32) {
        if self == Self::C0 {
            m.write32(0x4980 + 4 * ch as u32, 1u32.checked_shl(exp).unwrap_or(0));
        } else {
            let off = 0x48a8 + 8 * ch as u32;
            write_qword(
                m,
                off,
                REUT_SEQ_CFG::EARLY_STEPPINGS_LOOP_COUNT
                    .val(if exp >= 32 { 0 } else { u64::from(exp + 1) })
                    .modify(read_qword(m, off)),
            );
        }
    }
}
fn cadb(m: MchBar, ch: usize, victim: u32) {
    m.write8(mc(0x41bc, ch), 0);
    for row in 0..8u64 {
        let mut word = 0;
        for bit in 0..22 {
            let pin = match bit {
                0..16 => bit,
                16..19 => bit + 8,
                _ => bit + 21,
            };
            let lfsr = if bit % 7 == victim { row } else { row >> 1 };
            word |= (lfsr & 1) << pin;
        }
        write_qword(m, mc(0x41c0, ch), word);
    }
    use REUT_PAT_CADB_MUX_CTRL as mux;
    use REUT_PAT_CL_MUX_LMN as lmn;
    m.write32(
        mc(0x41a0, ch),
        (mux::MUX_0_CTRL.val(2) + mux::MUX_1_CTRL.val(2) + mux::MUX_2_CTRL.val(2)).value,
    );
    m.write32(
        mc(0x41b0, ch),
        (lmn::EN_SWEEP_FREQ::SET
            + lmn::L_COUNTER.val(1)
            + lmn::M_COUNTER.val(1)
            + lmn::N_COUNTER.val(6))
        .value,
    );
    for (i, v) in [0xea1, 0xbeef, 0xdead].into_iter().enumerate() {
        m.write32(mc(0x41a4 + 4 * i as u32, ch), v);
    }
}
impl Profile {
    pub fn setup(self, m: MchBar, t: &Topology, step: Stepping) -> Result<(), Error> {
        let (num, lc, stop, inc, wait) = match self {
            Self::Receive => (2, 7, 9, 32, 8),
            Self::Mpr => (128, 255, 9, 4, 0),
            Self::WrLevel => (2, 4, 3, 1, 0),
            Self::Basic => (128, 15, 7, 4, 0),
            Self::Cadb => (128, 10, 9, 4, 0),
        };
        let cad = self == Self::Cadb;
        let rd = matches!(self, Self::Receive | Self::Mpr);
        let mut addr = Address {
            col: Axis {
                stop: 1023,
                inc: if cad { 53 } else { 1 },
                ..Axis::default()
            },
            ..Address::default()
        };
        if matches!(self, Self::Cadb | Self::Mpr | Self::Basic) {
            addr.rank = Axis {
                rate: 32,
                inc: 1,
                ..Axis::default()
            };
        }
        if cad {
            addr.bank = Axis {
                stop: 7,
                rate: 3,
                inc: 1,
                ..Axis::default()
            };
            addr.row = Axis {
                stop: 2047,
                rate: 3,
                inc: 73,
                ..Axis::default()
            };
        }
        let channels = t.channels().fold(0, |v, c| v | (1 << c));
        IoTest {
            commands: if rd {
                Commands::Read
            } else {
                Commands::WriteRead
            },
            cachelines: num,
            loops: lc,
            address: addr,
            stop_on_error: u32::from(cad),
            wdb_start: 0,
            wdb_stop: stop,
            wdb_rate: inc,
            en_cadb: cad,
            pattern: if cad { Pattern::Cadb } else { Pattern::Basic },
            wait,
        }
        .setup(m, t, step, channels)
    }
    pub fn run(self, m: MchBar, channels: u8) -> Result<u8, Error> {
        (if self == Self::Cadb {
            Pattern::Cadb
        } else {
            Pattern::Basic
        })
        .run(m, channels, true)
    }
}

/// CADB subsequence command stream (B = base address, O = offset address).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Commands {
    WriteRead,
    Write,
    Read,
    ReadWriteTurn,
    WriteReadTurn,
    OdtTurn,
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Pattern {
    Basic,
    Lmn,
    Cadb,
    Segment { stepping: Stepping, loop_exp: u32 },
    WriteTurn,
    OdtTurn,
    ReadTurn,
    ReadTurnAll,
}
/// General test-I/O parameters as accepted by coreboot setup_io_test().
pub struct IoTest {
    pub commands: Commands,
    pub cachelines: u32,
    pub loops: u32,
    pub address: Address,
    pub stop_on_error: u32,
    pub wdb_start: u32,
    pub wdb_stop: u32,
    pub wdb_rate: u32,
    pub pattern: Pattern,
    pub en_cadb: bool,
    pub wait: u32,
}
impl IoTest {
    pub fn setup(self, m: MchBar, t: &Topology, step: Stepping, channels: u8) -> Result<(), Error> {
        use REUT_ERR_CTL as e;
        use REUT_PAT_WDB_CL_CTRL as w;
        use REUT_PAT_WDB_CL_MUX_CFG as mux;
        use REUT_SEQ_CFG as cfg;
        use REUT_SUBSEQ_CTL as ss;
        let Self {
            commands,
            cachelines: num,
            loops: lc,
            address: addr,
            stop_on_error,
            wdb_start,
            wdb_stop: stop,
            wdb_rate: inc,
            pattern,
            en_cadb,
            wait,
        } = self;
        if channels == 0
            || channels & !3 != 0
            || num == 0
            || num > 32767
            || stop_on_error > 3
            || wdb_start > stop
            || stop >= 64
        {
            return Err(Error::Reut);
        }
        let lmn = matches!(pattern, Pattern::Cadb | Pattern::Lmn);
        let manual = matches!(commands, Commands::WriteReadTurn | Commands::OdtTurn);
        let (first, last) = match commands {
            Commands::WriteRead => (0, 1),
            Commands::Read => (1, 1),
            Commands::WriteReadTurn => (0, 7),
            Commands::OdtTurn => (0, 3),
            _ => (0, 0),
        };
        for ch in 0..2 {
            let reg = 0x48a8 + 8 * ch as u32;
            if channels & (1 << ch) == 0 {
                write_qword(
                    m,
                    reg,
                    cfg::GLOBAL_CONTROL::CLEAR.modify(read_qword(m, reg)),
                );
                continue;
            }
            m.write8(mc(0x4198, ch), en_cadb.into());
            if en_cadb {
                cadb(m, ch, 8);
            }
            write_qword(
                m,
                reg,
                (cfg::GLOBAL_CONTROL::SET
                    + cfg::INITIALIZATION_MODE.val(1)
                    + cfg::SUBSEQUENCE_START_POINTER.val(first)
                    + cfg::SUBSEQUENCE_END_POINTER.val(last)
                    + cfg::START_TEST_DELAY.val(2))
                .value,
            );
            step.loop_count(
                m,
                ch,
                if manual {
                    0
                } else {
                    lc.saturating_sub(log2ceil(num))
                },
            );
            m.write32(
                0x48b8 + 4 * ch as u32,
                REUT_SEQ_CTL::CLEAR_ERRORS::SET.value,
            );
            let subseq = |i: u32, n: u32, op: u32| {
                m.write32(
                    0x4808 + 40 * ch as u32 + 4 * i,
                    (ss::NUM_CACHELINES_WITH_SCALE.val(if n > 127 {
                        log2ceil(n)
                    } else {
                        n | 128
                    }) + ss::SUBSEQ_WAIT.val(wait)
                        + ss::RESET_CURR_BASE_ADDR_TO_START::SET
                        + ss::SUBSEQ_TYPE.val(op))
                    .value,
                )
            };
            match commands {
                Commands::WriteReadTurn => {
                    subseq(0, num, 1);
                    for i in 1..7 {
                        subseq(i, 2 * num, 2);
                    }
                    subseq(7, num, 0);
                }
                Commands::ReadWriteTurn => subseq(0, 2 * num, 3),
                Commands::OdtTurn => {
                    subseq(0, num, 1);
                    subseq(1, 2 * num, 2);
                    subseq(2, num, 0);
                    subseq(3, 2 * num, 3);
                }
                _ => {
                    subseq(0, num, 1);
                    subseq(1, num, 0);
                }
            }
            addr.program(m, ch);
            m.write32(
                mc(0x4200, ch),
                (w::START_PTR.val(wdb_start)
                    + w::END_PTR.val(stop)
                    + w::INC_RATE.val(if inc >= 32 { log2ceil(inc) } else { inc })
                    + w::INC_SCALE.val(u32::from(inc < 32)))
                .value,
            );
            m.write32(
                mc(0x4040, ch),
                (mux::RELOAD_LFSR_SEED_RATE.val(if manual { log2ceil(num) + 1 } else { 0 })
                    + mux::SAVE_LFSR_SEED_RATE.val(manual.into())
                    + mux::MUX_0_CONTROL.val(if lmn { 0 } else { 2 })
                    + mux::MUX_1_CONTROL.val(2)
                    + mux::MUX_2_CONTROL.val(2)
                    + mux::ECC_DATA_SOURCE_SEL::SET)
                    .value,
            );
            m.write32(mc(0x4084, ch), 0);
            m.write32(
                mc(0x4098, ch),
                (e::SELECTIVE_ERR_ENABLE_CACHELINE.val(255)
                    + e::SELECTIVE_ERR_ENABLE_CHUNK.val(255)
                    + e::STOP_ON_ERROR_CONTROL.val(stop_on_error)
                    + e::STOP_ON_NTH_ERROR.val(1))
                .value,
            );
            write_qword(m, mc(0x40d8, ch), 0);
            m.write8(mc(0x409c, ch), 0);
        }
        reut::zq(m, t, channels, reut::Calibration::Short)
    }
}
impl Pattern {
    pub fn run(self, m: MchBar, channels: u8, clear: bool) -> Result<u8, Error> {
        use REUT_PAT_WDB_CL_CTRL as w;
        use REUT_SEQ_ADDR_INC_CTL as inc;
        use REUT_SEQ_CFG as cfg;
        if channels == 0 || channels & !3 != 0 {
            return Err(Error::Reut);
        }
        let active = || (0..2).filter(|ch| channels & (1 << ch) != 0);
        let read_turn = matches!(self, Self::ReadTurn | Self::ReadTurnAll);
        let mut bank = [0; 2];
        if read_turn {
            for ch in active() {
                bank[ch] = m.read32(mc(0x4004, ch));
            }
        }
        let count = match self {
            Self::Segment { .. } | Self::OdtTurn => 4,
            Self::Cadb => 7,
            Self::WriteTurn | Self::ReadTurnAll => 8,
            Self::ReadTurn => 2,
            _ => 1,
        };
        let result = (|| {
            let mut errs = 0;
            for n in 0..count {
                for ch in active() {
                    match self {
                        Self::Segment { stepping, loop_exp } => {
                            let (start, end, rate, extra) = [
                                (0, 9, 25, 0),
                                (0, 9, 25, 0),
                                (10, 63, 19, 4),
                                (10, 63, 10, 2),
                            ][n as usize];
                            m.write32(
                                mc(0x4200, ch),
                                (w::START_PTR.val(start)
                                    + w::END_PTR.val(end)
                                    + w::INC_RATE.val(rate)
                                    + w::INC_SCALE::SET)
                                    .value,
                            );
                            stepping.loop_count(m, ch, loop_exp.saturating_add(extra));
                        }
                        Self::Cadb => cadb(m, ch, n),
                        Self::WriteTurn | Self::OdtTurn => {
                            let reg = 0x48a8 + 8 * ch as u32;
                            write_qword(
                                m,
                                reg,
                                (cfg::SUBSEQUENCE_START_POINTER.val(n.into())
                                    + cfg::SUBSEQUENCE_END_POINTER.val(n.into()))
                                .modify(read_qword(m, reg)),
                            );
                            let rate = 128
                                | u64::from(
                                    n != 0
                                        && !(self == Self::WriteTurn && n == 7
                                            || self == Self::OdtTurn && n == 2),
                                );
                            let reg = 0x4910 + 8 * ch as u32;
                            write_qword(
                                m,
                                reg,
                                (inc::RANK_ADDR_UPDATE.val(rate) + inc::COL_ADDR_UPDATE.val(rate))
                                    .modify(read_qword(m, reg)),
                            );
                        }
                        Self::ReadTurn | Self::ReadTurnAll => {
                            use TC_BANK_RANK_A as a;
                            let mut val = a::TRDRD_SR.val(4 + n % 4).modify(bank[ch]);
                            if self == Self::ReadTurnAll {
                                val = a::TRDRD_DR
                                    .val(a::TRDRD_DR.read(bank[ch]) + n % 4)
                                    .modify(val);
                            }
                            m.write32(mc(0x4004, ch), val);
                            if self == Self::ReadTurnAll {
                                let reg = 0x4910 + 8 * ch as u32;
                                write_qword(
                                    m,
                                    reg,
                                    inc::RANK_ADDR_UPDATE
                                        .val(128 | if n < 4 { 31 } else { 0 })
                                        .modify(read_qword(m, reg)),
                                );
                            }
                        }
                        _ => {}
                    }
                }
                let soe = active().any(|ch| {
                    REUT_ERR_CTL::STOP_ON_ERROR_CONTROL.read(m.read32(mc(0x4098, ch))) != 0
                });
                io_reset(m)?;
                m.write32(
                    0x4800,
                    (REUT_SEQ_CTL::START_TEST::SET
                        + REUT_SEQ_CTL::CLEAR_ERRORS.val(u32::from(clear && n == 0)))
                    .value,
                );
                poll(
                    || {
                        let word = m.read32(0x4804);
                        errs = REUT_GLOBAL_ERR::CH_ERROR.read(word) as u8;
                        REUT_GLOBAL_ERR::CH_TEST_DONE.read(word) as u8 & channels == channels
                    },
                    2_000_000,
                )?;
                if soe && errs & channels != 0 {
                    break;
                }
            }
            Ok(errs)
        })();
        if read_turn {
            for ch in active() {
                m.write32(mc(0x4004, ch), bank[ch]);
            }
        }
        result
    }
}
pub fn select_ranks(m: MchBar, t: &Topology, ch: usize, mask: u8) -> u8 {
    let mut n = 0;
    let mut word = 0;
    for r in t.populated_ranks(ch).filter(|&r| mask & (1 << r) != 0) {
        word |= (r as u32) << (4 * n);
        n += 1;
    }
    m.write32(0x4930 + 4 * ch as u32, word);
    let cfg = 0x48a8 + 8 * ch as u32;
    let old = read_qword(m, cfg);
    if n != 0 {
        let off = 0x48e8 + 8 * ch as u32;
        write_qword(
            m,
            off,
            REUT_SEQ_BASE_ADDR::RANK_ADDR
                .val(n - 1)
                .modify(read_qword(m, off)),
        );
    }
    write_qword(
        m,
        cfg,
        REUT_SEQ_CFG::GLOBAL_CONTROL
            .val(u64::from(n != 0))
            .modify(old),
    );
    if n == 0 { 0 } else { 1 << ch }
}
pub(super) fn rank_channels(m: MchBar, t: &Topology, r: usize) -> u8 {
    select_ranks(m, t, 0, 1 << r) | select_ranks(m, t, 1, 1 << r)
}
pub(super) fn run_mpr(m: MchBar) -> Result<(), Error> {
    io_reset(m)?;
    m.write32(0x4800, 1);
    for _ in 0..3 {
        let _ = m.read32(0x4ce0);
    }
    let result = io_reset(m);
    for _ in 0..3 {
        let _ = m.read32(0x4ce0);
    }
    m.write32(0x4800, 2);
    result
}
