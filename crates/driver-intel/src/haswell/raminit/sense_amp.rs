//! Sense amplifier calibration: select lane Vref, then center each DQ's
//! four-bit offset at the last-one/first-zero crossover.
use super::{
    change_margin::{Margin, Param, RegFile, Target, download, set_rx},
    configure_mc::{TrainingState, io},
    spd::Topology,
    *,
};
use DDR_DATA_CONTROL_0 as dq;

fn propagate(m: MchBar, s: &TrainingState, ch: usize) {
    let v = (dq::READ_RF_RD::SET
        + dq::READ_RF_WR::CLEAR
        + dq::READ_RF_RANK.val(0)
        + dq::FORCE_ODT_ON::SET
        + dq::SAMP_TRAIN_MODE::SET)
        .modify(s.dq0[ch]);
    m.write32(io(0x3074, ch, 0, 0), v);
    fstart_arch::x86::udelay(1);
    m.write32(io(0x3074, ch, 0, 0), dq::SAMP_TRAIN_MODE::CLEAR.modify(v));
}
#[derive(Clone, Copy)]
struct Eye {
    high: u8,
    low: u8,
    sum: i8,
    best: i8,
    left: i8,
    right: i8,
}
fn vref(m: MchBar, t: &Topology, s: &mut TrainingState) -> Result<(), Error> {
    let mut eye = [[Eye {
        high: 0,
        low: 255,
        sum: 0,
        best: -9,
        left: 0,
        right: 0,
    }; 9]; 2];
    let lanes = usize::from(t.lanes);
    for ch in t.channels() {
        for b in 0..lanes {
            let f = DDR_DATA_CONTROL_2::FORCE_BIAS_ON::SET + DDR_DATA_CONTROL_2::FORCE_RX_ON::SET;
            m.write32(io(0x64, ch, 0, b), f.modify(s.dq2[ch][b]));
        }
    }
    let margin = Margin {
        param: Param::RdV,
        target: Target::All,
        rank: 0,
        regfile: RegFile::Rank,
        commit: false,
    };
    for v in -15..=15 {
        margin.apply(m, t, s, v.into())?;
        for (positive, pattern) in [(true, 0xffffffff), (false, 0x11111111)] {
            m.write32(0x364c, pattern);
            for ch in t.channels() {
                propagate(m, s, ch);
                for (b, e) in eye[ch][..lanes].iter_mut().enumerate() {
                    let fb = m.read32(io(0x54, ch, 0, b)) as u8;
                    if positive {
                        e.low &= fb;
                        e.sum = -(fb.count_ones() as i8);
                    } else {
                        e.high |= fb;
                        e.sum += fb.count_ones() as i8;
                        if e.sum > e.best {
                            e.best = e.sum;
                            e.left = v;
                            e.right = v;
                        } else if e.sum == e.best {
                            e.right = v;
                        }
                    }
                }
            }
        }
    }
    margin.apply(m, t, s, 0)?;
    let mut result = Ok(());
    for ch in t.channels() {
        for (b, e) in eye[ch][..lanes].iter().enumerate() {
            if e.high ^ e.low != 0xff {
                result = Err(Error::SenseAmp);
            }
            let center = e.left + (e.right - e.left) / 2;
            let v = (center + if center < 0 { -1 } else { 1 }) / 2;
            for r in t.populated_ranks(ch) {
                let word = &mut s.rx[ch][r][b];
                *word = DDR_DATA_RX_TRAIN_RANK::VREF.val(v as u32).modify(*word);
                set_rx(m, ch, r, b, *word);
            }
        }
    }
    result
}
fn bit_center((zero, one): (u8, u8)) -> u32 {
    match (zero, one) {
        (_, 0) => 0,
        (0, _) => 15,
        (z, o) => u32::from((z + o) / 2),
    }
}
fn pack(bits: [(u8, u8); 8]) -> u32 {
    bits.into_iter()
        .enumerate()
        .fold(0, |w, (bit, pair)| w | bit_center(pair) << (4 * bit))
}
fn samp(m: MchBar, t: &Topology, s: &mut TrainingState) {
    let mut crossings = [[[(0, 0); 8]; 9]; 2];
    let lanes = usize::from(t.lanes);
    for step in 1..=15 {
        m.write32(0x364c, 0x11111111 * u32::from(step));
        for ch in t.channels() {
            propagate(m, s, ch);
            for (b, bits) in crossings[ch][..lanes].iter_mut().enumerate() {
                let fb = m.read32(io(0x54, ch, 0, b));
                for (bit, (zero, one)) in bits.iter_mut().enumerate() {
                    if fb & (1 << bit) != 0 {
                        *one = step;
                    } else if *zero == 0 {
                        *zero = step;
                    }
                }
            }
        }
    }
    for ch in t.channels() {
        for (b, &bits) in crossings[ch][..lanes].iter().enumerate() {
            let word = pack(bits);
            s.vref_bit[ch][0][b] = word;
            m.write32(io(0x4c, ch, 0, b), word);
            download(m, ch, None, 0, RegFile::Rank, true, false);
        }
    }
}
pub fn train(m: MchBar, t: &Topology, s: &mut TrainingState) -> Result<(), Error> {
    let result = vref(m, t, s);
    samp(m, t, s);
    for ch in t.channels() {
        m.write32(io(0x3074, ch, 0, 0), s.dq0[ch]);
        for b in 0..usize::from(t.lanes) {
            m.write32(io(0x64, ch, 0, b), s.dq2[ch][b]);
        }
    }
    io_reset(m)?;
    result
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn packs_all_eight_offsets_including_saturation() {
        assert_eq!(
            pack([
                (1, 0),
                (0, 15),
                (3, 4),
                (4, 5),
                (5, 6),
                (6, 7),
                (7, 8),
                (8, 9)
            ]),
            0x876543f0
        );
    }
}
