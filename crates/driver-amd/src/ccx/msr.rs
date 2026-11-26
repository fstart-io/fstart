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
    /// MCA_MISC0/MISC1 (C000_2003 / C000_200A + 16·bank): a bank's error
    /// thresholding block (PPR 55901 MCA::UMC::MCA_MISC0_UMC).
    pub McaMisc [
        BLK_PTR OFFSET(24) NUMBITS(8) [],
        /// Counts up to FFFh; the threshold is FFFh minus the start value.
        ERR_CNT OFFSET(32) NUMBITS(12) [],
        OVRFLW OFFSET(48) NUMBITS(1) [],
        THRESHOLD_INT_TYPE OFFSET(49) NUMBITS(2) [
            None = 0,
            Apic = 1,
            Smi = 2
        ],
        CNT_EN OFFSET(51) NUMBITS(1) [],
        LVT_OFFSET OFFSET(52) NUMBITS(4) [],
        INT_P OFFSET(60) NUMBITS(1) [],
        LOCKED OFFSET(61) NUMBITS(1) [],
        CNT_P OFFSET(62) NUMBITS(1) [],
        VALID OFFSET(63) NUMBITS(1) []
    ],
    pub McaIntrCfg [
        THRESHOLD_LVT_VALID OFFSET(3) NUMBITS(1) []
    ],
    /// Core::X86::Msr::TW_CFG.
    pub TwCfg [
        /// Meaning not public. Zen 6: AmdCcxZen6Dxe sets it on every
        /// thread with SMM_ADDR/SMM_MASK (`CcxInitTsegAp`, Dell `0xed00`)
        /// and clears it after HWCR.SmmLock (`CcxSmmLockAp`, `0xe840`),
        /// instruction-checked; AmdCcxZen6Pei's lock (`0xeec170`) the same.
        SMM_SETUP OFFSET(27) NUMBITS(1) [],
        COMBINE_CR0_CD OFFSET(49) NUMBITS(1) []
    ],
    /// Core::X86::Msr::CSTATE_CONFIG.
    pub CstateConfig [
        CCR0_CC1DFSID OFFSET(0) NUMBITS(6) [],
        CCR0_CC6EN OFFSET(6) NUMBITS(1) [],
        CCR1_CC1DFSID OFFSET(8) NUMBITS(6) [],
        CCR1_CC6EN OFFSET(14) NUMBITS(1) [],
        CCR2_CC1DFSID OFFSET(16) NUMBITS(6) [],
        CCR2_CC6EN OFFSET(22) NUMBITS(1) [],
        /// WH: openSIL Zen6CStateInit (`f526e0`) programs a fourth field.
        CCR3_CC1DFSID OFFSET(24) NUMBITS(6) [],
        CCR3_CC6EN OFFSET(30) NUMBITS(1) []
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
    /// Core::X86::Msr::CPPC_CAPABILITY_1 (C001_02B0).
    pub CppcCap1 [
        LOWEST_PERF OFFSET(0) NUMBITS(8) [],
        LOWEST_NONLINEAR_PERF OFFSET(8) NUMBITS(8) [],
        NOMINAL_PERF OFFSET(16) NUMBITS(8) [],
        HIGHEST_PERF OFFSET(24) NUMBITS(8) []
    ],
    /// Core::X86::Msr::CPPC_REQUEST (C001_02B3).
    pub CppcRequest [
        MAX_PERF OFFSET(0) NUMBITS(8) [],
        MIN_PERF OFFSET(8) NUMBITS(8) [],
        DES_PERF OFFSET(16) NUMBITS(8) [],
        ENERGY_PERF_PREF OFFSET(24) NUMBITS(8) []
    ],
    /// C001_1004, the CPUID Fn0000_0001 feature override: EDX in \[31:0\],
    /// ECX in \[63:32\].
    pub CpuidFeatures [
        /// CPUID Fn0000_0001_ECX\[21\].
        X2APIC OFFSET(53) NUMBITS(1) []
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
/// Unnamed. WH's Zen6CStateInit (Dell SilVenicePei `f526e0`, instruction-
/// checked) writes it right after CSTATE_CONFIG with the same four
/// CC1DFSID/CC6EN fields, on core primaries.
pub const C001_0298: Msr<CstateConfig::Register> = Msr::new(0xc001_0298);
pub const CPPC_CAP1: Msr<CppcCap1::Register> = Msr::new(0xc001_02b0);
/// Core::X86::Msr::CPPC_ENABLE (C001_02B1).
pub const CPPC_ENABLE: Msr = Msr::new(0xc001_02b1);
/// Core::X86::Msr::CPPC_CAPABILITY_2 (C001_02B2).
pub const CPPC_CAP2: Msr = Msr::new(0xc001_02b2);
pub const CPPC_REQUEST: Msr<CppcRequest::Register> = Msr::new(0xc001_02b3);
/// Core::X86::Msr::CPPC_STATUS (C001_02B4).
pub const CPPC_STATUS: Msr = Msr::new(0xc001_02b4);
pub const MCODE_CTL: Msr = Msr::new(0xc001_1000);
pub const CPUID_FEATURES: Msr<CpuidFeatures::Register> = Msr::new(0xc001_1004);
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

/// The ECAM window of `MMIO_CFG_BASE` (`C001_0058`): its base and its PCI
/// segments of 256 buses, segment `s` at `base + s * 256 MiB`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EcamWindow {
    pub base: u64,
    /// `1..=8`.
    pub segments: u8,
}

impl EcamWindow {
    /// Most segments (`PciSegmentInfoLib`'s cap, BusRange 11).
    pub const MAX_SEGMENTS: u8 = 8;

    /// A window of `segments` (1, 2, 4 or 8) at `base`, aligned to its size.
    pub const fn new(base: u64, segments: u8) -> Option<Self> {
        let window = Self { base, segments };
        if segments == 0
            || segments > Self::MAX_SEGMENTS
            || !segments.is_power_of_two()
            || base & (window.size() - 1) != 0
            || base >> 52 != 0
        {
            return None;
        }
        Some(window)
    }

    /// Bytes: 256 MiB per segment ([`fstart_pci::ecam::SEGMENT_SIZE`]).
    pub const fn size(&self) -> u64 {
        self.segments as u64 * fstart_pci::ecam::SEGMENT_SIZE as u64
    }

    /// The buses the window decodes (`1 << BusRange`).
    pub const fn buses(&self) -> u16 {
        (self.segments as u16) << 8
    }

    /// The window `MMIO_CFG_BASE` describes, if enabled: `buses = 1 <<
    /// BusRange`, `segments = ceil(buses / 256)` capped at 8 (Dell
    /// PlatformDxe `GetPciSegmentInfo` `0x5080`, 2P_COMPLETE §2.1).
    pub fn decode(value: u64) -> Option<Self> {
        let reg = tock_registers::LocalRegisterCopy::<u64, MmioCfgBase::Register>::new(value);
        if !reg.is_set(MmioCfgBase::ENABLE) {
            return None;
        }
        let buses = 1u32 << reg.read(MmioCfgBase::BUS_RANGE);
        Some(Self {
            base: reg.read(MmioCfgBase::ADDRESS) << MmioCfgBase::ADDRESS.shift,
            segments: buses.div_ceil(256).min(u32::from(Self::MAX_SEGMENTS)) as u8,
        })
    }
}

/// The ECAM window the CPU decodes now, `None` if `MMIO_CFG_BASE` is off.
pub fn ecam_window(cpu: &impl MsrAccess) -> Option<EcamWindow> {
    EcamWindow::decode(cpu.reg(MMIO_CFG_BASE).get())
}

/// Program `MMIO_CFG_BASE` for `window` (BusRange 8..=11).
pub fn enable_ecam_window(cpu: &impl MsrAccess, window: EcamWindow) {
    cpu.reg(MMIO_CFG_BASE).write(
        MmioCfgBase::ADDRESS.val(window.base >> MmioCfgBase::ADDRESS.shift)
            + MmioCfgBase::BUS_RANGE.val(window.buses().trailing_zeros().into())
            + MmioCfgBase::ENABLE::SET,
    );
}

/// The ECAM the way the vendor's x86 PEI sets it (`AmdSocSp7WHPei`
/// `0xca9d60`, 2P_COMPLETE §2.1): an `MMIO_CFG_BASE` already enabled
/// (by the ABL, from the APCB's `DfPciMmio*` tokens) is kept, base, bus
/// range and all; otherwise `fallback`'s base is written with the bus range
/// already there when it names 1 to 8 segments (BusRange 8..=11), else
/// `fallback`'s. Returns the window the CPU decodes.
pub fn adopt_ecam(cpu: &impl MsrAccess, fallback: EcamWindow) -> EcamWindow {
    let raw = cpu.reg(MMIO_CFG_BASE).get();
    if let Some(window) = EcamWindow::decode(raw) {
        return window;
    }
    let range = tock_registers::LocalRegisterCopy::<u64, MmioCfgBase::Register>::new(raw)
        .read(MmioCfgBase::BUS_RANGE);
    let window = (8..=11)
        .contains(&range)
        .then(|| EcamWindow::new(fallback.base, 1 << (range - 8)))
        .flatten()
        .unwrap_or(fallback);
    enable_ecam_window(cpu, window);
    window
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

#[cfg(test)]
mod tests {
    use super::*;
    use core::cell::Cell;

    struct Msr(Cell<u64>);
    impl MsrAccess for Msr {
        fn read(&self, index: u32) -> u64 {
            assert_eq!(index, MMIO_CFG_BASE.index());
            self.0.get()
        }
        fn write(&self, index: u32, value: u64) {
            assert_eq!(index, MMIO_CFG_BASE.index());
            self.0.set(value)
        }
    }

    /// The donors' APCB windows: Dell 2 segments at 16 TiB, GIGABYTE one at
    /// `0x2FFC_8000_0000`.
    #[test]
    fn ecam_windows_follow_the_msr_or_the_fallback() {
        let dell = EcamWindow::new(0x1000_0000_0000, 2).unwrap();
        let gbt = EcamWindow::new(0x2ffc_8000_0000, 1).unwrap();
        assert_eq!(EcamWindow::new(0x2ffc_9000_0000, 2), None);
        assert_eq!(EcamWindow::new(0x1000_0000_0000, 3), None);
        assert_eq!(dell.size(), 512 << 20);
        // Enabled by the ABL: kept.
        let msr = Msr(Cell::new(0));
        enable_ecam_window(&msr, dell);
        assert_eq!(msr.0.get(), 0x1000_0000_0000 | 9 << 2 | 1);
        assert_eq!(ecam_window(&msr), Some(dell));
        assert_eq!(adopt_ecam(&msr, gbt), dell);
        assert_eq!(msr.0.get(), 0x1000_0000_0000 | 9 << 2 | 1);
        // Off with a bus range: the fallback's base, the bus range kept.
        msr.0.set(10 << 2);
        let four = adopt_ecam(&msr, EcamWindow::new(0x1000_0000_0000, 1).unwrap());
        assert_eq!((four.base, four.segments), (0x1000_0000_0000, 4));
        assert_eq!(ecam_window(&msr), Some(four));
        // Off without one: the fallback whole.
        msr.0.set(0);
        assert_eq!(adopt_ecam(&msr, gbt), gbt);
        assert_eq!(ecam_window(&msr), Some(gbt));
        // Fewer than 256 buses is one (short) segment.
        assert_eq!(
            EcamWindow::decode(0xe000_0000 | 6 << 2 | 1)
                .unwrap()
                .segments,
            1
        );
    }
}
