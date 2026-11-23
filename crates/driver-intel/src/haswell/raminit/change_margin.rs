//! DDRIO train-offset and per-bit margin controls (`change_margin.c`).
use super::{
    configure_mc::{TrainingState, io},
    reut::read_qword,
    spd::Topology,
    *,
};
use DDR_DATA_CONTROL_0 as dq;

#[derive(Clone, Copy)]
pub enum Target {
    All,
    Lane(usize, usize),
}
#[derive(Clone, Copy)]
pub enum RegFile {
    Rank,
    Start,
    Current,
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Param {
    RcvEna,
    RdT,
    WrT,
    WrDqsT,
    RdV,
    WrV,
    WrLevel,
    RdTBit,
    WrTBit,
    RdVBit,
}

pub(super) fn download(
    m: MchBar,
    ch: usize,
    byte: Option<usize>,
    rank: usize,
    regfile: RegFile,
    read: bool,
    write: bool,
) {
    let mapped = |base| {
        let addr = read_qword(m, base + 8 * ch as u32);
        m.read32(0x4930 + 4 * ch as u32) >> (REUT_SEQ_BASE_ADDR::RANK_ADDR.read(addr) * 4) & 3
    };
    let phys = match regfile {
        RegFile::Rank => rank as u32,
        RegFile::Start => mapped(0x48d8),
        RegFile::Current => mapped(0x48f8),
    };
    let reg = byte.map_or(io(0x3074, ch, 0, 0), |b| io(0x74, ch, 0, b));
    m.write32(
        reg,
        (dq::READ_RF_RD.val(read.into())
            + dq::READ_RF_WR.val(write.into())
            + dq::READ_RF_RANK.val(phys))
        .modify(m.read32(reg)),
    );
}
pub(super) fn set_rx(m: MchBar, ch: usize, r: usize, b: usize, word: u32) {
    m.write32(io(0, ch, r, b), word);
    download(m, ch, Some(b), r, RegFile::Rank, true, false);
}
pub(super) fn set_tx(m: MchBar, ch: usize, r: usize, b: usize, word: u32) {
    m.write32(io(0x20, ch, r, b), word);
    download(m, ch, Some(b), r, RegFile::Rank, false, true);
}

pub fn update_vref(
    m: MchBar,
    s: &mut TrainingState,
    circuit: usize,
    delta: i32,
    commit: bool,
    wait: bool,
) -> Result<(), Error> {
    use DDR_DATA_VREF_ADJUST as va;
    let offset = (s.vref[circuit] + delta).clamp(-54, 54);
    if commit {
        s.vref[circuit] = offset;
    }
    let field = [va::VREF_DQ_CH0_CTRL, va::VREF_DQ_CH1_CTRL, va::VREF_CA_CTRL][circuit];
    let done = [va::CH0_SLOW_BW, va::CH1_SLOW_BW, va::CA_SLOW_BW][circuit];
    m.write32(0xf78, field.val(offset as u32).modify(m.read32(0xf78)));
    // CA Vref in particular settles slowly; like coreboot's
    // update_vref_and_wait, a slow circuit is reported, not fatal.
    if wait && poll(|| done.is_set(m.read32(0xf78)), 50).is_err() {
        fstart_log::warn!("Haswell: Vref circuit {} failed to converge", circuit);
    }
    Ok(())
}
fn offset_word(p: Param, base: u32, value: i32) -> u32 {
    use DDR_DATA_OFFSET_TRAIN as f;
    let v = value.clamp(-31, 31) as u32;
    match p {
        Param::RcvEna => f::RCVEN.val(v),
        Param::RdT => f::RX_DQS.val(v),
        Param::WrT => f::TX_DQ.val(v),
        Param::WrDqsT => f::TX_DQS.val(v),
        Param::RdV => f::VREF.val(value.clamp(-54, 54) as u32),
        Param::WrLevel => f::TX_DQ.val(v) + f::TX_DQS.val(v),
        _ => unreachable!(),
    }
    .modify(base)
}
#[derive(Clone, Copy)]
pub struct Margin {
    pub param: Param,
    pub target: Target,
    pub rank: usize,
    pub regfile: RegFile,
    pub commit: bool,
}
impl Margin {
    pub fn apply(
        self,
        m: MchBar,
        t: &Topology,
        s: &mut TrainingState,
        value: i32,
    ) -> Result<(), Error> {
        let Self {
            param: p,
            target,
            rank: r,
            regfile,
            commit,
        } = self;
        let (ch, b, all) = match target {
            Target::All => (0, 0, true),
            Target::Lane(ch, b) => (ch, b, false),
        };
        if r >= 4 || (!all && (ch >= 2 || t.dpc[ch] == 0 || b >= usize::from(t.lanes))) {
            return Err(Error::UnsupportedMemory);
        }
        if p == Param::WrV {
            for c in t.channels().filter(|&c| all || c == ch) {
                update_vref(m, s, c, value.clamp(-54, 54), commit, true)?;
            }
            return Ok(());
        }
        let read = matches!(
            p,
            Param::RdT | Param::RdV | Param::RcvEna | Param::RdTBit | Param::RdVBit
        );
        let (base, rank, word) = match p {
            Param::RdTBit => (0x10, r, value as u32),
            Param::WrTBit => (0x30, r, value as u32),
            Param::RdVBit => (0x4c, 0, value as u32),
            _ => (
                0x70,
                0,
                offset_word(p, if all { 0 } else { s.offset[ch][b] }, value),
            ),
        };
        m.write32(
            if all {
                io(0x3600 + base, 0, rank, 0)
            } else {
                io(base, ch, rank, b)
            },
            word,
        );
        for c in t.channels().filter(|&c| all || c == ch) {
            download(
                m,
                c,
                if all { None } else { Some(b) },
                r,
                regfile,
                read,
                !read,
            );
            if commit {
                for lane in (0..usize::from(t.lanes)).filter(|&l| all || l == b) {
                    let slot = match p {
                        Param::RdTBit => &mut s.rx_bit[c][r][lane],
                        Param::WrTBit => &mut s.tx_bit[c][r][lane],
                        Param::RdVBit => &mut s.vref_bit[c][r][lane],
                        _ => &mut s.offset[c][lane],
                    };
                    *slot = word;
                }
            }
        }
        Ok(())
    }
}
/// Equalizer and DIMM impedance sweeps from `update_opt_param_offset`.
/// A rank number selects a physical slot's MR shadow via `rank / 2`.
pub enum Adjustment {
    Restore,
    TxEq,
    RxEq,
    RxBias,
    DimmRon,
    DimmOdt,
    DimmWriteOdt,
}
pub struct Lane {
    pub channel: usize,
    pub byte: usize,
    pub rankmask: u8,
}
impl Lane {
    #[allow(clippy::too_many_arguments)]
    pub fn adjust(
        self,
        m: MchBar,
        t: &Topology,
        s: &mut TrainingState,
        mrs: &mut ddr3::ModeRegisters,
        which: Adjustment,
        value: i16,
        commit: bool,
    ) -> Result<(), Error> {
        let ch = self.channel;
        let b = self.byte;
        if ch >= 2 || b >= usize::from(t.lanes) {
            return Err(Error::UnsupportedMemory);
        }
        let ranks = || {
            t.populated_ranks(ch)
                .filter(|r| self.rankmask & (1 << r) != 0)
        };
        match which {
            Adjustment::Restore => {
                m.write32(io(0x5c, ch, 0, b), s.comp[ch][b]);
                for r in ranks() {
                    set_rx(m, ch, r, b, s.rx[ch][r][b]);
                    set_tx(m, ch, r, b, s.tx[ch][r][b]);
                }
                m.write32(io(0x60, ch, 0, b), s.dq1[ch][b]);
            }
            Adjustment::TxEq => {
                for r in ranks() {
                    let w = DDR_DATA_TX_TRAIN_RANK::TX_EQ
                        .val(value.clamp(0, 11) as u32 | 0x30)
                        .modify(s.tx[ch][r][b]);
                    set_tx(m, ch, r, b, w);
                    if commit {
                        s.tx[ch][r][b] = w;
                    }
                }
            }
            Adjustment::RxEq => {
                let x = value.clamp(0, 19) as u32;
                for r in ranks() {
                    let w = DDR_DATA_RX_TRAIN_RANK::RX_EQ
                        .val((x / 5) * 8 + x % 5)
                        .modify(s.rx[ch][r][b]);
                    set_rx(m, ch, r, b, w);
                    if commit {
                        s.rx[ch][r][b] = w;
                    }
                }
            }
            Adjustment::RxBias => {
                let w = DDR_DATA_CONTROL_1::RX_BIAS_CTL
                    .val(value.clamp(0, 7) as u32)
                    .modify(s.dq1[ch][b]);
                m.write32(io(0x60, ch, 0, b), w);
                if commit {
                    s.dq1[ch][b] = w;
                }
            }
            Adjustment::DimmRon => {
                for r in ranks() {
                    let w = (mrs.0[1][ch][r / 2] & !2) | (u16::from(value != 0) << 1);
                    reut::mrs(m, t, ch, 1 << r, 1, w)?;
                    if commit {
                        mrs.0[1][ch][r / 2] = w;
                    }
                }
            }
            Adjustment::DimmOdt | Adjustment::DimmWriteOdt => {
                let both = matches!(which, Adjustment::DimmOdt);
                let x = value as u8;
                let wr = [0, 2, 1][(if both { x >> 4 } else { x }).min(2) as usize];
                for r in ranks() {
                    let w = (mrs.0[2][ch][r / 2] & !0x600) | (wr << 9);
                    reut::mrs(m, t, ch, 1 << r, 2, w)?;
                    if commit {
                        mrs.0[2][ch][r / 2] = w;
                    }
                    if both {
                        let ohms = [0, 120, 60, 40, 30, 20][(x & 15).min(5) as usize];
                        let w = mrs.0[1][ch][r / 2] & !0x244 | ddr3::rtt_nom(ohms).unwrap();
                        reut::mrs(m, t, ch, 1 << r, 1, w)?;
                        if commit {
                            mrs.0[1][ch][r / 2] = w;
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn signed_offsets_saturate_without_spilling() {
        assert_eq!(offset_word(Param::RdV, 0x55aa, -60), (74 << 24) | 0x55aa);
        assert_eq!(
            offset_word(Param::WrLevel, 3, 100),
            (31 << 12) | (31 << 18) | 3
        );
        assert_eq!(offset_word(Param::RdT, 0, -32), 33 << 6);
    }
}
