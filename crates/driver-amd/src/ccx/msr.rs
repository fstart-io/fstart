// SPDX-License-Identifier: MIT OR Apache-2.0
//! Family 17h MSRs, and the memory/configuration MSRs every Zen generation
//! programs the same way. The AMD64 ones (SYSCFG, TOP_MEM, ...) and the MSR
//! access itself are `fstart_arch::x86::cpu`'s.
use crate::table;
use fstart_arch::x86::cpu::{
    amd::msr::{MMIO_CFG_BASE, MmioCfgBase, SYSCFG, Syscfg, TOP_MEM, TOP_MEM2, TopMem, TopMem2},
    msr::{Msr, MsrAccess},
};
use tock_registers::{
    interfaces::{Readable, Writeable},
    register_bitfields,
};

register_bitfields![u64,
    pub McaConfig [
        MCAX OFFSET(0) NUMBITS(1) [],
        TRANSPARENT_LOGGING_SUPPORTED OFFSET(1) NUMBITS(1) [],
        DEFERRED_LOGGING_SUPPORTED OFFSET(2) NUMBITS(1) [],
        MCAX_ENABLE OFFSET(32) NUMBITS(1) [],
        TRANSPARENT_LOGGING_ENABLE OFFSET(33) NUMBITS(1) [],
        LOG_DEFERRED_IN_MCA_STAT OFFSET(34) NUMBITS(1) []
    ],
    /// MCA_MISC0/MISC1 threshold counter.
    pub McaMisc [
        THRESHOLD_INT_TYPE OFFSET(24) NUMBITS(8) []
    ],
    pub McaIntrCfg [
        THRESHOLD_LVT_VALID OFFSET(3) NUMBITS(1) []
    ],
    pub BuCfg [
        /// TW_CFG::CombineCr0Cd.
        COMBINE_CR0_CD OFFSET(49) NUMBITS(1) []
    ],
    pub DpmCfg [
        CFG_WEIGHTS_VALID OFFSET(63) NUMBITS(1) []
    ]
];

/// RDRAND needs the PSP MMIO base of the core's die here.
pub const PSP_ADDRESS: Msr = Msr::new(0xc001_10a2);
/// ACPI C-state control I/O base.
pub const CSTATE_BASE_ADDRESS: Msr = Msr::new(0xc001_0073);
pub const CPU_WDT_CFG: Msr = Msr::new(0xc001_0074);
pub const CSTATE_CONFIG: Msr = Msr::new(0xc001_0296);
pub const LS_CFG: Msr = Msr::new(0xc001_1020);
pub const DC_CFG: Msr = Msr::new(0xc001_1022);
pub const BU_CFG: Msr<BuCfg::Register> = Msr::new(0xc001_1023);
pub const DE_CFG: Msr = Msr::new(0xc001_1029);
pub const BU_CFG3: Msr = Msr::new(0xc001_102b);
pub const DPM_CFG: Msr<DpmCfg::Register> = Msr::new(0xc001_1074);
pub const DPM_WAC_INDEX: Msr = Msr::new(0xc001_1076);
pub const DPM_WAC_DATA: Msr = Msr::new(0xc001_1077);
pub const MCA_INTR_CFG: Msr<McaIntrCfg::Register> = Msr::new(0xc000_0410);

/// MCAX register `offset` of MCA `bank` (MSRC000_2xxx).
const fn mcax<R: tock_registers::RegisterLongName>(bank: u32, offset: u32) -> Msr<R> {
    Msr::new(0xc000_2000 + bank * 16 + offset)
}
pub const fn mca_misc0(bank: u32) -> Msr<McaMisc::Register> {
    mcax(bank, 3)
}
pub const fn mca_config(bank: u32) -> Msr<McaConfig::Register> {
    mcax(bank, 4)
}
pub const fn mca_misc1(bank: u32) -> Msr<McaMisc::Register> {
    mcax(bank, 10)
}
/// MCA_CTL_MASK of `bank`: errors whose bit is set are not reported.
pub const fn mca_ctl_mask(bank: u32) -> Msr {
    Msr::new(0xc001_0400 + bank)
}

/// One record of a vendor MSR table ([`crate::table`]); the meaning of
/// most of the bits isn't known. `C` is its condition, if any.
pub type MsrUpdate<C = ()> = table::Record<Msr, u64, C>;
/// `msr = (msr & !clear) | set`
pub const fn update(msr: u32, clear: u64, set: u64) -> MsrUpdate {
    table::rmw(Msr::new(msr), clear, set)
}
/// Apply a vendor MSR table in order.
pub fn apply(cpu: &impl MsrAccess, records: impl IntoIterator<Item = MsrUpdate>) {
    table::apply(records, |msr| cpu.reg(msr));
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryBoundary;

/// Decode the PSP-established memory ceiling without narrowing high RAM to u32.
pub fn memory_limits(cpu: &impl MsrAccess) -> Result<(u64, Option<u64>), MemoryBoundary> {
    let low = cpu.reg(TOP_MEM).read(TopMem::ADDRESS) << TopMem::ADDRESS.shift;
    if low == 0 || low >= 1 << 32 {
        return Err(MemoryBoundary);
    }
    let high = cpu
        .reg(SYSCFG)
        .is_set(Syscfg::TOM2_EN)
        .then(|| cpu.reg(TOP_MEM2).read(TopMem2::ADDRESS) << TopMem2::ADDRESS.shift);
    Ok((low, high))
}

/// AMD MMIO_CONF_BASE (not an Intel PCIEXBAR): `buses` must be a power of two
/// between 1 and 256 and `base` aligned to the window size.
pub fn enable_ecam(cpu: &impl MsrAccess, base: u64, buses: u16) {
    assert!(buses.is_power_of_two() && buses <= 256);
    assert!(base & ((u64::from(buses) << 20) - 1) == 0);
    cpu.reg(MMIO_CFG_BASE).write(
        MmioCfgBase::ADDRESS.val(base >> MmioCfgBase::ADDRESS.shift)
            + MmioCfgBase::BUS_RANGE.val(buses.trailing_zeros().into())
            + MmioCfgBase::ENABLE::SET,
    );
}
