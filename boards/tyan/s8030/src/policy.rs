//! S8030 silicon policy (BOARD_S8030 §4, §6).
//!
//! The vendor applies these through CBS/APCB/PCD; fstart hard-codes the
//! S8030 setup defaults (`setup-defaults-rome-s8030-v404.json`, varstore
//! `AmdSetupSSP`, and `setup-defaults-milan-s8030-v404.json`, `AmdSetup`;
//! IFR default = NVRAM `StdDefaults` for every knob below). Only values that
//! differ from the driver defaults are overridden; the others are listed so
//! the mapping stays reviewable:
//!
//! | Setup knob (Rome / Milan offset) | Default | Where |
//! |---|---|---|
//! | APBDIS (`0x19a` / `0x1d0`) | 1 | [`power`]: `apbdis` |
//! | Fixed SOC Pstate (`0x19c` / `0x1d2`) | P0 | [`power`]: `fixed_soc_pstate` |
//! | Determinism, cTDP, PPT, xGMI width | Auto | [`power`]: driver default |
//! | DF Cstates (`0x19b` / `0x1d1`) | Auto | `smu::v11::FeaturePolicy::default()` |
//! | Core Performance Boost, Global C-state Control, SMT | Auto | [`ccx`]: driver default |
//! | SMEE (AMI `Setup 0x179`; Milan CBS `0x3b`) | Enabled | [`ccx`]: `smee` |
//! | SEV-SNP (Milan `0x198`) | Disabled | [`ccx`]: `snp = None` (driver default) |
//! | IOMMU (`0x154` / `0x184`) | Auto | [`nbio`]: driver default (on) |
//! | ACS, ARI (`0x156`/`0x157`, `0x188`/`0x189`) | Disabled | no driver knob yet |
//! | NUMA nodes per socket (`0xba` / `0xd1`) | NPS4 (Setup) | ABL uses the flash APCB, read back from the APOB: NPS1 on the bench |
//! | CBS link speed, Early Link Speed, Preferred IO | Auto | [`dxio`]: driver default |
//! | Clock Spread Spectrum (AMI `Setup 0x101` / `0xc0`) | Rome on, Milan off | [`spread_spectrum`] |
//! | Restore AC Power Loss (`Setup 0x107`, Milan CBS `0x28f`) | Last State | [`fch_env`]: [`POWER_FAILURE`] |
//!
//! Slot, LAN, VGA and compliance knobs are [`crate::dxio::Options`].

use fstart_driver_zen::ccx;
use fstart_driver_zen::df::plan::Policy as DfPolicy;
use fstart_driver_zen::dxio::DxioPolicy;
use fstart_driver_zen::fch::common::env::{EnvPolicy, PowerFailState};
use fstart_driver_zen::nbio::NbioPolicy;
use fstart_driver_zen::smu::v11::pptable::PowerPolicy;
use fstart_driver_zen::soc::Generation;
use fstart_driver_zen::topology::MAX_ROOT_BRIDGES;

/// "Restore AC Power Loss" = Last State (Rome `Setup 0x107` = 3, Milan
/// `AmdSetup 0x28f` = 3); the FCH driver's donor default is Off.
pub const POWER_FAILURE: PowerFailState = PowerFailState::Previous;

/// FCH env task policy (`fch::common::env::hw_acpi`).
pub const fn fch_env() -> EnvPolicy {
    EnvPolicy {
        power_fail: POWER_FAILURE,
    }
}

/// FCH clock spread spectrum (Rome `Setup 0x101` = 1, Milan `Setup 0xc0`
/// = 0: the images differ, §4).
pub const fn spread_spectrum(generation: Generation) -> bool {
    matches!(generation, Generation::Rome)
}

/// I/O bytes per root bridge (logical order) and PCI buses per root
/// bridge, from TyanInitFabricResource (Rome `0xffe81988`) /
/// OemInitFabricResource (Milan `0xffdcacac`): bytes `40 40 40 40 04 04 04
/// 03` = 64 buses each, I/O 16K/16K/16K/12K (decompiler-only, SOC_DF §3.4).
pub const IO_SIZES: [u32; 4] = [0x4000, 0x4000, 0x4000, 0x3000];
pub const BUS_COUNTS: [u16; 4] = [0x40; 4];

const fn pad<T: Copy, const N: usize>(v: [T; N], zero: T) -> [T; MAX_ROOT_BRIDGES] {
    let mut out = [zero; MAX_ROOT_BRIDGES];
    let mut i = 0;
    while i < N {
        out[i] = v[i];
        i += 1;
    }
    out
}

/// The DF resource planner policy: the DF3 equal split with the S8030 bus
/// and I/O requests. The board is 1P; the entries of a second socket stay
/// 0 (keep its buses, no I/O window).
pub const fn df() -> DfPolicy {
    DfPolicy::df3()
        .with_bus_counts(pad(BUS_COUNTS, 0))
        .with_io_sizes(pad(IO_SIZES, 0))
}

/// The SMU PPTable policy: APBDIS on with the SOC/DF fixed in P0, the
/// limits fused (all Auto).
pub fn power() -> PowerPolicy {
    PowerPolicy {
        apbdis: true,
        fixed_soc_pstate: 0,
        ..PowerPolicy::default()
    }
}

/// NBIO policy: the donor defaults (IOMMU Auto = on, clock gating on).
pub fn nbio() -> NbioPolicy {
    NbioPolicy::default()
}

/// CCX policy: the driver defaults with SYSCFG.SMEE on, as both S8030
/// images ship it. With SMEE the CPU reduces the usable physical address
/// bits (CPUID 8000_001F EBX[11:6]); the DF planner input must use the
/// reduced width.
pub fn ccx() -> ccx::Policy {
    ccx::Policy {
        smee: true,
        ..ccx::Policy::default()
    }
}

/// DXIO policy: the generation's donor defaults; the S8030 changes none.
pub const fn dxio(generation: Generation) -> DxioPolicy {
    DxioPolicy::new(generation)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn s8030_overrides() {
        let df = df();
        assert_eq!(df.bus_counts.unwrap()[..5], [0x40, 0x40, 0x40, 0x40, 0]);
        assert_eq!(
            df.io_sizes.unwrap()[..5],
            [0x4000, 0x4000, 0x4000, 0x3000, 0]
        );
        // Without the requests it is the plain DF3 split.
        assert_eq!(
            DfPolicy {
                bus_counts: None,
                io_sizes: None,
                ..df
            },
            DfPolicy::df3()
        );
        let p = power();
        assert!(p.apbdis && p.fixed_soc_pstate == 0);
        assert_eq!(PowerPolicy { apbdis: false, ..p }, PowerPolicy::default());
        assert_eq!(
            ccx::Policy {
                smee: false,
                ..ccx()
            },
            ccx::Policy::default()
        );
    }
}
