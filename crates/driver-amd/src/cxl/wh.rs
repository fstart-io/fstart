// SPDX-License-Identifier: MIT OR Apache-2.0
//! WH CXL ports: the registers x86 programs after MPIO training
//! (openSIL `xUSL/Cxl/Wh/CxlInitWh.c`: `CxlFindPortsWh`,
//! `CxlAssignResourcesWh`, `CxlAssignBusWh`, the QoS step), from
//! SilVenicePei of the Dell PR9XJ 1.0.8 and GIGABYTE MD25-CG0 F01 images
//! (identical registers and values; reports/research/CXL.md §2.1, §2.2,
//! witnesses in `CXL/cxl-witnesses.json`).
//!
//! The port and core registers are [`crate::nbio::wh::pcie`]'s, at the
//! port's [`PortId`] (its core, the core's RC aperture,
//! [`crate::dxio::wh::Wrapper::rc`], and its port), through MPIO
//! `PCIE_PROXY` as the vendor reaches them (`CxlFindPortsWh` MPIO API
//! `+0x28`/`+0x30`, `CxlProgramQosWh` `+0x30`); CNLI and NBIO (`0x13B1`,
//! `0x19F0`) through host SMN, the latter on the root bridge's shifted view.
//!
//! At endpoint enable (AmdNbioDxe `CxlConfigurePortFeatures` `0x2E90`) a
//! CXL root port gets its link power settings ([`configure_port`]); a CXL
//! 1.1 device's BARs are sized (`AllocateForCxlIo`, [`RciepBars`]) and the
//! RCRBs forward the windows carved for them (AmdNbioDxe `0x357B0`,
//! [`forward_rch`]). (reports/research/CXL_COMPLETE.md §2.6, §2.7.)

use fstart_pci::{ConfigRegionAccess, PciAddress};
use tock_registers::interfaces::{ReadWriteable, Writeable};
use tock_registers::{LocalRegisterCopy, register_bitfields};

use super::{DxePolicy, IsolationPolicy, PmCaMemResponse, Policy, Qos, Version};
use crate::dxio::rs::mailbox;
use crate::dxio::wh::straps::{self, Space};
use crate::dxio::wh::{Proxy, Width};
use crate::nbio::wh::pcie::{
    self, CnliPoisonA, CnliPoisonB, CnliToken, CxlArbitration, CxlCamem, CxlIsolation, CxlLink,
    CxlPm, CxlPowerStates, CxlStatus, CxlTx, PortId, QosLimits, QosMode, QosTimer,
};
use crate::smn::{SmnAccess, SmnReg};

/// The bonus x4 core, which the vendor skips.
pub const BONUS_CORE: u8 = 6;

/// NBIO `0x13B1_565C`: the root bridge's CXL error reporting to the RCEC.
pub const NBIO_CXL_RCEC: SmnReg = SmnReg::new(0x13b1_565c);
/// NBIO `0x19F0_1000`: the RCEC's HWINIT write lock (bit 1).
pub const RCEC_LOCK: SmnReg<RcecLock::Register> = SmnReg::new(0x19f0_1000);
/// NBIO `0x19F0_0160`: the RCEC's associated bus range.
pub const RCEC_BUSES: SmnReg<RcecBuses::Register> = SmnReg::new(0x19f0_0160);
/// NBIO `0x13B2_0124`: CXL sync flood (root bridges 0..4; the vendor's
/// `0x13B2_0224` branch for higher indices is unreachable on WH).
pub const SYNC_FLOOD: SmnReg<SyncFlood::Register> = SmnReg::new(0x13b2_0124);
/// Root port config `0x370`/`0x374`: the RCRB of a restricted CXL host.
pub const RCRB_LOW: u16 = 0x370;
pub const RCRB_HIGH: u16 = 0x374;
/// Size of the RCRB pair (downstream and upstream port, 4 KiB each).
pub const RCRB_SIZE: u64 = 0x2000;
/// Size of a restricted CXL host's MEMBAR0 (component registers), above
/// 4 GiB.
pub const MEMBAR0_SIZE: u64 = 0x1_0000;

register_bitfields![u32,
    /// NBIO 0x19F0_1000 (32-bit): RCEC configuration.
    pub RcecLock [
        HWINIT_LOCK OFFSET(1) NUMBITS(1) []
    ],
    /// NBIO 0x19F0_0160 (32-bit): RCEC associated bus numbers.
    pub RcecBuses [
        NEXT_BUS OFFSET(8) NUMBITS(8) [],
        LAST_BUS OFFSET(16) NUMBITS(8) []
    ],
    /// NBIO 0x13B2_0124 (32-bit): sync flood on CXL errors (0x19).
    pub SyncFlood [
        ENABLE OFFSET(0) NUMBITS(1) [],
        BIT3 OFFSET(3) NUMBITS(1) [],
        BIT4 OFFSET(4) NUMBITS(1) []
    ],
    /// Root port config 0x370 (32-bit): RCRB base low.
    pub RcrbLow [
        ENABLE OFFSET(0) NUMBITS(1) [],
        /// Base bits 31:13 (8 KiB aligned).
        BASE OFFSET(13) NUMBITS(19) []
    ]
];

/// Why a port's CXL step failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// The vendor skips the bonus core.
    BonusCore,
    /// The port's status reads all ones.
    Unavailable,
    /// An MPIO `PCIE_PROXY` request failed.
    Mpio(mailbox::Error),
}

impl From<mailbox::Error> for Error {
    fn from(e: mailbox::Error) -> Self {
        Self::Mpio(e)
    }
}

/// What the CXL ports step found on one board-marked port.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Found {
    /// The link trained as plain PCIe.
    Pcie,
    /// The link negotiated CXL.
    Cxl,
}

/// The strap IDs `CxlFindPortsWh` writes 0 on a CXL link, port space then
/// TL space (`before_reconfig` sets them from `PciePort+0x3F`: Turin's DPC
/// enable; CXL_COMPLETE.md §2.7.1).
pub const DPC_STRAPS: [(u8, Space); 2] = [(0x19, Space::Port), (0x4e, Space::Tl)];

/// The index `CxlFindPortsWh` writes strap `id` of `port` at: its base
/// plus the port times the *port-space* stride for both straps (the MPIO
/// rev-xfer table's `+0x10` entry `0xF61110` calls the stride of space 0),
/// which differs from `before_reconfig`'s TL index; reproduced as the
/// vendor computes it (possibly a donor bug, §4 question 4).
pub const fn dpc_strap_index(a0: bool, id: u8, port: u8) -> u16 {
    straps::strap_index(a0, id, Some(port), Space::Port)
}

/// `CxlFindPortsWh` for a trained port the board marks CXL (its
/// `CxlControl`): the per-port policy, the alternate-protocol check and,
/// on a CXL link, the CXL enables and the DPC straps off. The version
/// comes from the port's config space afterwards ([`version`]). QoS is
/// separate ([`program_qos`]), as it applies to every trained port.
///
/// `proxy` is the IOD's MPIO, `iod` its SMN, `nbio` the root bridge's
/// shifted view; `a0` selects the strap index table.
pub fn find_port<A: SmnAccess>(
    proxy: &Proxy<'_, A>,
    iod: &impl SmnAccess,
    nbio: &impl SmnAccess,
    port: PortId,
    policy: &Policy,
    a0: bool,
) -> Result<Found, Error> {
    if port.core == BONUS_CORE {
        return Err(Error::BonusCore);
    }
    let status = proxy.read(pcie::CXL_STATUS.at(port).address(), Width::Dword)?;
    if status == u32::MAX {
        return Err(Error::Unavailable);
    }
    if !policy.report_errors_to_rcec {
        nbio.reg(NBIO_CXL_RCEC).set(0);
    }
    let arbitration = proxy.reg(pcie::CXL_ARBITRATION.at(port));
    if policy.io_arb_weights != 0 {
        arbitration.modify(CxlArbitration::IO_WEIGHTS.val(policy.io_arb_weights.into()))?;
        arbitration.modify(CxlArbitration::OVERRIDE::SET)?;
    }
    if policy.ca_mem_arb_weights != 0 {
        arbitration.modify(CxlArbitration::CA_MEM_WEIGHTS.val(policy.ca_mem_arb_weights.into()))?;
        arbitration.modify(CxlArbitration::OVERRIDE::SET)?;
    }
    if policy.cnli_token_advertisement != 0 {
        let token = proxy.reg(pcie::CNLI_TOKEN.at(port));
        token.modify(CnliToken::TOKENS.val(policy.cnli_token_advertisement.into()))?;
        token.modify(CnliToken::OVERRIDE::SET)?;
    }
    if let Some(response) = policy.pm_camem_response {
        proxy.reg(pcie::CXL_PM.at(port)).modify(match response {
            PmCaMemResponse::Bit9 => CxlPm::RESPONSE_A::SET + CxlPm::RESPONSE_B::CLEAR,
            PmCaMemResponse::Bit11 => CxlPm::RESPONSE_A::CLEAR + CxlPm::RESPONSE_B::SET,
        })?;
    }
    let status = LocalRegisterCopy::<u32, CxlStatus::Register>::new(status);
    if status.read(CxlStatus::ALTERNATE_PROTOCOL) != 3 {
        return Ok(Found::Pcie);
    }
    proxy
        .reg(pcie::CXL_CAMEM.at(port))
        .modify(CxlCamem::RX_OPTIMIZATION.val(policy.camem_rx_optimization.into()))?;
    let tx = proxy.reg(pcie::CXL_TX.at(port));
    tx.modify(CxlTx::DIRECT_OUT.val(policy.tx_direct_out.into()))?;
    tx.modify(CxlTx::BIT0::CLEAR)?;
    for (id, space) in DPC_STRAPS {
        proxy.write_strap(port.core, dpc_strap_index(a0, id, port.port), 0, space)?;
    }
    iod.reg(pcie::CNLI_POISON_A.at(port))
        .modify(CnliPoisonA::LOW.val(0) + CnliPoisonA::MASK::SET);
    iod.reg(pcie::CNLI_POISON_B.at(port))
        .modify(CnliPoisonB::BIT1::CLEAR + CnliPoisonB::BIT4::CLEAR + CnliPoisonB::MASK::SET);
    Ok(Found::Cxl)
}

/// The CXL version of a link from its root port: the Flex Bus port status
/// (68B flit and VH: 2.0), else Link Status 2's Flit Mode (bit 10: 3.x),
/// else a restricted CXL host — the vendor's order (it reads the same
/// registers through SMN at `0x1C40_033C` and `0x1C40_0088`).
pub fn version(flex_bus_status: Option<u16>, link_status2: u16) -> Version {
    use fstart_pci::cxl::dvsec::FlexBusPort;
    // Link Status 2 is the upper half of the dword [`pcie::LinkControl2`]
    // types.
    const FLIT_MODE: u16 = 1 << (pcie::LinkControl2::FLIT_MODE.shift - 16);
    let vh = flex_bus_status.is_some_and(|status| {
        LocalRegisterCopy::<u16, FlexBusPort::Register>::new(status)
            .is_set(FlexBusPort::VH_68B_FLIT)
    });
    if vh {
        Version::V2
    } else if link_status2 & FLIT_MODE != 0 {
        Version::V3
    } else {
        Version::Rch
    }
}

/// The CXL QoS of a trained port (`CxlProgramQosWh`, through MPIO): the
/// limits and timer written whole, the mode's fields modified, also when
/// all are zero.
pub fn program_qos<A: SmnAccess>(
    proxy: &Proxy<'_, A>,
    port: PortId,
    qos: &Qos,
) -> Result<(), Error> {
    let limits =
        QosLimits::NORMAL.val(qos.normal_limit.into()) + QosLimits::HIGH.val(qos.high_limit.into());
    proxy.write(
        pcie::QOS_LIMITS.at(port).address(),
        limits.value,
        Width::Dword,
    )?;
    let timer = QosTimer::TIMER_LIMIT.val(qos.timer_limit.into())
        + QosTimer::SCHED_GAP.val(qos.sched_gap.into());
    proxy.write(
        pcie::QOS_TIMER.at(port).address(),
        timer.value,
        Width::Dword,
    )?;
    proxy.reg(pcie::QOS_MODE.at(port)).modify(
        QosMode::VARIABLE_GAP.val(qos.variable_gap.into())
            + QosMode::RDSP_INC_MODE.val(qos.rdsp_inc_mode.into())
            + QosMode::TIMER_DEC_NUM.val(qos.timer_dec_num.into())
            + QosMode::RDSP_INC_NUM.val(qos.rdsp_inc_num.into())
            + QosMode::WRRSP_INC_NUM.val(qos.wrrsp_inc_num.into()),
    )?;
    Ok(())
}

/// Give a restricted CXL host's root port at `root_port` its RCRB pair at
/// `base` (8 KiB aligned, below 4 GiB): memory decode and bus master on,
/// then the base high and low with its enable (`CxlAssignResourcesWh`;
/// Turin wrote SMN `0x1A30_0640/0644` instead).
///
/// # Safety
/// `root_port` is the trained port's config space; `base..base + 8 KiB`
/// is MMIO nothing else decodes, routed to its root bridge.
pub unsafe fn assign_rcrb(pci: &impl ConfigRegionAccess, root_port: PciAddress, base: u64) {
    let mut low = LocalRegisterCopy::<u32, RcrbLow::Register>::new(0);
    low.write(RcrbLow::BASE.val((base as u32) >> 13) + RcrbLow::ENABLE::SET);
    // SAFETY: the caller's contract.
    unsafe {
        // Command = 6 exactly; zero leaves the write-1-to-clear status alone.
        pci.write(
            root_port,
            fstart_pci::PCI_COMMAND,
            u32::from(fstart_pci::PCI_CMD_MEMORY | fstart_pci::PCI_CMD_BUS_MASTER),
        );
        pci.write(root_port, RCRB_HIGH, (base >> 32) as u32);
        pci.write(root_port, RCRB_LOW, low.get());
    }
}

/// The downstream port RCRB's bus number dword (`+0x18`) with `bus` as
/// primary, secondary and subordinate bus (`bus * 0x10100`), the
/// latency byte kept.
pub const fn rcrb_buses(old: u32, bus: u8) -> u32 {
    (old & 0xff00_0000) | (bus as u32 * 0x1_0100)
}

/// Tell the root bridge's RCEC the bus range of its restricted CXL hosts'
/// devices (`CxlAssignBusWh`): unlock, write `next`..`last`, lock again.
pub fn assign_rcec_buses(nbio: &impl SmnAccess, next: u8, last: u8) {
    let lock = nbio.reg(RCEC_LOCK);
    lock.modify(RcecLock::HWINIT_LOCK::CLEAR);
    nbio.reg(RCEC_BUSES)
        .write(RcecBuses::NEXT_BUS.val(next.into()) + RcecBuses::LAST_BUS.val(last.into()));
    lock.modify(RcecLock::HWINIT_LOCK::SET);
}

/// `CxlConfigurePortFeatures` (AmdNbioDxe `0x2E90`) for a CXL root port
/// at `root_port` (`port` in the MPIO apertures) before its device's
/// memory is enabled: the Flex Bus port control's bit 5 in config space,
/// ASPM and the CXL.io / CXL.cache/mem L1 and L2 per `policy`, the
/// transmit path's bit 5 (cleared on a flit-mode link of a part other
/// than A0, `FUN_f930`), the isolation disable, and with
/// [`DxePolicy::mem_isolation`] the port's CXL.mem isolation ([`isolate`],
/// on the IOD's host SMN `iod`). The vendor also clears the transmit bit
/// when two engine fields (`+0x14[7:4]`, `+0x29[7:6]`) are both 2; they are
/// not decoded and fstart leaves the bit set then.
pub fn configure_port<A: SmnAccess>(
    proxy: &Proxy<'_, A>,
    iod: &impl SmnAccess,
    pci: &impl ConfigRegionAccess,
    root_port: PciAddress,
    port: PortId,
    policy: &DxePolicy,
    a0: bool,
) -> Result<Option<Isolation>, Error> {
    // The Flex Bus Port DVSEC's control (the vendor writes it at the
    // `0x33C` WH root ports have it at), status above it written 0 (its
    // bits are not write-1-to-clear).
    // SAFETY: the trained root port's own config space.
    let config = unsafe { fstart_pci::aer::Errors::new(pci) };
    if let Some(dvsec) = config.dvsec(
        root_port,
        fstart_pci::cxl::VENDOR,
        fstart_pci::cxl::dvsec::id::FLEX_BUS_PORT,
    ) {
        use fstart_pci::cxl::dvsec::{FLEX_BUS_CONTROL, FlexBusPort};
        let at = dvsec + FLEX_BUS_CONTROL;
        let control = config.config(root_port, at) & 0xffff;
        let vh = u32::from(FlexBusPort::VH_68B_FLIT::SET.value);
        // SAFETY: as above; `at` is dword aligned (DVSECs are).
        unsafe { pci.write(root_port, at, control | vh) };
    }
    let link = proxy.reg(pcie::LC_280.at(port));
    link.modify(CxlLink::ASPM_DISABLE.val((!policy.aspm).into()))?;
    link.modify(CxlLink::BITS_15_12.val(9))?;
    let tx = proxy.reg(pcie::CXL_TX.at(port));
    tx.modify(CxlTx::BIT5::SET)?;
    if !a0 {
        let link2 = proxy.read(pcie::LINK_CONTROL2.at(port).address(), Width::Dword)?;
        let link2 = LocalRegisterCopy::<u32, pcie::LinkControl2::Register>::new(link2);
        if link2.is_set(pcie::LinkControl2::FLIT_MODE) {
            tx.modify(CxlTx::BIT5::CLEAR)?;
        }
    }
    let power = proxy.reg(pcie::CXL_POWER_STATES.at(port));
    power.modify(CxlPowerStates::IO_L1_DISABLE.val((!policy.io_l1).into()))?;
    power.modify(CxlPowerStates::IO_L2_DISABLE.val((!policy.io_l2).into()))?;
    power.modify(CxlPowerStates::CAMEM_L1_DISABLE.val((!policy.camem_l1).into()))?;
    power.modify(CxlPowerStates::CAMEM_L2_DISABLE.val((!policy.camem_l2).into()))?;
    proxy
        .reg(pcie::CXL_ISOLATION.at(port))
        .modify(CxlIsolation::DISABLE.val((!policy.mem_isolation).into()))?;
    if !policy.mem_isolation {
        return Ok(None);
    }
    isolate(proxy, iod, port, &policy.isolation).map(Some)
}

/// What [`isolate`] enabled on a root port.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Isolation {
    /// The port has no CXL.mem isolation ("Error: CXL.mem Isolation
    /// enable is not supported").
    Unsupported,
    /// The control register as written.
    Enabled(u32),
}

/// A CXL.mem transaction timeout value the capability's ranges support
/// (AmdNbioDxe `0x28F0`): 0 (the default range) with any range; values
/// 1-2 in range A, 5-6 in B, 9-10 in C, 13-14 in D, each for the range
/// combinations PCIe defines (A; B; AB; BC; ABC; BCD; ABCD).
pub const fn timeout_supported(ranges: u8, value: u8) -> bool {
    if value == 0 {
        return ranges != 0;
    }
    let a = matches!(ranges, 0x1 | 0x3 | 0x7 | 0xf);
    let b = matches!(ranges, 0x2 | 0x3 | 0x6 | 0x7 | 0xe | 0xf);
    let c = matches!(ranges, 0x6 | 0x7 | 0xe | 0xf);
    let d = matches!(ranges, 0xe | 0xf);
    match value {
        1 | 2 => a,
        5 | 6 => b,
        9 | 10 => c,
        13 | 14 => d,
        _ => false,
    }
}

/// CXL.mem isolation of a root port (AmdNbioDxe `0x2AE0`, run when
/// `PcdAmdCxlMemIsolationEnable` is set; both donors build it 0), on its
/// CXL Timeout and Isolation registers in the CNLI aperture: isolation
/// enabled if the capability has it; with `FwNotification` the isolation
/// interrupt and the port's notify field 2; with `TransactionTimeoutEnable`
/// and a timeout capability the timeout enabled, at `TransactionTimeoutValue`
/// when the ranges support it, else at the default; with link-down
/// isolation supported, enabled or not per `Linkdown` (the port's
/// no-link-down bit the inverse).
pub fn isolate<A: SmnAccess>(
    proxy: &Proxy<'_, A>,
    iod: &impl SmnAccess,
    port: PortId,
    policy: &IsolationPolicy,
) -> Result<Isolation, Error> {
    use pcie::{
        IsolationSignal, TimeoutIsolationCapability as Cap, TimeoutIsolationControl as Ctl,
    };
    use tock_registers::interfaces::Readable;
    let cap = iod
        .reg(pcie::TIMEOUT_ISOLATION_CAPABILITY.at(port))
        .extract();
    if !cap.is_set(Cap::MEM_ISOLATION_SUPPORTED) {
        return Ok(Isolation::Unsupported);
    }
    let control = iod.reg(pcie::TIMEOUT_ISOLATION_CONTROL.at(port));
    let mut value = control.extract();
    value.modify(Ctl::MEM_ISOLATION_ENABLE::SET);
    let signal = proxy.reg(pcie::ISOLATION_SIGNAL.at(port));
    if policy.fw_notification {
        signal.modify(IsolationSignal::NOTIFY.val(2))?;
        value.modify(Ctl::ISOLATION_INTERRUPT_ENABLE::SET);
    }
    if let Some(timeout) = policy.timeout
        && cap.is_set(Cap::MEM_TIMEOUT_SUPPORTED)
    {
        let ranges = cap.read(Cap::MEM_TIMEOUT_RANGES) as u8;
        let timeout = if timeout_supported(ranges, timeout) {
            timeout
        } else {
            0
        };
        value.modify(Ctl::MEM_TIMEOUT_ENABLE::SET + Ctl::MEM_TIMEOUT_VALUE.val(timeout.into()));
    }
    if cap.is_set(Cap::MEM_ISOLATION_LINK_DOWN_SUPPORTED) {
        value.modify(Ctl::MEM_ISOLATION_LINK_DOWN_ENABLE.val(policy.link_down.into()));
        signal.modify(IsolationSignal::NO_LINK_DOWN.val((!policy.link_down).into()))?;
    }
    control.set(value.get());
    Ok(Isolation::Enabled(value.get()))
}

/// What a CXL 1.1 device's BARs need (`AllocateForCxlIo`, `0xF1F0F0`):
/// the 32-bit ones' total rounded to their largest alignment, below 4
/// GiB, and the 64-bit ones' total.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RciepBars {
    pub mmio32: u64,
    pub align32: u64,
    pub mmio64: u64,
    pub align64: u64,
}

/// The upstream port's component registers at the top of the 64-bit
/// carve (`0x357B0`: top − 64 KiB).
pub const US_COMPONENT_SIZE: u64 = 0x1_0000;

impl RciepBars {
    /// From the device's memory BARs `(size, 64-bit)`.
    pub fn of(bars: &[(u64, bool)]) -> Self {
        let mut out = Self::default();
        for &(size, wide) in bars.iter().filter(|(s, _)| *s != 0) {
            let size = size.next_power_of_two();
            if wide {
                out.mmio64 += size;
                out.align64 = out.align64.max(size);
            } else {
                out.mmio32 += size;
                out.align32 = out.align32.max(size);
            }
        }
        if out.align32 != 0 {
            out.mmio32 = out.mmio32.next_multiple_of(out.align32);
        }
        if out.align64 != 0 {
            out.mmio64 = out.mmio64.next_multiple_of(out.align64);
        }
        out
    }

    /// The windows carved off the top of the root bridge's MMIO below 4
    /// GiB (`top32`) and above (`top64`), `(base, size)` each (AmdNbioDxe
    /// `CxlResourceCallback` `0x35AD0`): the 32-bit BARs, 1 MiB granular;
    /// the 64-bit BARs and the upstream port's component registers at the
    /// top.
    pub fn carve(&self, top32: u64, top64: u64) -> ((u64, u64), (u64, u64)) {
        const MIB: u64 = 1 << 20;
        let size32 = self.mmio32.next_multiple_of(MIB).max(MIB);
        let align32 = self.align32.max(MIB);
        let base32 = (top32 - size32) & !(align32 - 1);
        let size64 = (self.mmio64 + US_COMPONENT_SIZE).next_multiple_of(MIB);
        let align64 = self.align64.max(MIB);
        let base64 = (top64 - size64) & !(align64 - 1);
        ((base32, top32 - base32), (base64, top64 - base64))
    }
}

/// Size the memory BARs of `device` (probe and restore): `(size, 64-bit)`
/// per BAR.
///
/// # Safety
/// `device` is a present function the caller owns, its memory decode off.
pub unsafe fn probe_bars(
    pci: &impl ConfigRegionAccess,
    device: PciAddress,
) -> heapless::Vec<(u16, u64, bool), 6> {
    let mut out = heapless::Vec::new();
    let mut bar = 0x10u16;
    while bar <= 0x24 {
        // SAFETY: the caller's contract; every BAR is restored.
        let (size, wide) = unsafe {
            let low = pci.read(device, bar);
            if low & 1 != 0 {
                bar += 4;
                continue;
            }
            let wide = low & 0b110 == 0b100 && bar < 0x24;
            pci.write(device, bar, u32::MAX);
            let mask_low = pci.read(device, bar) & !0xf;
            pci.write(device, bar, low);
            let mask_high = if wide {
                let high = pci.read(device, bar + 4);
                pci.write(device, bar + 4, u32::MAX);
                let m = pci.read(device, bar + 4);
                pci.write(device, bar + 4, high);
                m
            } else {
                u32::MAX
            };
            let mask = u64::from(mask_high) << 32 | u64::from(mask_low);
            (
                (!mask).wrapping_add(1) & if wide { u64::MAX } else { 0xffff_ffff },
                wide,
            )
        };
        if size != 0 {
            let _ = out.push((bar, size, wide));
        }
        bar += if wide { 8 } else { 4 };
    }
    out
}

/// Forward the carved windows through a CXL 1.1 port's RCRBs (AmdNbioDxe
/// `0x357B0`): the downstream port's memory and prefetchable windows, the
/// upstream port's BAR0 at the top of the 64-bit window minus 64 KiB.
///
/// # Safety
/// `downstream` is the enabled RCRB pair's base (the upstream port's
/// follows at +4 KiB), identity-mapped uncached.
pub unsafe fn forward_rch(downstream: u64, mmio32: (u64, u64), mmio64: (u64, u64)) {
    use fstart_pci::cxl::rcrb;
    // SAFETY: the caller's contract.
    let (ds, us) = unsafe {
        (
            &*(downstream as usize as *const rcrb::DownstreamPort),
            &*((downstream + rcrb::SIZE) as usize as *const rcrb::UpstreamPort),
        )
    };
    ds.set_windows(mmio32, mmio64);
    us.set_bar0(mmio64.0 + mmio64.1 - US_COMPONENT_SIZE);
}

/// Sync flood on CXL errors of the root bridge (AmdNbioDxe
/// `CxlEnableSyncFloodCallback`: OR `0x19`).
pub fn enable_sync_flood(nbio: &impl SmnAccess) {
    nbio.reg(SYNC_FLOOD)
        .modify(SyncFlood::ENABLE::SET + SyncFlood::BIT3::SET + SyncFlood::BIT4::SET);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dxio::rs::mailbox::{CONTROL, Poll};
    use crate::dxio::wh::proxy_firmware;
    use crate::smn::testing::SmnSpace;
    use fstart_core::services::timer::ManualTimer;

    fn proxied() -> SmnSpace {
        let mut space = SmnSpace::default();
        space.on_write = Some(proxy_firmware);
        space.set(CONTROL, 1 << 31);
        space
    }

    #[test]
    fn a_cxl_port_gets_the_vendor_values() {
        let space = proxied();
        let smn = space.smn();
        let timer = ManualTimer::new();
        let proxy = unsafe { Proxy::new(space.smn(), Poll::default(), &timer) };
        // Core 1 (RC aperture 2), port 3.
        let port = PortId {
            core: 1,
            rc: 2,
            port: 3,
        };
        let hi = |a: u32| a + (2 << 20) + (3 << 12);
        let cnli = |a: u32| a + (1 << 16) + (3 << 13);
        space.set(hi(0x1c44_0424), 3 << 12);
        space.set(hi(0x1c44_045c), 0b01);
        space.set(cnli(0x1f10_0090), 0b111);
        space.set(cnli(0x1f10_0084), 0b1_0010);
        space.set(NBIO_CXL_RCEC.address(), 0xdead);
        let policy = Policy {
            report_errors_to_rcec: false,
            io_arb_weights: 5,
            cnli_token_advertisement: 7,
            camem_rx_optimization: true,
            tx_direct_out: true,
            pm_camem_response: Some(PmCaMemResponse::Bit11),
            ..Policy::DEFAULT
        };
        assert_eq!(
            find_port(&proxy, &smn, &smn, port, &policy, false),
            Ok(Found::Cxl)
        );
        assert_eq!(space.get(NBIO_CXL_RCEC.address()), 0);
        assert_eq!(space.get(hi(0x1c44_0440)), 5 << 16 | 1 << 24);
        assert_eq!(space.get(hi(0x1c44_00bc)), 7 << 10 | 1 << 15);
        assert_eq!(space.get(hi(0x1c44_04d0)), 1 << 11);
        assert_eq!(space.get(0x1c48_04e4u32 + (2 << 20)), 1 << 2);
        assert_eq!(space.get(hi(0x1c44_045c)), 0b10);
        assert_eq!(space.get(cnli(0x1f10_0090)), 0x20);
        assert_eq!(space.get(cnli(0x1f10_0084)), 0x400);

        // Plain PCIe: policy, no enables.
        space.set(hi(0x1c44_0424), 1 << 12);
        space.set(hi(0x1c44_045c), 0b01);
        assert_eq!(
            find_port(&proxy, &smn, &smn, port, &Policy::DEFAULT, false),
            Ok(Found::Pcie)
        );
        assert_eq!(space.get(hi(0x1c44_045c)), 0b01);

        // Core 2's RC aperture is 3 (`Wrapper::rc`); the bonus core is refused.
        let core2 = PortId {
            core: 2,
            rc: 3,
            port: 0,
        };
        space.set(0x1c44_0424u32 + (3 << 20), 3 << 12);
        assert_eq!(
            find_port(&proxy, &smn, &smn, core2, &Policy::DEFAULT, false),
            Ok(Found::Cxl)
        );
        assert_eq!(
            find_port(
                &proxy,
                &smn,
                &smn,
                PortId {
                    core: 6,
                    rc: 5,
                    port: 0
                },
                &Policy::DEFAULT,
                false
            ),
            Err(Error::BonusCore)
        );
        assert_eq!(proxy.error(), None);
    }

    /// The proxy firmware, recording strap requests (`0x0C`) at
    /// `STRAPS + index` with `value | space << 31`.
    const STRAPS: u32 = 0xf000_0000;
    fn strap_recorder(space: &SmnSpace, address: u32, value: u32) {
        use crate::dxio::rs::mailbox::{ARGUMENTS, CONTROL, DOORBELL_V2};
        if address == DOORBELL_V2.address() && (space.get(CONTROL) >> 8) & 0xfff == 0x0c {
            let args: [u32; 3] =
                core::array::from_fn(|i| space.get(ARGUMENTS.offset(4 * i as u32)));
            space.set(STRAPS + (args[0] & 0xffff), args[1] | args[2] << 31);
        }
        proxy_firmware(space, address, value);
    }

    #[test]
    fn a_cxl_link_turns_its_dpc_straps_off() {
        let mut space = proxied();
        space.on_write = Some(strap_recorder);
        let smn = space.smn();
        let timer = ManualTimer::new();
        let proxy = unsafe { Proxy::new(space.smn(), Poll::default(), &timer) };
        let port = PortId {
            core: 1,
            rc: 2,
            port: 3,
        };
        space.set(0x1c44_0424u32 + (2 << 20) + (3 << 12), 3 << 12);
        let port19 = dpc_strap_index(false, 0x19, 3);
        let tl4e = dpc_strap_index(false, 0x4e, 3);
        space.set(STRAPS + u32::from(port19), 0xdead);
        space.set(STRAPS + u32::from(tl4e), 0xdead);
        assert_eq!(
            find_port(&proxy, &smn, &smn, port, &Policy::DEFAULT, false),
            Ok(Found::Cxl)
        );
        // Both value 0; the TL one in space 1 at the port-stride index.
        assert_eq!(space.get(STRAPS + u32::from(port19)), 0);
        assert_eq!(space.get(STRAPS + u32::from(tl4e)), 1 << 31);
        assert_eq!(
            tl4e,
            straps::strap_index(false, 0x4e, None, Space::Tl) + 3 * 0xbc
        );
    }

    #[test]
    fn endpoint_enable_port_features_and_rch_carves() {
        use crate::cxl::DxePolicy;
        struct Config(core::cell::RefCell<std::collections::BTreeMap<u16, u32>>);
        impl ConfigRegionAccess for Config {
            unsafe fn read(&self, _: PciAddress, offset: u16) -> u32 {
                *self.0.borrow().get(&offset).unwrap_or(&0)
            }
            unsafe fn write(&self, _: PciAddress, offset: u16, value: u32) {
                self.0.borrow_mut().insert(offset, value);
            }
        }
        let space = proxied();
        let timer = ManualTimer::new();
        let proxy = unsafe { Proxy::new(space.smn(), Poll::default(), &timer) };
        let port = PortId {
            core: 0,
            rc: 1,
            port: 2,
        };
        let hi = |a: u32| a + (1 << 20) + (2 << 12);
        space.set(hi(0x1c44_0280), 1 << 24);
        // Flit mode in Link Status 2.
        space.set(hi(0x1c40_0088), 1 << 26);
        let cfg = Config(Default::default());
        // The Flex Bus Port DVSEC at 0x330 (after one capability at 0x100),
        // as WH's root ports have it: control at 0x33C.
        const FLEX_BUS_CONTROL: u16 = 0x33c;
        cfg.0.borrow_mut().extend([
            (0x100, 0x3301_0001),
            (0x330, 0x0001_0023),
            (0x334, 0x1e98),
            (0x338, 7),
            (FLEX_BUS_CONTROL, 0x0020_0000),
        ]);
        let rp = PciAddress::new(0, 0x20, 1, 1);
        let iod = space.smn();
        assert_eq!(
            configure_port(&proxy, &iod, &cfg, rp, port, &DxePolicy::WH, false),
            Ok(None)
        );
        assert_eq!(cfg.0.borrow()[&FLEX_BUS_CONTROL], 0x20);
        assert_eq!(space.get(hi(0x1c44_0280)), 9 << 12);
        assert_eq!(space.get(hi(0x1c44_045c)), 0);
        assert_eq!(space.get(hi(0x1c44_0444)), 0);
        assert_eq!(space.get(0x1a38_031cu32 + (2 << 12)), 1 << 10);
        // A0 keeps bit 5 on a flit link.
        configure_port(&proxy, &iod, &cfg, rp, port, &DxePolicy::WH, true).unwrap();
        assert_eq!(space.get(hi(0x1c44_045c)), 1 << 5);

        // Firmware isolation: CNLI core 0 port 2; the capability has
        // isolation, link-down isolation and timeout ranges B and C.
        let cnli = |a: u32| a + (2 << 13);
        let isolating = DxePolicy {
            mem_isolation: true,
            isolation: crate::cxl::IsolationPolicy {
                fw_notification: true,
                link_down: false,
                timeout: Some(9),
            },
            ..DxePolicy::WH
        };
        space.set(cnli(0x1f10_00e8), 1 << 17 | 1 << 16 | 1 << 4 | 0x6);
        space.set(0x1a34_0264u32 + (2 << 12), 0xffff_ffff);
        assert_eq!(
            configure_port(&proxy, &iod, &cfg, rp, port, &isolating, false),
            Ok(Some(Isolation::Enabled(1 << 25 | 1 << 16 | 1 << 4 | 9)))
        );
        assert_eq!(space.get(0x1a38_031cu32 + (2 << 12)), 0);
        assert_eq!(space.get(cnli(0x1f10_00f0)), 1 << 25 | 1 << 16 | 1 << 4 | 9);
        // Notify field 2, no-link-down set (link-down isolation off).
        assert_eq!(space.get(0x1a34_0264u32 + (2 << 12)), 0xffff_fffb);
        // A timeout the ranges lack falls back to the default; no isolation
        // capability, nothing.
        assert!(!timeout_supported(0x6, 1) && timeout_supported(0x6, 0));
        space.set(cnli(0x1f10_00e8), 0);
        assert_eq!(
            configure_port(&proxy, &iod, &cfg, rp, port, &isolating, false),
            Ok(Some(Isolation::Unsupported))
        );

        // BAR sizing: two 32-bit BARs (64 KiB, 1 MiB), one 64-bit (4 GiB).
        cfg.0.borrow_mut().clear();
        let bars = RciepBars::of(&[(0x1_0000, false), (0x10_0000, false), (1 << 32, true)]);
        assert_eq!(
            bars,
            RciepBars {
                mmio32: 0x20_0000,
                align32: 0x10_0000,
                mmio64: 1 << 32,
                align64: 1 << 32
            }
        );
        // BAR probing: a 64 KiB 32-bit BAR at 0x10, a 4 GiB 64-bit one at
        // 0x18, restored afterwards.
        struct Bars(core::cell::RefCell<std::collections::BTreeMap<u16, u32>>);
        impl ConfigRegionAccess for Bars {
            unsafe fn read(&self, _: PciAddress, offset: u16) -> u32 {
                *self.0.borrow().get(&offset).unwrap_or(&0)
            }
            unsafe fn write(&self, _: PciAddress, offset: u16, value: u32) {
                let v = match (offset, value) {
                    (0x10, u32::MAX) => 0xffff_0000,
                    (0x18, u32::MAX) => 0x0000_000c,
                    (0x1c, u32::MAX) => 0xffff_ffff,
                    // Unimplemented BARs read 0.
                    (_, u32::MAX) => 0,
                    _ => value,
                };
                self.0.borrow_mut().insert(offset, v);
            }
        }
        let model = Bars(Default::default());
        model.0.borrow_mut().insert(0x18, 0xc);
        let ep = PciAddress::new(0, 0x7f, 0, 0);
        let probed = unsafe { probe_bars(&model, ep) };
        assert_eq!(probed[..], [(0x10, 0x1_0000, false), (0x18, 1 << 32, true)]);
        assert_eq!(model.0.borrow()[&0x18], 0xc);
        let (w32, w64) = bars.carve(0xf000_0000, 0x80_0000_0000);
        assert_eq!(w32, (0xefe0_0000, 0x20_0000));
        // 4 GiB + 64 KiB rounded to 1 MiB, aligned down to 4 GiB.
        assert_eq!(w64, (0x7e_0000_0000, 0x2_0000_0000));
    }

    #[test]
    fn qos_versions_and_rch_values() {
        let space = proxied();
        let timer = ManualTimer::new();
        let proxy = unsafe { Proxy::new(space.smn(), Poll::default(), &timer) };
        let port = PortId {
            core: 2,
            rc: 3,
            port: 1,
        };
        let lo = |a: u32| a + (2 << 20) + (1 << 12);
        space.set(lo(0x1a34_0924), 0xffff_0000);
        program_qos(
            &proxy,
            port,
            &Qos {
                normal_limit: 0x10,
                high_limit: 0x20,
                timer_limit: 0x30,
                sched_gap: 0x40,
                variable_gap: true,
                rdsp_inc_mode: false,
                timer_dec_num: 1,
                rdsp_inc_num: 2,
                wrrsp_inc_num: 3,
            },
        )
        .unwrap();
        assert_eq!(space.get(lo(0x1a34_091c)), 0x0010_0020);
        assert_eq!(space.get(lo(0x1a34_0920)), 0x0030_0040);
        assert_eq!(space.get(lo(0x1a34_0924)), 0xffff_2123);

        assert_eq!(version(Some(1 << 5 | 0b101), 0), Version::V2);
        assert_eq!(version(Some(0b101), 1 << 10), Version::V3);
        assert_eq!(version(None, 0), Version::Rch);

        let smn = space.smn();
        assert_eq!(rcrb_buses(0xab00_0000, 0x7f), 0xab7f_7f00);
        assign_rcec_buses(&smn, 0x7f, 0x7f);
        assert_eq!(space.get(RCEC_BUSES.address()), 0x007f_7f00);
        assert_eq!(space.get(RCEC_LOCK.address()), 1 << 1);
        enable_sync_flood(&smn);
        assert_eq!(space.get(SYNC_FLOOD.address()), 0x19);
    }
}
