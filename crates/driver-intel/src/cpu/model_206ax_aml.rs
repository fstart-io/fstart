//! Sandy Bridge / Ivy Bridge processor objects (coreboot
//! `cpu/intel/model_206ax/acpi.c`): FFixedHW `_PCT`, `_PPC`, `_PSD` and the
//! ratio-based `_PSS`, MWAIT `_CST` from the board's C-state choice,
//! T-states (`_TSD`, `_PTC`, `_TPC`, `_TSS`), and the `PPKG`/`CNOT`
//! helpers with the `PNOT`/`PPCN`/`TNOT` notifications of `cpu.asl`.

extern crate alloc;

use alloc::vec::Vec;
use fstart_acpi::Aml;
use fstart_acpi::aml::{Arg, Method, Name, Notify, PackageBuilder, Path};
use fstart_acpi::gas::{AccessSize, AddressSpace, GAS};
use fstart_acpi::processor::{
    Coordination, Dependency, PerformanceState, PowerObjects, PowerState, ProcessorName, cst_aml,
    processor_device,
};
use fstart_arch::x86::cpu::intel::model_206ax::{self, CpuCstate, Pstate, PstateLimits};

/// coreboot `PSS_LATENCY_TRANSITION` / `PSS_LATENCY_BUSMASTER`.
const PSS_LATENCY: u32 = 10;

/// coreboot `cstate_map`: (latency us, power mW, MWAIT hint).
const fn cstate(state: CpuCstate) -> Option<(u16, u32, u8)> {
    match state {
        CpuCstate::Disabled => None,
        CpuCstate::C1 => Some((1, 1000, 0x00)),
        CpuCstate::C1E => Some((1, 1000, 0x01)),
        CpuCstate::C3 => Some((63, 500, 0x10)),
        CpuCstate::C6 => Some((87, 350, 0x20)),
        CpuCstate::C7 => Some((90, 200, 0x30)),
        CpuCstate::C7S => Some((90, 200, 0x31)),
    }
}

const ORDER: [CpuCstate; 7] = [
    CpuCstate::Disabled,
    CpuCstate::C1,
    CpuCstate::C1E,
    CpuCstate::C3,
    CpuCstate::C6,
    CpuCstate::C7,
    CpuCstate::C7S,
];

/// coreboot `get_supported_cstate`: the state, or the next lower one the
/// CPU's CPUID.5 MWAIT sub-states support. `substates` is CPUID.5:EDX.
fn supported(mut state: CpuCstate, substates: u32) -> CpuCstate {
    while state != CpuCstate::Disabled {
        let (_, _, hint) = cstate(state).expect("enabled C-state");
        let level = u32::from(hint >> 4) + 1;
        let sub = u32::from(hint & 0xf);
        if (substates >> (4 * level)) & 0xf > sub {
            break;
        }
        state = ORDER[state as usize - 1];
    }
    state
}

/// coreboot `generate_C_state_entries`.
fn cst_entries(requested: &[CpuCstate; 3], substates: u32) -> Vec<PowerState> {
    let mut chosen = [CpuCstate::Disabled; 3];
    for (i, state) in requested.iter().enumerate() {
        if *state == CpuCstate::Disabled {
            continue;
        }
        let state = supported(*state, substates);
        if !chosen[..i].contains(&state) {
            chosen[i] = state;
        }
    }
    chosen
        .iter()
        .enumerate()
        .filter_map(|(i, state)| {
            let (latency, power, hint) = cstate(*state)?;
            Some(PowerState {
                register: GAS::new(
                    AddressSpace::FunctionalFixedHardware,
                    1,
                    2,
                    AccessSize::ByteAccess,
                    u64::from(hint),
                ),
                kind: i as u8 + 1,
                latency_us: latency,
                milliwatts: power,
            })
        })
        .collect()
}

/// coreboot T-state tables: (percent, power, control).
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

fn ffh() -> GAS {
    GAS::new(
        AddressSpace::FunctionalFixedHardware,
        0,
        0,
        AccessSize::Undefined,
        0,
    )
}

/// coreboot `generate_T_state_entries`.
fn tstates(domain: u32, processors: u32, fine: bool) -> Vec<u8> {
    use fstart_acpi::aml::{ResourceTemplate, Return};
    let mut out = Vec::new();
    // _TSD: SW_ALL coordination.
    let mut entry = PackageBuilder::new();
    [5u32, 0, domain, Coordination::SwAll as u32, processors]
        .iter()
        .for_each(|v| entry.add_element(v));
    let mut tsd = PackageBuilder::new();
    tsd.add_element(&entry);
    Name::new("_TSD".into(), &tsd).to_aml_bytes(&mut out);
    // _PTC: FFixedHW, the OS uses IA32_CLOCK_MODULATION.
    let reg = fstart_acpi::processor::RegisterDescriptor(ffh());
    let template = ResourceTemplate::new(alloc::vec![&reg]);
    let mut ptc = PackageBuilder::new();
    ptc.add_element(&template);
    ptc.add_element(&template);
    Name::new("_PTC".into(), &ptc).to_aml_bytes(&mut out);
    // _TPC: the limit in \TLVL.
    let tlvl = Path::new("\\TLVL");
    let ret = Return::new(&tlvl);
    Method::new("_TPC".into(), 0, false, alloc::vec![&ret]).to_aml_bytes(&mut out);
    let table: &[(u32, u32, u32)] = if fine { &TSS_FINE } else { &TSS_COARSE };
    let mut tss = PackageBuilder::new();
    let entries: Vec<PackageBuilder> = table
        .iter()
        .map(|&(percent, power, control)| {
            let mut p = PackageBuilder::new();
            [percent, power, 0, control, 0]
                .iter()
                .for_each(|v| p.add_element(v));
            p
        })
        .collect();
    entries.iter().for_each(|p| tss.add_element(p));
    Name::new("_TSS".into(), &tss).to_aml_bytes(&mut out);
    out
}

/// The CPU namespace for `logical_cpus` processors of one package: devices
/// `C000`.., `PPKG`, `CNOT`, and root `PNOT`/`PPCN`/`TNOT`, `PPCM`, `TLVL`.
/// `limits` and `substates` (CPUID.5:EDX) are read from the CPU in firmware.
pub fn cpu_devices_aml(
    logical_cpus: usize,
    cores_per_package: u32,
    cstates: &[CpuCstate; 3],
    limits: &PstateLimits,
    substates: u32,
    fine_throttle: bool,
) -> Vec<u8> {
    let pstates: Vec<PerformanceState> = model_206ax::pstates(limits)
        .iter()
        .map(|&Pstate { mhz, mw, ratio }| PerformanceState {
            mhz,
            milliwatts: mw,
            latency_us: PSS_LATENCY,
            bus_master_latency_us: PSS_LATENCY,
            control: ratio << 8,
            status: ratio << 8,
        })
        .collect();
    let cst = cst_entries(cstates, substates);
    let coordination = if limits.hw_coordination {
        Coordination::HwAll
    } else {
        Coordination::SwAny
    };
    let cores = cores_per_package.max(1);
    let names: Vec<ProcessorName> = (0..logical_cpus)
        .map(|cpu| ProcessorName::new(u16::try_from(cpu).expect("too many CPUs")))
        .collect();
    let mut out = Vec::new();
    for (uid, name) in names.iter().enumerate() {
        let package = uid as u32 / cores;
        let mut body = PowerObjects {
            control: ffh(),
            status: ffh(),
            pstates: &pstates,
            // coreboot `acpigen_write_PPC_NVS`: the GNVS limit, here PPCM.
            ppc: None,
            dependency: Some(Dependency {
                domain: package,
                coordination,
                processors: cores,
            }),
            cstates: &[],
        }
        .aml();
        let ppcm = Path::new("\\PPCM");
        let ret = fstart_acpi::aml::Return::new(&ppcm);
        Method::new("_PPC".into(), 0, false, alloc::vec![&ret]).to_aml_bytes(&mut body);
        if !cst.is_empty() {
            body.extend(cst_aml(&cst));
        }
        body.extend(tstates(package, cores, fine_throttle));
        out.extend(
            processor_device(*name, uid as u32, &body)
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
    // cpu.asl helpers and the GNVS P-/T-state limits they read. `PNOT`,
    // the power-source notification, is the EC's (H8 `ac.asl`).
    Name::new("PPCM".into(), &0u32).to_aml_bytes(&mut out);
    Name::new("TLVL".into(), &0u32).to_aml_bytes(&mut out);
    out.extend_from_slice(&fstart_acpi_macros::acpi_dsl! {
        Method("PPCN", 0, NotSerialized) { CNOT(0x80u32); }
        Method("TNOT", 0, NotSerialized) { CNOT(0x82u32); }
    });
    out
}

/// Read the P-state facts, C-state support and throttling granularity from
/// the running CPU and build the namespace (firmware only).
#[cfg(target_os = "none")]
pub fn current_cpu_devices_aml(logical_cpus: usize, cstates: &[CpuCstate; 3]) -> Vec<u8> {
    let substates = fstart_arch::x86::cpuid(5).3;
    let fine = fstart_arch::x86::cpuid(6).0 & (1 << 5) != 0;
    cpu_devices_aml(
        logical_cpus,
        model_206ax::threads_per_package(),
        cstates,
        &PstateLimits::current(),
        substates,
        fine,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn count(aml: &[u8], name: &[u8]) -> usize {
        aml.windows(name.len()).filter(|w| *w == name).count()
    }

    #[test]
    fn cstates_downgrade_and_deduplicate_like_coreboot() {
        // C1 (2 substates), C3 (1), C6 (1), no C7.
        let substates = 0x0000_1120;
        let entries = cst_entries(&[CpuCstate::C1, CpuCstate::C3, CpuCstate::C7], substates);
        let hints: Vec<u64> = entries.iter().map(|e| e.register.address.get()).collect();
        assert_eq!(hints, [0x00, 0x10, 0x20]);
        assert_eq!(entries[2].kind, 3);
        // Without C6/C7, C7 falls back to C3, a duplicate.
        let entries = cst_entries(&[CpuCstate::C1, CpuCstate::C3, CpuCstate::C7], 0x0000_0120);
        assert_eq!(entries.len(), 2);
    }

    #[test]
    fn processor_namespace() {
        let limits = PstateLimits {
            ratio_min: 8,
            ratio_max: 25,
            ratio_turbo: Some(32),
            power_max_mw: 35_000,
            hw_coordination: true,
        };
        let aml = cpu_devices_aml(
            4,
            4,
            &[CpuCstate::C1, CpuCstate::C3, CpuCstate::C7],
            &limits,
            0x0002_1120,
            true,
        );
        for name in [
            &b"_PSS"[..],
            b"_PCT",
            b"_PSD",
            b"_CST",
            b"_TSS",
            b"_TSD",
            b"_PTC",
        ] {
            assert_eq!(count(&aml, name), 4, "{name:?}");
        }
        assert_eq!(count(&aml, b"PPKG"), 1);
        assert_eq!(count(&aml, b"PNOT"), 0);
    }
}
