// SPDX-License-Identifier: MIT OR Apache-2.0
//! Retained-MPIO alias preflight and single-controller native training.
//! DMA backing stays reserved through reset, including every failure.
//! Shared Rs mechanism; fuse selection, connectors and physical PERST belong
//! to the SoC/board. Genoa's public MpioInitLib.h agrees on the 52-byte ASK,
//! six arguments, message IDs and setup phases; live alias checks are mandatory.
extern crate alloc;
use core::num::NonZeroU32;
use fstart_driver_amd::{
    ccx::tsc::Tsc,
    df::v4::Root,
    mpio::mailbox::{self, Control, Error, Mailbox, Poll},
    smn::Smn,
};
use fstart_pci::{EcamDevice, ecam::GlobalEcam};
use tock_registers::interfaces::Readable;

pub fn probe(roots: &[Root]) -> Result<(), Error> {
    let mhz = u32::try_from(fstart_arch::x86::tsc_frequency_hz() / 1_000_000)
        .ok()
        .and_then(NonZeroU32::new)
        .filter(|n| (100..=5000).contains(&n.get()))
        .unwrap_or(NonZeroU32::new(1000).unwrap());
    let mut delay = Tsc::new(mhz);
    for root in roots {
        // This platform currently publishes only the segment0 ECAM window.
        // Never alias another segment through a segment-zero device handle.
        if root.route.segment != 0 {
            return Err(Error::Unavailable);
        }
        let device = EcamDevice::new(root.route.base, 0, 0);
        let vendor = device.vendor_id();
        fstart_log::info!(
            "rs: MPIO IOS={} bus={:#x} host-ID={:#x}:{:#x}",
            root.ios.physical_index,
            root.route.base,
            vendor,
            device.device_id()
        );
        if vendor != 0x1022 {
            return Err(Error::Unavailable);
        }
        // SAFETY: validated live host bridge, BSP-exclusive index/data;
        // AP callbacks have completed and never use this SMN pair.
        let smn = unsafe { Smn::on_bus(GlobalEcam, root.route.base) };
        let before = smn.reg(mailbox::CONTROL).extract();
        fstart_log::info!(
            "rs: MPIO IOS={} control={:#x} clock-MHz={}",
            root.ios.physical_index,
            before.get(),
            mhz.get()
        );
        if before.get() == u32::MAX {
            return Err(Error::Unavailable);
        }
        if !before.is_set(Control::READY) {
            return Err(Error::ReadyTimeout);
        }
        // SAFETY: READY establishes the preceding PSP operation completed.
        // Only this root access path is used; no pointers are submitted and
        // failure returns immediately, never bypassing a poisoned session.
        let mut mailbox = unsafe {
            Mailbox::new(Poll {
                reads: NonZeroU32::new(10_000).unwrap(),
                interval_us: NonZeroU32::new(100).unwrap(),
            })
        };
        let reply = match mailbox.status(&smn, &mut delay) {
            Ok(reply) => reply,
            Err(error) => {
                match error {
                    Error::Rejected(code) => fstart_log::error!(
                        "rs: MPIO IOS={} STATUS rejected response={:#x}",
                        root.ios.physical_index,
                        code
                    ),
                    _ => fstart_log::error!(
                        "rs: MPIO IOS={} STATUS transport failed",
                        root.ios.physical_index
                    ),
                }
                return Err(error);
            }
        };
        fstart_log::info!(
            "rs: MPIO IOS={} STATUS=[{:#x},{:#x},{:#x},{:#x},{:#x},{:#x}]",
            root.ios.physical_index,
            reply[0],
            reply[1],
            reply[2],
            reply[3],
            reply[4],
            reply[5]
        );
        if reply[0] != 0 {
            mailbox.wait_idle(&smn, &mut delay, NonZeroU32::new(8).unwrap())?;
        }
        fstart_log::info!(
            "rs: MPIO IOS={} retained firmware idle; no training submitted",
            root.ios.physical_index
        );
    }
    // Root paths are not proof of independent MPIO controllers. Establish
    // aliases before introducing DMA or per-instance configuration sessions.
    let mut views = heapless::Vec::<Smn<GlobalEcam>, 4>::new();
    let reference = roots
        .iter()
        .position(|r| r.ios.physical_index == fstart_driver_amd::df::v4::PRIMARY_IOS)
        .ok_or(Error::InvalidView)?;
    for root in roots {
        // SAFETY: all paths above are validated and idle, BSP-exclusive.
        views
            .push(unsafe { Smn::on_bus(GlobalEcam, root.route.base) })
            .map_err(|_| Error::InvalidView)?;
    }
    // SAFETY: every path completed STATUS/idle above; no DMA/configuration
    // exists and no other CPU accesses these mailboxes. Abort on any error.
    let aliases = unsafe { mailbox::alias_mask(&views, reference) }?;
    fstart_log::info!(
        "rs: MPIO primary-IOS={} alias-mask={:#x}; argument restored; no doorbell/DMA/configuration",
        roots[reference].ios.physical_index,
        aliases
    );
    for (i, root) in roots.iter().enumerate() {
        fstart_log::info!(
            "rs: MPIO IOS={} shares-primary-mailbox={}",
            root.ios.physical_index,
            aliases & (1 << i) != 0
        );
    }
    if aliases != (1u8 << roots.len()) - 1 {
        return Err(Error::InvalidView);
    }
    Ok(())
}

fn checked<T>(
    result: Result<T, fstart_driver_amd::mpio::session::Error>,
    step: u8,
) -> Result<T, ()> {
    use fstart_driver_amd::mpio::session::Error as E;
    result.map_err(|error| match error {
        E::Receipt(command, value) => fstart_log::error!(
            "rs: MPIO step={} receipt cmd={:#x} value={:#x}",
            step,
            command,
            value
        ),
        E::Mailbox(Error::Rejected(code)) => {
            fstart_log::error!("rs: MPIO step={} rejected={:#x}", step, code)
        }
        E::Mailbox(_) => fstart_log::error!(
            "rs: MPIO step={} transport/idle failure; DMA retained",
            step
        ),
        E::Guard => fstart_log::error!(
            "rs: MPIO step={} DMA guard changed; allocation retained",
            step
        ),
        E::Lanes(_) | E::Overlap => {
            fstart_log::error!("rs: MPIO step={} invalid/overlapping lane policy", step)
        }
        _ => fstart_log::error!("rs: MPIO step={} buffer/policy/state failure", step),
    })
}

fn initialize_iohc_dram(roots: &[Root], smn: &Smn<GlobalEcam>) -> Result<(), ()> {
    use fstart_arch::x86::cpu::{
        amd::msr::{SYSCFG, Syscfg, TOP_MEM, TOP_MEM2},
        msr::{MsrAccess, NativeMsr},
    };
    use fstart_driver_amd::nbio::iohc::{self, DramTops, TopStyle};
    use fstart_pci::{ConfigRegister, PciAddress};
    use tock_registers::interfaces::ReadWriteable;
    let msr = unsafe { NativeMsr::new() };
    let tom = msr.reg(TOP_MEM).get();
    let tom2 = msr
        .reg(SYSCFG)
        .is_set(Syscfg::TOM2_EN)
        .then(|| msr.reg(TOP_MEM2).get());
    fstart_log::info!("rs: IOHC CPU TOM={:#x} TOM2={:#x}", tom, tom2.unwrap_or(0));
    // The shared layout is verified for Rs below1TiB; do not silently use
    // SSP/GN's different above-1TiB truncation policy on Siena.
    if tom == 0 || tom >= 1 << 32 || tom & 0x7fffff != 0 || tom2.is_some_and(|t| t > 1 << 40) {
        return Err(());
    }
    for root in roots {
        let bank = u32::from(root.ios.physical_index) * 0x100000;
        let slot = unsafe {
            ConfigRegister::<_, iohc::TopOfDram::Register>::new(
                GlobalEcam,
                PciAddress::new(0, root.route.base, 0, 0),
                iohc::NB_TOP_OF_DRAM_SLOT1,
            )
        };
        let lower = smn.reg(iohc::NB_LOWER_TOP_OF_DRAM2.offset(bank)).get();
        let upper = smn.reg(iohc::NB_UPPER_TOP_OF_DRAM2.offset(bank)).get();
        let control = smn.reg(iohc::NB_PCI_CTRL.offset(bank)).get();
        fstart_log::info!(
            "rs: IOHC IOS={} before TOM={:#x} lower={:#x} upper={:#x} ctrl={:#x}",
            root.ios.physical_index,
            slot.get(),
            lower,
            upper,
            control
        );
        if [slot.get(), lower, upper, control].contains(&u32::MAX) {
            return Err(());
        }
    }
    for root in roots {
        let bank = u32::from(root.ios.physical_index) * 0x100000;
        let slot = unsafe {
            ConfigRegister::<_, iohc::TopOfDram::Register>::new(
                GlobalEcam,
                PciAddress::new(0, root.route.base, 0, 0),
                iohc::NB_TOP_OF_DRAM_SLOT1,
            )
        };
        iohc::set_tom_at(smn, &slot, DramTops { tom, tom2 }, TopStyle::Tom3, bank);
        let bus_number = smn.reg(iohc::NB_BUS_NUM_CNTL.offset(bank));
        if bus_number.get() == u32::MAX {
            return Err(());
        }
        bus_number.modify(
            iohc::NbBusNumCntl::BUS.val(root.route.base.into()) + iohc::NbBusNumCntl::LAT_MODE::SET,
        );
        if bus_number.read(iohc::NbBusNumCntl::BUS) != u32::from(root.route.base) {
            return Err(());
        }
        smn.reg(iohc::NB_TOP_OF_DRAM3.offset(bank))
            .modify(iohc::TopOfDram3::ENABLE::CLEAR);
        if slot.read(iohc::TopOfDram::TOM) != ((tom >> 23) as u32) {
            return Err(());
        }
        let lower = smn.reg(iohc::NB_LOWER_TOP_OF_DRAM2.offset(bank)).extract();
        let upper = smn.reg(iohc::NB_UPPER_TOP_OF_DRAM2.offset(bank)).extract();
        if let Some(top) = tom2
            && (!lower.is_set(iohc::LowerTopOfDram2::ENABLE)
                || lower.read(iohc::LowerTopOfDram2::TOM2) != (top >> 23) as u32 & 0x1ff
                || upper.read(iohc::UpperTopOfDram2::TOM2) != (top >> 32) as u32)
        {
            return Err(());
        }
        fstart_log::info!(
            "rs: IOHC IOS={} DRAM route readback TOM={:#x} lower={:#x} upper={:#x}",
            root.ios.physical_index,
            slot.get(),
            lower.get(),
            upper.get()
        );
    }
    Ok(())
}

pub fn train(
    roots: &[Root],
    configs: &[fstart_driver_amd::mpio::session::Config<'_>],
    psp_vetting: bool,
    encoding: mailbox::Encoding,
    heap: core::ops::Range<u64>,
    release_perst: unsafe fn() -> Result<(), &'static str>,
    required_pcie: &[(u8, u16)],
) -> Result<heapless::Vec<fstart_pci::PciAddress, 32>, ()> {
    use fstart_core::services::timer::Timer;
    use fstart_driver_amd::mpio::{
        ports,
        rs::{Kind, Lane, Status},
        session::{self, Prepared, Session},
    };
    let mut ids = heapless::Vec::<u8, 4>::new();
    for root in roots {
        ids.push(root.ios.physical_index).map_err(|_| ())?;
    }
    let mut policy = heapless::Vec::<fstart_driver_amd::mpio::rs::Port, 32>::new();
    for config in configs {
        policy.push(config.port).map_err(|_| ())?;
    }
    let prepared = checked(Prepared::new(0, &ids, configs), 2)?
        .with_psp_vetting(psp_vetting)
        .with_encoding(encoding);
    let layout = alloc::alloc::Layout::from_size_align(session::DMA_SIZE, 64).unwrap();
    // Deliberately leaked. No Box/drop can reclaim an outstanding DMA target.
    let ptr = unsafe { alloc::alloc::alloc_zeroed(layout) };
    let physical = ptr as u64;
    if ptr.is_null()
        || physical < heap.start
        || physical
            .checked_add(session::DMA_SIZE as u64)
            .is_none_or(|end| end > heap.end)
    {
        return Err(());
    }
    fstart_log::info!(
        "rs: MPIO reserved DMA={:#x} bytes={:#x} ASKs={}; BMC early-link path disabled",
        physical,
        session::DMA_SIZE,
        configs.len()
    );
    let dma = unsafe { session::Dma::new(ptr, session::DMA_SIZE, physical) }.map_err(|_| ())?;
    let primary = roots
        .iter()
        .find(|r| r.ios.physical_index == fstart_driver_amd::df::v4::PRIMARY_IOS)
        .ok_or(())?;
    // SAFETY: probe validated every alias and completed all idle operations;
    // BSP owns the controller; the complete allocation is permanently leaked.
    let smn = unsafe { Smn::on_bus(GlobalEcam, primary.route.base) };
    initialize_iohc_dram(roots, &smn)?;
    let mut controller = checked(
        unsafe {
            Session::new(
                smn,
                dma,
                prepared,
                fstart_arch::x86::writeback_cache_range,
                Poll {
                    reads: NonZeroU32::new(300_000).unwrap(),
                    interval_us: NonZeroU32::new(100).unwrap(),
                },
                NonZeroU32::new(100_000).unwrap(),
            )
        },
        3,
    )?;
    let mhz =
        NonZeroU32::new((fstart_arch::x86::tsc_frequency_hz() / 1_000_000) as u32).ok_or(())?;
    let mut delay = Tsc::new(mhz);
    fstart_log::info!("rs: MPIO submit global configuration, ancillary and ASK");
    checked(controller.submit(&mut delay), 4)?;
    fstart_log::info!("rs: MPIO MAP");
    checked(controller.advance(&mut delay), 5)?;
    for (i, ask) in checked(controller.asks(), 6)?.iter().enumerate() {
        fstart_log::info!(
            "rs: MPIO MAP result={} lane={} count={} reversed={} controller={} status={:#x} state={} local={}",
            i,
            ask.lane().read(Lane::START),
            ask.lane().read(Lane::COUNT),
            ask.lane().read(Lane::REVERSED),
            if ask.kind() == Some(Kind::Sata) {
                1u8
            } else {
                0u8
            },
            ask.training_state(),
            ask.status().read(Status::STATE),
            ask.status().read(Status::LOCAL_PORT_ID)
        );
    }
    for (i, port) in policy.iter().enumerate() {
        let m =
            fstart_driver_amd::mpio::rs::translate(&ids, port.start, port.end).map_err(|_| ())?;
        fstart_log::info!(
            "rs: MPIO expected={} board={}..{} translated={}..{} IOS={} wrapper={}",
            i,
            port.start,
            port.end,
            m.start,
            m.end,
            m.root_index,
            m.wrapper
        );
    }
    let mapped = ports::bind(&ids, &policy, checked(controller.asks(), 6)?).map_err(|error| {
        let label = match error {
            ports::Error::Root => "root",
            ports::Error::Port => "local-port",
            ports::Error::Missing => "missing",
            ports::Error::Duplicate => "duplicate",
            ports::Error::Unsupported => "unsupported",
            _ => "register",
        };
        fstart_log::error!("rs: MPIO map-result binding failed: {}", label);
    })?;
    for b in &mapped {
        fstart_log::info!(
            "rs: MPIO map policy={} IOS={} wrapper={} local={} slot={} devfn={:#x} LBID={}",
            b.policy,
            b.root,
            b.wrapper,
            b.local_port,
            b.slot,
            b.devfn,
            b.logical_bridge
        );
    }
    ports::program(&smn, &ids, &mapped).map_err(|_| {
        fstart_log::error!("rs: NBIO native devfn map inaccessible/readback failed");
    })?;
    fstart_log::info!("rs: MPIO CONFIGURE");
    checked(controller.advance(&mut delay), 7)?;
    fstart_log::info!("rs: MPIO PERST");
    checked(controller.advance(&mut delay), 8)?;
    // SAFETY: single socket, checked MPIO PERST completion, BSP-exclusive FCH.
    unsafe { release_perst() }.map_err(|message| {
        fstart_log::error!("rs: {}", message);
    })?;
    delay.delay_ms(200);
    fstart_log::info!("rs: MPIO TRAIN after physical PERST release +200ms");
    checked(controller.advance(&mut delay), 9)?;
    let asks = checked(controller.asks(), 10)?;
    for (i, ask) in asks.iter().enumerate() {
        fstart_log::info!(
            "rs: MPIO result={} lane={} count={} controller={} status={:#x} state={} local={}",
            i,
            ask.lane().read(Lane::START),
            ask.lane().read(Lane::COUNT),
            if ask.kind() == Some(Kind::Sata) {
                1u8
            } else {
                0u8
            },
            ask.training_state(),
            ask.status().read(Status::STATE),
            ask.status().read(Status::LOCAL_PORT_ID)
        );
    }
    let trained = ports::bind(&ids, &policy, asks).map_err(|_| ())?;
    let mut logical = heapless::Vec::<Option<u8>, 32>::new();
    for _ in asks {
        logical.push(None).map_err(|_| ())?;
    }
    let mut trained_count = 0;
    let mut visible_lan = 0;
    let mut active_ports = heapless::Vec::<fstart_pci::PciAddress, 32>::new();
    for b in &trained {
        if !mapped
            .iter()
            .any(|old| old.policy == b.policy && old.root == b.root && old.slot == b.slot)
        {
            return Err(());
        }
        logical[b.ask] = Some(b.logical_bridge);
        let state = asks[b.ask].status().read(Status::STATE);
        if state == 6 {
            trained_count += 1;
        }
        let root = roots
            .iter()
            .find(|r| r.ios.physical_index == b.root)
            .ok_or(())?;
        let dev = EcamDevice::new(root.route.base, b.devfn >> 3, b.devfn & 7);
        fstart_log::info!(
            "rs: PCIe root-port bus={:#x} devfn={:#x} ID={:#x}:{:#x} state={}",
            root.route.base,
            b.devfn,
            dev.vendor_id(),
            dev.device_id(),
            state
        );
        if state == 6 && dev.is_present() {
            use fstart_pci::config::PCI_EXPRESS_LINK_STATUS as LinkStatus;
            let cap = dev
                .find_capability(0x10)
                .filter(|&c| c + 0x14 <= 0x100)
                .ok_or(())?;
            let link = tock_registers::LocalRegisterCopy::<u16, LinkStatus::Register>::new(
                dev.read16(cap + 0x12),
            );
            fstart_log::info!(
                "rs: PCIe negotiated policy={} generation={} width=x{} training={} DLL-active={}",
                b.policy,
                link.read(LinkStatus::SPEED),
                link.read(LinkStatus::WIDTH),
                link.is_set(LinkStatus::TRAINING),
                link.is_set(LinkStatus::DLL_ACTIVE)
            );
            if link.read(LinkStatus::SPEED) == 0
                || link.read(LinkStatus::WIDTH) == 0
                || link.is_set(LinkStatus::TRAINING)
            {
                return Err(());
            }
            active_ports.push(dev.address()).map_err(|_| ())?;
            let secondary = ((root.route.base + 1)..=root.route.limit)
                .rev()
                .find(|&candidate| {
                    !ports::NATIVE_DEVFNS.iter().any(|&number| {
                        let bridge = EcamDevice::new(root.route.base, number >> 3, number & 7);
                        if !bridge.is_present()
                            || bridge.read8(fstart_pci::PCI_HEADER_TYPE) & 0x7f != 1
                        {
                            return false;
                        }
                        let buses = tock_registers::LocalRegisterCopy::<
                            u32,
                            fstart_pci::config::PCI_BRIDGE_BUSES::Register,
                        >::new(
                            bridge.read32(fstart_pci::PCI_PRIMARY_BUS)
                        );
                        let first =
                            buses.read(fstart_pci::config::PCI_BRIDGE_BUSES::SECONDARY) as u8;
                        let last =
                            buses.read(fstart_pci::config::PCI_BRIDGE_BUSES::SUBORDINATE) as u8;
                        first != 0 && first <= candidate && candidate <= last
                    })
                })
                .ok_or(())?;
            // SAFETY: BSP-exclusive bridge, known routed free bus, no host
            // endpoint DMA has been enabled. Routing is restored immediately.
            let endpoint = unsafe {
                fstart_pci::register::probe_direct_child(&GlobalEcam, dev.address(), secondary)
            }
            .map_err(|_| ())?;
            if let Some(identity) = endpoint {
                fstart_log::info!(
                    "rs: PCIe endpoint IOS={} policy={} root-devfn={:#x} config-ID={:#x}",
                    b.root,
                    b.policy,
                    b.devfn,
                    identity
                );
                if required_pcie.iter().any(|&(lane, vendor)| {
                    lane == policy[b.policy].start && u32::from(vendor) == identity & 0xffff
                }) {
                    visible_lan += 1;
                }
            } else {
                fstart_log::warn!(
                    "rs: trained link policy={} has no direct dev0/fn0 config identity",
                    b.policy
                );
            }
        }
    }
    let mut trained_bridges = heapless::Vec::<(u8, u8), 32>::new();
    for b in &trained {
        if asks[b.ask].status().read(Status::STATE) == 6 {
            trained_bridges
                .push((b.root, b.logical_bridge))
                .map_err(|_| ())?;
        }
    }
    checked(controller.update(&logical, &mut delay), 11)?;
    fstart_log::info!(
        "rs: MPIO update complete; {} PCIe links TRAINED, {} required endpoints visible",
        trained_count,
        visible_lan
    );
    hide_untrained_bridges(&smn, roots, &trained_bridges)?;
    if trained_count == 0 || visible_lan != required_pcie.len() {
        return Err(());
    }
    Ok(active_ports)
}

/// NFX PcieControlPorts(hide) then PcieEnablePorts: after training, only
/// ports with a link keep a visible IOHC bridge. Like Oxide, every other PCIe
/// logical bridge of the root is hidden too, not only those with an ASK:
/// config cycles must never reach a port whose link (and core) is down. A
/// port this session did not train but whose data link is active (a
/// PSP-trained, retained path such as a BMC link) stays visible. No hot-plug
/// port is kept visible.
fn hide_untrained_bridges(
    smn: &impl fstart_driver_amd::smn::SmnAccess,
    roots: &[Root],
    trained: &[(u8, u8)],
) -> Result<(), ()> {
    use fstart_driver_amd::{mpio::ports::NATIVE_DEVFNS, nbio::rs};
    use fstart_pci::config::PCI_EXPRESS_LINK_STATUS as LinkStatus;
    for root in roots {
        let ios = root.ios.physical_index;
        let mut shown = 0u32;
        for lbid in 0..rs::pcie_bridges(ios) {
            let devfn = NATIVE_DEVFNS[usize::from(lbid)];
            let dev = EcamDevice::new(root.route.base, devfn >> 3, devfn & 7);
            let retained = !trained.contains(&(ios, lbid))
                && dev.is_present()
                && dev
                    .find_capability(0x10)
                    .filter(|&c| c + 0x14 <= 0x100)
                    .is_some_and(|cap| {
                        tock_registers::LocalRegisterCopy::<u16, LinkStatus::Register>::new(
                            dev.read16(cap + 0x12),
                        )
                        .is_set(LinkStatus::DLL_ACTIVE)
                    });
            if retained {
                fstart_log::info!(
                    "rs: IOHC IOS={} LBID={} devfn={:#x} retained active link stays visible",
                    ios,
                    lbid,
                    devfn
                );
            }
            let visible = retained || trained.contains(&(ios, lbid));
            let value = rs::set_bridge_visible(smn, ios, lbid, visible).ok_or(())?;
            if value == u32::MAX || (value & 0b101 == 0) != visible {
                fstart_log::error!(
                    "rs: IOHC bridge IOS={} LBID={} control readback {:#x}",
                    ios,
                    lbid,
                    value
                );
                return Err(());
            }
            shown |= u32::from(visible) << lbid;
        }
        fstart_log::info!(
            "rs: IOHC IOS={} visible PCIe bridges LBID-mask={:#x}; others hidden",
            ios,
            shown
        );
    }
    Ok(())
}
