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
//! Not done: the two per-port strap writes (value 0) of `CxlFindPortsWh`
//! (their strap IDs are not recovered), the RCiEP BAR sizing of a CXL 1.1
//! device (`AllocateForCxlIo`).

use fstart_pci::{ConfigRegionAccess, PciAddress};
use tock_registers::interfaces::{ReadWriteable, Writeable};
use tock_registers::{LocalRegisterCopy, register_bitfields};

use super::{PmCaMemResponse, Policy, Qos, Version};
use crate::dxio::rs::mailbox;
use crate::dxio::wh::{Proxy, Width};
use crate::nbio::wh::pcie::{
    self, CnliPoisonA, CnliPoisonB, CnliToken, CxlArbitration, CxlCamem, CxlPm, CxlStatus, CxlTx,
    PortId, QosLimits, QosMode, QosTimer,
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

/// `CxlFindPortsWh` for a trained port the board marks CXL (its
/// `CxlControl`): the per-port policy, the alternate-protocol check and,
/// on a CXL link, the CXL enables. The version comes from the port's
/// config space afterwards ([`version`]). QoS is separate
/// ([`program_qos`]), as it applies to every trained port.
///
/// `proxy` is the IOD's MPIO, `iod` its SMN, `nbio` the root bridge's
/// shifted view.
pub fn find_port<A: SmnAccess>(
    proxy: &Proxy<'_, A>,
    iod: &impl SmnAccess,
    nbio: &impl SmnAccess,
    port: PortId,
    policy: &Policy,
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
    const FLIT_MODE: u16 = 1 << 10;
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
        assert_eq!(find_port(&proxy, &smn, &smn, port, &policy), Ok(Found::Cxl));
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
            find_port(&proxy, &smn, &smn, port, &Policy::DEFAULT),
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
            find_port(&proxy, &smn, &smn, core2, &Policy::DEFAULT),
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
                &Policy::DEFAULT
            ),
            Err(Error::BonusCore)
        );
        assert_eq!(proxy.error(), None);
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
