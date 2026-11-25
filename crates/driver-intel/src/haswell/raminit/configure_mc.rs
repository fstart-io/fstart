//! Initial DDRIO impedances, PI codes, scheduler and DRAM decode. DDR3
//! (including 1.35 V DDR3L) only; ULT is not synonymous with LPDDR3.
use super::{
    Error, MchBar, mc,
    spd::Topology,
    timings::{Clock, Timings},
};
use crate::haswell::registers::*;
use fstart_driver_spd::ChipWidth;

pub(super) const fn io(base: u32, ch: usize, rank: usize, byte: usize) -> u32 {
    base + 0x100 * ch as u32 + 4 * rank as u32 + 0x200 * byte as u32
}
#[derive(Default, zerocopy::FromZeros)]
pub struct TrainingState {
    pub dq0: [u32; 2],
    pub dq1: [[u32; 9]; 2],
    pub dq2: [[u32; 9]; 2],
    pub rx: [[[u32; 9]; 4]; 2],
    pub tx: [[[u32; 9]; 4]; 2],
    pub offset: [[u32; 9]; 2],
    pub comp: [[u32; 9]; 2],
    pub comp_ctl: [u32; 2],
    pub misc: u32,
    pub vref: [i32; 3],
    pub rx_bit: [[[u32; 9]; 4]; 2],
    pub tx_bit: [[[u32; 9]; 4]; 2],
    pub vref_bit: [[[u32; 9]; 4]; 2],
}
impl TrainingState {
    const EMPTY_RANKS: [[[u32; 9]; 4]; 2] = [[[0; 9]; 4]; 2];
    pub(super) const EMPTY: Self = Self {
        dq0: [0; 2],
        dq1: [[0; 9]; 2],
        dq2: [[0; 9]; 2],
        rx: Self::EMPTY_RANKS,
        tx: Self::EMPTY_RANKS,
        offset: [[0; 9]; 2],
        comp: [[0; 9]; 2],
        comp_ctl: [0; 2],
        misc: 0,
        vref: [0; 3],
        rx_bit: Self::EMPTY_RANKS,
        tx_bit: Self::EMPTY_RANKS,
        vref_bit: Self::EMPTY_RANKS,
    };
}
/// Effective compensation divider and target resistances in ohms, in
/// coreboot `rcomp_cfg` order: read ODT, write DQ, CMD, CTL, CLK. These
/// are the effective resistances, *not* raw SM_RCOMP0/1/2 ball values.
#[derive(Clone, Copy, Debug)]
pub struct Rcomp {
    pub resistor: [u8; 5],
    pub target: [u8; 5],
}
impl Rcomp {
    pub const fn native(ult: bool, two_dpc: bool) -> Self {
        if ult {
            Self {
                resistor: [50, 40, 20, 20, 40],
                target: [50, 40, 20, 20, 29],
            }
        } else {
            Self {
                resistor: [50, 25, 20, 20, 25],
                target: [if two_dpc { 60 } else { 50 }, 33, 20, 20, 29],
            }
        }
    }
    fn codes(self) -> Result<[u32; 5], Error> {
        if self.resistor.contains(&0) || self.target.contains(&0) {
            return Err(Error::UnsupportedMemory);
        }
        Ok(core::array::from_fn(|i| {
            rcomp(self.resistor[i].into(), self.target[i].into(), i == 0)
        }))
    }
}
#[derive(Clone, Copy, Debug, Default)]
pub struct Config {
    pub ult: bool,
    pub dq_pins_interleaved: bool,
    pub double_refresh: bool,
    /// None uses the native ULT/traditional and population-dependent settings.
    pub rcomp: Option<Rcomp>,
}
fn bias(c: Clock, ult: bool, cb: u32) -> u32 {
    let f = c
        .multiplier
        .saturating_sub(if c.reference == 133 { 4 } else { 6 }) as usize;
    // Native raminit drives 1.35 V on ULT and 1.50 V on traditional.
    if ult {
        [[5, 6, 6, 5], [5, 6, 6, 5], [4, 6, 6, 6]][f.min(2)][cb as usize]
    } else {
        [
            [4, 3, 3, 2],
            [4, 4, 3, 2],
            [5, 4, 3, 3],
            [5, 4, 4, 3],
            [4, 4, 3, 3],
        ][f.min(4)][cb as usize]
    }
}
fn data(m: MchBar, t: &Topology, c: Clock, tm: Timings, ult: bool, s: &mut TrainingState) {
    use DDR_DATA_CONTROL_0 as d0;
    use DDR_DATA_CONTROL_1 as d1;
    use DDR_DATA_CONTROL_2 as d2;
    use DDR_DATA_RX_TRAIN_RANK as rx;
    use DDR_DATA_TX_TRAIN_RANK as tx;
    let rx = (rx::RCVEN.val(64) + rx::DQS_P.val(32) + rx::DQS_N.val(32) + rx::RX_EQ.val(1)).value;
    let tx = (tx::TX_EQ.val(0x3b) + tx::DQ_DELAY.val(96) + tx::DQS_DELAY.val(64)).value;
    for rank in 0..4 {
        if (t.rankmap[0] | t.rankmap[1]) & (1 << rank) == 0 {
            continue;
        }
        for (off, val) in [
            (0x3600, rx),
            (0x3610, 0),
            (0x3620, tx),
            (0x3630, 0x88888888),
        ] {
            m.write32(off + 4 * rank as u32, val);
        }
        for ch in 0..2 {
            s.rx[ch][rank].fill(rx);
            s.tx[ch][rank].fill(tx);
            s.tx_bit[ch][rank].fill(0x88888888);
        }
    }
    for (off, val) in [(0x3648, 0), (0x364c, 0x88888888), (0x3670, 0), (0x365c, 0)] {
        m.write32(off, val);
    }
    let dq0 = (d0::INTERNAL_CLOCKS_ON.val((!ult).into())
        + d0::DATA_VCCDDQ_HI.val((!ult).into())
        + d0::DISABLE_ODT_STATIC.val(ult.into()))
    .value;
    m.write32(0x3674, dq0);
    let dq1 = (d1::DLL_MASK.val(1)
        + d1::RX_BIAS_CTL.val(bias(c, ult, m.read32(0x3918) & 3))
        + d1::ODT_DELAY.val(14)
        + d1::ODT_DURATION.val(7)
        + d1::SENSE_AMP_DELAY.val(14)
        + d1::SENSE_AMP_DURATION.val(7))
    .value;
    m.write32(0x3660, dq1);
    let dq2 = if ult {
        (d2::RX_DQS_AMP_OFFSET.val(8) + d2::RX_CLK_STG_NUM.val(31)).value
    } else {
        0
    };
    for ch in t.channels() {
        s.dq0[ch] = dq0;
        s.dq1[ch].fill(dq1);
        for (b, stg) in [0, 4, 1, 5, 2, 6, 3, 7, 8]
            .iter()
            .take(t.lanes.into())
            .enumerate()
        {
            let val = dq2
                | d2::RX_STAGGER_CTL
                    .val((2 * tm.aa - 6) * stg / u32::from(t.lanes))
                    .value;
            m.write32(io(0x64, ch, 0, b), val);
            s.dq2[ch][b] = val;
        }
    }
}
fn ca(m: MchBar, t: &Topology, ult: bool) {
    use DDR_CKE_CTL_CONTROLS as ctl;
    use DDR_CLK_CONTROLS as clk;
    use DDR_CMD_CONTROLS as cmd;
    let hi = u32::from(!ult);
    let clk = (clk::DLL_MASK.val(1) + clk::VCCDDQ_HI.val(hi)).value;
    let cmd = (cmd::DLL_MASK.val(1)
        + cmd::VCCDDQ_HI.val(hi)
        + cmd::EARLY_WEAK_DRIVE.val(3)
        + cmd::CMD_TX_EQ.val(1))
    .value;
    let common = ctl::DLL_MASK.val(1)
        + ctl::VCCDDQ_HI.val(hi)
        + ctl::CTL_TX_EQ.val(1)
        + ctl::CTL_SR_DRV.val(2);
    let cke = (common + ctl::EARLY_WEAK_DRIVE.val(3) + ctl::CMD_TX_EQ.val(1)).value;
    let ctl = (common + ctl::LA_DRV_EN_OVRD::SET).value;
    let two = 64 | 64 << 7;
    let four = two | two << 14;
    for ch in t.channels() {
        for (off, val) in [
            (0x1810, clk),
            (0x320c, cmd),
            (0x121c, cke),
            (0x1c1c, ctl),
            (0x3208, two),
            (0x1208, two),
            (0x3418, four),
            (0x180c, four),
            (0x3204, 0),
            (0x1808, 0),
            (0x3414, 0),
        ] {
            m.write32(io(off, ch, 0, 0), val);
        }
    }
}
fn rcomp(r: i32, target: i32, odt: bool) -> u32 {
    let steps = if odt { 96 } else { 64 };
    let lim = if odt { 16 } else { 8 };
    ((steps * (r - target)) / (2 * (r + target))).clamp(-lim, lim - 1) as u32
}
fn slew(ps: u32, min: u32, qclk: u32) -> u32 {
    let stages = ((qclk + ps / 2) / ps).max(5);
    if ps < min || stages > 16 {
        stages / 2 - 1
    } else {
        (stages - 1) | 16
    }
}
fn vsshi_ref(qclk: u32, tgt: u32, up: bool) -> u32 {
    // ln_x100's argument is 100..271 for both supported supply voltages.
    let x = (100 * tgt / (tgt - 15)) as i32;
    let ln = ((-16 * x * x + 11578 * x - 978860) / 10000) as u32;
    let r = qclk / ln;
    let result = 128 * if up { 1000 } else { r } / (1000 + r);
    result.wrapping_sub(if up { 64 } else { 0 })
}
pub(super) fn force_comp(m: MchBar, us: u32) {
    m.write32(
        0x5f08,
        (PCU_COMP::COMP_DISABLE::SET + PCU_COMP::COMP_INTERVAL.val(10) + PCU_COMP::COMP_FORCE::SET)
            .value,
    );
    fstart_arch::x86::udelay(us);
}
fn address_decode(m: MchBar, t: &Topology, ult: bool) {
    use MAD_DIMM as d;
    m.write32(
        0x5024,
        (CHANNEL_HASH::ADDR_MASK.val(0x30ce)
            + CHANNEL_HASH::LSB_MASK_BIT.val(1)
            + CHANNEL_HASH::ENABLE::SET)
            .value,
    );
    let big = usize::from(t.channel_size_mib[0] < t.channel_size_mib[1]);
    let small = 1 - big;
    m.write32(
        0x5000,
        (MAD_CHNL::CH_A.val(big as u32) + MAD_CHNL::CH_B.val(small as u32) + MAD_CHNL::CH_C.val(2))
            .value,
    );
    let zone = t.channel_size_mib[small] / 256;
    m.write32(
        0x5014,
        (MAD_ZR::CH_B_DOUBLE.val(zone * 2) + MAD_ZR::CH_B_SINGLE.val(zone)).value,
    );
    for (ch, slots) in t.dimms.iter().enumerate() {
        let sizes = slots.map(|s| s.map_or(0, |d| d.size_mb() / 256));
        let a = usize::from(sizes[1] > sizes[0]);
        let b = 1 - a;
        let wide = |i: usize| slots[i].is_some_and(|s| s.width == ChipWidth::X16);
        let dual = |i: usize| slots[i].is_some_and(|s| s.ranks == 2);
        m.write32(
            0x5004 + 4 * ch as u32,
            (d::DIMM_A_SIZE.val(sizes[a])
                + d::DIMM_B_SIZE.val(sizes[b])
                + d::DIMM_A_SEL.val(a as u32)
                + d::DIMM_A_RANKS.val(dual(a).into())
                + d::DIMM_B_RANKS.val(dual(b).into())
                + d::DIMM_A_WIDTH.val(wide(a).into())
                + d::DIMM_B_WIDTH.val(wide(if ult { a } else { b }).into())
                + d::RANK_INTERLEAVE::SET
                + d::ENH_INTERLEAVE::SET)
                .value,
        );
    }
}
/// `enable_2x_refresh`: like coreboot, a mailbox failure or a lock with
/// other values is reported, not fatal; the DIMMs still get normal refresh.
fn double_refresh(m: MchBar, t: &Topology, tm: &mut Timings) {
    const ENABLE_AND_LOCK: u32 = 0x8000_0001;
    if t.thermal & 4 != 0 {
        // Timings are reconstructed from SPD even on a cache hit.
        tm.refi /= 2;
        return;
    }
    match m.pcode(0x17, None) {
        Ok(value) if value & (1 << 31) == 0 => {
            if m.pcode(0x18, Some(value | ENABLE_AND_LOCK)).is_err() {
                fstart_log::error!("Haswell: could not enable mailbox 2x refresh");
            }
        }
        Ok(ENABLE_AND_LOCK) => {}
        Ok(value) => fstart_log::error!(
            "Haswell: mailbox 2x refresh locked with {:#x}, not {:#x}",
            value,
            ENABLE_AND_LOCK
        ),
        Err(_) => fstart_log::error!("Haswell: PCODE mailbox timeout reading 2x refresh"),
    }
}
pub fn configure(
    m: MchBar,
    t: &Topology,
    c: Clock,
    tm: &mut Timings,
    cfg: Config,
    s: &mut TrainingState,
) -> Result<(), Error> {
    use DDR_COMP_CTL_0 as cc0;
    use DDR_COMP_CTL_1 as cc1;
    use DDR_COMP_VSSHI_CONTROL as vc;
    use DDR_DATA_VREF_ADJUST as va;
    use DDR_DATA_VREF_CONTROL as vr;
    use DDR_SCRAM_MISC_CONTROL as misc;
    let ult = cfg.ult;
    let comp = cfg
        .rcomp
        .unwrap_or(Rcomp::native(ult, t.dpc.contains(&2)))
        .codes()?;
    let hi = u32::from(!ult);
    let vdd = if ult { 1350 } else { 1500 };
    let vsshi = vdd - 950;
    m.write32(
        0x2008,
        misc::DDR_NO_CH_INTERLEAVE
            .val((ult && !cfg.dq_pins_interleaved).into())
            .value,
    );
    m.write32(0x5034, 0x01090000);
    for ch in 0..2 {
        let rank = t.rankmap[ch];
        m.write8(mc(0x42a0, ch), rank);
        for (off, val) in [
            (0x1800, rank),
            (0x1c20, rank | if ult && rank != 0 { 0x30 } else { 0 }),
            (0x1220, rank),
        ] {
            m.write32(io(off, ch, 0, 0), val.into());
        }
    }
    data(m, t, c, *tm, ult, s);
    let control = (vc::VSSHI_TARGET.val(vsshi * 192 / vdd - 20)
        + vc::HI_BW_DIVIDER.val(1)
        + vc::LO_BW_DIVIDER.val(1)
        + vc::BW_ERROR.val(2)
        + vc::PANIC_DRIVER_EN::SET
        + vc::PANIC_VOLTAGE.val(3)
        + vc::GAIN_BOOST::SET)
        .value;
    m.write32(if ult { 0x366c } else { 0x306c }, control);
    m.write32(0x3a24, control);
    // With 1 V VCCIO and 1.5 V VDD the traditional target slope is zero.
    m.write32(
        if ult { 0xf68 } else { 0xf6c },
        (vr::HI_BW_DIVIDER.val(if ult { 0 } else { 3 })
            + vr::LO_BW_DIVIDER.val(3)
            + vr::SAMPLE_DIVIDER.val(if ult { 1 } else { 3 })
            + vr::SLOW_BW_ERROR.val(1)
            + vr::HI_BW_ENABLE::SET
            + vr::VT_SLOPE_A.val(if ult { 4 } else { 0 }))
        .value,
    );
    m.write32(
        0x3678,
        (va::EN_DIMM_VREF_CA::SET
            + va::EN_DIMM_VREF_CH0::SET
            + va::EN_DIMM_VREF_CH1::SET
            + va::VCCDDQ_HI_QNNN_H.val(hi)
            + va::HI_Z_TIMER_CTRL.val(3))
        .value,
    );
    ca(m, t, ult);
    s.comp_ctl[0] = (cc0::DISABLE_ODT_STATIC.val(ult.into())
        + cc0::DQ_ODT_VREF.val(comp[0])
        + cc0::DQ_DRV_VREF.val(comp[1])
        + cc0::CMD_DRV_VREF.val(comp[2])
        + cc0::CTL_DRV_VREF.val(comp[3])
        + cc0::CLK_DRV_VREF.val(comp[4]))
    .value;
    m.write32(0x3a14, s.comp_ctl[0]);
    let q = c.qclk_ps();
    s.comp_ctl[1] = (cc1::DQ_SCOMP.val(slew(59, 46, q))
        + cc1::CMD_SCOMP.val(slew(if tm.cmd == 2 { 89 } else { 53 }, 70, q))
        + cc1::CTL_SCOMP.val(slew(53, 70, q))
        + cc1::CLK_SCOMP.val(slew(53, 46, q))
        + cc1::VCCDDQ_HI.val(hi))
    .value;
    m.write32(0x3a18, s.comp_ctl[1]);
    let down = vsshi + 24;
    m.write32(
        0x3a1c,
        (DDR_COMP_VSSHI::PANIC_DRV_DOWN_VREF.val(vsshi_ref(q, down, false))
            + DDR_COMP_VSSHI::PANIC_DRV_UP_VREF.val(vsshi_ref(q, 1000 - down, true))
            + DDR_COMP_VSSHI::VT_OFFSET.val(28)
            + DDR_COMP_VSSHI::VT_SLOPE_A.val(4))
        .value,
    );
    s.misc = (misc::WEAKLOCK_LATENCY.val(12)
        + misc::WL_SLEEP_CYCLES.val(5)
        + misc::WL_WAKE_CYCLES.val(2))
    .modify(m.read32(0x2008));
    m.write32(0x2008, s.misc);
    m.write32(0x2000, 0);
    m.write32(0x2004, 0);
    force_comp(m, 10);
    for (off, width, shift) in [
        (0x3644, 3, 28),
        (0x3700, 3, 24),
        (0x3810, 3, 24),
        (0x3904, 3, 23),
        (0x3a04, 3, 28),
        (0x3a08, 3, 24),
        (0x3a0c, 4, 24),
        (0x3a10, 4, 23),
        (0x3a20, 3, 24),
    ] {
        m.clrsetbits32(off, ((1 << width) - 1) << shift, 2 << shift);
    }
    m.write32(0x2008, s.misc | misc::FORCE_COMP_UPDATE::SET.value);
    let odt = m.read32(0x3a04);
    let diff = DDR_DATA_COMP_1::RCOMP_ODT_DOWN
        .read(odt)
        .wrapping_sub(DDR_DATA_COMP_1::RCOMP_ODT_UP.read(odt));
    s.comp_ctl[0] =
        (cc0::ODT_UP_DOWN_OFF.val(diff) + cc0::FIXED_ODT_OFFSET::SET).modify(s.comp_ctl[0]);
    m.write32(0x3a14, s.comp_ctl[0]);
    if cfg.double_refresh {
        double_refresh(m, t, tm);
    }
    m.write32(0x58a4, ult.into());
    tm.configure(m, t, c);
    tm.configure_refresh(m, t, c, ult);
    m.write32(
        0x4c20,
        (MCSCHEDS_CBIT::DIS_WRITE_GAP::SET + MCSCHEDS_CBIT::DIS_ODT.val(ult.into())).value,
    );
    m.write32(0x4f8c, 0x553c3038);
    address_decode(m, t, ult);
    Ok(())
}
