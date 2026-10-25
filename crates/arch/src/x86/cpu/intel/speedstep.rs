//! Shared legacy Intel SpeedStep table generation for CPU MSRs and AML.
//! Coreboot's speedstep generator, without allocation during per-CPU init.

#[cfg(target_os = "none")]
use super::msr_register::Msr;
use heapless::Vec;
#[cfg(any(target_os = "none", test))]
use tock_registers::LocalRegisterCopy;
use tock_registers::register_bitfields;

register_bitfields![u64,
    Control [ VID OFFSET(0) NUMBITS(6) [], RATIO OFFSET(8) NUMBITS(5) [],
        NONINT OFFSET(14) NUMBITS(1) [], DYNFSB OFFSET(15) NUMBITS(1) [] ],
    Extended [ SLFM OFFSET(27) NUMBITS(2) [] ],
    Vcc [ TURBO_CONTROL OFFSET(32) NUMBITS(16) [], TURBO_SUPPORTED OFFSET(63) NUMBITS(1) [] ],
    Misc [ TURBO_DISABLE OFFSET(38) NUMBITS(1) [] ],
    Status [ MAX_RATIO OFFSET(40) NUMBITS(5) [] ],
    Fsb [ RATE OFFSET(0) NUMBITS(3) [] ]
];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PState {
    pub dynfsb: bool,
    pub nonint: bool,
    pub ratio: u8,
    pub vid: u8,
    pub is_turbo: bool,
    pub is_slfm: bool,
    pub power: u32,
}

impl PState {
    pub fn encoded(self) -> u32 {
        (Control::DYNFSB.val(u64::from(self.dynfsb))
            + Control::NONINT.val(u64::from(self.nonint))
            + Control::RATIO.val(u64::from(self.ratio))
            + Control::VID.val(u64::from(self.vid)))
        .value as u32
    }
    pub fn double_ratio(self) -> u32 {
        u32::from(self.ratio) * 2 + u32::from(self.nonint)
    }
}

#[derive(Clone, Copy, Default)]
pub struct Limits {
    pub slfm: PState,
    pub min: PState,
    pub max: PState,
    pub turbo: PState,
}

pub type PStates = Vec<PState, 8>;

pub fn pstates(params: Limits) -> PStates {
    let power_diff2 = params.max.power.saturating_sub(params.min.power) * 2;
    let vid_diff2 = u32::from(params.max.vid.saturating_sub(params.min.vid)) * 2;
    let ratio_diff2 = params
        .max
        .double_ratio()
        .saturating_sub(params.min.double_ratio());
    let mut step2 = 4;
    while ratio_diff2 / step2 + 1 > 5 {
        step2 += 4;
    }
    let mut normal_states = (ratio_diff2 / step2 + 1).max(2);
    let mut out = PStates::new();
    if params.turbo.is_turbo {
        out.push(params.turbo).unwrap();
    }
    let mut max = params.max;
    if (params.max.dynfsb, params.max.nonint, params.max.ratio)
        == (params.min.dynfsb, params.min.nonint, params.min.ratio)
    {
        max.vid = params.min.vid;
    }
    out.push(max).unwrap();
    normal_states -= 1;
    let power_step = power_diff2 / normal_states / 2;
    let vid_step = vid_diff2 / normal_states / 2;
    let ratio_step = step2 / 2;
    for index in (0..normal_states).rev() {
        out.push(PState {
            ratio: (u32::from(params.min.ratio) + index * ratio_step) as u8,
            vid: (u32::from(params.min.vid) + index * vid_step) as u8,
            power: params.min.power + index * power_step,
            ..PState::default()
        })
        .unwrap();
    }
    if params.slfm.is_slfm {
        out.push(params.slfm).unwrap();
    }
    out
}

/// EMTTM has six integer-ratio entries, lowest first; exclude turbo and SLFM
/// and repeat the lowest normal state to fill unused entries.
pub fn emttm_entries(states: &[PState]) -> [u32; 6] {
    let mut normal = states
        .iter()
        .rev()
        .filter(|state| !state.is_turbo && !state.is_slfm);
    let lowest = normal
        .next()
        .expect("SpeedStep always includes normal states");
    let count = states
        .iter()
        .filter(|state| !state.is_turbo && !state.is_slfm)
        .count();
    let repeats = 6usize.saturating_sub(count) + 1;
    let mut state = lowest;
    core::array::from_fn(|index| {
        if index >= repeats {
            state = normal.next().unwrap_or(state);
        }
        state.encoded() & !(1 << 14)
    })
}

#[cfg(any(target_os = "none", test))]
fn state_from_msr(value: u32, penryn: bool) -> PState {
    let value = LocalRegisterCopy::<u64, Control::Register>::new(u64::from(value));
    PState {
        nonint: penryn && value.is_set(Control::NONINT),
        ratio: value.read(Control::RATIO) as u8,
        vid: value.read(Control::VID) as u8,
        ..PState::default()
    }
}

/// Current Intel model ID without the stepping nibble (as used by coreboot).
pub fn model_id() -> u32 {
    (crate::x86::cpuid(1).0 >> 4) & 0xffff
}

/// Host table tests use synthetic limits and never execute MSR reads.
///
/// # Safety
/// Firmware callers must run on a legacy Intel CPU implementing SpeedStep MSRs.
pub unsafe fn current_pstates() -> PStates {
    #[cfg(target_os = "none")]
    let limits = unsafe {
        let id = model_id();
        let penryn = id == 0x1067;
        let mut limits = Limits::default();
        let vcc = Msr::<Vcc::Register>::new(0xce).read();
        if Msr::<Extended::Register>::new(0xee)
            .read()
            .read(Extended::SLFM)
            == 3
        {
            limits.slfm = state_from_msr(vcc.get() as u32, penryn);
            limits.slfm.dynfsb = true;
            limits.slfm.is_slfm = true;
        }
        limits.min = state_from_msr(
            Msr::<Control::Register>::new(0x19d).read().get() as u32,
            penryn,
        );
        limits.max = state_from_msr(
            Msr::<Control::Register>::new(0x17).read().get() as u32,
            penryn,
        );
        if id == 0x006e {
            limits.max.ratio = Msr::<Status::Register>::new(0x198)
                .read()
                .read(Status::MAX_RATIO) as u8;
        }
        if vcc.is_set(Vcc::TURBO_SUPPORTED)
            && !Msr::<Misc::Register>::new(0x1a0)
                .read()
                .is_set(Misc::TURBO_DISABLE)
        {
            limits.turbo = state_from_msr(vcc.read(Vcc::TURBO_CONTROL) as u32, penryn);
            limits.turbo.is_turbo = true;
        }
        limits.slfm.power = 12_000;
        limits.min.power = if penryn { 15_000 } else { 25_000 };
        limits.max.power = 35_000;
        limits.turbo.power = 35_000;
        limits
    };
    #[cfg(not(target_os = "none"))]
    let limits = Limits {
        min: PState {
            ratio: 6,
            vid: 0x20,
            power: 25_000,
            ..PState::default()
        },
        max: PState {
            ratio: 10,
            vid: 0x12,
            power: 35_000,
            ..PState::default()
        },
        ..Limits::default()
    };
    pstates(limits)
}

/// # Safety
/// Firmware callers must run on a legacy Intel CPU implementing MSR_FSB_FREQ.
pub unsafe fn fsb_x3() -> Option<u32> {
    #[cfg(target_os = "none")]
    {
        let index = unsafe { Msr::<Fsb::Register>::new(0xcd).read() }.read(Fsb::RATE) as usize;
        let fsb = match model_id() {
            0x006e | 0x106c => [
                None,
                Some(133),
                None,
                Some(166),
                None,
                Some(100),
                None,
                None,
            ][index],
            0x006f | 0x1067 => [
                Some(266),
                Some(133),
                Some(200),
                Some(166),
                Some(333),
                Some(100),
                Some(400),
                None,
            ][index],
            _ => None,
        }?;
        Some(100 * ((3 * fsb + 50) / 100))
    }
    #[cfg(not(target_os = "none"))]
    {
        Some(600)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn penryn_tables_exclude_special_modes_from_emttm() {
        let states = pstates(Limits {
            min: PState {
                ratio: 6,
                vid: 10,
                power: 15_000,
                ..PState::default()
            },
            max: PState {
                ratio: 10,
                nonint: true,
                vid: 30,
                power: 35_000,
                ..PState::default()
            },
            turbo: PState {
                ratio: 11,
                vid: 31,
                is_turbo: true,
                ..PState::default()
            },
            slfm: PState {
                ratio: 6,
                vid: 8,
                dynfsb: true,
                is_slfm: true,
                ..PState::default()
            },
        });
        assert_eq!(
            states
                .iter()
                .map(|s| s.ratio)
                .collect::<Vec<_, 8>>()
                .as_slice(),
            &[11, 10, 8, 6, 6]
        );
        assert_eq!(
            emttm_entries(&states),
            [0x60a, 0x60a, 0x60a, 0x60a, 0x814, 0xa1e]
        );
        assert_eq!(state_from_msr(0x4a1e, true).encoded(), 0x4a1e);
        assert_eq!(state_from_msr(0x4a1e, false).encoded(), 0xa1e);
    }
}
