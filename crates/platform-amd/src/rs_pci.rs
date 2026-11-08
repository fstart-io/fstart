// SPDX-License-Identifier: MIT OR Apache-2.0
//! Rs PCIe resource ownership over explicitly trained/retained port paths.
//! Active USB/chipset scopes remain intact. Retained endpoints are matched
//! through temporary config-only topology, not guessed BDFs or retraining.
use alloc::vec::Vec;
use core::ops::Range;
use fstart_driver_amd::{
    df::v4::{self, Root},
    mpio::ports,
    nbio::iohc::NbProgDeviceRemap,
    smn::Smn,
};
use fstart_pci::PciRole::{Forwarded, Window};
use fstart_pci::{
    EcamDevice, PCI_BRIDGE_BUSES, PciAddress, PciEcam, PciRoot, PciRootInfo, PciWindow,
};
use tock_registers::{LocalRegisterCopy, interfaces::Readable};

/// Allocate each root's trained/retained subtrees into its share of the
/// low `window` and, if given, of the above-4-GiB `window64`; both are
/// split evenly in `roots` order and routed to the root through DF4 MMIO.
///
/// Each allocator keeps the description of its root bridge: its share of
/// the windows and of the I/O for PCI devices, and its part of the `usb`
/// reservation, which it forwards to an already placed xHCI. The tables are
/// built from that copy.
pub fn initialize(
    roots: &[Root],
    ports: &[PciAddress],
    window: Range<u64>,
    window64: Option<Range<u64>>,
    io_budgets: &[(u64, u64)],
    usb: &Range<u64>,
    required_retained: &[(u8, u16, u16)],
) -> Result<Vec<PciEcam>, ()> {
    let size64 = match &window64 {
        Some(w) => {
            let size = w
                .end
                .checked_sub(w.start)
                .and_then(|span| span.checked_div(roots.len() as u64))
                .ok_or(())?;
            if !size.is_power_of_two()
                || size < 1 << 32
                || w.start & (size - 1) != 0
                || w.end.checked_sub(w.start) != size.checked_mul(roots.len() as u64)
            {
                return Err(());
            }
            size
        }
        None => 0,
    };
    let size = window
        .end
        .checked_sub(window.start)
        .and_then(|span| span.checked_div(roots.len() as u64))
        .ok_or(())?;
    if roots.is_empty()
        || roots.len() > fstart_pci::MAX_PCI_ROOTS
        || (ports.is_empty() && required_retained.is_empty())
        || ports.len() + required_retained.len() > 32
        || io_budgets.len() != roots.len()
        || io_budgets.iter().any(|&(base, size)| {
            base < 0x1000 || size == 0 || base.checked_add(size).is_none_or(|end| end > 0x10000)
        })
        || required_retained
            .iter()
            .any(|&(physical, _, _)| !roots.iter().any(|r| r.ios.physical_index == physical))
        || !size.is_power_of_two()
        || size < 0x10000
        || window.end.checked_sub(window.start) != size.checked_mul(roots.len() as u64)
        || window.start & (size - 1) != 0
        || window.end > 0xe000_0000
        || ports.iter().any(|p| {
            !roots
                .iter()
                .any(|r| p.segment() == 0 && p.bus() == r.route.base)
        })
    {
        return Err(());
    }
    fstart_log::info!(
        "rs: PCIe resource entry roots={} ports={}",
        roots.len(),
        ports.len()
    );
    // SAFETY: caller checked single-socket Rs identity and DF4 discovery;
    // BSP-exclusive DF access, no AP callback or SMU command uses this pair.
    let df = unsafe { v4::Df::new(fstart_pci::ecam::GlobalEcam) };
    let mut owners = Vec::new();
    for (index, root) in roots.iter().enumerate() {
        let mut selected: heapless::Vec<PciAddress, 32> = ports
            .iter()
            .copied()
            .filter(|p| p.bus() == root.route.base)
            .collect();
        let wanted = required_retained
            .iter()
            .enumerate()
            .filter(|(_, (physical, _, _))| *physical == root.ios.physical_index)
            .fold(0u32, |mask, (index, _)| mask | (1 << index));
        let mut candidates = heapless::Vec::<PciAddress, 32>::new();
        if root.route.segment != 0 || root.route.limit <= root.route.base {
            return Err(());
        }
        let start = window.start + index as u64 * size;
        let start64 = window64
            .as_ref()
            .map_or(0, |w| w.start + index as u64 * size64);
        fstart_log::info!(
            "rs: PCIe allocator construct IOS={}",
            root.ios.physical_index
        );
        let usb = super::rs_resources::usb_controller_mmio(root.ios.physical_index, usb)
            .map_or((0, 0), |usb| (usb.start, usb.end - usb.start));
        let described = PciRoot::new(
            PciRootInfo {
                segment: 0,
                ecam_base: fstart_pci::ecam::base() as u64,
                bus_start: root.route.base,
                bus_end: root.route.limit,
            },
            [
                Window.io(io_budgets[index].0, io_budgets[index].1),
                Window.mmio(start, size),
                Forwarded.mmio(usb.0, usb.1),
                Window.mmio(start64, size64),
            ],
        )
        .map_err(|_| ())?;
        let mut pci = PciEcam::new([described]).map_err(|_| ())?;
        pci.disable_auto_bus_master().map_err(|_| ())?;
        // Preserve every existing unselected bridge's routing, not merely a
        // guessed USB bus number. Reserve the complete prefix through the
        // highest retained subordinate before any temporary/new route.
        let mut retained = heapless::Vec::<(EcamDevice, u32), 32>::new();
        let mut through = root.route.base;
        // SAFETY: checked physical IOHC, BSP-exclusive SMN. The MPIO/SMU
        // sessions are complete; active xHCI DMA does not access SMN.
        let smn = unsafe { Smn::on_bus(fstart_pci::ecam::GlobalEcam, root.route.base) };
        for slot in 0..ports::NATIVE_DEVFNS.len() as u8 {
            let reg = ports::remap_register(root.ios.physical_index, slot).map_err(|_| ())?;
            let raw = smn.reg(reg).get();
            if raw == u32::MAX {
                return Err(());
            }
            let devfn = LocalRegisterCopy::<u32, NbProgDeviceRemap::Register>::new(raw)
                .read(NbProgDeviceRemap::DEVFN) as u8;
            fstart_log::info!(
                "rs: PCIe retain-check IOS={} slot={} devfn={:#x}",
                root.ios.physical_index,
                slot,
                devfn
            );
            let dev = EcamDevice::new(root.route.base, devfn >> 3, devfn & 7);
            if !dev.is_present()
                || selected.contains(&dev.address())
                || dev.read8(fstart_pci::PCI_HEADER_TYPE) & 0x7f != 1
            {
                continue;
            }
            let raw = dev.read32(fstart_pci::PCI_PRIMARY_BUS);
            let route = LocalRegisterCopy::<u32, PCI_BRIDGE_BUSES::Register>::new(raw);
            let first = route.read(PCI_BRIDGE_BUSES::SECONDARY) as u8;
            let last = route.read(PCI_BRIDGE_BUSES::SUBORDINATE) as u8;
            if first == 0 && last == 0 {
                if wanted != 0 {
                    use fstart_pci::config::PCI_EXPRESS_LINK_STATUS as Link;
                    if let Some(cap) = dev.find_capability(0x10).filter(|c| c + 0x14 <= 0x100) {
                        let link =
                            LocalRegisterCopy::<u16, Link::Register>::new(dev.read16(cap + 0x12));
                        if link.read(Link::WIDTH) != 0
                            && !link.is_set(Link::TRAINING)
                            && link.is_set(Link::DLL_ACTIVE)
                        {
                            if dev.read16(fstart_pci::PCI_COMMAND) & fstart_pci::PCI_CMD_BUS_MASTER
                                != 0
                            {
                                return Err(());
                            }
                            candidates.push(dev.address()).map_err(|_| ())?;
                        }
                    }
                }
                continue;
            }
            if route.read(PCI_BRIDGE_BUSES::PRIMARY) != u32::from(root.route.base)
                || first <= root.route.base
                || last < first
                || last > root.route.limit
            {
                return Err(());
            }
            through = through.max(last);
            retained.push((dev, raw)).map_err(|_| ())?;
        }
        fstart_log::info!(
            "rs: PCIe retained routes validated IOS={} through={:#x}",
            root.ios.physical_index,
            through
        );
        pci.reserve_bus_prefix(0, root.route.base, through)
            .map_err(|_| ())?;
        let mut found = 0u32;
        for candidate in candidates {
            // SAFETY: checked programmed IOHC bridge, stable active link and
            // root bus-master disabled. Routes start after all live prefixes;
            // callback only reads identity/command, never sizes BARs or MMIO.
            let (matched, idle) = unsafe {
                pci.with_config_topology(&[candidate], |view| {
                    let mut matched = 0u32;
                    let mut idle = true;
                    for address in view.devices() {
                        if let Some(device) = view.device(address) {
                            idle &= device.read16(fstart_pci::PCI_COMMAND)
                                & fstart_pci::PCI_CMD_BUS_MASTER
                                == 0;
                            for (i, &(physical, vendor, id)) in required_retained.iter().enumerate()
                            {
                                if physical == root.ios.physical_index
                                    && device.vendor_id() == vendor
                                    && device.device_id() == id
                                {
                                    matched |= 1 << i;
                                }
                            }
                        } else {
                            idle = false;
                        }
                    }
                    (matched, idle)
                })
            }
            .map_err(|_| ())?;
            if matched != 0 {
                if !idle {
                    return Err(());
                }
                fstart_log::info!(
                    "rs: retained PCIe endpoint match IOS={} device={} function={} mask={:#x}",
                    root.ios.physical_index,
                    candidate.device(),
                    candidate.function(),
                    matched
                );
                found |= matched;
                selected.push(candidate).map_err(|_| ())?;
            }
        }
        if found != wanted {
            fstart_log::error!(
                "rs: missing retained endpoints IOS={} wanted={:#x} found={:#x}",
                root.ios.physical_index,
                wanted,
                found
            );
            return Err(());
        }
        // SAFETY: only the selected, not-yet-owned trained port subtrees;
        // temporary routes start AFTER all retained/active USB bus prefixes.
        // The callback reads command registers, never BARs/MMIO or storage.
        let idle = selected.is_empty()
            || unsafe {
                pci.with_config_topology(&selected, |view| {
                    view.devices().all(|p| {
                        view.device(p).is_some_and(|d| {
                            d.read16(fstart_pci::PCI_COMMAND) & fstart_pci::PCI_CMD_BUS_MASTER == 0
                        })
                    })
                })
            }
            .map_err(|_| ())?;
        if !idle {
            return Err(());
        }
        for (base, length) in [(start, size), (start64, size64)] {
            if length == 0 {
                continue;
            }
            let table = v4::mmio::snapshot(&df).map_err(|_| ())?;
            let claim = v4::mmio::Claim::prepare(&table, base, length, root.ios.fabric_id)
                .map_err(|_| ())?;
            let pair = claim.pair();
            // SAFETY: disjoint PSP-declared low-MMIO or above-DRAM pool,
            // verified destination, unused rule only. No existing
            // USB/PSP/BMC aperture is displaced.
            unsafe { claim.install(&df) }.map_err(|_| ())?;
            fstart_log::info!(
                "rs: PCIe IOS={} DF pair={} MMIO={:#x} size={:#x} preserved-through={:#x}",
                root.ios.physical_index,
                pair,
                base,
                length,
                through
            );
        }
        if !selected.is_empty() {
            pci.enumerate_root_ports_and_allocate(&selected)
                .map_err(|error| {
                    fstart_log::error!("rs: PCIe allocation failed: {:?}; no retry", error);
                })?;
        }
        for &(dev, buses) in &retained {
            if dev.read32(fstart_pci::PCI_PRIMARY_BUS) != buses {
                return Err(());
            }
        }
        for address in pci.devices() {
            let dev = pci.device(address).ok_or(())?;
            if dev.read16(fstart_pci::PCI_COMMAND) & fstart_pci::PCI_CMD_BUS_MASTER != 0 {
                return Err(());
            }
            fstart_log::info!(
                "rs: PCIe assigned IOS={} bus={:#x} device={} function={} ID={:#x}:{:#x} class={:#x} DMA=off",
                root.ios.physical_index,
                address.bus(),
                address.device(),
                address.function(),
                dev.vendor_id(),
                dev.device_id(),
                dev.read32(fstart_pci::PCI_CLASS_REVISION) >> 8
            );
        }
        for (address, bar) in pci.assigned_bars() {
            let inside = |(base, length): (u64, u64)| {
                bar.base >= base
                    && bar
                        .base
                        .checked_add(bar.size)
                        .is_some_and(|end| end <= base + length)
            };
            let owned = match bar.kind {
                fstart_pci::PciWindowKind::Mmio => {
                    inside((start, size)) || (size64 != 0 && inside((start64, size64)))
                }
                fstart_pci::PciWindowKind::Io => inside(io_budgets[index]),
                _ => false,
            };
            if !owned {
                return Err(());
            }
            fstart_log::info!(
                "rs: PCIe BAR bus={:#x} device={} function={} base={:#x} size={:#x} io={}",
                address.bus(),
                address.device(),
                address.function(),
                bar.base,
                bar.size,
                bar.kind == fstart_pci::PciWindowKind::Io
            );
        }
        owners.push(pci);
    }
    Ok(owners)
}

/// Route the legacy VGA aperture to the root of the one display with PCI
/// `identity`, and returns that root's physical IOS.
/// # Safety
/// Exclusive checked Genoa DF/config, allocated display resources with DMA
/// off, and legacy A0000h..BFFFFh excluded from firmware/OS usable memory.
/// Routing only: no option ROM execution, GPU register or framebuffer writes.
pub unsafe fn route_vga(
    roots: &[fstart_driver_amd::df::v4::Root],
    owners: &mut [fstart_pci::PciEcam],
    identity: (u16, u16),
) -> Result<u8, ()> {
    use fstart_driver_amd::df::v4::{self, io};
    use fstart_pci::{PCI_BRIDGE_CONTROL_BITS as Control, PciType1Config};
    use tock_registers::interfaces::{Readable, Writeable};
    if roots.len() != owners.len() {
        return Err(());
    }
    let mut found = None;
    for (index, (root, owner)) in roots.iter().zip(owners.iter()).enumerate() {
        for address in owner.devices() {
            let device = owner.device(address).ok_or(())?;
            if (device.vendor_id(), device.device_id()) != identity {
                continue;
            }
            if found.replace((index, address)).is_some() {
                return Err(());
            }
            if device.read32(fstart_pci::PCI_CLASS_REVISION) >> 16 != 0x0300
                || address.bus() <= root.route.base
                || address.bus() > root.route.limit
            {
                return Err(());
            }
        }
    }
    let (index, address) = found.ok_or(())?;
    let (root, owner) = (&roots[index], &mut owners[index]);
    let path = owner.device_path(address).ok_or(())?;
    if path.len() < 2 {
        return Err(());
    }
    for &bridge in &path[..path.len() - 1] {
        let device = owner.device(bridge).ok_or(())?;
        if device.read8(fstart_pci::PCI_HEADER_TYPE) & 0x7f != 1
            || device.read16(fstart_pci::PCI_COMMAND) & fstart_pci::PCI_CMD_BUS_MASTER != 0
        {
            return Err(());
        }
    }
    // SAFETY: checked/exclusive DF4 and actual display's IOHC destination.
    let df = unsafe { v4::Df::new(fstart_pci::ecam::GlobalEcam) };
    unsafe { io::route_vga(&df, root.ios.fabric_id) }.map_err(|_| ())?;
    for &bridge in &path[..path.len() - 1] {
        let device = owner.device(bridge).ok_or(())?;
        // SAFETY: preflighted Type1 header, exclusively owned config. Existing
        // overlay ensures 16-bit control writes don't touch adjacent status.
        let header = unsafe { device.regs::<PciType1Config>() };
        let before = header.bridge_control.extract();
        let mut write = before;
        write.modify(Control::VGA_ENABLE::SET + Control::DISCARD_TIMER_STATUS::CLEAR);
        header.bridge_control.set(write.get());
        if header.bridge_control.get() != before.get() | Control::VGA_ENABLE::SET.value {
            return Err(());
        }
    }
    // The root forwards the legacy VGA memory from here on: its `_CRS` says so.
    owner
        .forward(0, root.route.base, PciWindow::mmio(0xa_0000, 0x2_0000))
        .map_err(|_| ())?;
    fstart_log::info!(
        "rs: legacy VGA routed IOS={} bus={:#x} device={} function={} bridges={}; config only",
        root.ios.physical_index,
        address.bus(),
        address.device(),
        address.function(),
        path.len() - 1
    );
    Ok(root.ios.physical_index)
}

/// Complete transmit identity independently of the IOHC config devfn map.
/// Includes the PSP-trained BMC port, which must never enter normal MPIO ASK.
/// # Safety
/// Checked one-socket Rs; BSP-exclusive config/SMN, IF=0 on all CPUs,
/// selected ports allocated with DMA off, USB halted. No retry after a write.
pub unsafe fn initialize_requesters(
    roots: &[fstart_driver_amd::df::v4::Root],
    owners: &[fstart_pci::PciEcam],
) -> Result<(), ()> {
    use fstart_driver_amd::{
        mpio::ports,
        nbio::{
            iohc::NbProgDeviceRemap,
            pcie::rs::{self as nbio_pcie, Port},
        },
        smn::Smn,
    };
    use fstart_pci::config::PCI_EXPRESS_LINK_STATUS as Link;
    use tock_registers::{LocalRegisterCopy, interfaces::Readable};
    if roots.is_empty() || roots.len() != owners.len() {
        return Err(());
    }
    let smn = unsafe { Smn::on_bus(fstart_pci::ecam::GlobalEcam, roots[0].route.base) };
    let mut targets = heapless::Vec::<Port, 32>::new();
    for (root, owner) in roots.iter().zip(owners) {
        if owner.bus_start() != root.route.base || owner.bus_end() != root.route.limit {
            return Err(());
        }
        // Reuse the allocator's owned root-port inventory, not untrained
        // function/link reset values. The retained BMC path is included here.
        for address in owner.devices().filter(|a| a.bus() == root.route.base) {
            let device = owner.device(address).ok_or(())?;
            let devfn = fstart_pci::devfn(address.device(), address.function());
            let mut slot = None;
            for candidate in 0..22 {
                if root.ios.physical_index & 1 != 0 && candidate >= 18 {
                    continue;
                }
                let remap = smn.reg(
                    ports::remap_register(root.ios.physical_index, candidate).map_err(|_| ())?,
                );
                if remap.get() == u32::MAX {
                    return Err(());
                }
                if remap.read(NbProgDeviceRemap::DEVFN) == u32::from(devfn)
                    && slot.replace(candidate).is_some()
                {
                    return Err(());
                }
            }
            let slot = slot.ok_or(())?;
            if device.vendor_id() != 0x1022
                || !matches!(device.device_id(), 0x14a5 | 0x14aa | 0x14ab)
                || device.read8(fstart_pci::PCI_HEADER_TYPE) & 0x7f != 1
            {
                return Err(());
            }
            let cap = device
                .find_capability(0x10)
                .filter(|c| c + 0x14 <= 0x100)
                .ok_or(())?;
            let link = LocalRegisterCopy::<u16, Link::Register>::new(device.read16(cap + 0x12));
            if link.read(Link::WIDTH) == 0
                || link.is_set(Link::TRAINING)
                || !link.is_set(Link::DLL_ACTIVE)
                || device.read16(fstart_pci::PCI_COMMAND) & fstart_pci::PCI_CMD_BUS_MASTER != 0
            {
                return Err(());
            }
            let port = Port::from_slot(root.ios.physical_index, slot, root.route.base, devfn)
                .map_err(|_| ())?;
            let register = port.register().map_err(|_| ())?;
            fstart_log::info!(
                "rs: PCIe TX requester preflight IOS={} slot={} wrapper={} local={} BDF={:#x} SMN={:#x} old={:#x}",
                port.root,
                slot,
                port.wrapper,
                port.local_port,
                (u32::from(port.bus) << 8) | u32::from(port.devfn),
                register.address(),
                smn.reg(register).get()
            );
            targets.push(port).map_err(|_| ())?;
        }
    }
    if targets.is_empty() {
        return Err(());
    }
    unsafe { nbio_pcie::initialize_requesters(&smn, &targets) }.map_err(|error| {
        fstart_log::error!(
            "rs: PCIe TX requester failure {:?}; no retry/handoff",
            error
        );
    })?;
    for port in targets {
        let value = smn.reg(port.register().map_err(|_| ())?).get();
        fstart_log::info!(
            "rs: PCIe TX requester verified IOS={} bus={:#x} devfn={:#x} value={:#x}; DMA off",
            port.root,
            port.bus,
            port.devfn,
            value
        );
    }
    Ok(())
}

/// For a firmware boot loader whose drivers enable bus mastering only on
/// their own endpoints (CrabEFI): every bridge the root allocators set up
/// masters, so those endpoints' DMA reaches memory. Endpoints stay off, as
/// [`initialize`] left them; an OS handoff skips this (Linux enables bridges
/// itself).
pub fn enable_bridge_mastering(pci: &[PciEcam]) -> Result<(), ()> {
    use fstart_pci::{PCI_COMMAND_BITS, PciType1Config};
    use tock_registers::interfaces::ReadWriteable;
    let mut bridges = 0;
    for allocator in pci {
        for address in allocator.devices() {
            let device = allocator.device(address).ok_or(())?;
            if device.read8(fstart_pci::PCI_HEADER_TYPE) & 0x7f != 1 {
                continue;
            }
            // SAFETY: an allocated Type1 bridge with a stable ECAM mapping.
            let command = &unsafe { device.regs::<PciType1Config>() }.command;
            command.modify(PCI_COMMAND_BITS::BUS_MASTER::SET);
            if !command.is_set(PCI_COMMAND_BITS::BUS_MASTER) {
                return Err(());
            }
            bridges += 1;
        }
    }
    fstart_log::info!(
        "rs: bus mastering on {} PCIe bridges for the loader",
        bridges
    );
    Ok(())
}
