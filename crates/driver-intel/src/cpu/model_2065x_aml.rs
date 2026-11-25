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
use fstart_acpi::gas::{AccessSize, AddressSpace, GAS};
use fstart_acpi::processor::{PerformanceState, PowerState};

pub use super::ratio_aml::RatioLimits;

const BCLK_MHZ: u32 = 133;
/// coreboot `PSS_MAX_ENTRIES`.
const PSS_MAX_ENTRIES: u32 = 16;
/// coreboot hard-codes the CPU TDP to 25 W for the P-state power.
const POWER_MAX_MW: u32 = 25_000;

/// coreboot `generate_P_state_entries()`'s `_PSS`.
#[must_use]
pub fn pstates(limits: &RatioLimits) -> Vec<PerformanceState> {
    super::ratio_aml::ratio_states(
        limits,
        |ratio| ratio * BCLK_MHZ + ratio / 3,
        POWER_MAX_MW,
        0,
        1,
        PSS_MAX_ENTRIES,
    )
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

/// The processor devices, `PPKG` and `CNOT` for `logical_cpus` threads of
/// one package (coreboot `generate_cpu_entries()`).
#[must_use]
pub fn cpu_devices_aml(logical_cpus: usize, limits: &RatioLimits) -> Vec<u8> {
    let pstates = pstates(limits);
    let cstates = cstates(limits.mwait_substates);
    super::ratio_aml::package(logical_cpus, limits, &pstates, &cstates, false)
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
