//! Skylake/Kaby Lake processor power objects (coreboot
//! `soc/intel/common/block/acpi/acpi.c` `generate_cpu_entries()` with the
//! Skylake `soc_get_cstate_map()`): `_CST`, `_CPC` (CPPC v2 over the HWP
//! MSRs), and with EIST `_PCT`/`_PPC`/`_PSD`/`_PSS`, under one `ACPI0007`
//! device per logical CPU; then `PPKG` and `CNOT`.
//!
//! `_PPC` returns 0 where coreboot reads the GNVS `PPCM` its firmware never
//! changes. Skylake has no T-state table.

extern crate alloc;

use alloc::vec::Vec;
use fstart_acpi::Aml;
use fstart_acpi::aml::{Arg, Method, Name, Notify, PackageBuilder, Path};
use fstart_acpi::gas::{AccessSize, AddressSpace, GAS};
use fstart_acpi::processor::{
    Coordination, CpcEntry, Dependency, PerformanceState, PowerObjects, PowerState, ProcessorName,
    cpc_aml, processor_device,
};

/// The ratio, power and capability facts the tables derive from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// `MSR_PLATFORM_INFO[47:40]`, the maximum efficiency ratio.
    pub min: u32,
    /// The nominal TDP ratio with config TDP levels, else
    /// `MSR_PLATFORM_INFO[15:8]` (coreboot `cpu_get_max_ratio()`).
    pub max: u32,
    /// The single-core turbo ratio when turbo is enabled.
    pub turbo: Option<u32>,
    /// `MSR_PKG_POWER_SKU` TDP in mW (coreboot `cpu_get_power_max()`).
    pub tdp_mw: u32,
    /// `CPUID.6:EAX[7]`: HWP, which the CPPC registers describe.
    pub hwp: bool,
    /// The board enables EIST (coreboot `eist_enable`).
    pub eist: bool,
}

impl Limits {
    /// Read the running CPU (firmware only: the MSRs are the live ones).
    #[must_use]
    pub fn current() -> Self {
        use crate::skylake::package_power::{NOMINAL, Nominal, SKU, Sku, UNITS, Units};
        use fstart_arch::x86::cpu::msr::{MsrAccess, NativeMsr, PLATFORM_INFO, PlatformInfo};
        use tock_registers::interfaces::Readable;
        // SAFETY: the Skylake platform pairs only with Skylake/Kaby Lake.
        let cpu = unsafe { NativeMsr::new() };
        let info = cpu.reg(PLATFORM_INFO);
        let max = if info.read(PlatformInfo::CONFIG_TDP_LEVELS) != 0 {
            cpu.reg(NOMINAL).read(Nominal::RATIO)
        } else {
            info.read(PlatformInfo::MAX_NON_TURBO_RATIO)
        };
        let unit = 1u64 << cpu.reg(UNITS).read(Units::POWER);
        Self {
            min: info.read(PlatformInfo::MAX_EFFICIENT_RATIO) as u32,
            max: max as u32,
            turbo: fstart_arch::x86::cpu::intel::skylake::turbo_ratio(&cpu).map(u32::from),
            tdp_mw: (cpu.reg(SKU).read(Sku::TDP) * 1000 / unit) as u32,
            hwp: fstart_arch::x86::cpuid(6).0 & (1 << 7) != 0,
            eist: fstart_arch::x86::cpu::intel::skylake::eist_enabled(&cpu),
        }
    }
}

const BCLK_MHZ: u32 = 100;
/// Skylake `pm.h` `PSS_MAX_ENTRIES`, `PSS_RATIO_STEP`, `PSS_LATENCY_*`.
const PSS_MAX_ENTRIES: u32 = 8;
const PSS_RATIO_STEP: u32 = 2;
const PSS_LATENCY: u32 = 10;

/// coreboot `generate_p_state_entries()`'s `_PSS`: control is the
/// `IA32_PERF_CTL` ratio field, `ratio << 8`.
#[must_use]
pub fn pstates(limits: &Limits) -> Vec<PerformanceState> {
    let span = limits.max.saturating_sub(limits.min);
    let mut step = PSS_RATIO_STEP;
    let entries = loop {
        let n = span / step
            + 1
            + u32::from(!span.is_multiple_of(step))
            + u32::from(limits.turbo.is_some());
        if n <= PSS_MAX_ENTRIES {
            break n;
        }
        step += 1;
    };
    let state = |mhz, milliwatts, ratio: u32| PerformanceState {
        mhz,
        milliwatts,
        latency_us: PSS_LATENCY,
        bus_master_latency_us: PSS_LATENCY,
        control: ratio << 8,
        status: ratio << 8,
    };
    let clock_max = limits.max * BCLK_MHZ;
    let mut states = Vec::new();
    let mut remaining = entries;
    if let Some(turbo) = limits.turbo {
        states.push(state(clock_max + 1, limits.tdp_mw, turbo));
        remaining -= 1;
    }
    states.push(state(clock_max, limits.tdp_mw, limits.max));
    remaining -= 1;
    if remaining != 0 {
        let mut ratio = limits.min + (remaining - 1) * step;
        loop {
            states.push(state(
                ratio * BCLK_MHZ,
                super::model_2065x_aml::calculate_power(limits.tdp_mw, limits.max, ratio),
                ratio,
            ));
            if ratio < limits.min + step {
                break;
            }
            ratio -= step;
        }
    }
    states
}

/// Skylake `cpu.h`: `C_STATE_LATENCY_FROM_LAT_REG()` with the 1024-ns IRTL
/// unit, `(1 << 10) * limit / 1000` microseconds.
const fn irtl_latency(limit: u32) -> u16 {
    ((1024 * limit) / 1000) as u16
}

/// Skylake `cstate_set_non_s0ix`: C1E, C3, C8 as ACPI C1-C3 (`cstate_map`
/// latencies and `C*_POWER`), MWAIT hints with hardware coordination.
#[must_use]
pub fn cstates() -> Vec<PowerState> {
    const HW_COORD: AccessSize = AccessSize::ByteAccess; // ACPI_FFIXEDHW_FLAG_HW_COORD
    let states: [(u16, u32, u8); 3] = [
        (0, 1000, 0x01),                 // C1E
        (irtl_latency(0x4e), 500, 0x10), // C3, latency control 0
        (irtl_latency(0xfa), 200, 0x40), // C8, latency control 3
    ];
    states
        .into_iter()
        .enumerate()
        .map(|(index, (latency_us, milliwatts, hint))| PowerState {
            // FFixedHW: width = vendor 1 (Intel), offset = class 2 (MWAIT).
            register: GAS::new(
                AddressSpace::FunctionalFixedHardware,
                1,
                2,
                HW_COORD,
                u64::from(hint),
            ),
            kind: index as u8 + 1,
            latency_us,
            milliwatts,
        })
        .collect()
}

/// coreboot `cpu_init_cppc_config()`, version 2.
#[must_use]
pub fn cppc_entries() -> Vec<CpcEntry> {
    const HWP_CAPABILITIES: u64 = 0x771;
    const HWP_REQUEST: u64 = 0x774;
    const HWP_STATUS: u64 = 0x777;
    const PM_ENABLE: u64 = 0x770;
    const PLATFORM_INFO: u64 = 0xce;
    const MPERF: u64 = 0xe7;
    const APERF: u64 = 0xe8;
    let msr = |address, offset, width| {
        CpcEntry::Register(GAS::new(
            AddressSpace::FunctionalFixedHardware,
            width,
            offset,
            AccessSize::QwordAccess,
            address,
        ))
    };
    let unsupported = CpcEntry::Register(GAS::default());
    alloc::vec![
        msr(HWP_CAPABILITIES, 0, 8),  // highest performance
        msr(PLATFORM_INFO, 8, 8),     // nominal performance
        msr(HWP_CAPABILITIES, 16, 8), // lowest nonlinear performance
        msr(HWP_CAPABILITIES, 24, 8), // lowest performance
        msr(HWP_CAPABILITIES, 8, 8),  // guaranteed performance
        msr(HWP_REQUEST, 16, 8),      // desired performance
        msr(HWP_REQUEST, 0, 8),       // minimum performance
        msr(HWP_REQUEST, 8, 8),       // maximum performance
        unsupported,                  // performance reduction tolerance
        unsupported,                  // time window
        unsupported,                  // counter wraparound time
        msr(MPERF, 0, 64),            // reference performance counter
        msr(APERF, 0, 64),            // delivered performance counter
        msr(HWP_STATUS, 2, 1),        // performance limited
        msr(PM_ENABLE, 0, 1),         // CPPC enable
        CpcEntry::Integer(1),         // autonomous selection enable
        msr(HWP_REQUEST, 32, 10),     // autonomous activity window
        msr(HWP_REQUEST, 24, 8),      // energy performance preference
        unsupported,                  // reference performance
    ]
}

/// The processor devices, `PPKG` and `CNOT` for `logical_cpus` threads of
/// one package.
#[must_use]
pub fn cpu_devices_aml(logical_cpus: usize, limits: &Limits) -> Vec<u8> {
    let ffh = GAS::new(
        AddressSpace::FunctionalFixedHardware,
        0,
        0,
        AccessSize::Undefined,
        0,
    );
    let cstates = cstates();
    let pstates = if limits.eist {
        pstates(limits)
    } else {
        Vec::new()
    };
    let cppc = limits.hwp.then(|| cpc_aml(2, &cppc_entries()));
    let names: Vec<ProcessorName> = (0..logical_cpus)
        .map(|cpu| ProcessorName::new(u16::try_from(cpu).expect("too many CPUs")))
        .collect();
    let mut out = Vec::new();
    for (uid, name) in names.iter().enumerate() {
        // acpigen_write_PSD_package(core, 1, HW_ALL): one domain per thread.
        let mut body = PowerObjects {
            control: ffh,
            status: ffh,
            pstates: &pstates,
            ppc: limits.eist.then_some(0),
            dependency: limits.eist.then_some(Dependency {
                domain: uid as u32,
                coordination: Coordination::HwAll,
                processors: 1,
            }),
            cstates: &cstates,
        }
        .aml();
        if let Some(cppc) = &cppc {
            body.extend_from_slice(cppc);
        }
        out.extend(
            processor_device(*name, uid as u32, &body)
                .expect("CPU AML device exceeds length limit"),
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
        notifies.iter().map(|notify| notify as &dyn Aml).collect(),
    )
    .to_aml_bytes(&mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An i5-6500T: ratios 8..25, turbo 31, 35 W, HWP.
    const I5_6500T: Limits = Limits {
        min: 8,
        max: 25,
        turbo: Some(31),
        tdp_mw: 35_000,
        hwp: true,
        eist: true,
    };

    #[test]
    fn pss_matches_coreboot() {
        let states = pstates(&I5_6500T);
        // Span 17: step 2 needs 8+1+1+1 = 11 entries, step 3 needs
        // 5+1+1+1 = 8: turbo, P1, then 23 down to 8 in steps of 3.
        assert_eq!(states.len(), 8);
        assert_eq!(states[0].control, 31 << 8);
        assert_eq!(states[0].mhz, 2501);
        assert_eq!(states[1].control, 25 << 8);
        assert_eq!(states[1].milliwatts, 35_000);
        assert_eq!(states[2].control, 23 << 8);
        assert_eq!(states.last().unwrap().control, 8 << 8);
        assert_eq!(states.last().unwrap().mhz, 800);
        assert!(states.len() as u32 <= PSS_MAX_ENTRIES);
    }

    #[test]
    fn cstates_are_c1e_c3_c8() {
        let states = cstates();
        assert_eq!(
            states
                .iter()
                .map(|s| (s.kind, s.latency_us, s.milliwatts))
                .collect::<Vec<_>>(),
            [(1, 0, 1000), (2, 79, 500), (3, 256, 200)]
        );
    }

    #[test]
    fn processor_devices_carry_every_object() {
        let aml = cpu_devices_aml(4, &I5_6500T);
        let count = |name: &[u8]| aml.windows(name.len()).filter(|w| *w == name).count();
        for name in [&b"_PSS"[..], b"_PSD", b"_PCT", b"_PPC", b"_CST", b"_CPC"] {
            assert_eq!(count(name), 4, "{name:?}");
        }
        assert_eq!(count(b"PPKG"), 1);
        assert_eq!(count(b"CNOT"), 1);
        let no_eist = cpu_devices_aml(
            2,
            &Limits {
                eist: false,
                hwp: false,
                ..I5_6500T
            },
        );
        let count = |name: &[u8]| no_eist.windows(name.len()).filter(|w| *w == name).count();
        assert_eq!((count(b"_PSS"), count(b"_CPC"), count(b"_CST")), (0, 0, 2));
    }
}
