// SPDX-License-Identifier: MIT OR Apache-2.0
//! CXL on the SP7 SoCs: the driver's AMD-common CXL steps and its WH
//! registers ([`fstart_driver_amd::cxl`]) placed in the flow
//! (reports/research/CXL.md §1.3):
//!
//! 1. after MPIO training, on every IOD of every socket ([`after_training`]):
//!    QoS on every trained port; on the ports the board marks CXL the
//!    policy, the CXL check and enables, the version from the root port,
//!    a CXL 1.1 port's RCRB, its device's bus and MEMBAR0; then the IOD's
//!    device list to MPIO (`0x2C`);
//! 2. after PCI resource allocation ([`after_resources`]): the devices
//!    again with their BDF (`0x0D`), the APOB `9/0x18` windows planned into
//!    CFMWS and host bridge decoders, each CXL host bridge's emulated
//!    component registers (64 KiB of firmware DRAM) behind a 512 KiB trap
//!    window of its non-PCI MMIO above 4 GiB (`0x30`), then each IOD's
//!    finalization (`0x30`);
//! 3. the memory map ([`map_windows`]): the mapped windows as RAM or
//!    soft-reserved (board policy; soft-reserved is both vendors' default,
//!    `PcdAmdCxlSpm`);
//! 4. ACPI ([`cedt`], [`root_device`], [`is_host_bridge`],
//!    [`numa_regions`], [`add_domains`]): the CEDT (CHBS per host bridge,
//!    CFMWS per window), `ACPI0016` on the CXL root bridges with the CXL
//!    `_OSC`, `ACPI0017`, one SRAT domain per window.
//!
//! What a board decides is [`Config`]; with none, nothing of this runs and
//! the SoC behaves as without CXL. The memory needs the board's APCB CXL
//! regions (`cxl-regions` in its `[package.metadata.fstart.amd]`), which
//! the ABL maps; [`Config::regions`] names the root port of each.
//!
//! The vendor DXE's memory steps after enumeration are
//! [`super::cxl_memory`]'s: TOM2 lowered to the lowest window and restored
//! once a pool is enabled, the devices sized and, with
//! [`Config::apcb_update`], the APCB's regions rewritten in the flash with
//! a warm reset ([`super::cxl_apcb`]), the root ports' CXL features, the
//! endpoints' DVSEC ranges and HDM decoders committed, the switches'
//! decoders, sync flood, the HMAT from the devices' CDAT
//! (reports/research/CXL_COMPLETE.md).

extern crate alloc;

use alloc::vec::Vec;

use fstart_driver_amd::apob::cxl::Window;
use fstart_driver_amd::apob::sysmap::Region;
use fstart_driver_amd::cxl::{self, DeviceType, Version, host};

/// A root port by its socket and first board lane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Lane {
    pub socket: u8,
    pub lane: u8,
}

/// How the OS gets the CXL windows the ABL mapped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Memory {
    /// Usable RAM in the memory map, for a window below TOM2, which
    /// `SYSCFG.Tom2ForceMemTypeWB` caches (no MTRRs for CXL, as the
    /// vendors). TOM2 is lowered to the lowest window until a pool is
    /// enabled ([`super::cxl_memory`]); a window above the ABL's TOM2
    /// would be uncached RAM, so it is soft-reserved instead
    /// ([`map_windows`]). A window whose devices' decoders were not all
    /// committed (so also every window while TOM2 stays lowered) is
    /// soft-reserved before the payload starts ([`not_ram`]): the OS never
    /// gets uncached or undecoded RAM.
    Online,
    /// Soft-reserved (EFI specific-purpose, `EfiConventionalMemory` with
    /// `EFI_MEMORY_SP`): the OS's CXL or DAX drivers claim it. Both
    /// vendors' default (`PcdAmdCxlSpm` 1).
    SoftReserved,
}

/// The CXL input block of SP7 boards: the WH block's values as both
/// Venice donors build it.
pub const POLICY: cxl::Policy = cxl::Policy::WH;
/// The CXL steps after enumeration of SP7 boards: both Venice donors'
/// PCDs.
pub const DXE_POLICY: cxl::DxePolicy = cxl::DxePolicy::WH;

/// A board's CXL on an SP7 SoC.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Config {
    /// The root ports wired to CXL slots or devices (the vendor's per-port
    /// `CxlControl`), on PCIe cores 0..3 (the bonus core 6 has no CXL).
    pub ports: &'static [Lane],
    /// Every trained port is a CXL port (`AmdCxlOnAllPorts`, GIGABYTE's
    /// build value): `ports` is not consulted. The MPIO port policy's
    /// `cxl_on_all_ports` gives them the CXL straps.
    pub on_all_ports: bool,
    /// The root port of each of the board's build-time APCB CXL regions,
    /// in region order: the APOB names a window's devices by region index.
    /// Empty: the regions the APCB booted with name their devices by bus
    /// (the runtime loop's, [`Self::apcb_update`]).
    pub regions: &'static [Lane],
    pub memory: Memory,
    /// The CXL input block.
    pub policy: cxl::Policy,
    /// Lock the emulated host bridge decoders on commit (the vendor's
    /// default; Dell has an opt-out).
    pub lock_host_decoders: bool,
    /// The steps after enumeration (DVSEC lock, lock on commit, SPA,
    /// switch interleave, CDAT, link power).
    pub dxe: cxl::DxePolicy,
    /// Size the devices at every boot and rewrite the APCB's CXL regions
    /// of the ISH slot the PSP booted when they change, then warm-reset
    /// (the vendor DXE's loop; this writes the SPI flash,
    /// [`super::cxl_apcb`]). Off: the build-time regions stay.
    pub apcb_update: bool,
    /// Sync flood on CXL errors of the CXL root bridges (AmdNbioDxe
    /// `CxlEnableSyncFloodCallback`, which the vendor runs when CXL is
    /// present).
    pub sync_flood: bool,
}

/// A root port that negotiated CXL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Port {
    pub at: Lane,
    pub die: u8,
    /// The root bridge's `RootBridge::index`.
    pub index: u8,
    pub core: u8,
    pub port: u8,
    /// The root port (on its root bridge's PCI segment).
    pub root_port: fstart_pci::PciAddress,
    pub version: Version,
    pub device_type: DeviceType,
    /// The RCRB pair of a CXL 1.1 port.
    pub rcrb: Option<u64>,
}

/// A CXL host bridge: a root bridge with CXL ports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HostBridge {
    pub socket: u8,
    pub index: u8,
    /// Its ACPI `_UID`.
    pub uid: u32,
    /// The trapped MMIO window of its component registers (VH only).
    pub trap: Option<u64>,
}

/// What the CXL steps hand on to the tables.
#[derive(Debug, Clone, Default)]
pub struct Cxl {
    pub ports: heapless::Vec<Port, 16>,
    pub bridges: heapless::Vec<HostBridge, 16>,
    pub plans: heapless::Vec<host::WindowPlan, { fstart_driver_amd::apob::cxl::MAX_WINDOWS }>,
    /// The non-PCI MMIO taken so far, per root bridge `(socket, index)`:
    /// its pool below 4 GiB as the NBIO BARs left it (`State::non_pci`,
    /// one allocator: the RCRBs come from its top) and its pool above.
    #[cfg_attr(not(all(feature = "bundle", target_os = "none")), allow(dead_code))]
    pools: heapless::Vec<Pools, 16>,
    /// What the memory steps after enumeration found ([`super::cxl_memory`]).
    pub memory: super::cxl_memory::Found,
}

#[derive(Debug, Clone, Copy)]
#[cfg_attr(not(all(feature = "bundle", target_os = "none")), allow(dead_code))]
struct Pools {
    rb: (u8, u8),
    below: crate::nonpci::NonPci,
    above: Option<crate::nonpci::NonPci>,
}

/// The ACPI `_UID` of root bridge `bus_base` among `buses`: its rank in
/// bus order (`(segment, bus)` with several segments), as the DSDT numbers
/// its `PCIn` (and as the vendor counts the root bridges below it).
pub fn uid<T: Ord>(buses: impl Iterator<Item = T>, bus_base: T) -> u32 {
    buses.filter(|bus| *bus < bus_base).count() as u32
}

/// Whether root bridge `(socket, index)` is a CXL host bridge (`ACPI0016`).
pub fn is_host_bridge(cxl: &Cxl, socket: u8, index: u8) -> bool {
    cxl.bridges
        .iter()
        .any(|b| (b.socket, b.index) == (socket, index))
}

/// The mapped windows the memory map and the SRAT get: `(base, end)`.
pub fn mapped(windows: &[Window]) -> impl Iterator<Item = (u64, u64)> + '_ {
    windows
        .iter()
        .filter(|w| w.mapped())
        .map(|w| (w.base, w.end()))
}

/// Add the mapped CXL `windows` to `e820` as RAM or soft-reserved: ranges
/// the APOB's memory map left out are added, ranges it has are retyped.
/// RAM ([`Memory::Online`]) only below `tom2`, which the CPUs cache: a
/// window that ends above it is soft-reserved. Returns how many were.
pub fn map_windows(
    e820: &mut fstart_core::services::memory_detect::E820State,
    windows: &[Window],
    memory: Memory,
    tom2: u64,
) -> Result<usize, fstart_core::services::ServiceError> {
    use fstart_core::services::memory_detect::E820Kind;
    let mut uncached = 0;
    for (base, end) in mapped(windows) {
        let kind = match memory {
            Memory::Online if end <= tom2 => E820Kind::Ram,
            Memory::Online => {
                uncached += 1;
                E820Kind::SoftReserved
            }
            Memory::SoftReserved => E820Kind::SoftReserved,
        };
        if e820.insert(base, end - base, kind).is_err() {
            e820.set_range_kind(base, end - base, kind)?;
        }
    }
    Ok(uncached)
}

/// The memory map ranges of the mapped CXL `windows` (`(base, end)`) that
/// must not stay RAM: with [`Memory::Online`], each one not among the
/// `decoded` windows (all its devices' decoders committed). They become
/// soft-reserved; the soft-reserved policy has no RAM to take back.
pub fn not_ram(
    windows: &[(u64, u64)],
    memory: Memory,
    decoded: &[(u64, u64)],
) -> Vec<core::ops::Range<u64>> {
    if memory != Memory::Online {
        return Vec::new();
    }
    windows
        .iter()
        .filter(|w| !decoded.contains(w))
        .map(|&(base, end)| base..end)
        .collect()
}

/// Withhold CXL 1.1 device bus `bus`, which the RCRB routes, from the
/// buses PCI enumeration hands out on its root bridge (`buses`): the RCH
/// buses are the root bridge's last ones, so its limit comes down below
/// `bus` (one already above it stays out). `false` if `bus` is the root
/// bus or below it.
pub fn withhold_bus(buses: &mut fstart_driver_amd::df::BusRange, bus: u8) -> bool {
    if bus <= buses.base {
        return false;
    }
    buses.limit = buses.limit.min(bus - 1);
    true
}

/// The RAM `regions` without the CXL windows: what the DF DRAM rules of
/// the UMCs cover (the SRAT's coverage check).
pub fn numa_regions(regions: &[Region], windows: &[(u64, u64)]) -> Vec<Region> {
    let mut out = Vec::new();
    for region in regions {
        let mut pieces = alloc::vec![(region.base, region.end())];
        for &(base, end) in windows {
            pieces = pieces
                .into_iter()
                .flat_map(|(start, stop)| {
                    [(start, stop.min(base)), (start.max(end), stop)]
                        .into_iter()
                        .filter(|(a, b)| a < b)
                })
                .collect();
        }
        out.extend(pieces.into_iter().map(|(start, stop)| Region {
            base: start,
            size: stop - start,
            kind: region.kind,
        }));
    }
    out
}

/// The mapped CXL windows of the APOB as `(base, end, socket)`, when the
/// board has CXL: what the memory map has of them and the SRAT describes.
pub fn windows(
    apob: Option<&fstart_driver_amd::apob::v2::ApobV2<'_>>,
    config: Option<&Config>,
) -> Vec<(u64, u64, u8)> {
    config
        .and(apob)
        .and_then(|apob| apob.cxl_windows().ok())
        .map(|windows| {
            windows
                .iter()
                .filter(|w| w.mapped())
                .map(|w| (w.base, w.end(), w.socket))
                .collect()
        })
        .unwrap_or_default()
}

/// One proximity domain per CXL window after the CPU/DRAM domains (the
/// vendor gives CXL windows physical domains of their own): the windows'
/// memory in `numa`, and the new domains' sockets.
pub fn add_domains(numa: &mut super::numa::Numa, windows: &[(u64, u64, u8)]) -> Vec<u8> {
    let mut sockets = Vec::new();
    for &(base, end, socket) in windows {
        let domain = numa.domains as u32;
        numa.memory.push((base, end, domain));
        numa.domains += 1;
        sockets.push(socket);
    }
    numa.memory.sort_unstable();
    sockets
}

/// The CEDT of `cxl`: one CHBS per host bridge (its trap window, or the
/// RCRB of each CXL 1.1 port), one CFMWS per window. `None` without CXL.
pub fn cedt(cxl: &Cxl) -> Option<Result<Vec<u8>, fstart_acpi::cedt::Error>> {
    use fstart_acpi::cedt::{CedtEntry, ChbsVersion, RCRB_LENGTH};
    if cxl.bridges.is_empty() {
        return None;
    }
    let uid_of = |p: &Port| {
        cxl.bridges
            .iter()
            .find(|b| (b.socket, b.index) == (p.at.socket, p.index))
            .map(|b| b.uid)
    };
    let vh = cxl.bridges.iter().filter_map(|b| {
        Some(CedtEntry::Chbs {
            uid: b.uid,
            version: ChbsVersion::Vh,
            base: b.trap?,
            length: host::CHBS_LENGTH,
        })
    });
    let rch = cxl.ports.iter().filter_map(|p| {
        Some(CedtEntry::Chbs {
            uid: uid_of(p)?,
            version: ChbsVersion::Rch,
            base: p.rcrb?,
            length: RCRB_LENGTH,
        })
    });
    let windows = cxl.plans.iter().map(|p| CedtEntry::Cfmws(p.cfmws()));
    let entries: Vec<CedtEntry> = vh.chain(rch).chain(windows).collect();
    Some(fstart_acpi::cedt::build_cedt(
        fstart_acpi::OEM_ID,
        fstart_acpi::OEM_TABLE_ID,
        &entries,
    ))
}

/// The `ACPI0017` CXL root device for `\_SB`, when there are CXL host
/// bridges (inline: SoC code keeps no strings of its own).
#[inline]
pub fn root_device(cxl: &Cxl) -> Vec<u8> {
    if cxl.bridges.is_empty() {
        return Vec::new();
    }
    fstart_acpi::cedt::root_device("CXLD").unwrap_or_default()
}

#[cfg(all(feature = "bundle", target_os = "none"))]
pub(super) use run::{after_resources, after_training, map_memory, on_bus, tables, warm_reset};

#[cfg(all(feature = "bundle", target_os = "none"))]
mod run {
    extern crate alloc;

    use core::num::NonZeroU32;

    use fstart_driver_amd::apob::cxl::Window;
    use fstart_driver_amd::cxl::mpio::{self, Device, List};
    use fstart_driver_amd::cxl::{DeviceType, Version, host, wh};
    use fstart_driver_amd::dxio::rs::mailbox::Poll;
    use fstart_driver_amd::dxio::wh::{Binding, Proxy};
    use fstart_driver_amd::nbio::wh as nbio_wh;
    use fstart_driver_amd::smn::Smn;
    use fstart_pci::config::{PCI_BRIDGE_BUSES, PCI_EXPRESS_LINK_CAPABILITIES};
    use fstart_pci::cxl::{dvsec, rcrb};
    use fstart_pci::{ConfigRegionAccess, EcamDevice, PciAddress, ecam::GlobalEcam};
    use heapless::Vec;
    use tock_registers::LocalRegisterCopy;
    use tock_registers::interfaces::ReadWriteable;

    use super::{Config, Cxl, HostBridge, Lane, Pools, Port};
    use crate::flow::Ctx;
    use crate::nonpci::NonPci;
    use crate::sp7::fabric::{self, rb_index};
    use crate::sp7::init::State;
    use crate::sp7::{Sp7, Variant};

    /// The MPIO of IOD `(socket, die)` once training is done.
    fn iod_mpio<'t>(
        state: &State,
        socket: u8,
        die: u8,
        timer: &'t dyn fstart_core::services::Timer,
    ) -> Option<Proxy<'t, Smn<GlobalEcam>>> {
        let root = state.node_root(socket, die)?;
        let poll = Poll {
            reads: const { NonZeroU32::new(300_000).unwrap() },
            interval_us: const { NonZeroU32::new(100).unwrap() },
        };
        // SAFETY: the BSP owns the IOD's MPIO after its training session.
        Some(unsafe { Proxy::new(Smn::on_root(GlobalEcam, root), poll, timer) })
    }

    /// A DMA-able device list buffer, reserved for good.
    fn list() -> Result<&'static mut List, &'static str> {
        let bytes = crate::memory::dma_alloc(size_of::<List>(), 64)?;
        // SAFETY: fresh, zeroed, aligned and large enough for a `List`.
        Ok(unsafe { &mut *bytes.as_mut_ptr().cast::<List>() })
    }

    /// The root bus (segment and bus) of a binding's root bridge.
    fn rb_root(state: &State, socket: u8, b: &Binding) -> Option<PciAddress> {
        state.rb_root(socket, rb_index(b.iod, b.rb_on_iod))
    }

    /// Function 0 of device 0 on `bus` of `at`'s segment.
    pub(in crate::sp7) fn on_bus(at: PciAddress, bus: u8) -> EcamDevice {
        EcamDevice::new_address(PciAddress::new(at.segment(), bus, 0, 0))
    }

    /// The CXL device type behind root port `rp` before enumeration: its
    /// secondary bus set to `temporary` for the probe, then restored (the
    /// vendor restores the root bridge's host bridge instead, a bug).
    fn probe_type(rp: PciAddress, temporary: u8) -> DeviceType {
        const BUSES: u16 = fstart_pci::PCI_PRIMARY_BUS;
        let pci = GlobalEcam;
        // SAFETY: a trained, visible root port the BSP owns before
        // enumeration; its bus numbers are restored below.
        let saved = unsafe { pci.read(rp, BUSES) };
        let mut buses = LocalRegisterCopy::<u32, PCI_BRIDGE_BUSES::Register>::new(saved);
        buses.modify(
            PCI_BRIDGE_BUSES::PRIMARY.val(rp.bus().into())
                + PCI_BRIDGE_BUSES::SECONDARY.val(temporary.into())
                + PCI_BRIDGE_BUSES::SUBORDINATE.val(temporary.into()),
        );
        unsafe { pci.write(rp, BUSES, buses.get()) };
        let device = on_bus(rp, temporary);
        let kind = if !device.is_present() {
            DeviceType::Unknown
        } else if (device.read32(fstart_pci::PCI_CLASS_REVISION) >> 16) as u16
            == fstart_pci::PCI_CLASS_BRIDGE_PCI
        {
            // A switch's upstream port: the vendor reports type 3.
            DeviceType::Type3
        } else {
            fstart_pci::cxl::find_dvsec(&device, dvsec::id::DEVICE)
                .map_or(DeviceType::Unknown, |d| {
                    DeviceType::of_capability(device.read16(d + dvsec::DEVICE_CAPABILITY))
                })
        };
        unsafe { pci.write(rp, BUSES, saved) };
        kind
    }

    /// The version a CXL root port negotiated, from its config space.
    fn version(rp: &EcamDevice) -> Version {
        let flex = fstart_pci::cxl::flex_bus_status(rp).map(|s| s.get());
        let status2 = rp
            .find_capability(fstart_pci::PCI_CAP_ID_EXP)
            .map_or(0, |cap| rp.read16(cap + fstart_pci::PCI_EXP_LNKSTA2));
        wh::version(flex, status2)
    }

    /// The non-PCI pools of root bridge `(socket, index)`, as far as the
    /// CXL steps took them.
    fn pools<'a>(cxl: &'a mut Cxl, state: &State, socket: u8, index: u8) -> Option<&'a mut Pools> {
        if let Some(i) = cxl.pools.iter().position(|p| p.rb == (socket, index)) {
            return cxl.pools.get_mut(i);
        }
        let rb = state
            .plan()
            .root_bridges
            .iter()
            .find(|rb| (rb.socket, rb.index) == (socket, index))?;
        cxl.pools
            .push(Pools {
                rb: (socket, index),
                below: state.non_pci(socket, index)?,
                above: rb.above_pools.map(|p| NonPci::new(p.non_pci)),
            })
            .ok()?;
        cxl.pools.last_mut()
    }

    /// `CxlAssignResourcesWh` for a CXL 1.1 port: its RCRB pair below 4 GiB,
    /// its device's bus (the root bridge's last free one, the RCEC told),
    /// the device's CXL type and the downstream port's MEMBAR0 above 4 GiB.
    /// The device's own BARs are not sized (the vendor's `AllocateForCxlIo`).
    fn assign_rch<V: Variant>(
        cxl: &mut Cxl,
        state: &State,
        nbio: &impl fstart_driver_amd::smn::SmnAccess,
        found: &mut Port,
        rp: PciAddress,
    ) -> Option<()> {
        let (socket, index) = (found.at.socket, found.index);
        let limit = state
            .roots()
            .iter()
            .find(|rb| (rb.socket, rb.index) == (socket, index))?
            .bus_limit;
        let earlier = cxl
            .ports
            .iter()
            .filter(|p| (p.at.socket, p.index) == (socket, index) && p.rcrb.is_some())
            .count() as u8;
        let bus = limit.checked_sub(earlier)?;
        let pools = pools(cxl, state, socket, index)?;
        let rcrb = pools.below.alloc(wh::RCRB_SIZE)?;
        // SAFETY: a trained CXL 1.1 root port; `rcrb` is non-PCI MMIO of
        // its root bridge nothing else decodes.
        unsafe { wh::assign_rcrb(&GlobalEcam, rp, rcrb) };
        found.rcrb = Some(rcrb);
        // SAFETY: the downstream port RCRB just enabled, identity-mapped UC.
        let downstream = unsafe { &*(rcrb as usize as *const rcrb::DownstreamPort) };
        // The device's bus as secondary and subordinate, primary 0, the
        // latency byte kept (`CxlAssignResourcesWh`).
        downstream.buses.modify(
            PCI_BRIDGE_BUSES::PRIMARY.val(0)
                + PCI_BRIDGE_BUSES::SECONDARY.val(bus.into())
                + PCI_BRIDGE_BUSES::SUBORDINATE.val(bus.into()),
        );
        wh::assign_rcec_buses(nbio, bus, limit);
        let device = on_bus(rp, bus);
        match fstart_pci::cxl::find_dvsec(&device, dvsec::id::DEVICE) {
            Some(d) => {
                found.device_type =
                    DeviceType::of_capability(device.read16(d + dvsec::DEVICE_CAPABILITY))
            }
            None => fstart_log::warn!(
                "{}: CXL 1.1 device on bus {:#x}: no CXL DVSEC ({:#x})",
                V::NAME,
                bus,
                fstart_driver_amd::cxl::error::NO_DVSEC
            ),
        }
        let Some(membar0) = pools.above.as_mut().and_then(|a| a.alloc(wh::MEMBAR0_SIZE)) else {
            fstart_log::warn!(
                "{}: no CXL 1.1 MEMBAR0 ({:#x})",
                V::NAME,
                fstart_driver_amd::cxl::error::NO_MMIO
            );
            return Some(());
        };
        downstream.set_membar0(membar0);
        Some(())
    }

    /// Step 1, after MPIO training ([`super`]). Failures leave a port as
    /// PCIe and are logged; nothing here fails the boot.
    pub fn after_training<V: Variant>(ctx: &Ctx<'_, Sp7<V>>, state: &State) -> Cxl {
        let mut cxl = Cxl::default();
        let Some(config) = ctx.config.cxl.as_ref() else {
            return cxl;
        };
        for &(socket, b) in state.bindings() {
            let Some(root) = rb_root(state, socket, &b) else {
                continue;
            };
            let Some(node) = state.node_root(socket, b.iod) else {
                continue;
            };
            // SAFETY: the BSP owns the IOD's SMN and the root bridge's NBIO.
            let iod = unsafe { Smn::on_root(GlobalEcam, node) };
            let Some(nbio) = (unsafe { nbio_wh::RootBridge::on_root(GlobalEcam, b.iohc, root) })
            else {
                continue;
            };
            let Some(mpio) = iod_mpio(state, socket, b.iod, &ctx.clock) else {
                continue;
            };
            let port = b.port_id();
            // The vendor jumps past QoS on the bonus core too.
            if b.core != wh::BONUS_CORE && wh::program_qos(&mpio, port, &config.policy.qos).is_err()
            {
                fstart_log::warn!("{}: CXL QoS S{} core {} refused", V::NAME, socket, b.core);
            }
            let bus = root.bus();
            let rp = PciAddress::new(root.segment(), bus, b.devfn >> 3, b.devfn & 7);
            let Some(lane) = state.lane_of(socket, rp) else {
                continue;
            };
            let at = Lane { socket, lane };
            if !config.on_all_ports && !config.ports.contains(&at) {
                continue;
            }
            let a0 = super::super::mpio::layout().is_a0();
            match wh::find_port(&mpio, &iod, &nbio, port, &config.policy, a0) {
                Ok(wh::Found::Cxl) => {}
                Ok(wh::Found::Pcie) => {
                    fstart_log::info!(
                        "{}: CXL port S{} lane {} trained as PCIe",
                        V::NAME,
                        socket,
                        lane
                    );
                    continue;
                }
                Err(error) => {
                    let why = match error {
                        wh::Error::BonusCore => "bonus core",
                        wh::Error::Unavailable => "registers read all ones",
                        wh::Error::Mpio(_) => "MPIO refused",
                    };
                    fstart_log::warn!("{}: CXL port S{} lane {}: {}", V::NAME, socket, lane, why);
                    continue;
                }
            }
            let device = EcamDevice::new_address(rp);
            let version = version(&device);
            let index = rb_index(b.iod, b.rb_on_iod);
            let mut found = Port {
                at,
                die: b.iod,
                index,
                core: b.core,
                port: b.local_port,
                root_port: rp,
                version,
                device_type: DeviceType::Unknown,
                rcrb: None,
            };
            if version == Version::Rch {
                if assign_rch::<V>(&mut cxl, state, &nbio, &mut found, rp).is_none() {
                    fstart_log::warn!(
                        "{}: no CXL 1.1 resources for S{} lane {}",
                        V::NAME,
                        socket,
                        lane
                    );
                    continue;
                }
            } else if bus < u8::MAX {
                found.device_type = probe_type(rp, bus + 1);
            }
            fstart_log::info!(
                "{}: CXL v{} type {} at S{} lane {} ({}:{}:{}.{})",
                V::NAME,
                found.version as u8,
                found.device_type as u8,
                socket,
                lane,
                rp.segment(),
                bus,
                b.devfn >> 3,
                b.devfn & 7
            );
            let _ = cxl.ports.push(found);
        }
        report::<V>(ctx, state, &cxl, false);
        cxl
    }

    /// The CXL ports of each IOD to its MPIO: after training without BDF
    /// (`0x2C`), after enumeration the VH devices with theirs (`0x0D`).
    fn report<V: Variant>(ctx: &Ctx<'_, Sp7<V>>, state: &State, cxl: &Cxl, enumerated: bool) {
        for &(socket, die) in state.nodes() {
            let devices: Vec<Device, { mpio::MAX_DEVICES }> = cxl
                .ports
                .iter()
                .filter(|p| (p.at.socket, p.die) == (socket, die))
                .filter(|p| !enumerated || p.version != Version::Rch)
                .map(|p| Device {
                    // MPIO knows the IOD's buses, not their segment.
                    bdf: enumerated
                        .then(|| endpoint(p))
                        .flatten()
                        .map(|a| (a.bus(), a.device(), a.function())),
                    iohc: p.core,
                    port: p.port,
                    device_type: p.device_type,
                    version: p.version,
                })
                .collect();
            if devices.is_empty() {
                continue;
            }
            let Some(iod) = iod_mpio(state, socket, die, &ctx.clock) else {
                continue;
            };
            let result = list().map_err(|_| None).and_then(|list| {
                // SAFETY: the IOD's idle MPIO; the list is reserved DMA
                // memory below 4 GiB, written back by the sync.
                unsafe {
                    let sync = fstart_arch::x86::writeback_cache_range;
                    if enumerated {
                        mpio::report_enumerated(&iod, list, &devices, sync)
                    } else {
                        mpio::report_devices(&iod, list, &devices, sync)
                    }
                }
                .map_err(Some)
            });
            if result.is_err() {
                fstart_log::warn!(
                    "{}: MPIO CXL devices S{} IOD{} refused",
                    V::NAME,
                    socket,
                    die
                );
            }
        }
    }

    /// The enumerated address of a VH port's device: the root port's
    /// secondary bus, device 0, on the root port's segment.
    fn endpoint(port: &Port) -> Option<PciAddress> {
        let rp = EcamDevice::new_address(port.root_port);
        let secondary = rp.read8(fstart_pci::PCI_SECONDARY_BUS);
        let device = on_bus(port.root_port, secondary);
        (secondary != 0 && device.is_present()).then(|| device.address())
    }

    /// The PCIe port number of a root port (Link Capabilities [31:24]): the
    /// host bridge decoder's target ID.
    fn port_number(port: &Port) -> Option<u8> {
        let rp = EcamDevice::new_address(port.root_port);
        rp.find_capability(fstart_pci::PCI_CAP_ID_EXP).map(|cap| {
            let link = LocalRegisterCopy::<u32, PCI_EXPRESS_LINK_CAPABILITIES::Register>::new(
                rp.read32(cap + fstart_pci::PCI_EXP_LNKCAP),
            );
            link.read(PCI_EXPRESS_LINK_CAPABILITIES::PORT_NUMBER) as u8
        })
    }

    /// Step 3: the board's CXL windows in the memory map.
    pub fn map_memory<V: Variant>(
        e820: &mut fstart_core::services::memory_detect::E820State,
        apob: &fstart_driver_amd::apob::v2::ApobV2<'_>,
        config: Option<&Config>,
        tom2: u64,
    ) {
        let Some(config) = config else {
            return;
        };
        match apob.cxl_windows() {
            Ok(windows) => {
                for (base, end) in super::mapped(&windows) {
                    fstart_log::info!("{}: CXL window {:#x}..{:#x}", V::NAME, base, end);
                }
                match super::map_windows(e820, &windows, config.memory, tom2) {
                    Ok(0) => {}
                    Ok(n) => fstart_log::warn!(
                        "{}: {} CXL window(s) above TOM2 soft-reserved, not RAM: uncached",
                        V::NAME,
                        n as u32
                    ),
                    Err(_) => {
                        fstart_log::warn!("{}: CXL windows do not fit the memory map", V::NAME)
                    }
                }
            }
            Err(_) => fstart_log::warn!("{}: malformed APOB CXL address map", V::NAME),
        }
    }

    /// Step 4: the CEDT among the SoC's tables, `ACPI0017` in `\_SB`.
    pub fn tables<V: Variant>(
        cxl: &Cxl,
        tables: &mut alloc::vec::Vec<alloc::vec::Vec<u8>>,
        aml: &mut alloc::vec::Vec<u8>,
    ) {
        match super::cedt(cxl) {
            Some(Ok(cedt)) => {
                tables.push(cedt);
                aml.extend(super::root_device(cxl));
            }
            Some(Err(_)) => fstart_log::error!("{}: CEDT not built", V::NAME),
            None => {}
        }
    }

    /// The warm reset after the APCB's CXL regions were rewritten
    /// (`ResetSystem(EfiResetWarm)`): the SP7 guarded warm reset
    /// ([`super::super::reset::firmware_warm_reset`], `Reason::CxlApcb`).
    /// Returns if the guard refuses it.
    pub(in crate::sp7) fn warm_reset<V: Variant>(ctx: &Ctx<'_, Sp7<V>>) {
        use super::super::reset::{Reason, firmware_warm_reset};
        fstart_log::warn!("{}: warm reset to map the CXL regions", V::NAME);
        firmware_warm_reset::<V>(ctx.hw, Reason::CxlApcb)
    }

    /// Step 2, after PCI resource allocation ([`super`]).
    pub fn after_resources<V: Variant>(ctx: &Ctx<'_, Sp7<V>>, state: &mut State, cxl: &mut Cxl) {
        let Some(config) = ctx.config.cxl.as_ref() else {
            return;
        };
        if cxl.ports.is_empty() {
            return;
        }
        // Which ISH slot's APCB the ABL mapped the windows from.
        let ish = super::super::cxl_apcb::active_slot::<V>(state, &ctx.clock);
        let state: &State = state;
        for port in cxl.ports.iter_mut().filter(|p| p.version != Version::Rch) {
            if let Some(address) = endpoint(port) {
                let device = EcamDevice::new_address(address);
                if let Some(d) = fstart_pci::cxl::find_dvsec(&device, dvsec::id::DEVICE) {
                    port.device_type =
                        DeviceType::of_capability(device.read16(d + dvsec::DEVICE_CAPABILITY));
                }
            }
        }
        report::<V>(ctx, state, cxl, true);

        let buses = || state.roots().iter().map(|rb| (rb.segment, rb.bus_base));
        for p in &cxl.ports {
            if cxl
                .bridges
                .iter()
                .any(|b| (b.socket, b.index) == (p.at.socket, p.index))
            {
                continue;
            }
            let Some(rb) = state
                .roots()
                .iter()
                .find(|rb| (rb.socket, rb.index) == (p.at.socket, p.index))
            else {
                continue;
            };
            let _ = cxl.bridges.push(HostBridge {
                socket: p.at.socket,
                index: p.index,
                uid: super::uid(buses(), (rb.segment, rb.bus_base)),
                trap: None,
            });
        }

        let windows: heapless::Vec<Window, { fstart_driver_amd::apob::cxl::MAX_WINDOWS }> =
            match ctx.hw.apob.as_ref().map(|apob| apob.cxl_windows()) {
                Some(Ok(windows)) => windows,
                Some(Err(_)) => {
                    fstart_log::warn!("{}: malformed APOB CXL address map", V::NAME);
                    heapless::Vec::new()
                }
                None => heapless::Vec::new(),
            };
        cxl.memory.booted = super::super::cxl_memory::booted::<V>(ctx, ish);
        // Only the array the ABL mapped this boot's windows from names
        // their devices.
        let current: alloc::vec::Vec<_> = cxl
            .memory
            .booted
            .as_ref()
            .map(|b| b.regions().into_iter().collect())
            .unwrap_or_default();
        cxl.memory.matched = fstart_driver_amd::cxl::endpoint::consistent(&current, &windows);
        if !cxl.memory.matched {
            fstart_log::error!(
                "{}: the APCB's CXL regions are not the ones the ABL mapped: no CXL pool enabled",
                V::NAME
            );
        }
        let booted = cxl.memory.booted.as_ref().filter(|_| cxl.memory.matched);
        let target = |region: u8| {
            let port = match config.regions.get(usize::from(region)) {
                Some(&at) => cxl.ports.iter().find(|p| p.at == at)?,
                None => super::super::cxl_memory::region_port(&cxl.ports, booted?, region)?,
            };
            let at = port.at;
            let uid = cxl
                .bridges
                .iter()
                .find(|b| (b.socket, b.index) == (at.socket, port.index))?
                .uid;
            Some(host::Target {
                uid,
                port: port_number(port)?,
            })
        };
        cxl.plans = host::plan(&windows, target);
        for window in windows.iter().filter(|w| w.mapped()) {
            if !cxl.plans.iter().any(|p| p.base == window.base) {
                fstart_log::warn!(
                    "{}: CXL window {:#x}+{:#x} has no CFMWS",
                    V::NAME,
                    window.base,
                    window.size
                );
            }
        }

        // The emulated component registers of every VH host bridge.
        let mut taken = Cxl {
            pools: core::mem::take(&mut cxl.pools),
            ..Cxl::default()
        };
        let mut trapped: heapless::Vec<(u8, u8), { fabric::MAX_NODES }> = heapless::Vec::new();
        for bridge in cxl.bridges.iter_mut() {
            let (socket, index) = (bridge.socket, bridge.index);
            if !cxl
                .ports
                .iter()
                .any(|p| (p.at.socket, p.index) == (socket, index) && p.version != Version::Rch)
            {
                continue;
            }
            let die = fabric::rb_location(index).0;
            let Some(trap) = pools(&mut taken, state, socket, index)
                .and_then(|p| p.above.as_mut())
                .and_then(|above| above.alloc(host::TRAP_SIZE))
            else {
                fstart_log::warn!("{}: no CXL trap window on S{} RB{}", V::NAME, socket, index);
                continue;
            };
            let decoders = host::host_decoders(&cxl.plans, bridge.uid, config.lock_host_decoders);
            let Ok(shadow) = crate::memory::dma_alloc(host::SHADOW_SIZE, 0x1000) else {
                continue;
            };
            if host::build(shadow, &decoders).is_err() {
                fstart_log::warn!(
                    "{}: CXL host bridge decoders of S{} RB{}",
                    V::NAME,
                    socket,
                    index
                );
                continue;
            }
            // SAFETY: the shadow is ours; MPIO reads it from DRAM.
            unsafe { fstart_arch::x86::writeback_cache_range(shadow.as_ptr(), shadow.len()) };
            let Some(iod) = iod_mpio(state, socket, die, &ctx.clock) else {
                continue;
            };
            // SAFETY: the IOD's idle MPIO; the shadow is reserved firmware
            // DRAM and the trap window non-PCI MMIO of this root bridge.
            match unsafe { mpio::trap_host_bridge(&iod, trap, shadow.as_ptr() as u64) } {
                Ok(()) => {
                    bridge.trap = Some(trap);
                    if !trapped.contains(&(socket, die)) {
                        let _ = trapped.push((socket, die));
                    }
                }
                Err(_) => {
                    fstart_log::warn!("{}: MPIO CXL trap S{} RB{} refused", V::NAME, socket, index)
                }
            }
        }
        cxl.pools = taken.pools;
        for &(socket, die) in &trapped {
            if let Some(iod) = iod_mpio(state, socket, die, &ctx.clock)
                && mpio::finalize_host_bridges(&iod).is_err()
            {
                fstart_log::warn!(
                    "{}: MPIO CXL finalize S{} IOD{} refused",
                    V::NAME,
                    socket,
                    die
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fstart_driver_amd::apob::sysmap::RegionKind;

    #[test]
    fn online_windows_nothing_decodes_are_not_ram() {
        const G: u64 = 1 << 30;
        let windows = [(0x400 * G, 0x410 * G), (0x500 * G, 0x510 * G)];
        let decoded = [(0x500 * G, 0x510 * G)];
        let ranges = not_ram(&windows, Memory::Online, &decoded);
        assert_eq!((ranges.len(), &ranges[0]), (1, &(0x400 * G..0x410 * G)));
        // No pool enabled (TOM2 left lowered): every window.
        assert_eq!(not_ram(&windows, Memory::Online, &[]).len(), 2);
        assert!(not_ram(&windows, Memory::SoftReserved, &[]).is_empty());
    }

    #[test]
    fn a_cxl_1_1_bus_leaves_enumeration() {
        use fstart_driver_amd::df::BusRange;
        let mut buses = BusRange {
            base: 0x40,
            limit: 0x7f,
        };
        // Two RCH ports: the root bridge's last two buses.
        assert!(withhold_bus(&mut buses, 0x7f));
        assert!(withhold_bus(&mut buses, 0x7e));
        assert_eq!(buses.limit, 0x7d);
        // Already out: unchanged.
        assert!(withhold_bus(&mut buses, 0x7f));
        assert_eq!(buses.limit, 0x7d);
        // Never the root bus.
        assert!(!withhold_bus(&mut buses, 0x40));
        assert_eq!(
            buses,
            BusRange {
                base: 0x40,
                limit: 0x7d
            }
        );
    }

    #[test]
    fn host_bridge_uids_follow_bus_order() {
        let buses = [0x00u8, 0x40, 0x20, 0x80];
        assert_eq!(uid(buses.into_iter(), 0x00), 0);
        assert_eq!(uid(buses.into_iter(), 0x40), 2);
        assert_eq!(uid(buses.into_iter(), 0x80), 3);
    }

    #[test]
    fn cxl_windows_leave_the_numa_coverage_and_get_domains() {
        const G: u64 = 1 << 30;
        let ram = |base, size| Region {
            base,
            size,
            kind: RegionKind::Ram,
        };
        // An online CXL window inside the e820 RAM, and one outside it.
        let regions = [ram(0, 2 * G), ram(4 * G, 60 * G)];
        let left = numa_regions(&regions, &[(32 * G, 48 * G), (0x400 * G, 0x410 * G)]);
        assert_eq!(
            left.iter().map(|r| (r.base, r.end())).collect::<Vec<_>>(),
            [(0, 2 * G), (4 * G, 32 * G), (48 * G, 64 * G)]
        );

        let mut numa = super::super::numa::Numa {
            nps: fstart_driver_amd::df::dram::Nps::Nps1,
            domains: 1,
            memory: alloc::vec![(0, 2 * G, 0), (4 * G, 32 * G, 0), (48 * G, 64 * G, 0)],
            cpus: alloc::vec![0],
            ccx: alloc::vec::Vec::new(),
        };
        assert_eq!(add_domains(&mut numa, &[(32 * G, 48 * G, 1)]), [1]);
        assert_eq!(numa.domains, 2);
        assert_eq!(numa.memory[2], (32 * G, 48 * G, 1));
    }

    #[test]
    fn windows_go_into_the_memory_map_per_policy() {
        use fstart_core::services::memory_detect::{E820Entry, E820Kind, E820State};
        const G: u64 = 1 << 30;
        let window = |base, size| Window {
            base,
            size,
            status: 0,
            socket: 0,
            nbio_map: 1,
            alignment: 28,
            interleave_size: 0xff,
            sub_interleave_map: [0; 4],
            sub_interleave_size: 0xff,
            memory: fstart_driver_amd::apob::cxl::Memory::Volatile,
            targets: heapless::Vec::new(),
        };
        let mut e820 = E820State::new();
        e820.entries_mut()[0] = E820Entry::new(0, 64 * G, E820Kind::Ram);
        e820.set_detected(1, 64 * G).unwrap();
        // One window the APOB map has as RAM, one above it.
        let windows = [window(32 * G, 16 * G), window(0x400 * G, 16 * G)];
        let fresh = || {
            let mut e820 = E820State::new();
            e820.entries_mut()[0] = E820Entry::new(0, 64 * G, E820Kind::Ram);
            e820.set_detected(1, 64 * G).unwrap();
            e820
        };
        let kinds = |e820: &E820State| -> Vec<_> {
            e820.entries().iter().map(|e| (e.addr, e.kind)).collect()
        };
        assert_eq!(
            map_windows(&mut e820, &windows, Memory::SoftReserved, 64 * G),
            Ok(0)
        );
        let soft = [
            (0, 1),
            (32 * G, 0xefff_ffff),
            (48 * G, 1),
            (0x400 * G, 0xefff_ffff),
        ];
        assert_eq!(kinds(&e820), soft);
        // Online: RAM below TOM2 (cached), soft-reserved above it.
        let mut e820 = fresh();
        assert_eq!(
            map_windows(&mut e820, &windows, Memory::Online, 64 * G),
            Ok(1)
        );
        assert_eq!(
            kinds(&e820),
            [(0, 1), (0x400 * G, 0xefff_ffff)],
            "the first window stays RAM, merged"
        );
        let mut e820 = fresh();
        assert_eq!(
            map_windows(&mut e820, &windows, Memory::Online, 0x800 * G),
            Ok(0)
        );
        assert_eq!(kinds(&e820), [(0, 1), (0x400 * G, 1)]);
    }
}
