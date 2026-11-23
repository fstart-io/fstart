//! JEDEC write leveling and the subsequent byte-wise whole-cycle search.
use super::{
    centering::restore_dq,
    change_margin::{Margin, Param, RegFile, Target, set_tx},
    configure_mc::{TrainingState, io},
    ddr3::ModeRegisters,
    ranges::Phase,
    spd::Topology,
    testing_io::{Profile, Stepping, rank_channels},
    timings::Clock,
    *,
};
use DDR_DATA_CONTROL_0 as dq;
use DDR_DATA_CONTROL_2 as dq2;
use DDR_DATA_TX_TRAIN_RANK as tx;
use fstart_arch::x86::udelay;

fn add_delay(m: MchBar, ch: usize, r: usize, target: i32) {
    m.clrsetbits32(
        mc(0x40d0, ch),
        3 << (2 * r),
        (target.clamp(0, 2) as u32) << (2 * r),
    );
}
fn global(v: i32) -> i32 {
    128 * (v - v.clamp(0, 2))
}
fn shift(w: u32, off: i32) -> u32 {
    (tx::DQ_DELAY.val(tx::DQ_DELAY.read(w).wrapping_add_signed(off))
        + tx::DQS_DELAY.val(tx::DQS_DELAY.read(w).wrapping_add_signed(off)))
    .modify(w)
}
fn pattern(m: MchBar, t: &Topology, invert: bool) {
    let mask = if invert {
        [1, 1, 0, 0, 0, 0, 1, 1]
    } else {
        [0, 0, 1, 1, 1, 1, 0, 0]
    };
    for (i, pair) in [[0, 255], [255, 0], [0xc3, 0x3c], [0x3c, 0xc3]]
        .iter()
        .enumerate()
    {
        wdb::fixed_pattern(m, t, pair, &mask, i as u32);
    }
}
fn cleanup(
    m: MchBar,
    t: &Topology,
    mrs: &ModeRegisters,
    r: usize,
    s: &mut TrainingState,
) -> Result<(), Error> {
    let mask = rank_channels(m, t, r);
    if mask == 0 {
        return Ok(());
    }
    let channels = || (0..2).filter(|ch| mask & (1 << ch) != 0);
    let lanes = usize::from(t.lanes);
    let dll = |ch: usize| reut::mrs(m, t, ch, 1 << r, 0, mrs.0[0][ch][r / 2] | 1 << 8);
    let mut invert = false;
    let mut offs = [[0i32; 9]; 2];
    let mut passed = [0u32; 2];
    let all = (1 << lanes) - 1;
    'coarse: for wr in [0, 1, -1, 2, 3] {
        for ch in channels() {
            for b in 0..lanes {
                set_tx(m, ch, r, b, shift(s.tx[ch][r][b], global(wr)));
            }
            add_delay(m, ch, r, wr);
        }
        io_reset(m)?;
        for ch in channels() {
            dll(ch)?;
        }
        udelay(1);
        for offset in [0, -10, 10, -5, 5, -15, 15] {
            Margin {
                param: Param::WrT,
                target: Target::All,
                rank: r,
                regfile: RegFile::Rank,
                commit: false,
            }
            .apply(m, t, s, offset)?;
            pattern(m, t, invert);
            invert = !invert;
            Profile::WrLevel.run(m, mask)?;
            for ch in channels() {
                let bad = m.read32(mc(0x40ec, ch)) | passed[ch];
                for (b, off) in offs[ch][..lanes].iter_mut().enumerate() {
                    if bad & (1 << b) == 0 {
                        *off = wr;
                        passed[ch] |= 1 << b;
                    }
                }
            }
            if channels().all(|ch| passed[ch] == all) {
                break 'coarse;
            }
        }
    }
    if channels().any(|ch| passed[ch] != all) {
        return Err(Error::WriteLevel);
    }
    for ch in channels() {
        let sum: i32 = offs[ch][..lanes].iter().sum();
        let count = i32::from(t.lanes);
        let mut tgt = (sum + sum.signum() * (count / 2)) / count;
        let mut ok = false;
        for _ in 0..4 {
            add_delay(m, ch, r, tgt);
            let mut delta = 0;
            for (b, &off) in offs[ch][..lanes].iter().enumerate() {
                let w = s.tx[ch][r][b];
                let a = tx::DQ_DELAY.read(w) as i32;
                let d = tx::DQS_DELAY.read(w) as i32;
                let pi = (off - tgt) * 128 + global(tgt);
                if a.max(d) + pi >= 448 {
                    delta = 1;
                    break;
                }
                if a.min(d) + pi < 96 {
                    delta = -1;
                    break;
                }
            }
            if delta == 0 {
                ok = true;
                break;
            }
            tgt += delta;
        }
        if !ok {
            return Err(Error::WriteLevel);
        }
        for (b, &off) in offs[ch][..lanes].iter().enumerate() {
            let w = &mut s.tx[ch][r][b];
            *w = shift(*w, (off - tgt) * 128 + global(tgt));
            set_tx(m, ch, r, b, *w);
        }
        add_delay(m, ch, r, tgt);
        dll(ch)?;
        udelay(1);
    }
    Ok(())
}

pub fn train(
    m: MchBar,
    t: &Topology,
    c: Clock,
    step: Stepping,
    ult: bool,
    mrs: &ModeRegisters,
    s: &mut TrainingState,
) -> Result<(), Error> {
    let us = (c.qclk_ps() * 1024).div_ceil(1_000_000);
    let lanes = usize::from(t.lanes);
    for ch in t.channels() {
        m.write32(
            io(0x3074, ch, 0, 0),
            (dq::READ_RF_RD::CLEAR + dq::READ_RF_WR::SET + dq::READ_RF_RANK.val(0))
                .modify(s.dq0[ch]),
        );
        for b in 0..lanes {
            m.write32(
                io(0x64, ch, 0, b),
                (dq2::FORCE_BIAS_ON::SET + dq2::FORCE_RX_ON::CLEAR + dq2::WL_LONG_DELAY::CLEAR)
                    .modify(s.dq2[ch][b]),
            );
        }
    }
    for ch in t.channels() {
        reut::mrs_all(m, t, ch, 1, mrs.0[1][ch].map(|v| v | 1 << 12))?;
    }
    let result = (|| {
        for r in 0..4 {
            let channels = || t.channels().filter(|&ch| t.rankmap[ch] & (1 << r) != 0);
            if channels().next().is_none() {
                continue;
            }
            for ch in channels() {
                let mut mr = mrs.0[1][ch][r / 2] | 1 << 7;
                if ult {
                    mr = mr & !0x244 | 1 << 6;
                } else if t.dpc[ch] == 2 {
                    mr = mr & !0x244 | 1 << 2;
                }
                reut::mrs(m, t, ch, 1 << r, 1, mr)?;
                let other = if t.dpc[ch] == 2 {
                    1 << (2 * (1 - r / 2))
                } else {
                    0
                };
                m.write32(
                    mc(0x4194, ch),
                    if ult {
                        0
                    } else {
                        (REUT_MISC_ODT_CTRL::ODT_ON.val(1 << r | other)
                            + REUT_MISC_ODT_CTRL::ODT_OVERRIDE.val(15))
                        .value
                    },
                );
            }
            for ch in channels() {
                m.write32(
                    io(0x3074, ch, 0, 0),
                    (dq::WL_TRAINING_MODE::SET
                        + dq::TX_PI_ON::SET
                        + dq::READ_RF_RD::CLEAR
                        + dq::READ_RF_WR::SET
                        + dq::READ_RF_RANK.val(r as u32))
                    .modify(s.dq0[ch]),
                );
            }
            let mut regions = [[Phase::default(); 9]; 2];
            for delay in (192..320).step_by(2) {
                for ch in channels() {
                    for b in 0..lanes {
                        set_tx(
                            m,
                            ch,
                            r,
                            b,
                            tx::DQS_DELAY.val(delay as u32).modify(s.tx[ch][r][b]),
                        );
                    }
                }
                if delay == 192 {
                    udelay(us);
                }
                io_reset(m)?;
                udelay(us);
                for ch in channels() {
                    for (b, p) in regions[ch][..lanes].iter_mut().enumerate() {
                        p.record(m.read32(io(0x54, ch, 0, b)) & 511 >= 16, delay, 192, 2);
                    }
                }
            }
            for ch in channels() {
                for p in &mut regions[ch][..lanes] {
                    p.wrap(192, 128, false);
                }
                m.write32(mc(0x4194, ch), 0);
                reut::mrs(m, t, ch, 1 << r, 1, mrs.0[1][ch][r / 2] | 1 << 12)?;
                let x16 = t.dimms[ch][r / 2]
                    .is_some_and(|d| d.width == fstart_driver_spd::ChipWidth::X16);
                for b in 0..lanes {
                    let eye = regions[ch][b].largest();
                    if !(33..96).contains(&eye.width()) {
                        return Err(Error::WriteLevel);
                    }
                    let mut start = eye.start;
                    if x16 && b % 2 == 1 {
                        let prev = regions[ch][b - 1].largest().start;
                        if start > prev + 64 {
                            start -= 128;
                        } else if start < prev - 64 {
                            start += 128;
                        }
                    }
                    if ult {
                        start += 64;
                    }
                    let w = &mut s.tx[ch][r][b];
                    *w = (tx::DQS_DELAY.val(start as u32) + tx::DQ_DELAY.val((start + 32) as u32))
                        .modify(*w);
                    set_tx(m, ch, r, b, *w);
                }
            }
        }
        Ok(())
    })();
    restore_dq(m, t, s);
    for ch in t.channels() {
        m.write32(mc(0x4194, ch), 0);
        reut::mrs_all(m, t, ch, 1, mrs.0[1][ch])?;
    }
    result?;
    wdb::program_lfsr(m, t, true);
    Profile::WrLevel.setup(m, t, step)?;
    use REUT_PAT_WDB_CL_MUX_CFG as mux;
    for ch in t.channels() {
        m.write32(
            mc(0x4040, ch),
            (mux::MUX_0_CONTROL.val(1)
                + mux::MUX_1_CONTROL.val(1)
                + mux::MUX_2_CONTROL.val(1)
                + mux::ECC_DATA_SOURCE_SEL::SET)
                .value,
        );
    }
    let result = (0..4).try_for_each(|r| cleanup(m, t, mrs, r, s));
    wdb::va_pattern(m, t, 0, 0x01010101, 8, 0);
    wdb::program_lfsr(m, t, false);
    m.write32(0x3670, 0);
    io_reset(m)?;
    result
}
