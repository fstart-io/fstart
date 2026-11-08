// SPDX-License-Identifier: MIT OR Apache-2.0
//! Rs USB: the SMU's cold setup of the FCH xHCIs, then each controller
//! assigned and decoded behind its NBIF bridge, never started here. The
//! payload's xHCI driver (Linux's, CrabEFI's) takes it from there.
use core::num::NonZeroU32;
use fstart_driver_amd::{
    ccx::tsc::Tsc,
    df::v4::{PRIMARY_IOS, Root},
    fch::songshan::usb,
    smn::Smn,
    smu::{Smu, v13},
};
use fstart_pci::ecam::GlobalEcam;
use tock_registers::interfaces::Readable;

/// Open the NBIF USB bridge's whole routed reservation `window` for what
/// follows: the USB-only allocator sized it to the 1-MiB xHCI BAR, retained
/// NBIF siblings need the rest (their BARs are never moved). Every NBIF
/// function behind it must have mastering off; the bridge masters, the
/// function's mastering stays off until its driver enables it.
fn release_bridge(
    bridge: &fstart_pci::EcamDevice,
    bus: u8,
    window: &core::ops::Range<u64>,
) -> Result<(), ()> {
    use fstart_pci::PCI_COMMAND_BITS;
    use fstart_pci::config::PCI_BRIDGE_MEMORY_BOUND as Bound;
    use tock_registers::interfaces::ReadWriteable;
    for function in 0..8 {
        let device = fstart_pci::EcamDevice::new(bus, 0, function);
        if device.is_present()
            && device.read16(fstart_pci::PCI_COMMAND) & fstart_pci::PCI_CMD_BUS_MASTER != 0
        {
            return Err(());
        }
    }
    // SAFETY: the caller's stable bridge, verified as AMD 14a7/Type1.
    let route = unsafe { bridge.regs::<fstart_pci::PciType1Config>() };
    route.command.modify(PCI_COMMAND_BITS::BUS_MASTER::SET);
    let base = u64::from(route.memory_base.read(Bound::ADDRESS)) << 20;
    let limit = (u64::from(route.memory_limit.read(Bound::ADDRESS)) + 1) << 20;
    if base != window.start
        || window.end & 0xfffff != 0
        || limit > window.end
        || limit <= base
        || !route.command.is_set(PCI_COMMAND_BITS::BUS_MASTER)
    {
        return Err(());
    }
    route
        .memory_limit
        .modify(Bound::ADDRESS.val(((window.end - 1) >> 20) as u16));
    if (u64::from(route.memory_limit.read(Bound::ADDRESS)) + 1) << 20 != window.end {
        return Err(());
    }
    fstart_log::info!(
        "rs: USB/NBIF bridge memory={:#x}..{:#x}",
        window.start,
        window.end
    );
    Ok(())
}

pub fn initialize(
    roots: &[Root],
    window: core::ops::Range<u64>,
    policy: &usb::Policy,
    interface: usb::Interface,
    smu_limit: u32,
) -> Result<(), ()> {
    let mut boot = usb::ColdBoot::for_interface(policy, interface).map_err(|_| ())?;
    let primary = roots
        .iter()
        .find(|r| r.ios.physical_index == PRIMARY_IOS && r.route.segment == 0)
        .ok_or(())?;
    let mhz = u32::try_from(fstart_arch::x86::tsc_frequency_hz() / 1_000_000)
        .ok()
        .and_then(NonZeroU32::new)
        .filter(|n| (100..=5000).contains(&n.get()))
        .ok_or(())?;
    // SAFETY: validated primary host bridge; BSP-exclusive, no outstanding
    // SMU messages. MPIO has completed and uses a different mailbox.
    let smn = unsafe { Smn::on_bus(GlobalEcam, primary.route.base) };
    let smu = Smu::new(smn, v13::MAILBOX, Tsc::new(mhz), smu_limit).with_timeout_us(2_000_000);
    fstart_log::info!(
        "rs: USB SMU cold setup guard={:#x} primary-bus={:#x}",
        smu.smn().reg(usb::GUARD).get(),
        primary.route.base
    );
    boot.run(smu.smn(), &smu).map_err(|error| {
        let reason = match error {
            usb::Error::Policy => "policy",
            usb::Error::Unavailable => "unavailable",
            usb::Error::Disabled => "access guard",
            usb::Error::Consumed => "consumed attempt",
            usb::Error::Smu(_) => "SMU request",
        };
        fstart_log::error!(
            "rs: USB cold setup failed: {}; no retry before reset",
            reason
        );
    })?;
    for (i, reg) in usb::VISIBILITY.iter().enumerate() {
        fstart_log::info!(
            "rs: USB controller={} visibility={:#x}",
            i,
            smu.smn().reg(*reg).get()
        );
    }
    fstart_log::info!("rs: USB SMU initialization completed");
    // Read the real DF4 routes before assigning a second IOHC's MMIO. Do
    // not assume the primary controller's fallback aperture reaches IOS0.
    use fstart_driver_amd::df::v4;
    // SAFETY: BSP-exclusive node0 DF4 access; single socket verified earlier.
    let df = unsafe { v4::Df::new(GlobalEcam) };
    let mut table = v4::mmio::snapshot(&df).map_err(|_| ())?;
    let size = super::rs_resources::USB_CONTROLLER_MMIO_SIZE;
    if window.end.checked_sub(window.start) != Some(2 * size) {
        return Err(());
    }
    let primary_window = window.start..window.start + size;
    let mut covered = false;
    for raw in &table {
        if let Some(route) =
            v4::mmio::Route::decode(raw.base, raw.limit, raw.control, raw.extension)
                .map_err(|_| ())?
            && primary_window.start < route.end
            && route.base < primary_window.end
        {
            if route.destination != primary.ios.fabric_id
                || route.cpu_disabled
                || route.nonposted
                || route.base > primary_window.start
                || route.end < primary_window.end
            {
                return Err(());
            }
            covered = true;
        }
    }
    if policy.enabled[0] && !covered {
        let claim =
            v4::mmio::Claim::prepare(&table, primary_window.start, size, primary.ios.fabric_id)
                .map_err(|_| ())?;
        fstart_log::info!(
            "rs: USB0 DF4 MMIO claim pair={} base={:#x} size={:#x} destination={:#x}",
            claim.pair(),
            primary_window.start,
            size,
            primary.ios.fabric_id
        );
        // SAFETY: complete PSP-MMIO window, checked live destination, unused
        // rule only, no existing range or controller resource is displaced.
        unsafe { claim.install(&df) }.map_err(|_| ())?;
        table = v4::mmio::snapshot(&df).map_err(|_| ())?;
    }
    if policy.enabled[0] {
        inspect_controller(primary, smu.smn(), primary_window, true)?;
    }
    if policy.enabled[1] {
        // NFX exposes controller1 via10134800 vs controller0's10334800:
        // same NBIF strap/function, physical IOS0 vs IOS2, not another
        // guessed function under the primary NBIF bridge.
        let secondary = roots.iter().find(|r| r.ios.physical_index == 0).ok_or(())?;
        let secondary_window = window.start + size..window.end;
        inspect_controller(secondary, smu.smn(), secondary_window.clone(), false)?;
        let claim = v4::mmio::Claim::prepare(
            &table,
            secondary_window.start,
            size,
            secondary.ios.fabric_id,
        )
        .map_err(|_| ())?;
        fstart_log::info!(
            "rs: USB1 DF4 MMIO claim pair={} base={:#x} size={:#x} destination={:#x}",
            claim.pair(),
            secondary_window.start,
            size,
            secondary.ios.fabric_id
        );
        // SAFETY: disjoint half of the validated PSP MMIO reservation, no
        // assigned BAR/mastering on this controller, actual IOS0 destination.
        // Claim refuses overlap/stale state and verifies the entire DF table;
        // no existing aperture (ECAM/FCH/GPU/etc.) is relocated or split.
        unsafe { claim.install(&df) }.map_err(|_| ())?;
        inspect_controller(secondary, smu.smn(), secondary_window, true)?;
    }
    Ok(())
}

/// Validate the xHCI behind `root`'s NBIF bridge; with `assign`, give it
/// its BAR in `window` and open the bridge.
fn inspect_controller(
    root: &Root,
    smn: &impl fstart_driver_amd::smn::SmnAccess,
    window: core::ops::Range<u64>,
    assign: bool,
) -> Result<(), ()> {
    use fstart_driver_amd::{mpio::ports, nbio::iohc::NbProgDeviceRemap};
    use fstart_pci::{
        EcamDevice, PCI_COMMAND_BITS, PciAddress, PciEcam, PciRole::Window, PciRoot, PciRootInfo,
    };
    use tock_registers::interfaces::ReadWriteable;
    // Slot22 is outside the MPIO wrappers. Bind to the actual programmed
    // native map and validate the NBIF bridge, not a guessed final BDF.
    let remap = ports::remap_register(root.ios.physical_index, 22).map_err(|_| ())?;
    let devfn = smn.reg(remap).read(NbProgDeviceRemap::DEVFN) as u8;
    let bridge = EcamDevice::new(root.route.base, devfn >> 3, devfn & 7);
    fstart_log::info!(
        "rs: USB NBIF IOS={} devfn={:#x} ID={:#x}:{:#x}",
        root.ios.physical_index,
        devfn,
        bridge.vendor_id(),
        bridge.device_id()
    );
    if bridge.vendor_id() != 0x1022
        || bridge.device_id() != 0x14a7
        || bridge.read8(fstart_pci::PCI_HEADER_TYPE) & 0x7f != 1
    {
        return Err(());
    }
    let child = PciAddress::new(0, root.route.base.checked_add(1).ok_or(())?, 0, 4);
    // A temporary allocator must never alias a retained PSP/BMC bus route.
    // Until final resource assignment, fail rather than stealing its prefix.
    for &number in &ports::NATIVE_DEVFNS {
        if number == devfn {
            continue;
        }
        let other = EcamDevice::new(root.route.base, number >> 3, number & 7);
        if other.is_present() && other.read8(fstart_pci::PCI_HEADER_TYPE) & 0x7f == 1 {
            let buses = tock_registers::LocalRegisterCopy::<
                u32,
                fstart_pci::config::PCI_BRIDGE_BUSES::Register,
            >::new(other.read32(fstart_pci::PCI_PRIMARY_BUS));
            let first = buses.read(fstart_pci::config::PCI_BRIDGE_BUSES::SECONDARY) as u8;
            let last = buses.read(fstart_pci::config::PCI_BRIDGE_BUSES::SUBORDINATE) as u8;
            if first != 0 && first <= child.bus() && child.bus() <= last {
                fstart_log::error!(
                    "rs: USB temporary bus conflicts with retained root devfn={:#x} buses={:#x}..{:#x}",
                    number,
                    first,
                    last
                );
                return Err(());
            }
        }
    }
    // A short-lived allocator for the controller alone: its root with the
    // controller's window and nothing else.
    let described = PciRoot::new(
        PciRootInfo {
            segment: 0,
            ecam_base: fstart_pci::ecam::base() as u64,
            bus_start: root.route.base,
            bus_end: root.route.limit,
        },
        [Window.mmio(window.start, window.end - window.start)],
    )
    .map_err(|_| ())?;
    let mut pci = PciEcam::new([described]).map_err(|_| ())?;
    // SAFETY: only validated root bridge plus the SMU-enabled NBIF USB
    // function4 are allowed. No reserved siblings, BAR writes or mastering.
    let present = unsafe {
        pci.with_config_functions(&[bridge.address(), child], |view| {
            let usb = view.device(child).unwrap();
            let class = usb.read32(fstart_pci::PCI_CLASS_REVISION) >> 8;
            fstart_log::info!(
                "rs: USB xHCI config ID={:#x}:{:#x} class={:#x} command={:#x} BAR0={:#x}",
                usb.vendor_id(),
                usb.device_id(),
                class,
                usb.read16(fstart_pci::PCI_COMMAND),
                usb.read32(fstart_pci::PCI_BAR0)
            );
            usb.vendor_id() == 0x1022
                && usb.device_id() == 0x14c9
                && class == 0x0c0330
                && usb.read8(fstart_pci::PCI_HEADER_TYPE) & 0x7f == 0
                && usb.memory_bar(fstart_pci::PCI_BAR0).is_none()
                && usb.read16(fstart_pci::PCI_COMMAND) & fstart_pci::PCI_CMD_BUS_MASTER == 0
        })
    }
    .map_err(|_| ())?;
    if !present {
        return Err(());
    }
    fstart_log::info!("rs: USB controller config validated; original NBIF routing restored");
    if !assign {
        return Ok(());
    }
    // Only this controller and its bridge are allocated. All other chipset
    // functions and existing MPIO/GPU resources remain untouched. The window
    // came from a validated PSP MMIO hole, excluding ECAM and fixed FCH MMIO.
    pci.enumerate_functions_and_allocate(&[bridge.address(), child])
        .map_err(|_| ())?;
    let usb = pci.device(child).ok_or(())?;
    // SAFETY: validated Type0 xHCI function with a stable ECAM mapping.
    let registers = unsafe { usb.regs::<fstart_pci::PciType0Config>() };
    registers
        .command
        .modify(PCI_COMMAND_BITS::BUS_MASTER::CLEAR + PCI_COMMAND_BITS::INT_DISABLE::SET);
    let bar = usb.memory_bar(fstart_pci::PCI_BAR0).ok_or(())?;
    let allocation = pci
        .assigned_memory_bars()
        .find(|(a, w)| *a == child && w.base == bar)
        .map(|(_, w)| w)
        .ok_or(())?;
    if allocation.base < window.start
        || allocation
            .base
            .checked_add(allocation.size)
            .is_none_or(|end| end > window.end)
        || allocation.size < 0x1000
    {
        return Err(());
    }
    fstart_log::info!(
        "rs: USB reserved MMIO BAR={:#x} size={:#x}; bus mastering disabled",
        bar,
        allocation.size
    );
    // SAFETY: owned controller BAR, memory decoding enabled, identity mapped
    // UC MMIO; no host-controller DMA or storage command has been started.
    let cap = unsafe { fstart_core::mmio::read32(bar as *const u32) };
    let hcs = unsafe { fstart_core::mmio::read32((bar + 4) as *const u32) };
    fstart_log::info!("rs: USB MMIO capability={:#x} HCSPARAMS1={:#x}", cap, hcs);
    if cap == 0 || cap == u32::MAX || hcs == u32::MAX {
        return Err(());
    }
    release_bridge(&bridge, child.bus(), &window)?;
    fstart_log::info!("rs: xHCI assigned and decoded, not started; the payload's driver owns it");
    Ok(())
}
