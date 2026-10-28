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
    /// MCA_CONFIG (C000_2004 + 16·bank).
    pub McaConfig [
        MCAX OFFSET(0) NUMBITS(1) [],
        TRANSPARENT_LOGGING_SUPPORTED OFFSET(1) NUMBITS(1) [],
        DEFERRED_LOGGING_SUPPORTED OFFSET(2) NUMBITS(1) [],
        MCAX_ENABLE OFFSET(32) NUMBITS(1) [],
        TRANSPARENT_LOGGING_ENABLE OFFSET(33) NUMBITS(1) [],
        LOG_DEFERRED_IN_MCA_STAT OFFSET(34) NUMBITS(1) []
    ],
    /// MCA_IPID (C000_2005 + 16·bank).
    pub McaIpid [
        HARDWARE_ID OFFSET(32) NUMBITS(12) [],
        MCA_TYPE OFFSET(48) NUMBITS(16) []
    ],
    /// MCA_MISC0/MISC1 (C000_2003 / C000_200A + 16·bank).
    pub McaMisc [
        BLK_PTR OFFSET(24) NUMBITS(8) []
    ],
    pub McaIntrCfg [
        THRESHOLD_LVT_VALID OFFSET(3) NUMBITS(1) []
    ],
    /// Core::X86::Msr::TW_CFG.
    pub TwCfg [
        COMBINE_CR0_CD OFFSET(49) NUMBITS(1) []
    ],
    /// Core::X86::Msr::CSTATE_CONFIG.
    pub CstateConfig [
        CCR0_CC1DFSID OFFSET(0) NUMBITS(6) [],
        CCR0_CC6EN OFFSET(6) NUMBITS(1) [],
        CCR1_CC1DFSID OFFSET(8) NUMBITS(6) [],
        CCR1_CC6EN OFFSET(14) NUMBITS(1) [],
        CCR2_CC1DFSID OFFSET(16) NUMBITS(6) [],
        CCR2_CC6EN OFFSET(22) NUMBITS(1) []
    ],
    /// Core::X86::Msr::CStateBaseAddr.
    pub CstateBaseAddr [
        CSTATE_ADDR OFFSET(0) NUMBITS(16) []
    ],
    /// Core::X86::Msr::CpuWdtCfg.
    pub CpuWdtCfg [
        EN OFFSET(0) NUMBITS(1) [],
        TIME_BASE OFFSET(1) NUMBITS(2) [],
        COUNT_SEL OFFSET(3) NUMBITS(4) [],
        /// Rome/Milan only (reserved on Raven and ZP).
        SEVERITY OFFSET(7) NUMBITS(3) []
    ],
    /// Core::X86::Msr::DPM_CFG.
    pub DpmCfg [
        JITTER_EN OFFSET(60) NUMBITS(1) [],
        /// The weights are loaded; accumulation starts. Written last.
        CFG_LOCKED OFFSET(63) NUMBITS(1) []
    ]
];

/// RDRAND needs the PSP MMIO base of the core's die here.
pub const PSP_ADDRESS: Msr = Msr::new(0xc001_10a2);
/// The processor name string, six MSRs of 8 ASCII bytes.
pub const NAME_STRING: Msr = Msr::new(0xc001_0030);
/// ACPI C-state control I/O base.
pub const CSTATE_BASE_ADDR: Msr<CstateBaseAddr::Register> = Msr::new(0xc001_0073);
pub const CPU_WDT_CFG: Msr<CpuWdtCfg::Register> = Msr::new(0xc001_0074);
/// The SEV-SNP reverse map table (Milan).
pub const RMP_BASE: Msr = Msr::new(0xc001_0132);
pub const RMP_END: Msr = Msr::new(0xc001_0133);
pub const CSTATE_CONFIG: Msr<CstateConfig::Register> = Msr::new(0xc001_0296);
pub const MCODE_CTL: Msr = Msr::new(0xc001_1000);
pub const LS_CFG: Msr = Msr::new(0xc001_1020);
pub const IC_CFG: Msr = Msr::new(0xc001_1021);
pub const DC_CFG: Msr = Msr::new(0xc001_1022);
pub const TW_CFG: Msr<TwCfg::Register> = Msr::new(0xc001_1023);
pub const DE_CFG: Msr = Msr::new(0xc001_1029);
pub const BU_CFG3: Msr = Msr::new(0xc001_102b);
pub const DPM_CFG: Msr<DpmCfg::Register> = Msr::new(0xc001_1074);
pub const DPM_WAC_ACC_INDEX: Msr = Msr::new(0xc001_1076);
pub const DPM_WAC_DATA: Msr = Msr::new(0xc001_1077);
/// Unnamed; Milan's PCD 0x302 controls bit 13.
pub const C001_10DD: Msr = Msr::new(0xc001_10dd);
pub const LS_CFG3: Msr = Msr::new(0xc001_10e5);
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
pub const fn mca_ipid(bank: u32) -> Msr<McaIpid::Register> {
    mcax(bank, 5)
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
