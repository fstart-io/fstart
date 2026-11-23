//! Final timings, scrambling, power policy and host-bridge lockdown.
use super::{
    configure_mc::{TrainingState, io},
    reut::write_qword,
    spd::Topology,
    testing_io::Stepping,
    timings::{Clock, Timings},
    *,
};
use DDR_DATA_CONTROL_1 as d1;
use TC_BANK_RANK_A as a;
use fstart_pci::ecam::EcamDevice;
pub(super) fn normal(m: MchBar, ch: usize) {
    write_qword(
        m,
        0x48a8 + 8 * ch as u32,
        REUT_SEQ_CFG::INITIALIZATION_MODE.val(3).value,
    );
}
pub fn normal_state(m: MchBar, t: &Topology) {
    m.write32(0x5f08, PCU_COMP::COMP_INTERVAL.val(10).value);
    for ch in t.channels() {
        normal(m, ch);
    }
    m.write32(0x4cb0, 0x6040);
}
pub fn post_training(m: MchBar, t: &Topology, tm: Timings) {
    for ch in t.channels() {
        let off = mc(0x4004, ch);
        let prev = m.read32(off);
        let old = a::CMD_STRETCH.read(prev).clamp(1, 3);
        let new = if tm.cmd == 1 { 0 } else { tm.cmd };
        m.write32(off, a::CMD_STRETCH.val(new).modify(prev));
        let off = mc(0x400c, ch);
        m.write32(off, TC_BANK_RANK_C::TXP.val(tm.xp).modify(m.read32(off)));
        let off = mc(0x4024, ch);
        let mut lat = m.read32(off).to_le_bytes();
        for r in t.populated_ranks(ch) {
            lat[r] = lat[r].wrapping_add_signed(2 * (tm.cmd as i8 - old as i8));
        }
        m.write32(off, u32::from_le_bytes(lat));
    }
}
fn seed(ch: usize) -> u32 {
    #[cfg(target_arch = "x86")]
    use core::arch::x86 as cpu;
    #[cfg(target_arch = "x86_64")]
    use core::arch::x86_64 as cpu;
    #[cfg(any(target_arch = "x86_64", target_arch = "x86"))]
    if cpu::__cpuid(1).ecx & (1 << 30) != 0 {
        for _ in 0..100000 {
            let mut x = 0; // SAFETY: RDRAND checked above.
            if unsafe { cpu::_rdrand32_step(&mut x) } != 0 && x != 0 {
                return x & 0xffff;
            }
        }
    }
    [0x893e, 0x28f4][ch]
}
fn signed4(v: u32) -> i32 {
    ((v << 28) as i32) >> 28
}
pub fn activate(
    m: MchBar,
    t: &Topology,
    c: Clock,
    tm: Timings,
    step: Stepping,
    ult: bool,
    s: &mut TrainingState,
) {
    let lanes = usize::from(t.lanes);
    for ch in t.channels() {
        let off = mc(0x4004, ch);
        if step == Stepping::B0 {
            let w = m.read32(off);
            m.write32(
                off,
                (a::TRDRD_SR.val(a::TRDRD_SR.read(w).max(6))
                    + a::TRDRD_DR.val(a::TRDRD_DR.read(w).max(6))
                    + a::TRDRD_DD.val(a::TRDRD_DD.read(w).max(6)))
                .modify(w),
            );
        }
        m.write32(
            0x2000 + 4 * ch as u32,
            (DDR_SCRAMBLE::SCRAM_KEY.val(seed(ch)) + DDR_SCRAMBLE::SCRAM_EN::SET).value,
        );
        if tm.cmd == 1 {
            let off = mc(0x4010, ch);
            m.write32(
                off,
                (MCMAIN_COMMAND_RATE_LIMIT::ENABLE_CMD_LIMIT::SET
                    + MCMAIN_COMMAND_RATE_LIMIT::CMD_RATE_LIMIT.val(3))
                .modify(m.read32(off)),
            );
        }
        m.clrbits32(off, a::CMD_3ST_DIS::SET.value);
        normal(m, ch);
        m.write8(mc(0x42a0, ch), t.rankmap[ch]);
        if t.lanes == 9 {
            m.setbits32(
                0x5004 + 4 * ch as u32,
                (MAD_DIMM::ECC_IO_ON::SET + MAD_DIMM::ECC_LOGIC_ON::SET).value,
            );
        }
    }
    if !ult {
        for ch in t.channels() {
            let keep = s.dq1[ch][..lanes].iter().any(|&w| {
                signed4(d1::ODT_DELAY.read(w)) + d1::ODT_DURATION.read(w) as i32 >= 7
                    || signed4(d1::SENSE_AMP_DELAY.read(w)) + d1::SENSE_AMP_DURATION.read(w) as i32
                        >= 7
            });
            s.dq0[ch] = DDR_DATA_CONTROL_0::INTERNAL_CLOCKS_ON
                .val(keep.into())
                .modify(s.dq0[ch]);
            m.write32(io(0x3074, ch, 0, 0), s.dq0[ch]);
        }
    }
    for ch in t.channels() {
        for b in 0..lanes {
            let max = t
                .populated_ranks(ch)
                .map(|r| {
                    let w = s.rx[ch][r][b];
                    DDR_DATA_RX_TRAIN_RANK::DQS_N
                        .read(w)
                        .max(DDR_DATA_RX_TRAIN_RANK::DQS_P.read(w))
                })
                .max()
                .unwrap_or(0);
            let w = &mut s.dq1[ch][b];
            *w = d1::SDLL_SEGMENT_DISABLE
                .val((7 - (max >> 3)) & !1)
                .modify(*w);
            m.write32(io(0x60, ch, 0, b), *w);
        }
    }
    let beod = match ((2860 + c.qclk_ps() / 2) / c.qclk_ps()).min(7) {
        0..=2 => 0,
        3 => 4,
        n => n,
    };
    for ch in t.channels() {
        for b in 0..lanes {
            let w = &mut s.dq1[ch][b];
            *w = d1::BURST_END_ODT_DELAY.val(beod).modify(*w);
            m.write32(io(0x60, ch, 0, b), *w);
        }
    }
    if ult {
        for ch in t.channels() {
            for b in 0..lanes {
                let d = s.dq1[ch][b];
                let clk = (signed4(d1::ODT_DELAY.read(d)) + d1::ODT_DURATION.read(d) as i32 + 14)
                    .max(17) as u32;
                let w = &mut s.dq2[ch][b];
                *w = DDR_DATA_CONTROL_2::RX_CLK_STG_NUM.val(clk).modify(*w);
                m.write32(io(0x64, ch, 0, b), *w);
            }
        }
    }
    m.write32(0x5f08, PCU_COMP::COMP_INTERVAL.val(10).value);
    for ch in t.channels() {
        let off = mc(0x4014, ch);
        let mut w = m.read32(off);
        if c.mhz >= 933 {
            w = TC_BANK_RANK_D::TCPDED.val(2).modify(w);
        }
        if c.mhz >= 1066 {
            w = TC_BANK_RANK_D::TPRPDEN.val(2).modify(w);
        }
        m.write32(off, w);
    }
    m.write32(0x4cb0, 0x6040);
    m.write32(0x501c, 1 << 30);
    m.write32(0x5060, 0x10200);
    fstart_arch::x86::udelay(1);
    for (off, val) in [(0x5888, 0xd0d), (0x5884, 4), (0x58e0, 0), (0x58e4, 0)] {
        m.write32(off, val);
    }
    for off in [
        0x5890, 0x5894, 0x5898, 0x589c, 0x58d0, 0x58d4, 0x58d8, 0x58dc,
    ] {
        m.write32(off, 0xffff);
    }
    for ch in 0..2 {
        for (off, val) in [
            (0x42ec, 15),
            (0x42f0, 9),
            (0x42f4, 0x93),
            (0x42f8, 0x87),
            (0x42fc, 0xde),
            (0x4328, 0x30),
        ] {
            m.write32(mc(off, ch), val);
        }
    }
    m.write32(0x5880, 0x40);
}
pub fn done(m: MchBar, host: EcamDevice) -> Result<(), Error> {
    m.setbits32(
        0x5030,
        (MC_INIT_STATE_G::REFRESH_ENABLE::SET
            + MC_INIT_STATE_G::PU_MRC_DONE::SET
            + MC_INIT_STATE_G::MRC_DONE::SET)
            .value,
    );
    m.write32(0x50fc, 0x8f);
    host.or16(0x50, 1);
    host.or32(0x5c, 1);
    host.or32(0x78, 1 << 10);
    for off in [0x90, 0x98, 0xa0, 0xa8, 0xb0, 0xb4, 0xbc] {
        host.or32(off, 1);
    }
    poll(
        || MC_INIT_STATE_G::MC_INIT_DONE_ACK.is_set(m.read32(0x5030)),
        2_000_000,
    )?;
    m.write32(0x5d10, 0x05a2404f);
    m.write32(0x5d14, 0x140000a0);
    Ok(())
}
