// SPDX-License-Identifier: MIT OR Apache-2.0
//! CXL memory on the SP7 SoCs once PCI is enumerated: the vendor DXE's
//! steps after its device list (CxlV2Manager `CxlMgrCallbackAfterPciIo`
//! `0xAB60`; reports/research/CXL_COMPLETE.md §1.1), placed in the flow:
//!
//! 1. `cpus_up` ([`lower_tom2`], `CxlManagerOnMpServices`): TOM2 lowered on
//!    every CPU to the lowest CXL window, so the windows are not cached as
//!    DRAM before their devices decode them;
//! 2. `after_resources` ([`after_resources`]): every CXL device below the
//!    board's CXL ports found (and the endpoints below a switch), read
//!    (DVSEC ranges, CDAT, a DCD's regions, link) and sized
//!    ([`fstart_driver_amd::cxl::apcb`]); with `Config::apcb_update` the
//!    booted ISH slot's APCB regions compared and, changed, rewritten in
//!    the flash and, verified, the system warm-reset
//!    ([`super::cxl_apcb`]; a failed write is undone and nothing resets);
//!    then the root ports' CXL features, each endpoint's DVSEC ranges and
//!    HDM decoders committed (locked on commit), the switches' decoders,
//!    the Enable SCM message before persistent pools, sync flood, and each
//!    window's performance from the devices' CDAT for the HMAT. Pools are
//!    matched to this boot's APOB windows only through the array the ABL
//!    mapped them from (`fstart_driver_amd::cxl::endpoint::consistent`);
//! 3. the tables ([`restore_tom2`], [`hmat_targets`]): TOM2 back once a
//!    pool is enabled (`CxlMgrV2EnableMemoryPool`; it stays lowered when
//!    none is), the CXL domains' HMAT targets (their SLIT distances,
//!    18/28, are `crate::acpi::numa`'s for `Domain::cxl`); an `Online`
//!    window not decoded leaves the RAM of the memory map
//!    ([`super::cxl::not_ram`]).
//!
//! The devices are found on their root port's PCI segment, through as
//! many switch levels as the vendor walks (four,
//! [`fstart_driver_amd::cxl::switch::MAX_DEPTH`]); a switch's window is
//! carved into its upstream ports' and endpoints' decoders by
//! [`fstart_driver_amd::cxl::switch::plan`]. A device's register blocks
//! are BARs PCI enumeration placed in its root bridge's windows, which the
//! DF plan keeps below its `above_cap` (8 TiB), inside the reset image's
//! identity map (128 TiB, the whole lower canonical half the 4-level
//! tables can map): the map check in [`run`] only guards that.

extern crate alloc;

use alloc::vec::Vec;

use fstart_driver_amd::apob::cxl::Window;
use fstart_driver_amd::cxl::hmat::Perf;

use super::cxl_apcb::Booted;

/// What the memory steps found, for the tables.
#[derive(Debug, Clone, Default)]
pub struct Found {
    /// The APCBs of the flash, when the board needs them (runtime regions
    /// or the loop).
    pub booted: Option<Booted>,
    /// TOM2 as the ABL left it and as lowered.
    pub tom2: Option<(u64, u64)>,
    /// The booted array matches this boot's APOB windows
    /// (`fstart_driver_amd::cxl::endpoint::consistent`): pools may be
    /// enabled, and the booted slot may be rewritten.
    pub matched: bool,
    /// An endpoint pool was enabled: TOM2 goes back.
    pub pool_enabled: bool,
    /// The mapped windows `(base, end)` whose every region's decoders
    /// committed ([`decoded`]).
    pub decoded: Vec<(u64, u64)>,
    /// The performance of each mapped window, by its base.
    pub perf: Vec<(u64, Perf)>,
}

/// TOM2 for the CXL `windows`: the lowest mapped window's base when it is
/// below `tom2` and no DRAM range of `dram` (`(base, end)`, without the
/// windows) ends above it, which lowering would cut off.
pub fn lowered_tom2(tom2: u64, windows: &[Window], dram: &[(u64, u64)]) -> Option<u64> {
    let lowest = windows
        .iter()
        .filter(|w| w.mapped())
        .map(|w| w.base)
        .min()?;
    let lowest = lowest & !((1 << 23) - 1);
    (lowest < tom2 && dram.iter().all(|&(_, end)| end <= lowest)).then_some(lowest)
}

/// The mapped `windows` (`(base, end)`) every region of which committed:
/// `committed` holds `(region, ok)` per region index enabled (an endpoint's
/// pool, a switch hierarchy's region), `ok` when every decoder of it (and
/// of the switches and endpoints below) committed. A window with a region
/// missing or failed is not decoded.
pub fn decoded(windows: &[Window], committed: &[(usize, bool)]) -> Vec<(u64, u64)> {
    windows
        .iter()
        .filter(|w| w.mapped() && !w.targets.is_empty())
        .filter(|w| {
            w.targets.iter().all(|&t| {
                let mut of = committed.iter().filter(|(r, _)| *r == usize::from(t));
                let first = of.next();
                first.is_some() && first.into_iter().chain(of).all(|(_, ok)| *ok)
            })
        })
        .map(|w| (w.base, w.end()))
        .collect()
}

/// The HMAT targets of the CXL domains (`FabricHmatGetCxlCdatPerf`):
/// `(domain, socket, performance)` of each mapped window of `windows`
/// (`(base, end, socket)`, the SRAT's CXL domains from `first` on in that
/// order), its devices' CDAT values or the vendor's defaults. The fabric's
/// latency and the cross-socket cap are the HMAT builder's
/// (`crate::acpi::hmat::seen`, the same rule for DRAM and CXL targets).
pub fn hmat_targets(
    found: &Found,
    first: u32,
    windows: &[(u64, u64, u8)],
) -> Vec<(u32, u8, fstart_acpi::hmat::Performance)> {
    windows
        .iter()
        .enumerate()
        .map(|(i, &(base, _, socket))| {
            let p = found
                .perf
                .iter()
                .find(|(b, _)| *b == base)
                .map_or(Perf::DEFAULT, |(_, p)| *p);
            (
                first + i as u32,
                socket,
                fstart_acpi::hmat::Performance {
                    read_latency: p.read_latency,
                    write_latency: p.write_latency,
                    read_bandwidth: p.read_bandwidth,
                    write_bandwidth: p.write_bandwidth,
                },
            )
        })
        .collect()
}

#[cfg(all(feature = "bundle", target_os = "none"))]
pub(super) use run::{
    after_resources, booted, carve_rch, lower_tom2, not_ram, region_port, restore_tom2,
};

#[cfg(all(feature = "bundle", target_os = "none"))]
mod run {
    extern crate alloc;

    use alloc::vec::Vec;

    use fstart_amdfw_format::apcb::CxlRegion;
    use fstart_arch::x86::cpu::msr::{MsrAccess, NativeMsr};
    use fstart_arch::x86::mp::MpHandle;
    use fstart_core::mmio::MmioWindow;
    use fstart_driver_amd::apob::cxl::Window;
    use fstart_driver_amd::cxl::apcb::PoolType;
    use fstart_driver_amd::cxl::apcb::{self, Device, RawRange, Sizing, SizingPolicy};
    use fstart_driver_amd::cxl::{self, Version, endpoint, hmat, switch, wh};
    use fstart_driver_amd::dxio::rs::mailbox::Poll;
    use fstart_driver_amd::dxio::wh::Proxy;
    use fstart_driver_amd::nbio::wh as nbio_wh;
    use fstart_driver_amd::smn::Smn;
    use fstart_pci::cxl::{cdat, doe, dvsec, hdm, mailbox};
    use fstart_pci::{EcamDevice, PciAddress, ecam::GlobalEcam};

    use super::super::cxl::{Config, Cxl, Port, on_bus};
    use super::super::cxl_apcb::{self, Booted};
    use super::super::fabric::rb_index;
    use super::super::init::State;
    use super::super::{Sp7, Variant};
    use crate::flow::Ctx;

    const TOP_MEM2: u32 = 0xc001_001d;
    /// The block sizes fstart maps of a device's register blocks.
    const COMPONENT_WINDOW: u32 = 0x1_0000;
    const MEMDEV_WINDOW: u32 = 0x10_0000;
    /// How long the vendor waits for Memory_Active at most (fstart: 1 s).
    const MEMORY_ACTIVE_US: u64 = 1_000_000;
    const VENDOR_1F16: u16 = 0x1f16;

    /// The APCBs of the flash, the one of ISH slot `ish` (the PSP's
    /// answer) the regions the ABL mapped the windows from.
    pub fn booted<V: Variant>(ctx: &Ctx<'_, Sp7<V>>, ish: Option<u8>) -> Option<Booted> {
        let booted = cxl_apcb::read::<V>(&ctx.hw.fch, ish);
        if booted.is_none() {
            fstart_log::warn!(
                "{}: no APCB CXL regions known (none in the flash, or the ISH slots' differ)",
                V::NAME
            );
        }
        booted
    }

    /// The root port below which APCB region `region`'s device sits (by
    /// its bus, within the root port's secondary..subordinate buses).
    pub fn region_port<'a>(ports: &'a [Port], booted: &Booted, region: u8) -> Option<&'a Port> {
        let r = *booted.regions().get(usize::from(region))?;
        if r.size == 0 {
            return None;
        }
        ports.iter().find(|p| {
            let rp = EcamDevice::new_address(p.root_port);
            let buses = tock_registers::LocalRegisterCopy::<
                u32,
                fstart_pci::config::PCI_BRIDGE_BUSES::Register,
            >::new(rp.read32(fstart_pci::PCI_PRIMARY_BUS));
            let secondary = buses.read(fstart_pci::config::PCI_BRIDGE_BUSES::SECONDARY) as u8;
            let subordinate = buses.read(fstart_pci::config::PCI_BRIDGE_BUSES::SUBORDINATE) as u8;
            u16::from(r.segment) == p.root_port.segment()
                && secondary != 0
                && (secondary..=subordinate).contains(&r.bus)
        })
    }

    /// The highest address the reset image maps (the 4-level identity map
    /// of [`crate::layout::Layout::pdpts`] PDPTs: 128 TiB on SP7).
    fn mapped<V: Variant>() -> u64 {
        (V::LAYOUT.pdpts * 512) << 30
    }

    /// A device's register block as a window, if mapped.
    fn block<V: Variant>(device: &EcamDevice, id: u8, size: u32) -> Option<MmioWindow> {
        let block = dvsec::find_register_block(device, id)?;
        let address = block.address(device)?;
        if address == 0 || address + u64::from(size) > mapped::<V>() {
            fstart_log::warn!("{}: CXL register block {:#x} not mapped", V::NAME, address);
            return None;
        }
        // SAFETY: a BAR PCI enumeration assigned, identity-mapped uncached
        // MMIO of the device.
        Some(unsafe { MmioWindow::new(address as usize, size) })
    }

    /// One CXL device (an endpoint or a switch's upstream port) below a
    /// CXL root port.
    struct Found {
        port: usize,
        address: PciAddress,
        /// The top switch's address of a device below a switch.
        top: Option<PciAddress>,
        /// Its upstream switch (index in the list) and its downstream
        /// port's number.
        parent: Option<(usize, u8)>,
        device: Device,
        sizing: Sizing,
        cdat: Option<Vec<u8>>,
    }

    fn tuple(a: PciAddress) -> (u8, u8, u8, u8) {
        (a.segment() as u8, a.bus(), a.device(), a.function())
    }

    /// PCIe Link Status of `port`: speed [3:0], width [9:4]; Link Status 2
    /// bit 10 (flit mode) of `device`.
    fn link(port: &EcamDevice, device: &EcamDevice) -> (u8, u8, bool) {
        let cap = port.find_capability(fstart_pci::PCI_CAP_ID_EXP);
        let status = cap.map_or(0, |c| port.read16(c + fstart_pci::PCI_EXP_LNKSTA));
        let flit = device
            .find_capability(fstart_pci::PCI_CAP_ID_EXP)
            .is_some_and(|c| device.read16(c + fstart_pci::PCI_EXP_LNKSTA2) & (1 << 10) != 0);
        ((status & 0xf) as u8, ((status >> 4) & 0x3f) as u8, flit)
    }

    /// The device's CDAT through its DOE mailboxes.
    fn read_cdat(device: &EcamDevice, timer: &dyn fstart_core::services::Timer) -> Option<Vec<u8>> {
        let offsets: Vec<u16> = device
            .ext_capabilities()
            .filter(|&(_, h)| h as u16 == doe::EXT_CAP_ID)
            .map(|(o, _)| o)
            .collect();
        for offset in offsets {
            // SAFETY: an enumerated device's DOE capability, BSP-owned.
            let mailbox = unsafe { doe::Doe::new(&GlobalEcam, device.address(), offset) };
            if !mailbox
                .supports(fstart_pci::cxl::VENDOR, cdat::TABLE_ACCESS, timer)
                .unwrap_or(false)
            {
                continue;
            }
            let length = cdat::length(&mailbox, timer).ok()?;
            let mut buf = alloc::vec![0u8; length.max(cdat::HEADER)];
            let table = cdat::read(&mailbox, &mut buf, timer).ok()?.len();
            buf.truncate(table);
            return Some(buf);
        }
        None
    }

    /// What sizing needs of `device` below `link_port` (the root port, or
    /// the switch's downstream port).
    fn read_device<V: Variant>(
        ctx: &Ctx<'_, Sp7<V>>,
        config: &Config,
        p: &Port,
        device: &EcamDevice,
        link_port: &EcamDevice,
        key: (u8, u8),
        switch: bool,
    ) -> (Device, Option<Vec<u8>>) {
        let (speed, width, flit) = link(link_port, device);
        let mut info = Device {
            address: tuple(device.address()),
            key,
            socket: p.at.socket,
            die: p.die,
            core: p.core,
            port: p.port,
            link_width: width,
            link_speed: speed,
            flit,
            switch,
            capability: 0,
            ranges: [RawRange::default(); 2],
            dsmas: heapless::Vec::new(),
            dsemts: None,
            dcd: None,
            class: device.read32(fstart_pci::PCI_CLASS_REVISION) >> 8,
            memory_active: true,
            healthy: true,
        };
        if switch {
            return (info, None);
        }
        let Some(dv) = fstart_pci::cxl::find_dvsec(device, dvsec::id::DEVICE) else {
            return (info, None);
        };
        info.capability = device.read16(dv + dvsec::DEVICE_CAPABILITY);
        for n in 0..2u16 {
            info.ranges[usize::from(n)] = RawRange {
                size_high: device.read32(dv + dvsec::range_size_high(n)),
                size_low: device.read32(dv + dvsec::range_size_low(n)),
            };
        }
        // Memory_Active of range 1 (CxlMgrMarkPoolInactive's poll).
        if info.ranges[0].size_low & 1 != 0 {
            info.memory_active =
                fstart_core::services::timer::poll_until(&ctx.clock, MEMORY_ACTIVE_US, || {
                    (device.read32(dv + dvsec::range_size_low(0)) & 2 != 0).then_some(())
                })
                .is_some();
        }
        let cdat = config
            .dxe
            .cdat
            .then(|| read_cdat(device, &ctx.clock))
            .flatten();
        if let Some(table) = cdat.as_deref().and_then(|t| cdat::Cdat::new(t).ok()) {
            for d in table.dsmas().filter(|d| d.dpa_length != 0) {
                let _ = info.dsmas.push(d.dpa_length);
            }
            info.dsemts = table.dsemts().next().map(|d| d.memory_type);
        }
        // DCD: the CEL lists Get DC Configuration.
        let memdev = block::<V>(device, dvsec::block::MEMORY_DEVICE, MEMDEV_WINDOW);
        if let Some(regs) = memdev
            && let Ok(mb) = mailbox::Mailbox::find(regs)
            && mb
                .cel_lists(mailbox::opcode::GET_DC_CONFIGURATION, &ctx.clock)
                .unwrap_or(false)
            && let Ok(lengths) = mb.dc_region_lengths(&ctx.clock)
        {
            info.dcd = Some(lengths);
        }
        // The media and HDM checks, skipped for a vendor 1F16h memory
        // controller without a Register Locator.
        let locator = fstart_pci::cxl::find_dvsec(device, dvsec::id::REGISTER_LOCATOR).is_some();
        let exempt = device.vendor_id() == VENDOR_1F16 && info.class >> 8 == 0x0502 && !locator;
        if !exempt {
            let media = memdev
                .and_then(|regs| {
                    let at =
                        mailbox::find_capability(&regs, mailbox::capability::MEMORY_DEVICE_STATUS)?;
                    Some(mailbox::memory_device_status::ready(regs.read32(at)))
                })
                .unwrap_or(false);
            let hdm = block::<V>(device, dvsec::block::COMPONENT, COMPONENT_WINDOW)
                .and_then(hdm::Hdm::of_component)
                .is_some();
            info.healthy = media && hdm;
        }
        (info, cdat)
    }

    /// The devices below the VH CXL ports: an endpoint, or a switch's
    /// upstream port and what is below its downstream ports, switches to
    /// [`switch::MAX_DEPTH`] levels (`CxlMgrEnableSwitchRecursive`'s limit),
    /// in depth-first order (a switch before what is below it).
    fn discover<V: Variant>(ctx: &Ctx<'_, Sp7<V>>, config: &Config, cxl: &Cxl) -> Vec<Found> {
        let mut out: Vec<Found> = Vec::new();
        for (i, p) in cxl.ports.iter().enumerate() {
            if p.version == Version::Rch {
                continue;
            }
            let rp = EcamDevice::new_address(p.root_port);
            let secondary = rp.read8(fstart_pci::PCI_SECONDARY_BUS);
            let dev = on_bus(p.root_port, secondary);
            if secondary == 0 || !dev.is_present() {
                continue;
            }
            let key = (p.root_port.segment() as u8, secondary);
            let switch = is_bridge(&dev);
            let (device, cdat) = read_device::<V>(ctx, config, p, &dev, &rp, key, switch);
            out.push(Found {
                port: i,
                address: dev.address(),
                top: None,
                parent: None,
                device,
                sizing: Sizing::default(),
                cdat,
            });
            if switch {
                let below = Below {
                    port: i,
                    top: dev.address(),
                    key,
                };
                let usp = out.len() - 1;
                walk_switch::<V>(ctx, config, p, &below, &mut out, usp, 1);
            }
        }
        out
    }

    /// What every device below one top switch shares.
    struct Below {
        port: usize,
        top: PciAddress,
        key: (u8, u8),
    }

    fn is_bridge(device: &EcamDevice) -> bool {
        (device.read32(fstart_pci::PCI_CLASS_REVISION) >> 16) as u16
            == fstart_pci::PCI_CLASS_BRIDGE_PCI
    }

    /// The devices on the downstream ports of the switch `out[usp]`, at
    /// switch level `depth`.
    fn walk_switch<V: Variant>(
        ctx: &Ctx<'_, Sp7<V>>,
        config: &Config,
        p: &Port,
        below: &Below,
        out: &mut Vec<Found>,
        usp: usize,
        depth: usize,
    ) {
        let upstream = EcamDevice::new_address(out[usp].address);
        let inner = upstream.read8(fstart_pci::PCI_SECONDARY_BUS);
        if inner == 0 {
            return;
        }
        for d in 0..32u8 {
            let dsp =
                EcamDevice::new_address(PciAddress::new(upstream.address().segment(), inner, d, 0));
            if !dsp.is_present() {
                continue;
            }
            let bus = dsp.read8(fstart_pci::PCI_SECONDARY_BUS);
            let dev = on_bus(dsp.address(), bus);
            if bus == 0 || !dev.is_present() {
                continue;
            }
            let switch = is_bridge(&dev);
            if switch && depth >= switch::MAX_DEPTH {
                fstart_log::warn!(
                    "{}: CXL switch below bus {:#x} beyond {} levels skipped",
                    V::NAME,
                    inner,
                    switch::MAX_DEPTH as u32
                );
                continue;
            }
            let dsp_port = port_number(&dsp).unwrap_or(d);
            let (device, cdat) = read_device::<V>(ctx, config, p, &dev, &dsp, below.key, switch);
            out.push(Found {
                port: below.port,
                address: dev.address(),
                top: Some(below.top),
                parent: Some((usp, dsp_port)),
                device,
                sizing: Sizing::default(),
                cdat,
            });
            if switch {
                let usp = out.len() - 1;
                walk_switch::<V>(ctx, config, p, below, out, usp, depth + 1);
            }
        }
    }

    fn port_number(port: &EcamDevice) -> Option<u8> {
        let cap = port.find_capability(fstart_pci::PCI_CAP_ID_EXP)?;
        Some((port.read32(cap + fstart_pci::PCI_EXP_LNKCAP) >> 24) as u8)
    }

    /// Size every device, switches as their endpoints' sum.
    fn size(found: &mut [Found], current: &[CxlRegion], apob: bool, dual: bool) {
        let policy = SizingPolicy {
            dual_hdm_ranges: dual,
        };
        for f in found.iter_mut() {
            let region = apcb::search(current, f.device.key);
            f.sizing = apcb::size(&f.device, region, apob, policy);
        }
        // Deepest first: a nested switch has its sum before its parent
        // adds it.
        for i in (0..found.len()).rev() {
            if let Some((usp, _)) = found[i].parent {
                let below = found[i].sizing.clone();
                apcb::add_to_switch(&mut found[usp].sizing, &below);
            }
        }
    }

    /// The APCB loop (`CxlMgrUpdateApcb`) on the booted ISH slot's APCB:
    /// returns when it is up to date, its tracking was cleared, it could
    /// not be written (restored, no reset), or the reset was refused.
    fn update_apcb<V: Variant>(
        ctx: &Ctx<'_, Sp7<V>>,
        state: &mut State,
        booted: &Booted,
        matched: bool,
        found: &[Found],
    ) {
        // Device order: endpoints directly below root ports and switches,
        // a switch's endpoints in its region.
        let devices: Vec<(Device, Sizing)> = found
            .iter()
            .filter(|f| f.parent.is_none())
            .map(|f| (f.device.clone(), f.sizing.clone()))
            .collect();
        let regions = apcb::regions(&devices);
        let Ok(mut new) = fstart_amdfw_format::apcb::cxl_array_of(&regions) else {
            return;
        };
        let excluded = found.iter().any(|f| f.sizing.tracking.exclude);
        let decision = apcb::decide(&booted.array, &new, excluded);
        let reset = match decision {
            apcb::Decision::UpToDate => {
                fstart_log::info!("{}: CXL APCB descriptors are up to date", V::NAME);
                return;
            }
            apcb::Decision::ClearTracking => {
                apcb::clear_tracking(&mut new);
                false
            }
            apcb::Decision::Rewrite => true,
        };
        // Only the slot the PSP booted, and only while its array is the
        // one the ABL mapped this boot's windows from.
        if booted.booted.is_none() {
            fstart_log::error!(
                "{}: the PSP did not say which ISH slot it booted: CXL APCB not written",
                V::NAME
            );
            return;
        }
        if !matched {
            fstart_log::error!(
                "{}: the booted CXL APCB does not match the ABL's windows: not written",
                V::NAME
            );
            return;
        }
        let Some(slot) = booted.stale(&new) else {
            return;
        };
        match cxl_apcb::write::<V>(state, &ctx.hw.fch, &ctx.clock, slot, &new) {
            Ok(()) if reset => {
                fstart_log::info!("{}: CXL APCB descriptors flushed to the flash", V::NAME);
                super::super::cxl::warm_reset::<V>(ctx);
            }
            Ok(()) => {
                fstart_log::info!("{}: CXL APCB failure tracking cleared", V::NAME);
            }
            Err(cxl_apcb::Failure::NotWritten) => fstart_log::error!(
                "{}: CXL APCB at flash {:#x} not written (the flash keeps the old one)",
                V::NAME,
                slot.offset
            ),
            Err(cxl_apcb::Failure::Damaged(offset)) => fstart_log::error!(
                "{}: CXL APCB at flash {:#x}: sector {:#x} could not be restored; reflash the ISH slot",
                V::NAME,
                slot.offset,
                offset
            ),
        }
    }

    /// Memory Space Enable on `device`.
    fn memory_enable(device: &EcamDevice) {
        device.or16(fstart_pci::PCI_COMMAND, fstart_pci::PCI_CMD_MEMORY);
    }

    /// One endpoint decoder to commit.
    #[derive(Clone, Copy)]
    struct EpDecoder {
        pool: usize,
        base: u64,
        size: u64,
        ways: u8,
        granularity: u32,
        persistent: bool,
    }

    /// Program endpoint `f` (`CxlMgrEnableEndpointMemoryPools`,
    /// `CxlMgrEndpointDecoderProgramming`): Memory Space, the DVSEC range
    /// bases, Mem_Enable and the lock, the Enable SCM message before a
    /// persistent pool, then its HDM decoders committed and enabled.
    /// Returns whether every decoder committed (`true` without decoders).
    fn program_endpoint<V: Variant>(
        ctx: &Ctx<'_, Sp7<V>>,
        state: &State,
        config: &Config,
        p: &Port,
        f: &Found,
        decoders: &[EpDecoder],
    ) -> bool {
        let device = EcamDevice::new_address(f.address);
        memory_enable(&device);
        if decoders.is_empty() {
            return true;
        }
        let Some(dv) = fstart_pci::cxl::find_dvsec(&device, dvsec::id::DEVICE) else {
            return false;
        };
        for d in decoders {
            if d.pool < 2 {
                dvsec::Range::set_base(&device, dv, d.pool as u16, d.base);
            }
            if d.persistent
                && cxl::enable_scm(&state.smu_on(p.at.socket, p.die, ctx.clock)).is_err()
            {
                fstart_log::warn!("{}: SMU Enable SCM refused", V::NAME);
            }
        }
        dvsec::enable(&device, dv, true);
        if config.dxe.dvsec_lock {
            dvsec::lock(&device, dv);
        }
        let Some(hdm) = block::<V>(&device, dvsec::block::COMPONENT, COMPONENT_WINDOW)
            .and_then(hdm::Hdm::of_component)
        else {
            fstart_log::warn!(
                "{}: CXL device {:#x}: no HDM decoders ({:#x})",
                V::NAME,
                f.address.bus(),
                cxl::error::HDM_CAPABILITY
            );
            return false;
        };
        let mut all = true;
        for d in decoders {
            let commit = hdm::Commit {
                base: d.base,
                size: d.size,
                ways: d.ways,
                granularity: d.granularity,
                targets: None,
                lock: config.dxe.lock_on_commit,
            };
            if hdm.commit(d.pool as u32, &commit, &ctx.clock).is_err() {
                all = false;
                fstart_log::warn!(
                    "{}: CXL device {:#x} decoder {} not committed ({:#x})",
                    V::NAME,
                    f.address.bus(),
                    d.pool as u32,
                    cxl::error::NOT_COMMITTED
                );
            }
        }
        hdm.enable();
        all
    }

    /// Enable endpoint `f` right below a root port: its pools' windows
    /// ([`endpoint::plan`]). Returns `(region, committed)` of each pool
    /// planned.
    fn enable_endpoint<V: Variant>(
        ctx: &Ctx<'_, Sp7<V>>,
        state: &State,
        config: &Config,
        cxl: &Cxl,
        f: &Found,
        (regions, windows): (&[CxlRegion], &[Window]),
    ) -> Vec<(usize, bool)> {
        let p = &cxl.ports[f.port];
        let plans = endpoint::plan(
            regions,
            windows,
            tuple(f.address),
            None,
            f.sizing.pools.len().max(1),
            p.at.socket,
            config.dxe.spa,
        );
        let decoders: Vec<EpDecoder> = plans
            .iter()
            .map(|plan| EpDecoder {
                pool: plan.pool,
                base: plan.base,
                size: plan.size,
                ways: plan.ways,
                granularity: plan.granularity,
                persistent: plan.persistent(),
            })
            .collect();
        let ok = program_endpoint::<V>(ctx, state, config, p, f, &decoders);
        plans.iter().map(|plan| (plan.region, ok)).collect()
    }

    /// A switch upstream port's decoders (`CxlMgrSwitchDecoderProgramming`,
    /// targets its downstream ports), or without an HDM capability its
    /// Port Extensions DVSEC range over all of them
    /// (`CxlSwitchUsExtDvsecProgramming`). Returns whether every decoder
    /// committed (or the range was set).
    fn program_switch<V: Variant>(
        ctx: &Ctx<'_, Sp7<V>>,
        usp: &Found,
        decoders: &[switch::Assigned],
    ) -> bool {
        let device = EcamDevice::new_address(usp.address);
        memory_enable(&device);
        if decoders.is_empty() {
            return true;
        }
        match block::<V>(&device, dvsec::block::COMPONENT, COMPONENT_WINDOW)
            .and_then(hdm::Hdm::of_component)
        {
            Some(hdm) => {
                let mut all = true;
                for d in decoders {
                    let commit = hdm::Commit {
                        base: d.base,
                        size: d.size,
                        ways: d.ways,
                        granularity: d.granularity,
                        targets: d.targets,
                        lock: false,
                    };
                    if hdm.commit(d.decoder.into(), &commit, &ctx.clock).is_err() {
                        all = false;
                        fstart_log::warn!(
                            "{}: CXL switch {:#x} decoder {} not committed ({:#x})",
                            V::NAME,
                            usp.address.bus(),
                            u32::from(d.decoder),
                            cxl::error::SWITCH_NOT_COMMITTED
                        );
                    }
                }
                hdm.enable();
                all
            }
            None => {
                fstart_log::info!(
                    "{}: CXL switch without HDM decoders ({:#x}): Port Extensions ranges",
                    V::NAME,
                    cxl::error::SWITCH_HDM_CAPABILITY
                );
                let base = decoders.iter().map(|d| d.base).min().unwrap_or(0);
                let end = decoders.iter().map(|d| d.base + d.size).max().unwrap_or(0);
                let Some(dv) = fstart_pci::cxl::find_dvsec(&device, dvsec::id::PORT_EXTENSIONS)
                else {
                    return false;
                };
                dvsec::port_extensions::set_range(&device, dv, base, end - base);
                true
            }
        }
    }

    /// The hierarchy below the top switch `found[top]`: its window (the
    /// top switch's region's, [`endpoint::plan`]) carved into the decoders
    /// of every switch and endpoint below it ([`switch::plan`]), each
    /// programmed. Returns `(region, committed)` of the top switch's
    /// region when an endpoint pool was planned below it.
    fn enable_hierarchy<V: Variant>(
        ctx: &Ctx<'_, Sp7<V>>,
        state: &State,
        config: &Config,
        cxl: &Cxl,
        found: &[Found],
        top: usize,
        (regions, windows): (&[CxlRegion], &[Window]),
    ) -> Vec<(usize, bool)> {
        let p = &cxl.ports[found[top].port];
        let Some((region, window)) = endpoint::plan(
            regions,
            windows,
            tuple(found[top].address),
            None,
            1,
            p.at.socket,
            config.dxe.spa,
        )
        .first()
        .map(|plan| {
            (
                plan.region,
                switch::Window {
                    base: plan.base,
                    size: plan.size,
                    ways: plan.ways,
                    granularity: plan.granularity,
                },
            )
        }) else {
            fstart_log::warn!(
                "{}: CXL switch {:#x}: no window mapped",
                V::NAME,
                found[top].address.bus()
            );
            program_switch::<V>(ctx, &found[top], &[]);
            return Vec::new();
        };
        // The subtree as the planner's nodes: the top switch first.
        let members: Vec<usize> = (top..found.len())
            .take_while(|&i| i == top || found[i].top == Some(found[top].address))
            .collect();
        let node_of = |i: usize| members.iter().position(|&m| m == i);
        let nodes: Vec<switch::Node> = members
            .iter()
            .map(|&i| switch::Node {
                parent: found[i]
                    .parent
                    .and_then(|(u, port)| Some((node_of(u)?, port))),
                switch: found[i].device.switch,
                pools: found[i].sizing.pools.clone(),
            })
            .collect();
        let assigned = switch::plan(&nodes, 0, window, config.dxe.switch_interleave);
        let (mut enabled, mut ok) = (false, true);
        for (n, &i) in members.iter().enumerate() {
            let mine: Vec<switch::Assigned> =
                assigned.iter().filter(|a| a.node == n).copied().collect();
            if found[i].device.switch {
                ok &= program_switch::<V>(ctx, &found[i], &mine);
                continue;
            }
            let pools = &found[i].sizing.pools;
            let decoders: Vec<EpDecoder> = mine
                .iter()
                .map(|a| EpDecoder {
                    pool: a.decoder.into(),
                    base: a.base,
                    size: a.size,
                    ways: a.ways,
                    granularity: a.granularity,
                    persistent: pools.get(usize::from(a.decoder)).is_some_and(|pool| {
                        matches!(pool.kind, PoolType::Persistent | PoolType::DualMode)
                    }),
                })
                .collect();
            enabled |= !decoders.is_empty();
            ok &= program_endpoint::<V>(ctx, state, config, p, &found[i], &decoders);
        }
        if enabled {
            alloc::vec![(region, ok)]
        } else {
            Vec::new()
        }
    }

    /// The performance of the mapped windows from their devices' CDAT: a
    /// switch's region from the endpoints below it (their first DSMAS).
    fn window_perf(
        found: &[Found],
        regions: &[CxlRegion],
        windows: &[Window],
    ) -> Vec<(u64, hmat::Perf)> {
        let perf = |f: &Found, ordinal: usize| match f
            .cdat
            .as_deref()
            .and_then(|t| cdat::Cdat::new(t).ok())
        {
            Some(t) => {
                let handle = t.dsmas().nth(ordinal).map_or(0, |d| d.handle);
                hmat::device(t.dslbis(), handle)
            }
            None => hmat::Perf::DEFAULT,
        };
        windows
            .iter()
            .filter(|w| w.mapped())
            .map(|w| {
                let mut perfs: Vec<hmat::Perf> = Vec::new();
                for &index in &w.targets {
                    let Some(r) = regions.get(usize::from(index)) else {
                        continue;
                    };
                    let key = (r.segment, r.bus, r.device, r.function);
                    let Some(f) = found.iter().find(|f| tuple(f.address) == key) else {
                        continue;
                    };
                    if f.device.switch {
                        perfs.extend(
                            found
                                .iter()
                                .filter(|e| e.top == Some(f.address) && !e.device.switch)
                                .map(|e| perf(e, 0)),
                        );
                        continue;
                    }
                    // The region's ordinal among its device's: the DSMAS
                    // of that ordinal.
                    let ordinal = regions[..usize::from(index)]
                        .iter()
                        .filter(|o| (o.segment, o.bus, o.device, o.function) == key)
                        .count();
                    perfs.push(perf(f, ordinal));
                }
                (w.base, hmat::domain(&perfs))
            })
            .collect()
    }

    /// Sync flood on CXL errors of every CXL root bridge.
    fn sync_flood(state: &State, cxl: &Cxl) {
        for bridge in &cxl.bridges {
            let Some(&(_, b)) = state
                .bindings()
                .iter()
                .find(|(s, b)| *s == bridge.socket && rb_index(b.iod, b.rb_on_iod) == bridge.index)
            else {
                continue;
            };
            let Some(root) = state.rb_root(bridge.socket, bridge.index) else {
                continue;
            };
            // SAFETY: the BSP owns the root bridge's NBIO.
            if let Some(nbio) = unsafe { nbio_wh::RootBridge::on_root(GlobalEcam, b.iohc, root) } {
                wh::enable_sync_flood(&nbio);
            }
        }
    }

    /// The root ports' CXL features (`CxlConfigurePortFeatures`).
    fn configure_ports<V: Variant>(
        ctx: &Ctx<'_, Sp7<V>>,
        state: &State,
        config: &Config,
        cxl: &Cxl,
    ) {
        let a0 = super::super::mpio::layout().is_a0();
        for p in cxl.ports.iter().filter(|p| p.version != Version::Rch) {
            let Some(&(_, b)) = state.bindings().iter().find(|(s, b)| {
                *s == p.at.socket && b.core == p.core && b.local_port == p.port && b.iod == p.die
            }) else {
                continue;
            };
            let Some(root) = state.node_root(p.at.socket, p.die) else {
                continue;
            };
            let poll = Poll {
                reads: const { core::num::NonZeroU32::new(300_000).unwrap() },
                interval_us: const { core::num::NonZeroU32::new(100).unwrap() },
            };
            // SAFETY: the BSP owns the IOD's MPIO and SMN after its training.
            let (proxy, iod) = unsafe {
                (
                    Proxy::new(Smn::on_root(GlobalEcam, root), poll, &ctx.clock),
                    Smn::on_root(GlobalEcam, root),
                )
            };
            let rp = p.root_port;
            let port = b.port_id();
            match wh::configure_port(&proxy, &iod, &GlobalEcam, rp, port, &config.dxe, a0) {
                Ok(None | Some(wh::Isolation::Enabled(_))) => {}
                Ok(Some(wh::Isolation::Unsupported)) => fstart_log::warn!(
                    "{}: CXL.mem isolation not supported at S{} lane {}",
                    V::NAME,
                    p.at.socket,
                    p.at.lane
                ),
                Err(_) => fstart_log::warn!(
                    "{}: CXL port features S{} lane {} refused",
                    V::NAME,
                    p.at.socket,
                    p.at.lane
                ),
            }
            memory_enable(&EcamDevice::new_address(rp));
        }
    }

    /// Step 2 ([`super`]): after `sp7::cxl::after_resources`.
    pub fn after_resources<V: Variant>(ctx: &Ctx<'_, Sp7<V>>, state: &mut State, cxl: &mut Cxl) {
        let Some(config) = ctx.config.cxl.as_ref() else {
            return;
        };
        if cxl.ports.is_empty() {
            return;
        }
        let windows: heapless::Vec<Window, { fstart_driver_amd::apob::cxl::MAX_WINDOWS }> = ctx
            .hw
            .apob
            .as_ref()
            .and_then(|apob| apob.cxl_windows().ok())
            .unwrap_or_default();
        let mut found = discover::<V>(ctx, config, cxl);
        let current: Vec<CxlRegion> = cxl
            .memory
            .booted
            .as_ref()
            .map(|b| b.regions().into_iter().collect())
            .unwrap_or_default();
        size(
            &mut found,
            &current,
            !windows.is_empty(),
            config.dxe.dual_hdm_ranges,
        );
        for f in &found {
            if let Some(agg) = f.sizing.aggregate {
                fstart_log::info!(
                    "{}: CXL device {:#x}: {:#x} bytes, tracking {:#x}",
                    V::NAME,
                    f.address.bus(),
                    agg.size,
                    f.sizing.tracking.status()
                );
            }
        }
        if config.apcb_update
            && let Some(booted) = cxl.memory.booted.clone()
        {
            update_apcb::<V>(ctx, state, &booted, cxl.memory.matched, &found);
        }
        configure_ports::<V>(ctx, state, config, cxl);
        // The regions the ABL mapped the windows from; none when the
        // booted array is not theirs.
        let mut regions = if cxl.memory.matched {
            current.clone()
        } else {
            Vec::new()
        };
        fill_addresses(&mut regions, cxl, config, &found);
        let mut committed: Vec<(usize, bool)> = Vec::new();
        for (i, f) in found.iter().enumerate().filter(|(_, f)| f.parent.is_none()) {
            committed.extend(if f.device.switch {
                enable_hierarchy::<V>(ctx, state, config, cxl, &found, i, (&regions, &windows))
            } else {
                enable_endpoint::<V>(ctx, state, config, cxl, f, (&regions, &windows))
            });
        }
        cxl.memory.pool_enabled = !committed.is_empty();
        cxl.memory.decoded = super::decoded(&windows, &committed);
        if config.sync_flood {
            sync_flood(state, cxl);
        }
        cxl.memory.perf = window_perf(&found, &regions, &windows);
    }

    /// The booted regions with the device addresses build-time regions
    /// lack (bus 0): the device below the board's root port of each
    /// ([`Config::regions`]).
    fn fill_addresses(regions: &mut [CxlRegion], cxl: &Cxl, config: &Config, found: &[Found]) {
        for (r, at) in regions.iter_mut().zip(config.regions) {
            if r.size == 0 || r.bus != 0 {
                continue;
            }
            let Some(f) = cxl
                .ports
                .iter()
                .position(|p| p.at == *at)
                .and_then(|i| found.iter().find(|f| f.port == i && f.parent.is_none()))
            else {
                continue;
            };
            (r.segment, r.bus, r.device, r.function) = tuple(f.address);
        }
    }

    /// CXL 1.1 devices (`AllocateForCxlIo`, `CxlResourceCallback`,
    /// `0x357B0`): their BARs sized, windows for them carved off the top of
    /// their root bridge's non-prefetchable pools below and above 4 GiB
    /// (which PCI enumeration then does not use), forwarded by the RCRBs
    /// with the upstream port's component registers at the top, and the
    /// device's BARs placed in them (fstart's: the vendor's code that
    /// assigns them was not found). The device's bus, which the RCRB
    /// routes, leaves the buses PCI enumeration hands out on the root
    /// bridge ([`super::super::cxl::withhold_bus`]), so no bridge is given
    /// it too. That range is also the root bridge's ACPI `_CRS` bus range;
    /// whether the vendor's includes the RCH bus is not known.
    pub fn carve_rch<V: Variant>(state: &mut State) {
        use fstart_pci::config::PCI_BRIDGE_BUSES;
        use tock_registers::interfaces::Readable;
        let rch: Vec<(u8, u8, u64)> = state
            .cxl()
            .ports
            .iter()
            .filter_map(|p| Some((p.at.socket, p.index, p.rcrb?)))
            .collect();
        for (socket, index, rcrb) in rch {
            // SAFETY: the RCRB pair `assign_rch` enabled, identity-mapped UC.
            let ds = unsafe { &*(rcrb as usize as *const fstart_pci::cxl::rcrb::DownstreamPort) };
            let bus = ds.buses.read(PCI_BRIDGE_BUSES::SECONDARY) as u8;
            if bus != 0
                && let Some(rb) = state
                    .plan_mut()
                    .root_bridges
                    .iter_mut()
                    .find(|rb| (rb.socket, rb.index) == (socket, index))
                && !super::super::cxl::withhold_bus(&mut rb.bus, bus)
            {
                fstart_log::warn!(
                    "{}: CXL 1.1 device bus {:#x} is its root bridge's root bus",
                    V::NAME,
                    bus
                );
            }
            let Some(root) = state.rb_root(socket, index) else {
                continue;
            };
            let device = on_bus(root, bus);
            if bus == 0 || !device.is_present() {
                continue;
            }
            // SAFETY: the RCiEP the RCRB routes, memory decode still off.
            let bars = unsafe { wh::probe_bars(&GlobalEcam, device.address()) };
            let sizes: Vec<(u64, bool)> =
                bars.iter().map(|&(_, size, wide)| (size, wide)).collect();
            let need = wh::RciepBars::of(&sizes);
            let Some(rb) = state
                .plan_mut()
                .root_bridges
                .iter_mut()
                .find(|rb| (rb.socket, rb.index) == (socket, index))
            else {
                continue;
            };
            let Some(above) = rb.above_pools.as_mut() else {
                fstart_log::warn!("{}: no MMIO above 4 GiB for a CXL 1.1 device", V::NAME);
                continue;
            };
            let below = &mut rb.below_pools.non_prefetch;
            let (w32, w64) = need.carve(below.end(), above.non_prefetch.end());
            if w32.0 < below.base || w64.0 < above.non_prefetch.base {
                fstart_log::warn!("{}: CXL 1.1 device on bus {:#x} does not fit", V::NAME, bus);
                continue;
            }
            below.size = w32.0 - below.base;
            above.non_prefetch.size = w64.0 - above.non_prefetch.base;
            // SAFETY: as above; the windows are this root bridge's MMIO,
            // taken from PCI.
            unsafe { wh::forward_rch(rcrb, w32, w64) };
            // The BARs, largest first in each window.
            let mut sorted: Vec<(u16, u64, bool)> = bars.iter().copied().collect();
            sorted.sort_unstable_by_key(|&(_, size, _)| core::cmp::Reverse(size));
            let (mut at32, mut at64) = (w32.0, w64.0);
            for (bar, size, wide) in sorted {
                let at = if wide { &mut at64 } else { &mut at32 };
                *at = at.next_multiple_of(size.max(16));
                device.write32(bar, *at as u32);
                if wide {
                    device.write32(bar + 4, (*at >> 32) as u32);
                }
                *at += size;
            }
            memory_enable(&device);
            fstart_log::info!(
                "{}: CXL 1.1 device on bus {:#x}: {:#x}+{:#x}, {:#x}+{:#x}",
                V::NAME,
                bus,
                w32.0,
                w32.1,
                w64.0,
                w64.1
            );
        }
    }

    fn write_tom2(mp: &MpHandle, value: u64) {
        let write = || {
            // SAFETY: TOM2 of the running CPU; the windows above it are
            // not in use.
            let msr = unsafe { NativeMsr::new() };
            msr.write(TOP_MEM2, value);
        };
        mp.scope(|s| s.broadcast(&write));
        write();
    }

    /// Step 1: TOM2 down to the lowest CXL window on every CPU, when the
    /// board has CXL and the ABL mapped windows below TOM2.
    pub fn lower_tom2<V: Variant>(ctx: &Ctx<'_, Sp7<V>>, mp: &MpHandle) -> Option<(u64, u64)> {
        ctx.config.cxl.as_ref()?;
        let apob = ctx.hw.apob.as_ref()?;
        let windows = apob.cxl_windows().ok()?;
        // SAFETY: the BSP's TOM2.
        let tom2 = unsafe { NativeMsr::new() }.read(TOP_MEM2);
        let map = apob.system_memory_map().ok()?;
        let ram = fstart_driver_amd::apob::sysmap::memory_map(
            super::super::HOLES,
            map.top,
            map.holes().iter().copied(),
        )
        .ok()?;
        let mapped: Vec<(u64, u64)> = super::super::cxl::mapped(&windows).collect();
        let dram: Vec<(u64, u64)> = super::super::cxl::numa_regions(&ram, &mapped)
            .iter()
            .map(|r| (r.base, r.end()))
            .collect();
        let lowered = super::lowered_tom2(tom2, &windows, &dram)?;
        write_tom2(mp, lowered);
        fstart_log::info!(
            "{}: TOM2 {:#x} lowered to {:#x} for CXL",
            V::NAME,
            tom2,
            lowered
        );
        Some((tom2, lowered))
    }

    /// Step 3, before the payload: the `Online` windows nothing decodes,
    /// which leave the RAM of the memory map ([`super::super::cxl::not_ram`]).
    pub fn not_ram<V: Variant>(ctx: &Ctx<'_, Sp7<V>>, cxl: &Cxl) -> Vec<core::ops::Range<u64>> {
        let Some(config) = ctx.config.cxl.as_ref() else {
            return Vec::new();
        };
        let windows: Vec<(u64, u64)> =
            super::super::cxl::windows(ctx.hw.apob.as_ref(), Some(config))
                .iter()
                .map(|&(base, end, _)| (base, end))
                .collect();
        let out = super::super::cxl::not_ram(&windows, config.memory, &cxl.memory.decoded);
        for range in &out {
            fstart_log::warn!(
                "{}: CXL window {:#x}..{:#x} not decoded: soft-reserved, not RAM",
                V::NAME,
                range.start,
                range.end
            );
        }
        out
    }

    /// Step 3: TOM2 back on every CPU once a pool is enabled.
    pub fn restore_tom2<V: Variant>(cxl: &Cxl, mp: &MpHandle) {
        let Some((saved, lowered)) = cxl.memory.tom2 else {
            return;
        };
        if cxl.memory.pool_enabled {
            write_tom2(mp, saved);
        } else {
            fstart_log::warn!(
                "{}: no CXL pool enabled: TOM2 stays at {:#x}",
                V::NAME,
                lowered
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fstart_driver_amd::apob::cxl::Memory;

    const G: u64 = 1 << 30;

    fn window(base: u64) -> Window {
        Window {
            base,
            size: 16 * G,
            status: 0,
            socket: 0,
            nbio_map: 1,
            alignment: 32,
            interleave_size: 0xff,
            sub_interleave_map: [0; 4],
            sub_interleave_size: 0xff,
            memory: Memory::Volatile,
            targets: heapless::Vec::new(),
        }
    }

    #[test]
    fn tom2_comes_down_to_the_lowest_window_above_dram() {
        let windows = [window(0x500 * G), window(0x400 * G)];
        assert_eq!(
            lowered_tom2(0x410 * G, &windows, &[(4 * G, 0x100 * G)]),
            Some(0x400 * G)
        );
        // Already below: nothing.
        assert_eq!(lowered_tom2(0x100 * G, &windows, &[]), None);
        // DRAM above the window: lowering would cut it off.
        assert_eq!(
            lowered_tom2(0x600 * G, &windows, &[(4 * G, 0x480 * G)]),
            None
        );
        assert_eq!(lowered_tom2(0x600 * G, &[], &[]), None);
    }

    #[test]
    fn a_window_is_decoded_only_when_all_its_regions_committed() {
        let with = |base, targets: &[u8]| Window {
            targets: heapless::Vec::from_slice(targets).unwrap(),
            ..window(base)
        };
        let windows = [
            with(0x400 * G, &[0, 1]),
            with(0x500 * G, &[2]),
            with(0x600 * G, &[3]),
            Window {
                status: fstart_driver_amd::apob::cxl::STATUS_OUT_OF_MEMORY_MAP,
                ..with(0x700 * G, &[4])
            },
        ];
        // Region 1 of the interleave failed, region 3 was never enabled,
        // the unmapped window is no RAM anyway.
        let committed = [(0, true), (1, false), (2, true), (4, true)];
        assert_eq!(decoded(&windows, &committed), [(0x500 * G, 0x510 * G)]);
        // A region enabled twice (two pools) with one failing is not
        // decoded.
        assert!(decoded(&windows[1..2], &[(2, true), (2, false)]).is_empty());
        assert!(decoded(&windows, &[]).is_empty());
    }

    #[test]
    fn cxl_domains_are_hmat_targets_with_cdat_or_default_performance() {
        let measured = Perf {
            read_latency: 150_000,
            write_latency: 200_000,
            read_bandwidth: 30_000,
            write_bandwidth: 30_000,
        };
        let found = Found {
            perf: alloc::vec![(0x400 * G, measured)],
            ..Found::default()
        };
        let t = hmat_targets(
            &found,
            2,
            &[(0x400 * G, 0x410 * G, 0), (0x500 * G, 0x510 * G, 1)],
        );
        assert_eq!((t[0].0, t[0].1, t[0].2.write_latency), (2, 0, 200_000));
        assert_eq!((t[1].0, t[1].1, t[1].2.read_latency), (3, 1, 400_000));
        assert_eq!(t[1].2.read_bandwidth, 10_000);
    }
}
