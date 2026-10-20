// SPDX-License-Identifier: MIT OR Apache-2.0
//! Native Zen/Zen+ CPU policy.
//! AMD MCAX, SYSCFG and MMCONFIG must not use legacy Intel CPU programming.
pub mod microcode;
pub mod thread;

use crate::fam17h::msr::{
    BU_CFG, BU_CFG3, BuCfg, CSTATE_CONFIG, DC_CFG, DE_CFG, DPM_CFG, DPM_WAC_DATA, DPM_WAC_INDEX,
    LS_CFG, MCA_INTR_CFG, McaConfig, McaIntrCfg, McaMisc, MsrUpdate, PSTATE_0, mca_config,
    mca_ctl_mask, mca_misc0, mca_misc1,
};
use fstart_arch::x86::cpu::{
    amd::msr::{SYSCFG, Syscfg},
    msr::{MCG_CAP, McgCap, MsrAccess},
};
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Silicon {
    Raven,
    Picasso,
    Raven2,
}
#[derive(Debug, Clone, Copy)]
pub struct Identity {
    pub silicon: Silicon,
    pub raven2_cpuid: bool,
    pub am4: bool,
    pub reduced_io: bool,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    CpuIdentity,
    McaBanks(u8),
    MemoryBoundary,
}
impl Identity {
    /// Mirrors the original CPUID versus SMU-silicon distinction, including
    /// reduced-I/O PCO Dali parts that retain a Family 17h model-1x CPUID.
    pub fn from_cpuid(signature: u32, extended_ebx: u32, silicon_id: u32) -> Result<Self, Error> {
        let base_family = (signature >> 8) & 0xf;
        let family = base_family
            + if base_family == 0xf {
                (signature >> 20) & 0xff
            } else {
                0
            };
        let model = (signature >> 4) & 0xf
            | if matches!(base_family, 6 | 15) {
                (signature >> 12) & 0xf0
            } else {
                0
            };
        let package = extended_ebx >> 28;
        if family != 0x17 || !matches!(model, 0x11 | 0x18 | 0x20) || !matches!(package, 0 | 2 | 3) {
            return Err(Error::CpuIdentity);
        }
        let raven2_cpuid = model >> 4 == 2;
        let silicon = if model == 0x20 || silicon_id & (1 << 30) != 0 {
            Silicon::Raven2
        } else if model == 0x18 {
            Silicon::Picasso
        } else {
            Silicon::Raven
        };
        let dali = package == 0 && (raven2_cpuid || silicon_id & (1 << 31) != 0);
        Ok(Self {
            silicon,
            raven2_cpuid,
            am4: package == 2,
            reduced_io: silicon == Silicon::Raven2 || dali,
        })
    }
}

const DEFERRED: u64 = (1 << 32) | (1 << 34) | (1 << 37);
const fn deferred(bank: u32) -> MsrUpdate {
    MsrUpdate::new(mca_config(bank).index(), 1 << 33, DEFERRED)
}
const RESET: &[MsrUpdate] = &[
    MsrUpdate::new(0x0000001b, 0, 1 << 11),
    MsrUpdate::new(0xc0010015, 0, 1 << 27),
    MsrUpdate::new(0xc0011005, (1 << 48) | (1 << 42), 0),
    MsrUpdate::new(0xc00110dc, 0x28000, 0),
    MsrUpdate::new(0xc0010074, 0, 9),
    deferred(0),
    deferred(1),
    deferred(2),
    deferred(3),
    deferred(5),
    deferred(6),
    MsrUpdate::new(0xc0010401, 0, 1 << 13),
    MsrUpdate::new(0xc0010406, 0, 1 << 6),
    MsrUpdate::new(0xc0010402, 0, 1 << 3),
    MsrUpdate::new(0xc0010407, 0, 1 << 7),
    MsrUpdate::new(LS_CFG.index(), 0, 0x100),
    MsrUpdate::new(0xc0011023, 0, 0x0000200000000000),
    MsrUpdate::new(0xc0011028, 0, 0x14),
    MsrUpdate::new(DE_CFG.index(), 0, 0x26000),
    MsrUpdate::new(0xc001102a, 0, 0x0280000000008080),
    MsrUpdate::new(0xc001102c, 0, 0x0168000000000000),
    MsrUpdate::new(0xc001102b, 0, 0x80000),
    MsrUpdate::new(0xc001102d, 0, 0x1000000100000000),
    MsrUpdate::new(0xc001102e, 0, 1),
    MsrUpdate::new(0xc0011093, 0x58d8c, 0x480003),
    MsrUpdate::new(0xc0011097, 8, 0xc54),
    MsrUpdate::new(DE_CFG.index(), 1 << 14, 0),
    MsrUpdate::new(LS_CFG.index(), 1 << 28, 0),
];

pub fn reset_registers(cpu: &impl MsrAccess) {
    RESET.iter().for_each(|update| update.apply(cpu));
}
pub fn bsp_init(cpu: &impl MsrAccess) {
    reset_registers(cpu);
    // No encrypted memory is configured; otherwise every DMA/MTRR consumer
    // would have to account for the C-bit.
    cpu.reg(SYSCFG).modify(Syscfg::SMEE::CLEAR);
}

pub fn mca_init(cpu: &impl MsrAccess) -> Result<(), Error> {
    let cap = cpu.reg(MCG_CAP).get();
    mca_init_with_cap(cpu, cap)
}

// The per-thread adapter snapshots/preflights capability before reset writes.
// Do not rediscover geometry halfway through that submitted CPU policy.
fn mca_init_with_cap(cpu: &impl MsrAccess, cap: u64) -> Result<(), Error> {
    let banks = (cap & McgCap::COUNT.mask) as u8;
    // This native flow accesses MISC1 for banks 15/16. Reject a mismatched CPU
    // before touching any MCAX register, rather than faulting midway through.
    if !(17..=32).contains(&banks) {
        return Err(Error::McaBanks(banks));
    }
    for bank in 0..u32::from(banks) {
        let config = cpu.reg(mca_config(bank)).extract();
        let mut value = config;
        value.modify(McaConfig::MCAX_ENABLE::CLEAR);
        if config.is_set(McaConfig::TRANSPARENT_LOGGING_SUPPORTED) {
            value.modify(McaConfig::TRANSPARENT_LOGGING_ENABLE::CLEAR);
        }
        value.modify(McaConfig::MCAX_ENABLE::SET);
        if config.is_set(McaConfig::DEFERRED_LOGGING_SUPPORTED) {
            value.modify(McaConfig::LOG_DEFERRED_IN_MCA_STAT::SET);
        }
        cpu.reg(mca_config(bank)).set(value.get());
        if bank != 15 {
            cpu.reg(mca_misc0(bank))
                .modify(McaMisc::THRESHOLD_INT_TYPE::CLEAR);
        }
    }
    cpu.reg(mca_misc1(15))
        .modify(McaMisc::THRESHOLD_INT_TYPE::CLEAR);
    cpu.reg(mca_misc1(16))
        .modify(McaMisc::THRESHOLD_INT_TYPE::CLEAR);
    cpu.reg(MCA_INTR_CFG)
        .modify(McaIntrCfg::THRESHOLD_LVT_VALID::SET);
    MsrUpdate::new(mca_ctl_mask(0).index(), 0, (1 << 9) | (1 << 10)).apply(cpu);
    MsrUpdate::new(mca_ctl_mask(6).index(), 0, 1 << 6).apply(cpu);
    if (cap >> 4) & 0xf == 0 {
        for bank in 8..=15 {
            MsrUpdate::new(mca_ctl_mask(bank).index(), 0, 1 << 7).apply(cpu);
        }
    }
    Ok(())
}

/// Native per-core tail. The platform serializes this across SMT siblings;
/// it installs identical AMD cache routing before invoking this function.
pub fn per_cpu_tail(cpu: &impl MsrAccess, primary_thread: bool) {
    if primary_thread {
        let value = cpu.reg(DPM_CFG).get();
        cpu.reg(DPM_CFG).set(0);
        for index in 0..21 {
            cpu.reg(DPM_WAC_INDEX).set(index);
            cpu.reg(DPM_WAC_DATA).set(0);
        }
        cpu.reg(DPM_CFG).set(value | 0x9000000000000000);
    }
    MsrUpdate::new(DE_CFG.index(), 0, 1 << 3).apply(cpu);
    MsrUpdate::new(LS_CFG.index(), 0, (1 << 13) | (1 << 15)).apply(cpu);
    cpu.reg(BU_CFG).modify(BuCfg::COMBINE_CR0_CD::SET);
    cpu.reg(CSTATE_CONFIG).set(0x484848);
    cpu.reg(PSTATE_0).set(cpu.reg(PSTATE_0).get());
    MsrUpdate::new(BU_CFG3.index(), 1 << 2, 1 << 3).apply(cpu);
    MsrUpdate::new(DC_CFG.index(), 0, (1 << 13) | (1 << 15)).apply(cpu);
}

/// Decode the PSP-established memory ceiling without narrowing high RAM to u32.
pub fn memory_limits(cpu: &impl MsrAccess) -> Result<(u64, Option<u64>), Error> {
    crate::fam17h::msr::memory_limits(cpu).map_err(|_| Error::MemoryBoundary)
}

pub fn enable_ecam(cpu: &impl MsrAccess) {
    // AMD MMIO_CONF_BASE: 64 buses (BusRange=6), enabled. Not PCIEXBAR.
    crate::fam17h::msr::enable_ecam(cpu, super::ECAM, super::ECAM_BUSES.into());
}
