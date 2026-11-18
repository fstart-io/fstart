//! Bay Trail (Silvermont) processor power-management AML, from coreboot's
//! `soc/intel/baytrail/acpi.c`: `_PSS` from the IA core ratio and VID
//! MSRs with an optional turbo entry, per-core `_PSD`, and MWAIT `_CST`
//! for C1, C6NS and C6FS.
//!
//! The reference's T-states (`_TSS`/`_PTC`/`_TPC`) depend on a GNVS
//! throttling limit this firmware does not publish and are not emitted.

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

/// The CPU facts the tables are built from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CpuFacts {
    pub ratio_min: u32,
    pub ratio_max: u32,
    pub ratio_turbo: u32,
    pub vid_min: u32,
    pub vid_max: u32,
    pub vid_turbo: u32,
    pub bclk_khz: u32,
    /// Package power limit in mW.
    pub tdp_mw: u32,
    pub turbo: bool,
}

impl CpuFacts {
    /// Read the MSRs of the executing core.
    #[cfg(target_os = "none")]
    pub fn read() -> Self {
        use fstart_arch::x86::cpu::intel::silvermont::*;
        use fstart_arch::x86::cpu::msr::{Msr, MsrAccess, NativeMsr};
        use tock_registers::interfaces::Readable;
        // SAFETY: Silvermont implements these MSRs.
        let cpu = unsafe { NativeMsr::new() };
        let ratios = cpu.reg(MSR_IACORE_RATIOS);
        let vids = cpu.reg(MSR_IACORE_VIDS);
        let power_unit = 1 << (cpu.reg(Msr::<()>::new(0x606)).get() & 0xf);
        let limit = cpu.reg(Msr::<()>::new(0x610)).get() & 0x7fff;
        let (eax, _, _, _) = fstart_arch::x86::cpuid(6);
        let misc = cpu.reg(Msr::<()>::new(0x1a0)).get();
        Self {
            ratio_min: ratios.read(IACORE::LFM) as u32,
            ratio_max: ratios.read(IACORE::MAX) as u32,
            ratio_turbo: (cpu.reg(MSR_IACORE_TURBO_RATIOS).get() & 0xff) as u32,
            vid_min: vids.read(IACORE::LFM) as u32,
            vid_max: vids.read(IACORE::MAX) as u32,
            vid_turbo: (cpu.reg(Msr::<()>::new(0x66d)).get() & 0xff) as u32,
            bclk_khz: bus_freq_khz(),
            tdp_mw: (limit / power_unit) as u32 * 1000,
            turbo: eax & (1 << 1) != 0 && misc & (1 << 38) == 0,
        }
    }
}

/// `((1.1 - (p1 - ratio) * 0.00625) / 1.1)^2 * ratio / p1 * TDP`, in the
/// reference's fixed point.
fn power(tdp: u32, p1_ratio: u32, ratio: u32) -> u32 {
    let m = (110_000 - (p1_ratio - ratio) * 625) / 11;
    let m = m * m / 1000;
    let mut power = (ratio * 100_000 / p1_ratio) / 100;
    power *= (m / 100) * (tdp / 1000);
    power / 1000
}

/// `_PSS`: turbo (if enabled), the guaranteed ratio, then down to LFM in
/// at most 15 steps.
pub fn pstates(facts: &CpuFacts) -> Vec<PerformanceState> {
    let state = |mhz, milliwatts, ratio: u32, vid: u32| PerformanceState {
        mhz,
        milliwatts,
        latency_us: 10,
        bus_master_latency_us: 10,
        control: ratio << 8 | vid,
        status: ratio << 8 | vid,
    };
    let clock_max = facts.ratio_max * facts.bclk_khz / 1000;
    let mut step = 1;
    let mut entries = facts.ratio_max.saturating_sub(facts.ratio_min) / step;
    while entries > 15 {
        step <<= 1;
        entries >>= 1;
    }
    let mut states = Vec::new();
    let (ratio_turbo, vid_turbo) = if facts.turbo {
        states.push(state(
            clock_max + 1,
            facts.tdp_mw,
            facts.ratio_turbo,
            facts.vid_turbo,
        ));
        (facts.ratio_turbo, facts.vid_turbo)
    } else {
        (facts.ratio_max, facts.vid_max)
    };
    states.push(state(
        clock_max,
        facts.tdp_mw,
        facts.ratio_max,
        facts.vid_max,
    ));
    let ratio_range = (ratio_turbo - facts.ratio_min) * 2;
    let vid_range = vid_turbo.saturating_sub(facts.vid_min) * 2;
    if entries == 0 || ratio_range == 0 {
        return states;
    }
    let mut ratio = facts.ratio_min + (entries - 1) * step;
    while ratio >= facts.ratio_min {
        let scaled = (ratio - facts.ratio_min) * vid_range;
        let vid =
            scaled / ratio_range + facts.vid_min + u32::from(!scaled.is_multiple_of(ratio_range));
        states.push(state(
            ratio * facts.bclk_khz / 1000,
            power(facts.tdp_mw, facts.ratio_max, ratio),
            ratio,
            vid & 0xff,
        ));
        if ratio < step {
            break;
        }
        ratio -= step;
    }
    states
}

/// MWAIT C-states: C1, C6NS without L2 shrink, C6FS with full shrink.
fn cstates() -> [PowerState; 3] {
    // FFixedHW, vendor Intel, MWAIT class, hardware coordination.
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
    [
        mwait(1, 1, 1000, 0x00),
        mwait(2, 500, 10, 0x51),
        mwait(3, 1500, 1, 0x52),
    ]
}

/// The processor devices, `PPKG` and `CNOT`.
pub fn cpu_devices_aml(cpus: usize, facts: &CpuFacts) -> Vec<u8> {
    let pstates = pstates(facts);
    let cstates = cstates();
    let coordination = if cpus > 2 {
        Coordination::SwAll
    } else {
        Coordination::HwAll
    };
    // The OS drives IA32_PERF_CTL/STATUS (empty `_PCT`).
    let ffh = GAS::new(
        AddressSpace::FunctionalFixedHardware,
        0,
        0,
        AccessSize::Undefined,
        0,
    );
    let names: Vec<ProcessorName> = (0..cpus)
        .map(|cpu| ProcessorName::new(u16::try_from(cpu).expect("too many CPUs")))
        .collect();
    let mut out = Vec::new();
    for (core, name) in names.iter().enumerate() {
        let power = PowerObjects {
            control: ffh,
            status: ffh,
            pstates: &pstates,
            ppc: Some(0),
            dependency: Some(Dependency {
                domain: core as u32,
                coordination,
                processors: 1,
            }),
            cstates: &cstates,
        }
        .aml();
        out.extend(
            processor_device(*name, core as u32, &power)
                .expect("required CPU AML device exceeds length limit"),
        );
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// E3845: LFM 6, guaranteed 0x13 (19) at 83.3 MHz, no turbo.
    const E3845: CpuFacts = CpuFacts {
        ratio_min: 6,
        ratio_max: 23,
        ratio_turbo: 23,
        vid_min: 0x22,
        vid_max: 0x3c,
        vid_turbo: 0x3c,
        bclk_khz: 83_333,
        tdp_mw: 10_000,
        turbo: false,
    };

    #[test]
    fn pss_starts_at_the_guaranteed_ratio_and_ends_at_lfm() {
        let states = pstates(&E3845);
        assert_eq!(states[0].control, 23 << 8 | 0x3c);
        assert_eq!(states[0].mhz, 1916);
        let last = states.last().unwrap();
        assert_eq!(last.control >> 8, 6);
        assert_eq!(last.control & 0xff, 0x22);
        assert!(states.len() <= 16);
    }

    #[test]
    fn turbo_adds_a_one_mhz_higher_entry() {
        let states = pstates(&CpuFacts {
            turbo: true,
            ratio_turbo: 25,
            vid_turbo: 0x40,
            ..E3845
        });
        assert_eq!(states[0].mhz, states[1].mhz + 1);
        assert_eq!(states[0].control, 25 << 8 | 0x40);
    }
}
