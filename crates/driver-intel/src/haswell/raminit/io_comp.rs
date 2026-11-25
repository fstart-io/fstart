//! Move average per-byte drive/ODT/slew offsets into global compensation,
//! keeping all individual lane codes as close to their trained values as possible.
use super::{
    configure_mc::{TrainingState, force_comp, io},
    spd::Topology,
    *,
};
use DDR_COMP_CTL_0 as c0;
use DDR_COMP_CTL_1 as c1;
use DDR_DATA_COMP_0 as d0;
use DDR_DATA_COMP_1 as d1;
use DDR_DATA_OFFSET_COMP as off;
#[derive(Clone, Copy, PartialEq, Eq)]
enum Param {
    Drive,
    Odt,
    Slew,
}
fn signed(word: u32, width: u32) -> i32 {
    ((word << (32 - width)) as i32) >> (32 - width)
}
/// DDRIO global compensation controls (data, command, control and clock).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Global {
    DqDrive,
    DqOdt,
    DqSlew,
    CmdDrive,
    CtlDrive,
    ClkDrive,
    CmdSlew,
    CtlSlew,
    ClkSlew,
    DisableStaticOdt,
}
impl From<Param> for Global {
    fn from(p: Param) -> Self {
        match p {
            Param::Odt => Self::DqOdt,
            Param::Drive => Self::DqDrive,
            Param::Slew => Self::DqSlew,
        }
    }
}
pub fn update_global(
    m: MchBar,
    t: &Topology,
    s: &mut TrainingState,
    p: Global,
    v: i32,
    commit: bool,
) -> u32 {
    let [mut a, mut b] = s.comp_ctl;
    match p {
        Global::DqDrive => a = c0::DQ_DRV_VREF.val(v as u32).modify(a),
        Global::DqOdt => {
            a = (c0::FIXED_ODT_OFFSET::CLEAR + c0::DQ_ODT_VREF.val(v as u32)).modify(a)
        }
        Global::DqSlew => b = c1::DQ_SCOMP.val(v as u32).modify(b),
        Global::CmdDrive => a = c0::CMD_DRV_VREF.val(v as u32).modify(a),
        Global::CtlDrive => a = c0::CTL_DRV_VREF.val(v as u32).modify(a),
        Global::ClkDrive => a = c0::CLK_DRV_VREF.val(v as u32).modify(a),
        Global::CmdSlew => b = c1::CMD_SCOMP.val(v as u32).modify(b),
        Global::CtlSlew => b = c1::CTL_SCOMP.val(v as u32).modify(b),
        Global::ClkSlew => b = c1::CLK_SCOMP.val(v as u32).modify(b),
        Global::DisableStaticOdt => {
            let disable = u32::from(v != 0);
            a = c0::DISABLE_ODT_STATIC.val(disable).modify(a);
            for ch in 0..2 {
                let w = DDR_DATA_CONTROL_0::DISABLE_ODT_STATIC
                    .val(disable)
                    .modify(s.dq0[ch]);
                m.write32(io(0x3074, ch, 0, 0), w);
                if commit {
                    s.dq0[ch] = w;
                }
            }
        }
    }
    m.write32(0x3a14, a);
    m.write32(0x3a18, b);
    if commit {
        s.comp_ctl = [a, b];
    }
    force_comp(m, 8);
    let data = m.read32(0x3a04);
    let up = d1::RCOMP_ODT_UP.read(data);
    if p == Global::DqOdt && !(16..=48).contains(&up) {
        let disable = u32::from(up < 16);
        a = c0::DISABLE_ODT_STATIC.val(disable).modify(a);
        for ch in t.channels() {
            s.dq0[ch] = DDR_DATA_CONTROL_0::DISABLE_ODT_STATIC
                .val(disable)
                .modify(s.dq0[ch]);
            m.write32(io(0x3074, ch, 0, 0), s.dq0[ch]);
        }
        m.write32(0x3a14, a);
        s.comp_ctl[0] = c0::DISABLE_ODT_STATIC.val(disable).modify(s.comp_ctl[0]);
        force_comp(m, 8);
    }
    match p {
        Global::DqOdt | Global::DisableStaticOdt => {
            a = (c0::ODT_UP_DOWN_OFF.val(d1::RCOMP_ODT_DOWN.read(data).wrapping_sub(up))
                + c0::FIXED_ODT_OFFSET::SET)
                .modify(a);
            m.write32(0x3a14, a);
            if commit {
                s.comp_ctl[0] = a;
            }
            up
        }
        Global::DqDrive => d0::RCOMP_DRV_UP.read(m.read32(0x3a00)),
        Global::DqSlew => d0::SLEW_RATE_COMP.read(m.read32(0x3a00)),
        Global::CmdDrive => DDR_COMP_CMD_COMP::RCOMP_DRV_UP.read(m.read32(0x3a08)),
        Global::CtlDrive => DDR_COMP_CTL_COMP::RCOMP_DRV_UP.read(m.read32(0x3a0c)),
        Global::ClkDrive => DDR_COMP_CLK_COMP::RCOMP_DRV_UP.read(m.read32(0x3a10)),
        Global::CmdSlew => DDR_COMP_CMD_COMP::SLEW_RATE_COMP.read(m.read32(0x3a08)),
        Global::CtlSlew => DDR_COMP_CTL_COMP::SLEW_RATE_COMP.read(m.read32(0x3a0c)),
        Global::ClkSlew => DDR_COMP_CLK_COMP::SLEW_RATE_COMP.read(m.read32(0x3a10)),
    }
}
fn optimise(m: MchBar, t: &Topology, s: &mut TrainingState, p: Param) {
    let (code, vref, lo, hi) = match p {
        Param::Drive => (
            d0::RCOMP_DRV_UP.read(m.read32(0x3a00)),
            signed(c0::DQ_DRV_VREF.read(s.comp_ctl[0]), 4),
            -8,
            7,
        ),
        Param::Odt => (
            d1::RCOMP_ODT_UP.read(m.read32(0x3a04)),
            signed(c0::DQ_ODT_VREF.read(s.comp_ctl[0]), 5),
            -16,
            15,
        ),
        Param::Slew => (
            d0::SLEW_RATE_COMP.read(m.read32(0x3a00)),
            (c1::DQ_SCOMP.read(s.comp_ctl[1]) & 15) as i32,
            4,
            15,
        ),
    };
    let code = code as i32;
    let lanes = usize::from(t.lanes);
    let mut saved = [[0; 9]; 2];
    let mut sum = 0;
    let mut n = 0;
    for ch in t.channels() {
        for (b, &w) in s.comp[ch][..lanes].iter().enumerate() {
            let v = match p {
                Param::Drive => signed(off::DRV_UP.read(w), 6),
                Param::Odt => signed(off::ODT_UP.read(w), 5),
                Param::Slew => signed(off::SLEW_RATE.read(w), 5),
            };
            sum += v;
            saved[ch][b] = code + v;
            n += 1;
        }
    }
    if n == 0 {
        return;
    }
    let avg = (sum + sum.signum() * n / 2) / n;
    if avg == 0 {
        return;
    }
    let dir = avg.signum() * if p == Param::Slew { -1 } else { 1 };
    let mut best = vref;
    let mut delta = avg.abs();
    let mut current = code;
    let phase = (c1::DQ_SCOMP.read(s.comp_ctl[1]) & 16) as i32;
    for i in 1.. {
        let mut candidate = vref + dir * i;
        if !(lo..=hi).contains(&candidate) || !(3..=60).contains(&current) {
            break;
        }
        if p == Param::Slew {
            candidate |= phase;
        }
        current = update_global(m, t, s, p.into(), candidate, false) as i32;
        let diff = (code + avg - current).abs();
        if diff >= avg.abs() {
            break;
        }
        if diff < delta {
            delta = diff;
            best = candidate;
            if diff == 0 {
                break;
            }
        }
    }
    current = update_global(m, t, s, p.into(), best, true) as i32;
    if best == vref {
        return;
    }
    for ch in t.channels() {
        for (b, &target) in saved[ch][..lanes].iter().enumerate() {
            let v = target - current;
            let field = match p {
                Param::Drive => LaneComp::Drive,
                Param::Odt => LaneComp::Odt,
                Param::Slew => LaneComp::Slew,
            };
            field.apply(m, s, ch, b, v, true);
        }
    }
}
/// Signed per-lane compensation deltas; TCO adjusts the clock-out leg.
pub enum LaneComp {
    Drive,
    Odt,
    Slew,
    Tco,
}
impl LaneComp {
    pub fn apply(
        self,
        m: MchBar,
        s: &mut TrainingState,
        ch: usize,
        b: usize,
        v: i32,
        commit: bool,
    ) {
        let n = v.clamp(-16, 15) as u32;
        let w = v.clamp(-32, 31) as u32;
        let field = match self {
            Self::Drive => off::DRV_UP.val(w) + off::DRV_DOWN.val(w),
            Self::Odt => off::ODT_UP.val(n) + off::ODT_DOWN.val(n),
            Self::Slew => off::SLEW_RATE.val(n),
            Self::Tco => off::T_CLK_OUT.val(n),
        };
        let val = field.modify(s.comp[ch][b]);
        m.write32(io(0x5c, ch, 0, b), val);
        if commit {
            s.comp[ch][b] = val;
        }
        m.write32(
            0x2008,
            DDR_SCRAM_MISC_CONTROL::FORCE_COMP_UPDATE::SET.modify(s.misc),
        );
    }
}
pub fn optimise_comp(m: MchBar, t: &Topology, s: &mut TrainingState) {
    for p in [Param::Drive, Param::Odt, Param::Slew] {
        optimise(m, t, s, p);
    }
}
