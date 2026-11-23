//! Read MPR eye search and 1D read/write timing and read voltage centering.
use super::{
    change_margin::{Margin, Param, RegFile, Target, set_rx, set_tx},
    configure_mc::{TrainingState, io},
    ranges::Linear,
    spd::Topology,
    testing_io::{Profile, Stepping, rank_channels, run_mpr},
    *,
};
use DDR_DATA_CONTROL_0 as dq;
use DDR_DATA_CONTROL_2 as dq2;
use DDR_DATA_RX_TRAIN_RANK as rx;
use DDR_DATA_TX_TRAIN_RANK as tx;
fn mask_channels(mask: u8) -> impl Iterator<Item = usize> {
    (0..2).filter(move |ch| mask & (1 << ch) != 0)
}
fn edges(v: u32, word: u32) -> u32 {
    (rx::DQS_P.val(v) + rx::DQS_N.val(v)).modify(word)
}
pub(super) fn restore_dq(m: MchBar, t: &Topology, s: &TrainingState) {
    for ch in t.channels() {
        m.write32(io(0x3074, ch, 0, 0), s.dq0[ch]);
        for b in 0..usize::from(t.lanes) {
            m.write32(io(0x64, ch, 0, b), s.dq2[ch][b]);
        }
    }
}
fn margin(p: Param, r: usize, regfile: RegFile) -> Margin {
    Margin {
        param: p,
        target: Target::All,
        rank: r,
        regfile,
        commit: false,
    }
}

pub fn train_mpr(
    m: MchBar,
    t: &Topology,
    step: Stepping,
    s: &mut TrainingState,
) -> Result<(), Error> {
    let lanes = usize::from(t.lanes);
    let mut result = Ok(());
    for ch in t.channels() {
        for r in t.populated_ranks(ch) {
            for b in 0..lanes {
                set_rx(m, ch, r, b, edges(32, s.rx[ch][r][b]));
            }
        }
    }
    s.offset = [[0; 9]; 2];
    Profile::Mpr.setup(m, t, step)?;
    for r in 0..4 {
        if (t.rankmap[0] | t.rankmap[1]) & (1 << r) == 0 {
            continue;
        }
        for ch in t.channels().filter(|&ch| t.rankmap[ch] & (1 << r) != 0) {
            reut::mrs(m, t, ch, 1 << r, 3, 4)?;
            m.setbits32(
                mc(0x4194, ch),
                REUT_MISC_ODT_CTRL::MPR_TRAIN_DDR_ON::SET.value,
            );
        }
        let mask = rank_channels(m, t, r);
        let mut regions = [[Linear::default(); 9]; 2];
        for delay in -32..32 {
            margin(Param::RdT, r, RegFile::Start).apply(m, t, s, delay)?;
            for ch in mask_channels(mask) {
                for b in 0..lanes {
                    let v =
                        (dq2::FORCE_BIAS_ON::SET + dq2::FORCE_RX_ON::SET + dq2::LEAKER_COMP.val(0))
                            .modify(s.dq2[ch][b]);
                    m.write32(io(0x64, ch, 0, b), v);
                }
                m.write32(
                    io(0x3074, ch, 0, 0),
                    (dq::RX_TRAINING_MODE::SET
                        + dq::FORCE_ODT_ON::SET
                        + dq::EN_READ_PREAMBLE::CLEAR
                        + dq::ODT_SAMP_EXTEND_EN::CLEAR)
                        .modify(s.dq0[ch]),
                );
            }
            run_mpr(m)?;
            for ch in mask_channels(mask) {
                for (b, region) in regions[ch][..lanes].iter_mut().enumerate() {
                    region.record(m.read32(io(0x54, ch, 0, b)) == 1, delay, -32, 1);
                }
            }
            for ch in mask_channels(mask) {
                for b in 0..lanes {
                    m.write32(io(0x64, ch, 0, b), s.dq2[ch][b]);
                }
                m.write32(io(0x3074, ch, 0, 0), s.dq0[ch]);
            }
            io_reset(m)?;
        }
        for ch in mask_channels(mask) {
            m.clrbits32(
                mc(0x4194, ch),
                REUT_MISC_ODT_CTRL::MPR_TRAIN_DDR_ON::SET.value,
            );
            reut::mrs(m, t, ch, 1 << r, 3, 0)?;
        }
        s.offset = [[0; 9]; 2];
        for ch in mask_channels(mask) {
            for (b, region) in regions[ch][..lanes].iter().enumerate() {
                if region.largest.width() <= 12 {
                    result = Err(Error::ReadMpr);
                }
                let word = &mut s.rx[ch][r][b];
                *word = edges((region.largest.center() + 32) as u32, *word);
            }
        }
    }
    for ch in t.channels() {
        for r in t.populated_ranks(ch) {
            for b in 0..lanes {
                set_rx(m, ch, r, b, s.rx[ch][r][b]);
            }
        }
    }
    margin(Param::RdT, 0, RegFile::Current).apply(m, t, s, 0)?;
    io_reset(m)?;
    result
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Eye {
    WriteTiming,
    ReadTiming,
    ReadVoltage,
}
/// Return left and right margins in tenths of a sweep step, indexed by
/// physical rank, channel and byte (not channel then rank).
pub type EyeMargins = [[[[u16; 2]; 9]; 2]; 4];
pub fn train_1d(
    m: MchBar,
    t: &Topology,
    step: Stepping,
    s: &mut TrainingState,
    eye: Eye,
    out: &mut EyeMargins,
) -> Result<(), Error> {
    Profile::Basic.setup(m, t, step)?;
    let mut status = Ok(());
    let lanes = usize::from(t.lanes);
    let (p, stop) = match eye {
        Eye::WriteTiming => (Param::WrT, 31),
        Eye::ReadTiming => (Param::RdT, 31),
        Eye::ReadVoltage => (Param::RdV, 54),
    };
    for (r, margins) in out.iter_mut().enumerate() {
        if (t.rankmap[0] | t.rankmap[1]) & (1 << r) == 0 {
            continue;
        }
        if eye != Eye::ReadVoltage {
            let mut perbit = margin(
                if eye == Eye::ReadTiming {
                    Param::RdTBit
                } else {
                    Param::WrTBit
                },
                r,
                RegFile::Rank,
            );
            perbit.commit = true;
            perbit.apply(
                m,
                t,
                s,
                if eye == Eye::ReadTiming {
                    0
                } else {
                    0x88888888u32 as i32
                },
            )?;
        }
        let mask = rank_channels(m, t, r);
        for ch in mask_channels(mask) {
            for b in 0..lanes {
                if eye == Eye::WriteTiming {
                    let w = &mut s.tx[ch][r][b];
                    *w = tx::DQ_DELAY
                        .val((tx::DQS_DELAY.read(*w) + 32).min(511 - stop as u32))
                        .modify(*w);
                    set_tx(m, ch, r, b, *w);
                } else {
                    let w = &mut s.rx[ch][r][b];
                    *w = if eye == Eye::ReadTiming {
                        edges(32, *w)
                    } else {
                        rx::VREF.val(0).modify(*w)
                    };
                    set_rx(m, ch, r, b, *w);
                }
            }
            m.write32(mc(0x40f0, ch), 0);
        }
        s.offset = [[0; 9]; 2];
        let mut regions = [[Linear::default(); 9]; 2];
        for v in -stop..=stop {
            margin(p, 0, RegFile::Start).apply(m, t, s, v)?;
            Profile::Basic.run(m, mask)?;
            for ch in mask_channels(mask) {
                let fail = m.read32(mc(0x40ec, ch));
                for (b, region) in regions[ch][..lanes].iter_mut().enumerate() {
                    region.record(fail & (1 << b) == 0, v, -stop, 1);
                }
            }
        }
        margin(p, 0, RegFile::Current).apply(m, t, s, 0)?;
        for ch in mask_channels(mask) {
            for (b, region) in regions[ch][..lanes].iter().enumerate() {
                let range = region.largest;
                let center = range.center();
                if range.width() < 8 {
                    status = Err(Error::Margin);
                }
                margins[ch][b] = [
                    ((center - range.start) * 10) as u16,
                    ((range.end - center) * 10) as u16,
                ];
                if eye == Eye::WriteTiming {
                    let w = &mut s.tx[ch][r][b];
                    *w = tx::DQ_DELAY
                        .val(tx::DQ_DELAY.read(*w).wrapping_add_signed(center))
                        .modify(*w);
                    set_tx(m, ch, r, b, *w);
                } else {
                    let w = &mut s.rx[ch][r][b];
                    *w = if eye == Eye::ReadTiming {
                        edges((32 + center) as u32, *w)
                    } else {
                        rx::VREF.val((center / 2) as u32).modify(*w)
                    };
                    set_rx(m, ch, r, b, *w);
                }
            }
        }
    }
    status
}
