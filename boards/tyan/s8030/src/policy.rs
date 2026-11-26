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
//! | Clock Spread Spectrum (AMI `Setup 0x101` / `0xc0`) | Rome on, Milan off | [`fch_env`]: [`spread_spectrum`] |
//! | Restore AC Power Loss (`Setup 0x107`, Milan CBS `0x28f`) | Last State | [`fch_env`]: [`POWER_FAILURE`] |
//!
//! Slot, LAN, VGA and compliance knobs are [`crate::dxio::Options`].

use fstart_driver_amd::ccx::init as ccx;
use fstart_driver_amd::df::plan::Policy as DfPolicy;
use fstart_driver_amd::df::topology::MAX_ROOT_BRIDGES;
use fstart_driver_amd::dxio::DxioPolicy;
use fstart_driver_amd::fch::tasks::env::{EnvPolicy, PowerFailState};
use fstart_driver_amd::nbio::NbioPolicy;
use fstart_driver_amd::smu::v11::pptable::PowerPolicy;
use fstart_platform_amd::sp3::silicon::Generation;

/// "Restore AC Power Loss" = Last State (Rome `Setup 0x107` = 3, Milan
/// `AmdSetup 0x28f` = 3); the FCH driver's donor default is Off.
pub const POWER_FAILURE: PowerFailState = PowerFailState::Previous;

/// FCH env task policy (`fch::tasks::env::{hw_acpi_pre, hw_acpi}`).
pub const fn fch_env(generation: Generation) -> EnvPolicy {
    EnvPolicy {
        power_fail: POWER_FAILURE,
        spread_spectrum: spread_spectrum(generation),
    }
}

/// FCH clock spread spectrum (Rome `Setup 0x101` = 1, Milan `Setup 0xc0`
/// = 0: the images differ, §4). Both FchHuashanDxe env tasks apply it to
/// MISCx08\[0\] (Rome V4.04 `0x2d1a`, policy byte `+0x3d`).
pub const fn spread_spectrum(generation: Generation) -> bool {
    matches!(generation, Generation::Rome)
}

/// I/O bytes and PCI buses per root bridge, from TyanInitFabricResource
/// (Rome `0xffe81988`) / OemInitFabricResource (Milan `0xffdcacac`): bytes
/// `40 40 40 40 04 04 04 03` = 64 buses each, I/O 16K/16K/16K/12K
/// (decompiler-only, SOC_DF §3.4).
///
/// The vendor table is indexed by IOMS; the planner takes the sizes in its
/// logical order, the primary root bridge first. On this board the primary
/// is IOMS3 (bus 0), so its 12K comes first, then IOMS0–2 (buses 0x40,
/// 0x80, 0xC0). The primary's window then ends at 0x4000 with the legacy
/// ports below 0x1000, as the stock BIOS routes it (bus 0's devices in
/// 0x1000–0x3FFF, the SAS3224 under bus 0x40 at 0x4000).
pub const IO_SIZES: [u32; 4] = [0x3000, 0x4000, 0x4000, 0x4000];
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
    DxioPolicy::new(generation.silicon())
}
