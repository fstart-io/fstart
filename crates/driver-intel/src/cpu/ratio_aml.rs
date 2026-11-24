//! Shared Intel ratio-based processor power AML (`_PCT`, `_PPC`, `_PSD`,
//! `_PSS`, `_CST`, `_TSD`, `_PTC`, `_TPC`, `_TSS`, `PPKG`, `CNOT`), used by
//! Arrandale/Clarkdale (`model_2065x_aml`) and Haswell.

extern crate alloc;

use alloc::vec::Vec;
use fstart_acpi::Aml;
use fstart_acpi::aml::{Arg, Method, Name, Notify, PackageBuilder, Path};
use fstart_acpi::gas::{AccessSize, AddressSpace, GAS};
use fstart_acpi::processor::{
    Coordination, Dependency, PerformanceState, PowerObjects, PowerState, ProcessorName,
    processor_device,
};

/// The ratio and turbo facts the processor tables derive from.
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
        use fstart_arch::x86::cpu::intel::ratio::{
            MISC_PWR_MGMT, MiscPwrMgmt, PLATFORM_INFO, PlatformInfo, TURBO_RATIO_LIMIT,
            TurboRatioLimit, TurboState, turbo_state,
        };
        use fstart_arch::x86::cpu::msr::{MsrAccess, NativeMsr};
        use tock_registers::interfaces::Readable;
        // SAFETY: supported Intel packages implement these MSRs.
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

/// coreboot `calculate_power()`.
pub(crate) fn calculate_power(tdp: u32, p1_ratio: u32, ratio: u32) -> u32 {
    let m = (110_000 - (p1_ratio - ratio) * 625) / 11;
    let m = (m * m) / 1000;
    let mut power = (ratio * 100_000 / p1_ratio) / 100;
    power *= (m / 100) * (tdp / 1000);
    power / 1000
}

/// coreboot `generate_P_state_entries()`'s `_PSS`.
#[must_use]
pub(crate) fn ratio_states(
    limits: &RatioLimits,
    clock: impl Fn(u32) -> u32,
    tdp: u32,
    shift: u32,
    mut step: u32,
    capacity: u32,
) -> Vec<PerformanceState> {
    let clock_max = clock(limits.max);
    let mut entries = limits.max.saturating_sub(limits.min) / step;
    while entries > capacity - 1 {
        step <<= 1;
        entries >>= 1;
    }
    let state = |mhz, milliwatts, ratio| PerformanceState {
        mhz,
        milliwatts,
        latency_us: 10,
        bus_master_latency_us: 10,
        control: ratio << shift,
        status: ratio << shift,
    };
    let mut states = Vec::new();
    if let Some(turbo) = limits.turbo {
        states.push(state(clock_max + 1, tdp, turbo));
    }
    states.push(state(clock_max, tdp, limits.max));
    if entries != 0 {
        let mut ratio = limits.min + (entries - 1) * step;
        loop {
            states.push(state(
                clock(ratio),
                calculate_power(tdp, limits.max, ratio),
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

/// coreboot `tss_table_fine`/`tss_table_coarse`: percent, power, control.
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
fn tstates_aml(domain: u32, processors: u32, fine: bool, mutable_limit: bool) -> Vec<u8> {
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
    let mut ptc = fstart_acpi::processor::pct_aml(ffh, ffh);
    // Same encoding as _PCT; only the name differs.
    ptc[1..5].copy_from_slice(b"_PTC");
    aml.extend(ptc);
    if mutable_limit {
        aml.extend_from_slice(&fstart_acpi_macros::acpi_dsl! {
            Method("_TPC", 0, NotSerialized) { Return(#{const "\\TLVL"}); }
        });
    } else {
        aml.extend_from_slice(&fstart_acpi_macros::acpi_dsl! {
            Method("_TPC", 0, NotSerialized) { Return(0u32); }
        });
    }
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

/// Emit processor devices, `PPKG`, and `CNOT` for `logical_cpus` threads.
#[must_use]
pub(crate) fn package(
    logical_cpus: usize,
    limits: &RatioLimits,
    pstates: &[PerformanceState],
    cstates: &[PowerState],
    thermal_ppc: bool,
) -> Vec<u8> {
    let ffh = GAS::new(
        AddressSpace::FunctionalFixedHardware,
        0,
        0,
        AccessSize::Undefined,
        0,
    );
    let mut power = PowerObjects {
        control: ffh,
        status: ffh,
        pstates,
        ppc: (!thermal_ppc).then_some(0),
        dependency: (!thermal_ppc).then_some(Dependency {
            domain: 0,
            coordination: if limits.sw_coordination {
                Coordination::SwAny
            } else {
                Coordination::HwAll
            },
            processors: logical_cpus as u32,
        }),
        cstates,
    }
    .aml();
    if thermal_ppc {
        power.extend_from_slice(&fstart_acpi_macros::acpi_dsl! {
            Method("_PPC", 0, NotSerialized) { Return(#{const "\\_SB_.PPCM"}); }
        });
    }
    power.extend(tstates_aml(
        0,
        logical_cpus as u32,
        limits.fine_throttling,
        thermal_ppc,
    ));

    let names: Vec<ProcessorName> = (0..logical_cpus)
        .map(|cpu| ProcessorName::new(u16::try_from(cpu).expect("too many CPUs")))
        .collect();
    let mut out = Vec::new();
    for (uid, name) in names.iter().enumerate() {
        let mut objects = power.clone();
        if thermal_ppc {
            objects.extend(fstart_acpi::processor::psd_aml(
                uid as u32,
                if limits.sw_coordination {
                    Coordination::SwAny
                } else {
                    Coordination::HwAll
                },
                1,
            ));
        }
        out.extend(
            processor_device(*name, uid as u32, &objects)
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

    #[test]
    fn haswell_psd_is_local_but_tsd_is_package_wide() {
        let limits = RatioLimits {
            min: 8,
            max: 16,
            turbo: None,
            sw_coordination: false,
            mwait_substates: 0,
            fine_throttling: false,
        };
        let aml = package(2, &limits, &[], &[], true);
        for cpu in 0..2 {
            let psd = fstart_acpi::processor::psd_aml(cpu, Coordination::HwAll, 1);
            assert_eq!(aml.windows(psd.len()).filter(|w| *w == psd).count(), 1);
        }
        let tsd = tstates_aml(0, 2, false, true);
        assert_eq!(aml.windows(tsd.len()).filter(|w| *w == tsd).count(), 2);
    }
}
