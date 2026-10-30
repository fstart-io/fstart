//! Intel Core/Core 2 processor power-management AML helpers.
//!
//! Mirrors coreboot's legacy Intel SpeedStep ACPI generator for Merom/Penryn
//! systems used with GM965/GM45/i945-era northbridges: `_PCT`, `_PSD`, `_PSS`,
//! and MWAIT `_CST` under ACPI0007 CPU devices, encoded by
//! [`fstart_acpi::processor`].

// The MSR probing only runs on the target.
#![cfg_attr(not(target_os = "none"), allow(dead_code))]

extern crate alloc;

use alloc::vec::Vec;
use fstart_acpi::Aml;
use fstart_acpi::aml::{Arg, Method, Name, Notify, PackageBuilder, Path};
use fstart_acpi::gas::{AccessSize, AddressSpace, GAS};
use fstart_acpi::processor::{
    Coordination, Dependency, PerformanceState, PowerObjects, PowerState, ProcessorName,
    processor_device,
};

const MSR_THERM2_CTL: u32 = 0x19d;
const MSR_FSB_FREQ: u32 = 0xcd;
const MSR_FSB_CLOCK_VCC: u32 = 0xce;
const MSR_EXTENDED_CONFIG: u32 = 0xee;
const IA32_PLATFORM_ID: u32 = 0x17;
const IA32_PERF_STATUS: u32 = 0x198;
const IA32_MISC_ENABLE: u32 = 0x1a0;

const SPEEDSTEP_RATIO_SHIFT: u32 = 8;
const SPEEDSTEP_RATIO_DYNFSB_SHIFT: u32 = 15;
const SPEEDSTEP_RATIO_NONINT_SHIFT: u32 = 14;
const SPEEDSTEP_RATIO_NONINT: u32 = 1 << SPEEDSTEP_RATIO_NONINT_SHIFT;
const SPEEDSTEP_RATIO_VALUE_MASK: u32 = 0x1f << SPEEDSTEP_RATIO_SHIFT;
const SPEEDSTEP_VID_MASK: u32 = 0x3f;
const SPEEDSTEP_MAX_NORMAL_STATES: usize = 5;

const SPEEDSTEP_MAX_POWER_MEROM: u32 = 35_000;
const SPEEDSTEP_MIN_POWER_MEROM: u32 = 25_000;
const SPEEDSTEP_SLFM_POWER_MEROM: u32 = 12_000;
const SPEEDSTEP_MAX_POWER_PENRYN: u32 = 35_000;
const SPEEDSTEP_MIN_POWER_PENRYN: u32 = 15_000;
const SPEEDSTEP_SLFM_POWER_PENRYN: u32 = 12_000;

#[derive(Clone, Copy, Default)]
struct SpeedstepState {
    dynfsb: bool,
    nonint: bool,
    ratio: u8,
    vid: u8,
    is_turbo: bool,
    is_slfm: bool,
    power: u32,
}

impl SpeedstepState {
    fn encoded(self) -> u32 {
        ((self.dynfsb as u32) << SPEEDSTEP_RATIO_DYNFSB_SHIFT)
            | ((self.nonint as u32) << SPEEDSTEP_RATIO_NONINT_SHIFT)
            | ((self.ratio as u32) << SPEEDSTEP_RATIO_SHIFT)
            | ((self.vid as u32) & SPEEDSTEP_VID_MASK)
    }

    fn double_ratio(self) -> u32 {
        (self.ratio as u32 * 2) + self.nonint as u32
    }
}

#[derive(Clone, Copy, Default)]
struct SpeedstepParams {
    slfm: SpeedstepState,
    min: SpeedstepState,
    max: SpeedstepState,
    turbo: SpeedstepState,
}

/// Generate Core/Core 2 (GM965-era) CPU power-management devices for the DSDT.
///
/// Uses the same generic SpeedStep MSR probing as coreboot's
/// `cpu/intel/speedstep` code and MWAIT C1/C2/C3 entries matching the Core 2
/// mobile boards in coreboot.
#[cfg(any(feature = "i945", feature = "gm965", test))]
pub fn cpu_devices_aml(logical_cpus: usize) -> Vec<u8> {
    // MWAIT hints 0x00/0x10/0x20 through FFixedHW class 1, bit offset 2.
    let mwait = |kind, latency_us, milliwatts, hint| PowerState {
        register: GAS::new(
            AddressSpace::FunctionalFixedHardware,
            1,
            2,
            AccessSize::ByteAccess,
            hint,
        ),
        kind,
        latency_us,
        milliwatts,
    };
    let cstates = [
        mwait(1, 1, 1000, 0x00),
        mwait(2, 1, 500, 0x10),
        mwait(3, 17, 250, 0x20),
    ];
    let coordination = if cpu_model_id() == 0x1067 {
        Coordination::HwAll
    } else {
        Coordination::SwAny
    };
    build_cpu_devices_aml(logical_cpus, &cstates, coordination)
}

/// Generate the Pineview CPU namespace (SpeedStep without board C-states).
#[cfg(any(feature = "pineview", test))]
pub(crate) fn pineview_cpu_devices_aml(logical_cpus: usize) -> Vec<u8> {
    build_cpu_devices_aml(logical_cpus, &[], Coordination::SwAny)
}

fn build_cpu_devices_aml(
    logical_cpus: usize,
    cstates: &[PowerState],
    coordination: Coordination,
) -> Vec<u8> {
    let fsb3 = ia32_fsb_x3().unwrap_or(600);
    let states = speedstep_pstates();
    let pstates: Vec<PerformanceState> = states
        .iter()
        .enumerate()
        .map(|(i, state)| {
            let mhz = if state.is_turbo && i + 1 < states.len() {
                states[i + 1].double_ratio() * fsb3 / 6 + 1
            } else if state.is_slfm {
                state.double_ratio() * fsb3 / 12
            } else {
                state.double_ratio() * fsb3 / 6
            };
            PerformanceState {
                mhz,
                milliwatts: state.power,
                latency_us: 0,
                bus_master_latency_us: 0,
                control: state.encoded(),
                status: state.encoded(),
            }
        })
        .collect();
    // coreboot's acpigen_write_empty_PCT(): the OS uses the architectural
    // IA32_PERF_CTL/STATUS.
    let ffh = GAS::new(
        AddressSpace::FunctionalFixedHardware,
        0,
        0,
        AccessSize::Undefined,
        0,
    );
    let power = PowerObjects {
        control: ffh,
        status: ffh,
        pstates: &pstates,
        ppc: Some(0),
        dependency: Some(Dependency {
            domain: 0,
            coordination,
            processors: logical_cpus as u32,
        }),
        cstates,
    }
    .aml();

    let names: Vec<ProcessorName> = (0..logical_cpus)
        .map(|cpu| ProcessorName::new(u16::try_from(cpu).expect("too many CPUs")))
        .collect();
    let mut out = Vec::new();
    for (uid, name) in names.iter().enumerate() {
        out.extend(
            processor_device(*name, uid as u32, &power)
                .expect("required CPU AML device exceeds length limit"),
        );
    }
    // PPKG: the processors; CNOT(n): notify each of them.
    let paths: Vec<Path> = names.iter().map(|name| Path::new(name.as_str())).collect();
    let mut package = PackageBuilder::new();
    paths.iter().for_each(|path| package.add_element(path));
    Name::new("PPKG".into(), &package).to_aml_bytes(&mut out);
    let notifies: Vec<Notify> = paths
        .iter()
        .map(|path| Notify::new(path, &Arg(0)))
        .collect();
    Method::new(
        "CNOT".into(),
        1,
        false,
        notifies.iter().map(|n| n as &dyn Aml).collect(),
    )
    .to_aml_bytes(&mut out);
    out
}

fn speedstep_pstates() -> Vec<SpeedstepState> {
    let params = speedstep_limits();
    let power_diff2 = params.max.power.saturating_sub(params.min.power) * 2;
    let vid_diff2 = (params.max.vid.saturating_sub(params.min.vid) as u32) * 2;
    let max_ratio2 = params.max.double_ratio();
    let min_ratio2 = params.min.double_ratio();
    let ratio_diff2 = max_ratio2.saturating_sub(min_ratio2);

    let mut step2 = 0u32;
    let mut normal_states;
    loop {
        step2 += 4;
        normal_states = ratio_diff2 / step2 + 1;
        if normal_states as usize <= SPEEDSTEP_MAX_NORMAL_STATES {
            break;
        }
    }
    if normal_states < 2 {
        normal_states = 2;
    }

    let mut out = Vec::new();
    if params.turbo.is_turbo {
        out.push(params.turbo);
    }

    let mut max = params.max;
    if params.max.dynfsb == params.min.dynfsb
        && params.max.nonint == params.min.nonint
        && params.max.ratio == params.min.ratio
    {
        max.vid = params.min.vid;
    }
    out.push(max);
    normal_states -= 1;

    let power_step = (power_diff2 / normal_states) / 2;
    let vid_step = (vid_diff2 / normal_states) / 2;
    let ratio_step = step2 / 2;
    let mut power = params.min.power + (normal_states - 1) * power_step;
    let mut vid = params.min.vid as u32 + (normal_states - 1) * vid_step;
    let mut ratio = params.min.ratio as u32 + (normal_states - 1) * ratio_step;

    while normal_states > 0 {
        out.push(SpeedstepState {
            ratio: ratio as u8,
            vid: vid as u8,
            power,
            ..SpeedstepState::default()
        });
        power = power.saturating_sub(power_step);
        vid = vid.saturating_sub(vid_step);
        ratio = ratio.saturating_sub(ratio_step);
        normal_states -= 1;
    }

    if params.slfm.is_slfm {
        out.push(params.slfm);
    }
    out
}

fn speedstep_limits() -> SpeedstepParams {
    #[cfg(target_os = "none")]
    unsafe {
        let cpu_id = cpu_model_id();
        let state_mask = if cpu_id == 0x1067 {
            SPEEDSTEP_RATIO_NONINT
        } else {
            0
        } | SPEEDSTEP_RATIO_VALUE_MASK
            | SPEEDSTEP_VID_MASK;
        let mut params = SpeedstepParams::default();

        if ((fstart_arch::x86::msr::rdmsr(MSR_EXTENDED_CONFIG) >> 27) & 3) == 3 {
            let lo = fstart_arch::x86::msr::rdmsr(MSR_FSB_CLOCK_VCC) as u32;
            params.slfm = state_from_msr(lo, state_mask);
            params.slfm.dynfsb = true;
            params.slfm.is_slfm = true;
        }

        params.min = state_from_msr(
            fstart_arch::x86::msr::rdmsr(MSR_THERM2_CTL) as u32,
            state_mask,
        );
        params.max = state_from_msr(
            fstart_arch::x86::msr::rdmsr(IA32_PLATFORM_ID) as u32,
            state_mask,
        );
        if cpu_id == 0x006e {
            params.max.ratio =
                ((fstart_arch::x86::msr::rdmsr(IA32_PERF_STATUS) >> 40) & 0x1f) as u8;
        }

        let fsb_clock_vcc = fstart_arch::x86::msr::rdmsr(MSR_FSB_CLOCK_VCC);
        let misc = fstart_arch::x86::msr::rdmsr(IA32_MISC_ENABLE);
        if (fsb_clock_vcc & (1u64 << 63)) != 0 && (misc & (1u64 << 38)) == 0 {
            params.turbo = state_from_msr((fsb_clock_vcc >> 32) as u32, state_mask);
            params.turbo.is_turbo = true;
        }

        match cpu_id {
            0x1067 => {
                params.slfm.power = SPEEDSTEP_SLFM_POWER_PENRYN;
                params.min.power = SPEEDSTEP_MIN_POWER_PENRYN;
                params.max.power = SPEEDSTEP_MAX_POWER_PENRYN;
                params.turbo.power = SPEEDSTEP_MAX_POWER_PENRYN;
            }
            _ => {
                params.slfm.power = SPEEDSTEP_SLFM_POWER_MEROM;
                params.min.power = SPEEDSTEP_MIN_POWER_MEROM;
                params.max.power = SPEEDSTEP_MAX_POWER_MEROM;
                params.turbo.power = SPEEDSTEP_MAX_POWER_MEROM;
            }
        }
        return params;
    }

    #[cfg(not(target_os = "none"))]
    {
        let _ = state_from_msr(0, 0);
        SpeedstepParams {
            min: SpeedstepState {
                ratio: 6,
                vid: 0x20,
                power: SPEEDSTEP_MIN_POWER_MEROM,
                ..SpeedstepState::default()
            },
            max: SpeedstepState {
                ratio: 10,
                vid: 0x12,
                power: SPEEDSTEP_MAX_POWER_MEROM,
                ..SpeedstepState::default()
            },
            ..SpeedstepParams::default()
        }
    }
}

fn state_from_msr(value: u32, mask: u32) -> SpeedstepState {
    SpeedstepState {
        nonint: ((value & mask) & SPEEDSTEP_RATIO_NONINT) != 0,
        ratio: (((value & mask) & SPEEDSTEP_RATIO_VALUE_MASK) >> SPEEDSTEP_RATIO_SHIFT) as u8,
        vid: ((value & mask) & SPEEDSTEP_VID_MASK) as u8,
        ..SpeedstepState::default()
    }
}

fn ia32_fsb_x3() -> Option<u32> {
    #[cfg(target_os = "none")]
    unsafe {
        let cpu_id = cpu_model_id();
        let idx = (fstart_arch::x86::msr::rdmsr(MSR_FSB_FREQ) & 7) as usize;
        let fsb = match cpu_id {
            0x006e | 0x106c => [
                None,
                Some(133),
                None,
                Some(166),
                None,
                Some(100),
                None,
                None,
            ][idx],
            0x006f | 0x1067 => [
                Some(266),
                Some(133),
                Some(200),
                Some(166),
                Some(333),
                Some(100),
                Some(400),
                None,
            ][idx],
            _ => None,
        }?;
        return Some(100 * ((3 * fsb + 50) / 100));
    }

    #[cfg(not(target_os = "none"))]
    {
        Some(600)
    }
}

fn cpu_model_id() -> u32 {
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    {
        let (eax, _, _, _) = fstart_arch::x86::cpuid(1);
        (eax >> 4) & 0xffff
    }

    #[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
    {
        0x006f
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    fn count(aml: &[u8], name: &[u8]) -> usize {
        aml.windows(name.len()).filter(|w| *w == name).count()
    }

    #[test]
    fn processor_devices() {
        let aml = cpu_devices_aml(2);
        for name in [
            &b"_PCT"[..],
            b"_PSS",
            b"_PPC",
            b"_PSD",
            b"_CST",
            b"ACPI0007",
        ] {
            assert_eq!(count(&aml, name), 2, "{name:?}");
        }
        // Two devices, PPKG and CNOT name both.
        assert_eq!(count(&aml, b"C001"), 3);
        assert_eq!(count(&aml, b"PPKG"), 1);
        assert_eq!(count(&aml, b"CNOT"), 1);
        // Pineview: no C-states.
        let aml = pineview_cpu_devices_aml(2);
        assert_eq!(count(&aml, b"_CST"), 0);
        assert_eq!(count(&aml, b"_PSS"), 2);
    }
}
