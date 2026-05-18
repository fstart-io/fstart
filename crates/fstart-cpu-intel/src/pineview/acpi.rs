//! Pineview processor power-management AML helpers.
//!
//! Mirrors coreboot's legacy Intel SpeedStep ACPI generator for Atom
//! Pineview systems. Pineview boards in coreboot expose `_PSS` but return no
//! board `_CST` entries.

#![allow(dead_code)]

extern crate alloc;

use alloc::string::ToString;
use alloc::vec;
use alloc::vec::Vec;

use acpi_tables::aml::{Name, PackageBuilder, Path};
use acpi_tables::Aml;

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
const SPEEDSTEP_RATIO_DYNFSB: u32 = 1 << SPEEDSTEP_RATIO_DYNFSB_SHIFT;
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

const SW_ANY: u32 = 0xfd;
const HW_ALL: u32 = 0xfe;

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

/// Generate Pineview CPU power-management devices for the DSDT.
///
/// Coreboot's Pineview boards provide SpeedStep `_PSS` but their
/// board-level `get_cst_entries()` returns no `_CST` entries.
pub fn cpu_devices_aml(logical_cpus: usize) -> Vec<u8> {
    let states = speedstep_pstates();
    let coordination = SW_ANY;
    let mut out = Vec::new();

    for cpu in 0..logical_cpus {
        let mut body = Vec::new();
        name_string(&mut body, "_HID", "ACPI0007");
        name_integer(&mut body, "_UID", cpu as u32);
        append_empty_pct(&mut body);
        append_psd(&mut body, 0, logical_cpus as u32, coordination);
        append_pss(&mut body, &states);
        append_device(&mut out, cpu_name(cpu), &body);
    }

    append_processor_package(&mut out, logical_cpus);
    append_cnot_method(&mut out, logical_cpus);
    out
}

fn cpu_name(cpu: usize) -> [u8; 4] {
    let hi = ((cpu / 16) & 0xf) as u8;
    let lo = (cpu & 0xf) as u8;
    [b'C', b'P', hex_digit(hi), hex_digit(lo)]
}

fn hex_digit(n: u8) -> u8 {
    if n < 10 {
        b'0' + n
    } else {
        b'A' + (n - 10)
    }
}

fn append_device(out: &mut Vec<u8>, name: [u8; 4], body: &[u8]) {
    let mut payload = Vec::new();
    payload.extend_from_slice(&name);
    payload.extend_from_slice(body);
    out.extend_from_slice(&[0x5b, 0x82]);
    out.extend_from_slice(&pkg_length(payload.len(), true));
    out.extend_from_slice(&payload);
}

fn append_processor_package(out: &mut Vec<u8>, logical_cpus: usize) {
    let mut pkg = PackageBuilder::new();
    for cpu in 0..logical_cpus {
        let path = Path::new(core::str::from_utf8(&cpu_name(cpu)).unwrap());
        pkg.add_element(&path);
    }
    Name::new(Path::new("PPKG"), &pkg).to_aml_bytes(out);
}

fn append_cnot_method(out: &mut Vec<u8>, logical_cpus: usize) {
    let mut body = Vec::new();
    for cpu in 0..logical_cpus {
        body.push(0x86); // NotifyOp
        body.extend_from_slice(&cpu_name(cpu));
        body.push(0x68); // Arg0Op
    }

    out.push(0x14); // MethodOp
    let mut payload = Vec::new();
    payload.extend_from_slice(b"CNOT");
    payload.push(0x01); // one argument, NotSerialized
    payload.extend_from_slice(&body);
    out.extend_from_slice(&pkg_length(payload.len(), true));
    out.extend_from_slice(&payload);
}

fn append_empty_pct(out: &mut Vec<u8>) {
    // Coreboot acpigen_write_empty_PCT(): two FFixedHW Register descriptors.
    out.extend_from_slice(&[
        0x08, 0x5f, 0x50, 0x43, 0x54, 0x12, 0x2c, 0x02, 0x11, 0x14, 0x0a, 0x11, 0x82, 0x0c, 0x00,
        0x7f, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x79, 0x00, 0x11,
        0x14, 0x0a, 0x11, 0x82, 0x0c, 0x00, 0x7f, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x79, 0x00,
    ]);
}

fn append_psd(out: &mut Vec<u8>, domain: u32, numprocs: u32, coordination: u32) {
    let mut inner = PackageBuilder::new();
    inner.add_element(&5u8);
    inner.add_element(&0u8);
    inner.add_element(&domain);
    inner.add_element(&coordination);
    inner.add_element(&numprocs);

    let mut outer = PackageBuilder::new();
    outer.add_element(&inner);
    Name::new(Path::new("_PSD"), &outer).to_aml_bytes(out);
}

fn append_pss(out: &mut Vec<u8>, states: &[SpeedstepState]) {
    let fsb3 = ia32_fsb_x3().unwrap_or(600);
    let mut outer = PackageBuilder::new();

    for (i, state) in states.iter().enumerate() {
        let freq = if state.is_turbo && i + 1 < states.len() {
            states[i + 1].double_ratio() * fsb3 / 6 + 1
        } else if state.is_slfm {
            state.double_ratio() * fsb3 / 12
        } else {
            state.double_ratio() * fsb3 / 6
        };

        let encoded = state.encoded();
        let mut entry = PackageBuilder::new();
        entry.add_element(&freq);
        entry.add_element(&state.power);
        entry.add_element(&0u32);
        entry.add_element(&0u32);
        entry.add_element(&encoded);
        entry.add_element(&encoded);
        outer.add_element(&entry);
    }

    Name::new(Path::new("_PSS"), &outer).to_aml_bytes(out);
    name_integer(out, "_PPC", 0);
}

fn name_integer(out: &mut Vec<u8>, name: &str, value: u32) {
    Name::new(Path::new(name), &value).to_aml_bytes(out);
}

fn name_string(out: &mut Vec<u8>, name: &str, value: &str) {
    let value = value.to_string();
    Name::new(Path::new(name), &value).to_aml_bytes(out);
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

        if ((fstart_arch_x86::x86::msr::rdmsr(MSR_EXTENDED_CONFIG) >> 27) & 3) == 3 {
            let lo = fstart_arch_x86::x86::msr::rdmsr(MSR_FSB_CLOCK_VCC) as u32;
            params.slfm = state_from_msr(lo, state_mask);
            params.slfm.dynfsb = true;
            params.slfm.is_slfm = true;
        }

        params.min = state_from_msr(
            fstart_arch_x86::x86::msr::rdmsr(MSR_THERM2_CTL) as u32,
            state_mask,
        );
        params.max = state_from_msr(
            fstart_arch_x86::x86::msr::rdmsr(IA32_PLATFORM_ID) as u32,
            state_mask,
        );
        if cpu_id == 0x006e {
            params.max.ratio =
                ((fstart_arch_x86::x86::msr::rdmsr(IA32_PERF_STATUS) >> 40) & 0x1f) as u8;
        }

        let fsb_clock_vcc = fstart_arch_x86::x86::msr::rdmsr(MSR_FSB_CLOCK_VCC);
        let misc = fstart_arch_x86::x86::msr::rdmsr(IA32_MISC_ENABLE);
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
        let idx = (fstart_arch_x86::x86::msr::rdmsr(MSR_FSB_FREQ) & 7) as usize;
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
        Some(100 * ((3 * fsb + 50) / 100))
    }

    #[cfg(not(target_os = "none"))]
    {
        Some(600)
    }
}

fn cpu_model_id() -> u32 {
    #[cfg(target_arch = "x86_64")]
    {
        let (eax, _, _, _) = fstart_arch_x86::cpuid(1);
        (eax >> 4) & 0xffff
    }

    #[cfg(not(target_arch = "x86_64"))]
    {
        0x006f
    }
}

fn pkg_length(len: usize, include_self: bool) -> Vec<u8> {
    let length_length = if len < (2usize.pow(6) - 1) {
        1
    } else if len < (2usize.pow(12) - 2) {
        2
    } else if len < (2usize.pow(20) - 3) {
        3
    } else {
        4
    };
    let length = len + if include_self { length_length } else { 0 };
    match length_length {
        1 => vec![length as u8],
        2 => vec![(1u8 << 6) | (length & 0xf) as u8, (length >> 4) as u8],
        3 => vec![
            (2u8 << 6) | (length & 0xf) as u8,
            (length >> 4) as u8,
            (length >> 12) as u8,
        ],
        _ => vec![
            (3u8 << 6) | (length & 0xf) as u8,
            (length >> 4) as u8,
            (length >> 12) as u8,
            (length >> 20) as u8,
        ],
    }
}
