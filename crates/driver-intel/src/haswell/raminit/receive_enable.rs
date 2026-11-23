//! Receive-enable training: center DQS high, walk back into preamble,
//! find its rising edge, then distribute delay between IO latency and PI.
use super::{
    centering::restore_dq,
    change_margin::set_rx,
    configure_mc::{TrainingState, io},
    ranges::Phase,
    spd::Topology,
    testing_io::{Profile, Stepping, rank_channels},
    timings::Clock,
    *,
};
use DDR_DATA_RX_TRAIN_RANK::RCVEN;
fn rcven(
    m: MchBar,
    s: &mut TrainingState,
    ch: usize,
    r: usize,
    b: usize,
    value: i32,
) -> Result<(), Error> {
    if !(0..512).contains(&value) {
        return Err(Error::ReceiveEnable);
    }
    let w = &mut s.rx[ch][r][b];
    *w = RCVEN.val(value as u32).modify(*w);
    set_rx(m, ch, r, b, *w);
    Ok(())
}
fn high(m: MchBar, ch: usize, b: usize) -> bool {
    m.read32(io(0x54, ch, 0, b)) & 511 >= 32
}
fn sync(
    m: MchBar,
    t: &Topology,
    s: &mut TrainingState,
    ch: usize,
    lat: [i32; 4],
) -> Result<(), Error> {
    let lanes = usize::from(t.lanes);
    let mut lo = [4096; 4];
    let mut hi = [-4096; 4];
    for r in t.populated_ranks(ch) {
        for &w in &s.rx[ch][r][..lanes] {
            let x = RCVEN.read(w) as i32 - 64 * lat[r];
            lo[r] = lo[r].min(x);
            hi[r] = hi[r].max(x);
        }
    }
    let center = (lo.iter().min().unwrap() + hi.iter().max().unwrap()) / 2;
    let delta = 320 - center;
    let target = (delta + delta.signum() * 32) / 64;
    let mut global = 0;
    let mut io_lat = [0; 4];
    for r in t.populated_ranks(ch) {
        let mut l = target;
        let lower = 64 * l + lo[r];
        if lower < 64 {
            l += (64 - lower + 63) / 64;
        }
        let upper = 64 * l + hi[r];
        if upper > 447 {
            l -= (upper - 447 + 63) / 64;
        }
        global = global.clamp(l - 14, l - 1);
        io_lat[r] = l;
        for b in 0..lanes {
            rcven(
                m,
                s,
                ch,
                r,
                b,
                RCVEN.read(s.rx[ch][r][b]) as i32 + 64 * (l - lat[r]),
            )?;
        }
    }
    let off = mc(0x4028, ch);
    let old = m.read32(off);
    let comp = SC_IO_LATENCY::RT_IOCOMP.read(old) as i32 + global;
    if !(0..64).contains(&comp) {
        return Err(Error::ReceiveEnable);
    }
    let mut val = SC_IO_LATENCY::RT_IOCOMP.val(comp as u32).modify(old) & !0xffff;
    for r in t.populated_ranks(ch) {
        let l = io_lat[r] - global;
        if !(0..16).contains(&l) {
            return Err(Error::ReceiveEnable);
        }
        val |= (l as u32) << (4 * r);
    }
    m.write32(off, val);
    Ok(())
}
/// Initial round-trip delta matching coreboot `get_rt_delta()` (`8859e389cf8`):
/// Haswell ULT uses 2, Crystalwell (`0x4066x`) uses 4, Haswell Trad uses 0.
pub const fn rt_delta(cpuid: u32) -> u32 {
    match cpuid & 0xffff_fff0 {
        0x40650 => 2,
        0x40660 => 4,
        _ => 0,
    }
}

pub fn train(
    m: MchBar,
    t: &Topology,
    c: Clock,
    step: Stepping,
    cpuid: u32,
    s: &mut TrainingState,
) -> Result<(), Error> {
    let lanes = usize::from(t.lanes);
    let all = (1u16 << lanes) - 1;
    let delta = rt_delta(cpuid);
    let mut lat = [[0i32; 4]; 2];
    Profile::Receive.setup(m, t, step)?;
    for ch in t.channels() {
        for b in 0..lanes {
            m.write32(
                io(0x64, ch, 0, b),
                DDR_DATA_CONTROL_2::FORCE_RX_ON::SET.modify(s.dq2[ch][b]),
            );
        }
        m.write32(
            io(0x3074, ch, 0, 0),
            (DDR_DATA_CONTROL_0::FORCE_ODT_ON::SET + DDR_DATA_CONTROL_0::RL_TRAINING_MODE::SET)
                .modify(s.dq0[ch]),
        );
        m.write32(
            mc(0x4028, ch),
            SC_IO_LATENCY::RT_IOCOMP.val(21 + delta).value,
        );
    }
    let result = (|| {
        for r in 0..4 {
            if (t.rankmap[0] | t.rankmap[1]) & (1 << r) == 0 {
                continue;
            }
            let mask = rank_channels(m, t, r);
            let channels = || (0..2).filter(|ch| mask & (1 << ch) != 0);
            let rt = (16 + delta + c.multiplier.max(4) + 5 + 2 * c.cas).min(63) as u8;
            for ch in channels() {
                m.write8(mc(0x4024 + r as u32, ch), rt);
            }
            let mut regions = [[Phase::default(); 9]; 2];
            for delay in (280..408).step_by(8) {
                for ch in channels() {
                    for b in 0..lanes {
                        set_rx(m, ch, r, b, RCVEN.val(delay as u32).modify(s.rx[ch][r][b]));
                    }
                }
                Profile::Receive.run(m, mask)?;
                for ch in channels() {
                    for (b, p) in regions[ch][..lanes].iter_mut().enumerate() {
                        p.record(high(m, ch, b), delay, 280, 8);
                    }
                }
            }
            for ch in channels() {
                for (b, p) in regions[ch][..lanes].iter_mut().enumerate() {
                    p.wrap(280, 128, true);
                    let eye = p.largest();
                    if !(33..96).contains(&eye.width()) || eye.center() > 408 {
                        return Err(Error::ReceiveEnable);
                    }
                    rcven(m, s, ch, r, b, eye.center())?;
                }
            }
            loop {
                Profile::Receive.run(m, mask)?;
                let mut done = true;
                for ch in channels() {
                    let bits =
                        (0..lanes).fold(0, |mask, b| mask | (u16::from(high(m, ch, b)) << b));
                    if bits == 0 {
                        continue;
                    }
                    done = false;
                    if bits == all && lat[ch][r] < 14 {
                        lat[ch][r] += 2;
                        m.clrsetbits32(
                            mc(0x4028, ch),
                            15 << (4 * r),
                            (lat[ch][r] as u32) << (4 * r),
                        );
                    } else {
                        for b in (0..lanes).filter(|b| bits & (1 << b) != 0) {
                            rcven(m, s, ch, r, b, RCVEN.read(s.rx[ch][r][b]) as i32 - 128)?;
                        }
                    }
                }
                if done {
                    break;
                }
            }
            for ch in channels() {
                for b in 0..lanes {
                    rcven(m, s, ch, r, b, RCVEN.read(s.rx[ch][r][b]) as i32 + 64)?;
                }
            }
            let mut passed = [0u16; 2];
            for _ in 0..64 {
                Profile::Receive.run(m, mask)?;
                for ch in channels() {
                    for b in 0..lanes {
                        if passed[ch] & (1 << b) != 0 {
                            continue;
                        }
                        if high(m, ch, b) {
                            passed[ch] |= 1 << b;
                        } else {
                            rcven(m, s, ch, r, b, RCVEN.read(s.rx[ch][r][b]) as i32 + 1)?;
                        }
                    }
                }
                if channels().all(|ch| passed[ch] == all) {
                    break;
                }
            }
            if channels().any(|ch| passed[ch] != all) {
                return Err(Error::ReceiveEnable);
            }
            for ch in channels() {
                for b in 0..lanes {
                    rcven(m, s, ch, r, b, RCVEN.read(s.rx[ch][r][b]) as i32 - 64)?;
                }
            }
        }
        Ok(())
    })();
    restore_dq(m, t, s);
    io_reset(m)?;
    result?;
    for ch in t.channels() {
        sync(m, t, s, ch, lat[ch])?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::rt_delta;

    #[test]
    fn rt_delta_matches_coreboot() {
        assert_eq!(rt_delta(0x306c2), 0);
        assert_eq!(rt_delta(0x306c3), 0);
        assert_eq!(rt_delta(0x40650), 2);
        assert_eq!(rt_delta(0x40651), 2);
        assert_eq!(rt_delta(0x40660), 4);
        assert_eq!(rt_delta(0x40661), 4);
    }
}
