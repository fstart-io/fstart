//! Arrandale/Clarkdale processor power objects (coreboot
//! `cpu/intel/model_2065x/acpi.c`): `_PCT`/`_PPC`/`_PSD`/`_PSS` from the
//! ratio MSRs, `_CST` for C1E/C3/C6 downgraded to what CPUID leaf 5
//! advertises, and the T-state objects, under one `ACPI0007` device per
//! logical CPU; then `PPKG` and `CNOT`.
//!
//! `_PPC` and `_TPC` return 0 where coreboot reads the GNVS `PPCM`/`TLVL`
//! fields its firmware never changes from 0.

extern crate alloc;

use alloc::vec::Vec;
use fstart_acpi::Aml;
use fstart_acpi::aml::{Arg, Method, Name, Notify, PackageBuilder, Path};
use fstart_acpi::gas::{AccessSize, AddressSpace, GAS};
use fstart_acpi::processor::{
    Coordination, Dependency, PerformanceState, PowerObjects, PowerState, ProcessorName,
    processor_device,
};

/// The ratio and turbo facts the tables derive from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RatioLimits {
    /// `MSR_PLATFORM_INFO[47:40]`.
    pub min: u32,
    /// `MSR_PLATFORM_INFO[15:8]`.
    pub max: u32,
    /// `MSR_TURBO_RATIO_LIMIT[7:0]` when turbo is enabled.
    pub turbo: Option<u32>,
    /// `MSR_MISC_PWR_MGMT[0]`: software P-state coordination.
    pub sw_coordination: bool,
    /// `CPUID.5:EDX`, the MWAIT sub-states per C-state.
    pub mwait_substates: u32,
    /// `CPUID.6:EAX[5]`, extended throttle levels.
    pub fine_throttling: bool,
}

impl RatioLimits {
    /// Read the running CPU.
    #[cfg(target_os = "none")]
    #[must_use]
    pub fn current() -> Self {
        use fstart_arch::x86::cpu::intel::model_2065x::{
            MISC_PWR_MGMT, MiscPwrMgmt, PLATFORM_INFO, PlatformInfo, TURBO_RATIO_LIMIT,
            TurboRatioLimit, TurboState, turbo_state,
        };
        use fstart_arch::x86::cpu::msr::{MsrAccess, NativeMsr};
        use tock_registers::interfaces::Readable;
        // SAFETY: the Ironlake platform pairs only with model 2065x.
        let cpu = unsafe { NativeMsr::new() };
        let info = cpu.reg(PLATFORM_INFO);
        Self {
            min: info.read(PlatformInfo::MAX_EFFICIENCY_RATIO) as u32,
            max: info.read(PlatformInfo::MAX_NON_TURBO_RATIO) as u32,
            turbo: (turbo_state(&cpu) == TurboState::Enabled)
                .then(|| cpu.reg(TURBO_RATIO_LIMIT).read(TurboRatioLimit::ONE_CORE) as u32),
            sw_coordination: cpu
                .reg(MISC_PWR_MGMT)
                .is_set(MiscPwrMgmt::EIST_HW_COORDINATION_DISABLE),
            mwait_substates: fstart_arch::x86::cpuid(5).3,
            fine_throttling: fstart_arch::x86::cpuid(6).0 & (1 << 5) != 0,
        }
    }
}

const BCLK_MHZ: u32 = 133;
/// coreboot `PSS_MAX_ENTRIES`, `PSS_RATIO_STEP`, `PSS_LATENCY_*`.
const PSS_MAX_ENTRIES: u32 = 16;
const PSS_LATENCY: u32 = 10;
/// coreboot hard-codes the CPU TDP to 25 W for the P-state power.
const POWER_MAX_MW: u32 = 25_000;

/// coreboot `calculate_power()`.
fn calculate_power(tdp: u32, p1_ratio: u32, ratio: u32) -> u32 {
    let m = (110_000 - (p1_ratio - ratio) * 625) / 11;
    let m = (m * m) / 1000;
    let mut power = (ratio * 100_000 / p1_ratio) / 100;
    power *= (m / 100) * (tdp / 1000);
    power / 1000
}

/// coreboot `generate_P_state_entries()`'s `_PSS`.
#[must_use]
pub fn pstates(limits: &RatioLimits) -> Vec<PerformanceState> {
    let clock_max = limits.max * BCLK_MHZ + limits.max / 3;
    let mut step = 1;
    let mut entries = limits.max.saturating_sub(limits.min) / step;
    while entries > PSS_MAX_ENTRIES - 1 {
        step <<= 1;
        entries >>= 1;
    }
    let state = |mhz, milliwatts, ratio| PerformanceState {
        mhz,
        milliwatts,
        latency_us: PSS_LATENCY,
        bus_master_latency_us: PSS_LATENCY,
        control: ratio,
        status: ratio,
    };
    let mut states = Vec::new();
    if let Some(turbo) = limits.turbo {
        states.push(state(clock_max + 1, POWER_MAX_MW, turbo));
    }
    states.push(state(clock_max, POWER_MAX_MW, limits.max));
    if entries != 0 {
        let mut ratio = limits.min + (entries - 1) * step;
        loop {
            states.push(state(
                ratio * BCLK_MHZ + ratio / 3,
                calculate_power(POWER_MAX_MW, limits.max, ratio),
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

/// One entry of coreboot's `cstate_map`: latency, power and MWAIT hint.
#[derive(Clone, Copy)]
struct MwaitState {
    latency: u16,
    power: u32,
    hint: u8,
}

/// coreboot `cstate_map` (latencies from the X201T vendor ACPI), indexed
/// C0, C1, C1E, C3, C6, C7.
const CSTATE_MAP: [MwaitState; 6] = [
    MwaitState {
        latency: 0,
        power: 0,
        hint: 0,
    },
    MwaitState {
        latency: 3,
        power: 1000,
        hint: 0x00,
    },
    MwaitState {
        latency: 3,
        power: 1000,
        hint: 0x01,
    },
    MwaitState {
        latency: 205,
        power: 500,
        hint: 0x10,
    },
    MwaitState {
        latency: 245,
        power: 350,
        hint: 0x20,
    },
    MwaitState {
        latency: 245,
        power: 200,
        hint: 0x30,
    },
];

/// coreboot `get_supported_cstate()`: the requested state or the next lower
/// one CPUID leaf 5 advertises enough sub-states for.
fn supported_cstate(requested: usize, substates: u32) -> usize {
    (1..=requested)
        .rev()
        .find(|&index| {
            let hint = CSTATE_MAP[index].hint;
            let state = u32::from(hint >> 4) + 1;
            (substates >> (4 * state)) & 0xf > u32::from(hint & 0xf)
        })
        .unwrap_or(0)
}

/// coreboot `generate_C_state_entries()`: C1E, C3 and C6 as ACPI C1-C3.
#[must_use]
pub fn cstates(substates: u32) -> Vec<PowerState> {
    const REQUESTED: [usize; 3] = [2, 3, 4];
    let mut chosen = [0usize; 3];
    for (i, requested) in REQUESTED.iter().enumerate() {
        chosen[i] = supported_cstate(*requested, substates);
        if chosen[..i].contains(&chosen[i]) {
            chosen[i] = 0;
        }
    }
    chosen
        .iter()
        .enumerate()
        .filter(|(_, index)| **index != 0)
        .map(|(position, index)| {
            let state = CSTATE_MAP[*index];
            PowerState {
                register: GAS::new(
                    AddressSpace::FunctionalFixedHardware,
                    1,
                    2,
                    AccessSize::ByteAccess,
                    u64::from(state.hint),
                ),
                kind: position as u8 + 1,
                latency_us: state.latency,
                milliwatts: state.power,
            }
        })
        .collect()
}

/// coreboot `tss_table_fine`/`tss_table_coarse`: percent, power, latency,
/// control, status.
const TSS_FINE: [(u32, u32, u32); 15] = [
    (100, 1000, 0x00),
    (94, 940, 0x1f),
    (88, 880, 0x1e),
    (82, 820, 0x1d),
    (75, 760, 0x1c),
    (69, 700, 0x1b),
    (63, 640, 0x1a),
    (57, 580, 0x19),
    (50, 520, 0x18),
    (44, 460, 0x17),
    (38, 400, 0x16),
    (32, 340, 0x15),
    (25, 280, 0x14),
    (19, 220, 0x13),
    (13, 160, 0x12),
];
const TSS_COARSE: [(u32, u32, u32); 8] = [
    (100, 1000, 0x00),
    (88, 875, 0x1f),
    (75, 750, 0x1e),
    (63, 625, 0x1d),
    (50, 500, 0x1c),
    (38, 375, 0x1b),
    (25, 250, 0x1a),
    (13, 125, 0x19),
];

/// coreboot `generate_T_state_entries()`: `_TSD` (SW_ALL), an FFixedHW
/// `_PTC`, `_TPC` and `_TSS`.
fn tstates_aml(domain: u32, processors: u32, fine: bool) -> Vec<u8> {
    const SW_ALL: u32 = 0xfc;
    let mut aml = Vec::new();
    let mut tsd_entry = PackageBuilder::new();
    for value in [5u32, 0, domain, SW_ALL, processors] {
        tsd_entry.add_element(&value);
    }
    let mut tsd = PackageBuilder::new();
    tsd.add_element(&tsd_entry);
    Name::new("_TSD".into(), &tsd).to_aml_bytes(&mut aml);
    // acpigen_write_empty_PTC(): FFixedHW control and status.
    let ffh = GAS::new(
        AddressSpace::FunctionalFixedHardware,
        0,
        0,
        AccessSize::Undefined,
        0,
    );
    let ptc = fstart_acpi::processor::pct_aml(ffh, ffh);
    // Same encoding as _PCT; only the name differs.
    let mut ptc = ptc;
    ptc[1..5].copy_from_slice(b"_PTC");
    aml.extend(ptc);
    aml.extend_from_slice(&fstart_acpi_macros::acpi_dsl! {
        Method("_TPC", 0, NotSerialized) { Return(0u32); }
    });
    let table: &[(u32, u32, u32)] = if fine { &TSS_FINE } else { &TSS_COARSE };
    let mut tss = PackageBuilder::new();
    for (percent, power, control) in table {
        let mut entry = PackageBuilder::new();
        for value in [*percent, *power, 0u32, *control, 0u32] {
            entry.add_element(&value);
        }
        tss.add_element(&entry);
    }
    Name::new("_TSS".into(), &tss).to_aml_bytes(&mut aml);
    aml
}

/// The processor devices, `PPKG` and `CNOT` for `logical_cpus` threads of
/// one package (coreboot `generate_cpu_entries()`).
#[must_use]
pub fn cpu_devices_aml(logical_cpus: usize, limits: &RatioLimits) -> Vec<u8> {
    let ffh = GAS::new(
        AddressSpace::FunctionalFixedHardware,
        0,
        0,
        AccessSize::Undefined,
        0,
    );
    let pstates = pstates(limits);
    let cstates = cstates(limits.mwait_substates);
    let mut power = PowerObjects {
        control: ffh,
        status: ffh,
        pstates: &pstates,
        ppc: Some(0),
        dependency: Some(Dependency {
            domain: 0,
            coordination: if limits.sw_coordination {
                Coordination::SwAny
            } else {
                Coordination::HwAll
            },
            processors: logical_cpus as u32,
        }),
        cstates: &cstates,
    }
    .aml();
    power.extend(tstates_aml(0, logical_cpus as u32, limits.fine_throttling));

    let names: Vec<ProcessorName> = (0..logical_cpus)
        .map(|cpu| ProcessorName::new(u16::try_from(cpu).expect("too many CPUs")))
        .collect();
    let mut out = Vec::new();
    for (uid, name) in names.iter().enumerate() {
        out.extend(
            processor_device(*name, uid as u32, &power)
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

    /// An i5-520M: ratios 9..18, turbo 22, C1E/C3/C6 MWAIT sub-states.
    const I5_520M: RatioLimits = RatioLimits {
        min: 9,
        max: 18,
        turbo: Some(22),
        sw_coordination: false,
        mwait_substates: 0x1120,
        fine_throttling: false,
    };

    #[test]
    fn pss_matches_coreboot() {
        let states = pstates(&I5_520M);
        // Turbo, then 18 and 9 entries from 17 down to 9.
        assert_eq!(states.len(), 2 + 9);
        assert_eq!(states[0].control, 22);
        assert_eq!(states[0].mhz, 18 * 133 + 6 + 1);
        assert_eq!(states[1].control, 18);
        assert_eq!(states[2].control, 17);
        assert_eq!(states.last().unwrap().control, 9);
        assert_eq!(states.last().unwrap().mhz, 9 * 133 + 3);
    }

    #[test]
    fn cstates_downgrade_to_supported_substates() {
        // C1 with two sub-states, C3 and C6 with one: C1E, C3, C6.
        let states = cstates(0x1120);
        assert_eq!(states.len(), 3);
        assert_eq!(
            states.iter().map(|s| s.latency_us).collect::<Vec<_>>(),
            [3, 205, 245]
        );
        // No C6: C1E, C3; the C6 request downgrades to the C3 duplicate.
        assert_eq!(cstates(0x0120).len(), 2);
    }

    #[test]
    fn processor_devices_carry_every_object() {
        let aml = cpu_devices_aml(4, &I5_520M);
        let count = |name: &[u8]| aml.windows(name.len()).filter(|w| *w == name).count();
        for name in [
            &b"_PSS"[..],
            b"_PSD",
            b"_CST",
            b"_TSS",
            b"_TSD",
            b"_PTC",
            b"_TPC",
        ] {
            assert_eq!(count(name), 4, "{name:?}");
        }
        assert_eq!(count(b"PPKG"), 1);
        assert_eq!(count(b"CNOT"), 1);
    }
}
