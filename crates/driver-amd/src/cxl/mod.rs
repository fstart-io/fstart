// SPDX-License-Identifier: MIT OR Apache-2.0
//! CXL on AMD I/O dies: the parts of AMD's CXL support that do not depend
//! on the IOD's version, and one module per version for what does.
//!
//! AMD splits CXL four ways (reports/research/CXL.md §1.1):
//! 1. the ABL maps CXL memory from the APCB's CXL region descriptors
//!    (`fstart_amdfw_format::apcb::CxlRegion`, written at build time) and
//!    reports the windows in APOB `9/0x18` ([`crate::apob::cxl`]);
//! 2. after MPIO training x86 classifies the CXL ports, programs their
//!    policy and QoS registers, gives restricted CXL hosts (CXL 1.1) an
//!    RCRB and tells MPIO the CXL devices ([`mpio`], message `0x2C`) — the
//!    registers are the version's ([`wh`]);
//! 3. after PCI enumeration x86 re-reports the devices with their real
//!    BDF (message `0x0D`), builds each CXL host bridge's component
//!    registers in DRAM, which MPIO serves behind a trapped MMIO window
//!    ([`host`], message `0x30`), and publishes the CEDT;
//! 4. the OS binds its CXL drivers to that.
//!
//! After enumeration the vendor DXE also sizes every device and rewrites
//! the APCB's regions when they changed ([`apcb`]: the sizing rules, the
//! region builder and the decision; the platform writes the flash and
//! warm-resets), enables the devices' memory: DVSEC ranges and endpoint
//! HDM decoders ([`endpoint`]), switch decoders ([`switch`]), and gives the
//! OS the windows' performance from the devices' CDAT ([`hmat`]).
//! (reports/research/CXL_COMPLETE.md.)
//!
//! Versions: [`wh`]. RS and BRH use the same
//! messages and host bridge emulation with other registers (CXL.md §3).

pub mod apcb;
pub mod endpoint;
pub mod hmat;
pub mod host;
pub mod mpio;
pub mod switch;
pub mod wh;

/// The CXL protocol version a port negotiated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Version {
    /// CXL 1.1: a restricted CXL host (RCH) with an RCRB.
    Rch = 1,
    /// CXL 2.0: a virtual hierarchy (68B flit).
    V2 = 2,
    /// CXL 3.x (256B flit).
    V3 = 3,
}

/// The CXL device type behind a port.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceType {
    /// Not probed (the MPIO list's 0).
    Unknown = 0,
    /// CXL.cache only (accelerator).
    Type1 = 1,
    /// CXL.cache and CXL.mem (accelerator with memory).
    Type2 = 2,
    /// CXL.mem only (memory expander), and switches.
    Type3 = 3,
}

impl DeviceType {
    /// From the device DVSEC's CXL Capability: cache 1, mem 4 (the vendor's
    /// `& 5`).
    pub fn of_capability(capability: u16) -> Self {
        use fstart_pci::cxl::dvsec::DeviceCapability as Cap;
        let cap = tock_registers::LocalRegisterCopy::<u16, Cap::Register>::new(capability);
        match (cap.is_set(Cap::CACHE_CAPABLE), cap.is_set(Cap::MEM_CAPABLE)) {
            (true, false) => Self::Type1,
            (true, true) => Self::Type2,
            (false, true) => Self::Type3,
            (false, false) => Self::Unknown,
        }
    }
}

/// The CXL input block of AMD's silicon library (openSIL class `0x11`,
/// 0x1A bytes; defaults identical in the Dell and GIGABYTE Venice images,
/// `SilVenicePei` `0xFA2658`): the per-port CXL policy x86 programs after
/// training.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    /// Report CXL errors to the RCEC (otherwise its CXL config is cleared).
    pub report_errors_to_rcec: bool,
    /// Non-zero: arbitration weights of CXL.io and CXL.cache/mem, and the
    /// CNLI token advertisement.
    pub io_arb_weights: u8,
    pub ca_mem_arb_weights: u8,
    pub cnli_token_advertisement: u8,
    // The input block's protocol error reporting and AER report
    // mechanism (`PcdAmdCxlProtocolErrorReporting`) are not here: CXL
    // ports report through the same AER path as PCIe ones, whose one
    // mechanism the SoC decides (`nbio::wh::init::Policy::aer_mechanism`,
    // downgraded to OS-first where its SMM handler cannot run).
    pub camem_rx_optimization: bool,
    pub tx_direct_out: bool,
    pub qos: Qos,
    /// The CXL.mem response a port gives in power management.
    pub pm_camem_response: Option<PmCaMemResponse>,
}

/// The two CXL.mem power-management responses (`PORT_HI 0x1C4404D0`
/// bits 9/11; their meaning is not documented).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PmCaMemResponse {
    /// Input 0: bit 9 set, bit 11 clear.
    Bit9,
    /// Input 1: bit 9 clear, bit 11 set.
    Bit11,
}

/// The CXL QoS of a trained port (WH; written on every trained port, also
/// with all zero).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Qos {
    pub normal_limit: u16,
    pub high_limit: u16,
    pub timer_limit: u16,
    pub sched_gap: u16,
    pub variable_gap: bool,
    pub rdsp_inc_mode: bool,
    pub timer_dec_num: u8,
    pub rdsp_inc_num: u8,
    pub wrrsp_inc_num: u8,
}

impl Policy {
    /// The vendors' input block: `01 00 00 00 00 01 00 .. 00 01 00`
    /// (the last 1 is the output AER mechanism, not an input here), no PM
    /// response override.
    pub const DEFAULT: Self = Self {
        report_errors_to_rcec: true,
        io_arb_weights: 0,
        ca_mem_arb_weights: 0,
        cnli_token_advertisement: 0,
        camem_rx_optimization: false,
        tx_direct_out: false,
        qos: Qos {
            normal_limit: 0,
            high_limit: 0,
            timer_limit: 0,
            sched_gap: 0,
            variable_gap: false,
            rdsp_inc_mode: false,
            timer_dec_num: 0,
            rdsp_inc_num: 0,
            wrrsp_inc_num: 0,
        },
        pm_camem_response: None,
    };

    /// The WH CXL block's build values in both firmwares seen for it
    /// (Dell named PCDs, GIGABYTE by its datum runs, CXL_COMPLETE.md
    /// §2.8, `pcd-cxl.json`): QoS normal limit 4, high limit 10,
    /// TxOptimizeDirectOutEn 1, the rest as [`Self::DEFAULT`] (openSIL's
    /// zeros). The SoC picks it for its boards.
    pub const WH: Self = Self {
        tx_direct_out: true,
        qos: Qos {
            normal_limit: 4,
            high_limit: 10,
            ..Self::DEFAULT.qos
        },
        ..Self::DEFAULT
    };
}

impl Default for Policy {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// The policy of the CXL steps after PCI enumeration (the vendor DXE's
/// PCDs, CXL_COMPLETE.md §2.1, §2.2, §2.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DxePolicy {
    /// `PcdAmdCxlAspm`: ASPM on CXL links (`0x1C44_0280` bit 24 clear).
    pub aspm: bool,
    /// `PcdAmdCxlIoL1`, `...IoL2`, `...CaMemL1`, `...CaMemL2`.
    pub io_l1: bool,
    pub io_l2: bool,
    pub camem_l1: bool,
    pub camem_l2: bool,
    /// `PcdAmdCxlMemIsolationEnable`: the firmware runs CXL.mem isolation
    /// on the root ports ([`wh::isolate`]); off, the OS may take it
    /// through `_OSC`.
    pub mem_isolation: bool,
    /// How, with [`Self::mem_isolation`].
    pub isolation: IsolationPolicy,
    /// `PcdAmdCxlDvsecLock`: lock the devices' DVSEC after enabling.
    pub dvsec_lock: bool,
    /// `PcdAmdCxlHdmDecoderLockOnCommit` (and APCB token `0x3267257D`
    /// "CXL Memory Online/Offline" 0): lock endpoint decoders on commit.
    pub lock_on_commit: bool,
    /// `PcdAmdCxlSpaEnable`: decoders take the window's system addresses;
    /// off is AMD's normalized-address mode, which needs the OS's AMD
    /// address translation.
    pub spa: bool,
    /// `PcdAmdCxlDualHdmRanges`: a device's second DVSEC range is a pool.
    pub dual_hdm_ranges: bool,
    /// `PcdAmdCxlSwitchInterleave`: interleave identical devices below a
    /// switch.
    pub switch_interleave: bool,
    /// `PcdCxlCdatSupport`: read the devices' CDAT (sizing, media type,
    /// HMAT).
    pub cdat: bool,
}

impl DxePolicy {
    /// The WH CXL steps' build values where named (Dell; GIGABYTE where
    /// its tokens align: the same). The SoC picks it for its boards.
    pub const WH: Self = Self {
        aspm: true,
        io_l1: true,
        io_l2: true,
        camem_l1: true,
        camem_l2: true,
        mem_isolation: false,
        isolation: IsolationPolicy::WH,
        dvsec_lock: true,
        lock_on_commit: true,
        spa: true,
        dual_hdm_ranges: false,
        switch_interleave: true,
        cdat: true,
    };
}

/// CXL.mem isolation of the root ports when the firmware runs it
/// (AmdNbioDxe `0x2AE0`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IsolationPolicy {
    /// `PcdAmdCxlMemIsolationFwNotification`: the isolation interrupt.
    pub fw_notification: bool,
    /// `PcdAmdCxlMemIsolationLinkdown`: isolate on link down.
    pub link_down: bool,
    /// `PcdAmdCxlMemIsolationTransactionTimeoutEnable` with
    /// `...TransactionTimeoutValue` (PCIe Completion Timeout encoding).
    pub timeout: Option<u8>,
}

impl IsolationPolicy {
    /// The WH build values (Dell's, all 0; GIGABYTE's do not align).
    pub const WH: Self = Self {
        fw_notification: false,
        link_down: false,
        timeout: None,
    };
}

impl Default for DxePolicy {
    fn default() -> Self {
        Self::WH
    }
}

/// Tell the SMU a persistent-memory pool is about to be enabled
/// (AmdNbioDxe `CxlSmuEnableScm` `0x3640`: message `0x22`, arguments 0;
/// the vendor's CxlV2Manager sends it per pool of type 1 or 3).
pub fn enable_scm<M: crate::smu::SmuMessages + ?Sized>(smu: &M) -> Result<(), crate::smu::Error> {
    crate::smu::v14::msg::cxl_enable_scm(smu)
}

/// AMD's CXL error namespace (`0x434C_GGCC`, "CL"), as the vendor error
/// log reports them (CXL.md §2.12).
pub mod error {
    /// A restricted CXL host's device has no CXL DVSEC.
    pub const NO_DVSEC: u32 = 0x434c_0501;
    /// No MMIO for a restricted CXL host's MEMBAR0.
    pub const NO_MMIO: u32 = 0x434c_0502;
    pub const DOE: u32 = 0x434c_0601;
    pub const DOE_BUSY: u32 = 0x434c_0602;
    pub const CDAT_UNSUPPORTED: u32 = 0x434c_0603;
    pub const POOLS: u32 = 0x434c_0901;
    pub const HDM_CAPABILITY: u32 = 0x434c_0907;
    pub const ENABLE_ENDPOINT: u32 = 0x434c_0908;
    pub const NOT_COMMITTED: u32 = 0x434c_0909;
    pub const GCD_CONVERT: u32 = 0x434c_090a;
    /// A DVSEC range larger than 2^56.
    pub const RANGE_SIZE: u32 = 0x434c_090b;
    /// A switch's upstream port has no HDM capability.
    pub const SWITCH_HDM_CAPABILITY: u32 = 0x434c_0a02;
    pub const SWITCH_NOT_COMMITTED: u32 = 0x434c_0a03;
    /// No memory range of a device enabled.
    pub const NOTHING_ENABLED: u32 = 0x8000_000e;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_types_from_the_dvsec_capability() {
        assert_eq!(DeviceType::of_capability(0b001), DeviceType::Type1);
        assert_eq!(DeviceType::of_capability(0b101), DeviceType::Type2);
        assert_eq!(DeviceType::of_capability(0b110), DeviceType::Type3);
        assert_eq!(DeviceType::of_capability(0b010), DeviceType::Unknown);
        assert_eq!(
            (
                Policy::WH.qos.normal_limit,
                Policy::WH.qos.high_limit,
                Policy::WH.tx_direct_out,
                Policy::WH.report_errors_to_rcec
            ),
            (4, 10, true, true)
        );
    }
}
