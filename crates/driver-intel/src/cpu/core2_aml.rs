//! Intel Core/Core 2 processor power-management AML helpers.
//!
//! Mirrors coreboot's legacy Intel SpeedStep ACPI generator for Merom/Penryn
//! systems used with GM965/GM45/i945-era northbridges: `_PCT`, `_PSD`, `_PSS`,
//! and MWAIT `_CST` under ACPI0007 CPU devices, encoded by
//! [`fstart_acpi::processor`]. The P-states come from the CPU architecture
//! module's SpeedStep generator, which Penryn's EMTTM tables share.

extern crate alloc;

use super::{CState, Core2Cstates};
use alloc::vec::Vec;
use fstart_acpi::Aml;
use fstart_acpi::aml::{Arg, Method, Name, Notify, PackageBuilder, Path};
use fstart_acpi::gas::{AccessSize, AddressSpace, GAS};
use fstart_acpi::processor::{
    Coordination, Dependency, PerformanceState, PowerObjects, PowerState, ProcessorName, cst_aml,
    cst_method_aml, processor_device,
};
use fstart_arch::x86::cpu::intel::speedstep::model_id;
use fstart_arch::x86::cpu::intel::speedstep::{current_pstates, fsb_x3};

/// Generate Core/Core 2 CPU power-management devices for the DSDT, with
/// the board's MWAIT C-states.
///
/// Uses the same generic SpeedStep MSR probing as coreboot's
/// `cpu/intel/speedstep` code. Penryn coordinates P-states in hardware.
pub fn cpu_devices_aml(logical_cpus: usize, cstates: &Core2Cstates) -> Vec<u8> {
    let coordination = if model_id() == 0x1067 {
        Coordination::HwAll
    } else {
        Coordination::SwAny
    };
    build_cpu_devices_aml(logical_cpus, Some(cstates), coordination)
}

/// Generate the Pineview CPU namespace (SpeedStep without board C-states).
pub(crate) fn pineview_cpu_devices_aml(logical_cpus: usize) -> Vec<u8> {
    build_cpu_devices_aml(logical_cpus, None, Coordination::SwAny)
}

/// An MWAIT C-state: FFixedHW class 1, bit offset 2 (break on interrupt).
fn mwait(state: &CState) -> PowerState {
    PowerState {
        register: GAS::new(
            AddressSpace::FunctionalFixedHardware,
            1,
            2,
            AccessSize::ByteAccess,
            u64::from(state.mwait_hint),
        ),
        kind: state.ctype,
        latency_us: state.latency,
        milliwatts: state.power,
    }
}

/// coreboot `write_cst_entries()`: a plain `_CST` package, or a method
/// choosing the battery table when `\PWRS` reports no AC power.
fn cst(cstates: &Core2Cstates) -> Vec<u8> {
    let ac: Vec<PowerState> = cstates.ac.iter().map(mwait).collect();
    if cstates.battery.is_empty() {
        return cst_aml(&ac);
    }
    let battery: Vec<PowerState> = cstates.battery.iter().map(mwait).collect();
    cst_method_aml("PWRS", &ac, &battery)
}

fn build_cpu_devices_aml(
    logical_cpus: usize,
    cstates: Option<&Core2Cstates>,
    coordination: Coordination,
) -> Vec<u8> {
    // SAFETY: these northbridges are paired with matched legacy Intel CPUs;
    // host namespace tests use synthetic limits instead of privileged reads.
    let (states, fsb3) = unsafe { (current_pstates(), fsb_x3().unwrap_or(600)) };
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
        cstates: &[],
    }
    .aml();
    let mut power = power;
    if let Some(cstates) = cstates {
        power.extend(cst(cstates));
    }

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

#[cfg(test)]
mod tests {
    use super::*;

    fn count(aml: &[u8], name: &[u8]) -> usize {
        aml.windows(name.len()).filter(|w| *w == name).count()
    }

    #[test]
    fn processor_devices() {
        let aml = cpu_devices_aml(2, &super::super::CORE2_MOBILE_CSTATES);
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
        // A battery table turns _CST into a method choosing on \\PWRS.
        const BATTERY: Core2Cstates = Core2Cstates {
            ac: super::super::CORE2_MOBILE_CSTATES.ac,
            battery: super::super::CORE2_MOBILE_CSTATES.ac,
        };
        let aml = cpu_devices_aml(2, &BATTERY);
        assert_eq!(count(&aml, b"_CST"), 2);
        assert_eq!(count(&aml, b"PWRS"), 2);
        // Pineview: no C-states.
        let aml = pineview_cpu_devices_aml(2);
        assert_eq!(count(&aml, b"_CST"), 0);
        assert_eq!(count(&aml, b"_PSS"), 2);
    }
}
