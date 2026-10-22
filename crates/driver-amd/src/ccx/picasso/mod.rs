// SPDX-License-Identifier: MIT OR Apache-2.0
//! Native Zen/Zen+ CPU policy (AGESA AmdCcxZenRv).
//! AMD MCAX, SYSCFG and MMCONFIG must not use legacy Intel CPU programming.
pub mod thread;
#[cfg(test)]
mod tests;

use super::{
    self as ccx, CAC_WEIGHT_COUNT, CacWeights, Ccx, L3Owner, Mca, Thread,
    msr::{self, BU_CFG3, DC_CFG, DE_CFG, LS_CFG, MsrUpdate, mca_config, update},
};
use crate::soc::picasso::{ECAM, ECAM_BUSES};
use fstart_arch::x86::cpu::{
    amd::msr::{SYSCFG, Syscfg},
    msr::MsrAccess,
};
use tock_registers::interfaces::ReadWriteable;

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
    update(mca_config(bank).index(), 1 << 33, DEFERRED)
}
/// The reset MSR table of the vendor CCX code, every thread.
pub const RESET: &[MsrUpdate] = &[
    update(0x0000001b, 0, 1 << 11),
    update(0xc0010015, 0, 1 << 27),
    update(0xc0011005, (1 << 48) | (1 << 42), 0),
    update(0xc00110dc, 0x28000, 0),
    update(0xc0010074, 0, 9),
    deferred(0),
    deferred(1),
    deferred(2),
    deferred(3),
    deferred(5),
    deferred(6),
    update(0xc0010401, 0, 1 << 13),
    update(0xc0010406, 0, 1 << 6),
    update(0xc0010402, 0, 1 << 3),
    update(0xc0010407, 0, 1 << 7),
    update(LS_CFG.index(), 0, 0x100),
    update(0xc0011023, 0, 0x0000200000000000),
    update(0xc0011028, 0, 0x14),
    update(DE_CFG.index(), 0, 0x26000),
    update(0xc001102a, 0, 0x0280000000008080),
    update(0xc001102c, 0, 0x0168000000000000),
    update(0xc001102b, 0, 0x80000),
    update(0xc001102d, 0, 0x1000000100000000),
    update(0xc001102e, 0, 1),
    update(0xc0011093, 0x58d8c, 0x480003),
    update(0xc0011097, 8, 0xc54),
    update(DE_CFG.index(), 1 << 14, 0),
    update(LS_CFG.index(), 1 << 28, 0),
];

pub fn reset_registers(cpu: &impl MsrAccess) {
    msr::apply(cpu, RESET.iter().copied());
}
pub fn bsp_init(cpu: &impl MsrAccess) {
    reset_registers(cpu);
    // No encrypted memory is configured; otherwise every DMA/MTRR consumer
    // would have to account for the C-bit.
    cpu.reg(SYSCFG).modify(Syscfg::SMEE::CLEAR);
}

/// The Picasso parameters of the Family 17h CCX init.
pub const CCX: Ccx = Ccx {
    mca: Mca {
        // MISC1 of banks 15 and 16.
        banks: 17..=32,
        no_misc0: 15..=15,
        // LS SystemReadDataErrorT0/T1, and FP.
        ctl_masks: &[(0, (1 << 9) | (1 << 10)), (6, 1 << 6)],
        l3_banks: 8..=15,
        l3_owner: L3Owner::NoExtraBanks,
    },
    ecam: ECAM,
    ecam_buses: ECAM_BUSES as u16,
    // ACPI_IO_BASE=0x400, CSTATE_CONTROL=+0x10.
    cstate_io_base: 0x410,
    watchdog: None,
    dpm_cfg: 0x9000_0000_0000_0000,
    tail: &[
        update(DE_CFG.index(), 0, 1 << 3),
        update(LS_CFG.index(), 0, (1 << 13) | (1 << 15)),
        update(BU_CFG3.index(), 1 << 2, 1 << 3),
        update(DC_CFG.index(), 0, (1 << 13) | (1 << 15)),
    ],
};

/// Picasso clears the DPM weights.
pub const CAC_WEIGHTS: CacWeights = [0; CAC_WEIGHT_COUNT];

pub fn mca_init(cpu: &impl MsrAccess, thread: Thread) -> Result<(), Error> {
    ccx::mca_init(cpu, &CCX.mca, thread)
        .map_err(|ccx::Error::McaBanks(banks)| Error::McaBanks(banks))
}

/// Decode the PSP-established memory ceiling without narrowing high RAM to u32.
pub fn memory_limits(cpu: &impl MsrAccess) -> Result<(u64, Option<u64>), Error> {
    msr::memory_limits(cpu).map_err(|_| Error::MemoryBoundary)
}

pub fn enable_ecam(cpu: &impl MsrAccess) {
    // AMD MMIO_CONF_BASE: 64 buses (BusRange=6), enabled. Not PCIEXBAR.
    msr::enable_ecam(cpu, ECAM, ECAM_BUSES.into());
}
