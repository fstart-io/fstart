// SPDX-License-Identifier: MIT OR Apache-2.0
//! The WH NBIO steps of SP7 at the vendor's points of the boot (Dell PR9XJ
//! 1.0.8 openSIL and AmdNbioDxe, NBIO.md §2.3, §2.9, §2.11), on every root
//! bridge of every socket in system order:
//!
//! | Flow step | Vendor | Here |
//! |---|---|---|
//! | `init_fch`, before the IOMMU/IOAPIC loop | `NbioBaseConfigurationWh` loops 1 and 3 | [`base_config`] |
//! | `init_fch`, per root bridge | `NbioIommuInitWh` | `RootBridge::iommu_init` (in `init`) |
//! | `train_dxio`, after MPIO | `MpioCfgAfterDxioInitWh` (requester IDs, AER, performance tables, ARI) | [`after_training`] |
//! | `after_resources` | DXE `PcieMiscInit` RAS, `StoreNbifEpRequesterId`, `NbioHwLock` | [`after_enumeration`] |
//! | `ready_to_boot` | `PcieLockRegistersCallBack`, `SmuMpioClockGatingRTBCallBack`, `PcieAerReportingPathExtCallBack` | [`ready_to_boot`] |
//!
//! The order differs from the vendor's in two places, harmless as far as
//! known (no register written later depends on them): the VDM node bus
//! ranges are written in `init_fch`'s per-root-bridge loop, after loop 3's
//! tables (vendor: in loop 1), and the PSP BAR after the IOMMU and IOAPIC
//! of its root bridge (vendor: loop 3 item 6).
//!
//! The IOHC TOM registers (`D0F0x90`, `0x13B1_0064/68/138`, `NB_PCI_CTRL`)
//! are not written: no Venice donor module (PEI or DXE, Dell or GIGABYTE)
//! names them, so the ABL is taken to own them (NBIO.md Q1, open on
//! hardware).

use core::num::NonZeroU32;

use fstart_driver_amd::df::plan::RootBridgePlan;
use fstart_driver_amd::dxio::rs::mailbox::Poll;
use fstart_driver_amd::dxio::wh::Proxy;
use fstart_driver_amd::nbio::wh::{self as nbio_wh, pcie};
use fstart_driver_amd::smn::Smn;
use fstart_driver_amd::smu::{SmuMessages, v14 as smu_v14};
use fstart_pci::ecam::GlobalEcam;

use super::fabric;
use super::init::State;
use super::{Sp7, Variant};
use crate::flow::Ctx;

/// The WH part (`cpu::wh::Part`): its PCIe layout, DF device table and
/// CPUID gates, decoded once from CPUID.
pub(super) fn part() -> nbio_wh::Part {
    let (eax, ebx, _, _) = fstart_arch::x86::cpuid(0x8000_0001);
    nbio_wh::Part::from_cpuid(eax, ebx)
}

/// The NBIO of root bridge `index` on bus `bus`, at its IOHC's SMN
/// instance (SP7 B0's third root bridge is IOHC 3, `Layout::iohc`).
pub(super) fn root_bridge(
    part: nbio_wh::Part,
    index: u8,
    bus: u8,
) -> Option<nbio_wh::RootBridge<GlobalEcam>> {
    let (_, rb) = fabric::rb_location(index);
    // SAFETY: the root bridge's bus is routed; the BSP owns SMN during boot.
    unsafe { nbio_wh::RootBridge::new(GlobalEcam, part.layout().iohc(rb), bus) }
}

/// The MPIO `PCIE_PROXY` of the IOD behind `bus`.
fn proxy(bus: u8, timer: &dyn fstart_core::services::Timer) -> Proxy<'_, Smn<GlobalEcam>> {
    let poll = Poll {
        reads: const { NonZeroU32::new(10_000).unwrap() },
        interval_us: const { NonZeroU32::new(10).unwrap() },
    };
    // SAFETY: the IOD's training sessions are over; the BSP owns its MPIO
    // mailbox and SMN.
    unsafe { Proxy::new(Smn::on_bus(GlobalEcam, bus), poll, timer) }
}

/// `NbioBaseConfigurationWh` on every root bridge: loop 1 on all of them
/// (bus number, SB location per IOD, NB SSID, bridges hidden; the VDM bus
/// ranges are written by `init_fch`'s loop), then loop 3 on all of them (the
/// base, A0, NBIF and per-IOHC tables).
/// Generic over the SoC: its strings are its own.
///
/// First, every root bridge's IOHC (`Layout::iohc`, SP7 B0's third root
/// bridge being IOHC 3, decompiler-inferred) is checked against the bus
/// number the firmware before us gave it (`RootBridge::numbering`): a
/// disagreement stops the boot, since every NBIO, MPIO and CXL step of
/// that root bridge would program the wrong IOHC.
pub(super) fn base_config<V: Variant>(
    roots: &[RootBridgePlan],
    part: nbio_wh::Part,
    policy: &nbio_wh::Policy,
) -> Result<(), &'static str> {
    let mut unverified = 0;
    for rb in roots {
        let nb = root_bridge(part, rb.index, rb.bus.base).ok_or("WH NBIO handle")?;
        match nb.numbering() {
            nbio_wh::Numbering::Agrees => {}
            nbio_wh::Numbering::Unprogrammed => unverified += 1,
            nbio_wh::Numbering::Disagrees { bus, found } => {
                fstart_log::error!(
                    "{}: S{} RB{} bus {:#x}: IOHC {} has bus {:?}, IOHC {:?} has this one",
                    V::NAME,
                    rb.socket,
                    rb.index,
                    rb.bus.base,
                    nb.iohc,
                    bus,
                    found
                );
                return Err("WH IOHC numbering");
            }
        }
    }
    if unverified != 0 {
        fstart_log::warn!(
            "{}: IOHC numbering of {} root bridge(s) not verifiable (no bus numbers yet)",
            V::NAME,
            unverified
        );
    }
    for rb in roots {
        root_bridge(part, rb.index, rb.bus.base)
            .ok_or("WH NBIO handle")?
            .early(part, policy);
    }
    for rb in roots {
        root_bridge(part, rb.index, rb.bus.base)
            .ok_or("WH NBIO handle")?
            .base_init(part, policy);
    }
    Ok(())
}

/// `MpioCfgAfterDxioInitWh` (SilVenicePei `0xf65630`) once the links are
/// trained and the bridges shown: through each IOD's MPIO, each trained
/// port's requester ID; then per root bridge the performance tables and
/// ARI. (The AER strap is MPIO's, before the reconfiguration:
/// `straps::core_before_reconfig`.)
pub(super) fn after_training<V: Variant>(ctx: &Ctx<'_, Sp7<V>>, state: &State) {
    let part = part();
    let policy = &ctx.config.nbio;
    for &(socket, die) in state.nodes() {
        let Some(bus) = state.node_bus(socket, die) else {
            continue;
        };
        let proxy = proxy(bus, &ctx.clock);
        for (_, b) in state
            .bindings()
            .iter()
            .filter(|(s, b)| (*s, b.iod) == (socket, die))
        {
            let Some(rb) = state
                .roots()
                .iter()
                .find(|r| (r.socket, r.index) == (socket, fabric::rb_index(b.iod, b.rb_on_iod)))
            else {
                continue;
            };
            if pcie::set_requester(&proxy, b.port_id(), (0, rb.bus_base, b.devfn)).is_err() {
                fstart_log::error!(
                    "{}: S{} IOD{} core {} port {} requester ID not set",
                    V::NAME,
                    socket,
                    die,
                    b.core,
                    b.local_port
                );
            }
        }
        for rb in state
            .roots()
            .iter()
            .filter(|r| r.socket == socket && fabric::rb_location(r.index).0 == die)
        {
            let Some(nb) = root_bridge(part, rb.index, rb.bus_base) else {
                continue;
            };
            let perf = nb.perf_init(
                part,
                policy,
                nbio_wh::perf_table(part),
                (socket, die),
                &proxy,
            );
            if perf.is_err() {
                fstart_log::error!(
                    "{}: S{} RB{} performance tables not applied",
                    V::NAME,
                    socket,
                    rb.index
                );
            }
            nb.enable_ari(policy);
        }
    }
}

/// After PCI enumeration (DXE `CallbackAfterPciIo`): the RAS setup of every
/// root bridge, the NBIF endpoint's requester and the NBIF HW-init locks.
pub(super) fn after_enumeration<V: Variant>(ctx: &Ctx<'_, Sp7<V>>, state: &State) {
    let part = part();
    let policy = &ctx.config.nbio;
    for (i, rb) in state.roots().iter().enumerate() {
        let Some(nb) = root_bridge(part, rb.index, rb.bus_base) else {
            continue;
        };
        nb.ras_init(part, policy, i == 0);
        nb.store_nbif_requester();
        nb.lock_nbif();
    }
}

/// Ready to boot: lock every PCIe core's registers and enable MPIO clock
/// gating through each IOD's MPIO, then tell each socket's SMU the AER
/// reporting policy.
pub(super) fn ready_to_boot<V: Variant>(ctx: &Ctx<'_, Sp7<V>>, state: &State) {
    let part = part();
    let policy = &ctx.config.nbio;
    for &(socket, die) in state.nodes() {
        let Some(bus) = state.node_bus(socket, die) else {
            continue;
        };
        let proxy = proxy(bus, &ctx.clock);
        for rb in state
            .roots()
            .iter()
            .filter(|r| r.socket == socket && fabric::rb_location(r.index).0 == die)
        {
            if let Some(nb) = root_bridge(part, rb.index, rb.bus_base) {
                let _ = nb.lock_pcie(part, &proxy);
            }
        }
        let _ = proxy.enable_clock_gating(policy.mpio_clock_gating);
        if proxy.error().is_some() {
            fstart_log::error!(
                "{}: S{} IOD{} PCIe lock / MPIO clock gating failed",
                V::NAME,
                socket,
                die
            );
        }
    }
    if policy.aer && policy.aer_mechanism != 0 {
        // The vendor programs the AER reporting path first
        // (`NbioPcieAerReportingPathExt`, `0x13B2_0114..012C`), which
        // fstart does not.
        fstart_log::warn!("{}: firmware-first AER path not programmed", V::NAME);
    }
    for socket in 0..state.sockets() {
        let mut args = [
            u32::from(policy.aer),
            policy.aer_mechanism.into(),
            0,
            0,
            0,
            0,
        ];
        let smu = state.smu_on(socket, 0, ctx.clock);
        if smu
            .send(smu_v14::msg::PCIE_AER_REPORTING, &mut args)
            .is_err()
        {
            fstart_log::warn!("{}: S{} SMU AER reporting rejected", V::NAME, socket);
        }
    }
}
