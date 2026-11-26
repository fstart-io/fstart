// SPDX-License-Identifier: MIT OR Apache-2.0
//! WH MPIO link training on SP7, on every IOD of every socket, each through
//! its own root bus (MPIO.md §2.2):
//! - the ABL-trained early links found (every IOD's bonus core) and the
//!   state the ABL left below them cleared (`EarlyBmcLinkTraining`);
//! - phase 1 per IOD: global configuration, ancillary data, ASKs; map, then
//!   the ports bound by `(MPIO start lane, controller)`, their devfns
//!   remapped and, per PCIe core, the pre-reconfiguration straps of its
//!   ports and its own (`dxio::wh::bifurcation`; a core with an
//!   ABL-trained port its register path instead, or nothing without the
//!   early-BMC lane); reconfigure, then per
//!   core its ports' and its own post-reconfiguration programming;
//!   pre-train;
//! - phase 2: PERST released on the primary and every secondary FCH;
//! - phase 3: the early train of the IODs whose ports ask for it;
//! - phase 4: TRAIN to every IOD with ports and WAIT to every IOD before
//!   any result is read; the logical bridges written back, `UPDATE_LINK`
//!   with UBM hotplug; the after-training programming, hotplug, and the
//!   bridges of ports that did not train hidden.

extern crate alloc;

use core::num::NonZeroU32;

use fstart_core::services::Timer;

use fstart_driver_amd::dxio::rs::ask::Status;
use fstart_driver_amd::dxio::rs::mailbox::Poll;
use fstart_driver_amd::dxio::wh::{
    self as dxio_wh, Binding, DMA_SIZE, Dma, Layout, Prepared, Session, bifurcation, early,
    hotplug,
    port::Hotplug,
    straps::{self, PortSettings},
};
use fstart_driver_amd::fch::qilian;
use fstart_driver_amd::nbio::wh::{
    self as nbio_wh,
    pcie::{self, PortId},
};
use fstart_driver_amd::smn::Smn;
use fstart_driver_amd::smu::v14 as smu_v14;
use fstart_driver_amd::window::Mmio;
use fstart_pci::{EcamDevice, PciAddress, ecam::GlobalEcam};
use heapless::Vec;
use tock_registers::interfaces::Readable;

use super::fabric::{self, MAX_NODES};
use super::init::State;
use super::{PortDesc, SocketTopology, Sp7, Variant, links};
use crate::flow::Ctx;

/// PCIe link state of a trained port in its returned MPIO ASK.
const TRAINED: u32 = 6;

/// Trained WH root ports, verified bindings, and lane-to-port associations:
/// every port of every IOD (up to `MAX_NODES * MAX_ASKS`), none dropped.
pub struct Trained {
    /// `(socket, first lane, root_port_bdf, link_up)` for trained & ABL-trained ports.
    pub lanes: alloc::vec::Vec<(u8, u8, PciAddress, bool)>,
    /// `(socket, binding)`: verified root-port identities.
    pub bindings: alloc::vec::Vec<(u8, Binding)>,
}

/// The PCIe complex of the part (`cpu::wh::Part::layout`): by CPUID, not
/// the count of unfused roots.
pub(super) fn layout() -> Layout {
    super::nbio::part().layout()
}

/// The ports of `socket`.
fn ports_of<'a>(topology: &'a [SocketTopology<'a>], socket: u8) -> &'a [PortDesc<'a>] {
    topology
        .iter()
        .find(|t| t.socket == socket)
        .map_or(&[], |t| t.ports)
}

/// The NBIO of the IOHC `iohc` behind root bus `root` (segment and bus).
fn iohc(iohc: u8, root: PciAddress) -> Option<nbio_wh::RootBridge<GlobalEcam>> {
    // SAFETY: a routed root bridge of the IOD; the BSP owns SMN during boot.
    unsafe { nbio_wh::RootBridge::on_root(GlobalEcam, iohc, root) }
}

/// Root bus of physical root bridge `rb` on `(socket, die)`.
fn rb_bus(state: &State, socket: u8, die: u8, rb: u8) -> Option<PciAddress> {
    state.rb_root(socket, fabric::rb_index(die, rb))
}

/// Function `devfn` on the root bus `root`.
fn on_root(root: PciAddress, devfn: u8) -> PciAddress {
    PciAddress::new(root.segment(), root.bus(), devfn >> 3, devfn & 7)
}

/// A read-modify-write of the root port's configuration space.
fn config_rmw(port: PciAddress, offset: u16, clear: u32, set: u32, word: bool) {
    let dev = EcamDevice::new_address(port);
    if word {
        let v = dev.read16(offset);
        dev.write16(offset, (v & !(clear as u16)) | set as u16);
    } else {
        dev.modify32(offset, !clear, set);
    }
}

/// The port settings of `binding` under the platform's policy.
fn settings(desc: &PortDesc<'_>, b: &Binding, policy: &super::Mpio) -> PortSettings {
    PortSettings::new(
        desc,
        b.local_port,
        b.core,
        b.rc,
        &policy.ports,
        policy.global.enable_2spc_gen5,
    )
}

/// Run WH MPIO link training on every IOD of every socket.
pub fn train<V: Variant>(
    ctx: &Ctx<'_, Sp7<V>>,
    state: &State,
    topology: &[SocketTopology<'_>],
) -> Result<Trained, &'static str> {
    let layout = layout();
    let cfg = &ctx.config.mpio;
    let timer = &ctx.clock;
    let poll = Poll {
        reads: const { NonZeroU32::new(300_000).unwrap() },
        interval_us: const { NonZeroU32::new(100).unwrap() },
    };
    let idle_reads = const { NonZeroU32::new(100_000).unwrap() };
    for t in topology {
        if !state.nodes().iter().any(|&(s, _)| s == t.socket) && !t.ports.is_empty() {
            fstart_log::error!("{}: MPIO ports on absent socket {}", V::NAME, t.socket);
            return Err("WH MPIO socket");
        }
    }

    let mut lanes = alloc::vec::Vec::new();
    let mut bindings = alloc::vec::Vec::new();
    // `(socket, die, rb, logical bridge)` of every bridge to keep visible:
    // one left out would hide a trained port.
    let mut visible: alloc::vec::Vec<(u8, u8, u8, u8)> = alloc::vec::Vec::new();

    // The ABL-trained links (`MpioCleanUpEarlyInitSP5Wh` `0xf61370`, run
    // with `EarlyBmcLinkTraining`): every IOD's bonus-core port whose LTSSM
    // is up keeps the devfn in its remap slot, what the ABL configured below
    // it goes, and its core is marked (`wrapper + 0x12` bit 10), which
    // decides its path before the bifurcation (`bifurcation::core_path`).
    // The ports of the `EarlyBmcLinkTraining` lane's core are listed under
    // that lane, the others under their core's first lane.
    let mut marked: alloc::vec::Vec<(u8, u8)> = alloc::vec::Vec::new();
    if let Some(bmc) = cfg.early_bmc {
        let early = dxio_wh::translate_socket_lane(layout, bmc.lane, bmc.lane)
            .map_err(|_| "WH early BMC lane")?;
        let (_, bonus, _, _) = layout.bonus_core();
        let wrapper = layout.wrapper(bonus).ok_or("WH bonus core")?;
        for &(socket, iod) in state.nodes() {
            let first = wrapper.user_start[usize::from(iod)];
            let Ok(map) = dxio_wh::translate_socket_lane(layout, first, first) else {
                continue;
            };
            let lane = if (socket, iod) == (bmc.socket, early.iod) {
                bmc.lane
            } else {
                first
            };
            let (node, rb_bus) = (
                state.node_root(socket, map.iod),
                rb_bus(state, socket, map.iod, map.rb_on_iod),
            );
            let (Some(node), Some(rb_bus)) = (node, rb_bus) else {
                continue;
            };
            let Some(nb) = iohc(map.iohc, rb_bus) else {
                continue;
            };
            // SAFETY: no MPIO session on this IOD yet; the BSP owns its mailbox.
            let proxy = unsafe { dxio_wh::Proxy::new(Smn::on_root(GlobalEcam, node), poll, timer) };
            for port in 0..map.max_ports {
                let id = PortId {
                    core: map.core,
                    rc: map.rc,
                    port,
                };
                if !early::trained(&proxy, id).unwrap_or(false) {
                    continue;
                }
                if !marked.contains(&(socket, iod)) {
                    marked.push((socket, iod));
                }
                let slot = map.slot_base + port;
                let default = layout.default_devfns()[usize::from(slot)];
                let devfn = layout
                    .slot_devfn(&nb, slot)
                    .map_or(default, |(d, f)| d << 3 | f);
                let address = on_root(rb_bus, devfn);
                // SAFETY: nothing else uses the buses below this root port yet.
                unsafe { early::clean_up(&GlobalEcam, address) };
                fstart_log::info!(
                    "{}: ABL-trained S{} IOD{} lane {} at {:x}:{}:{}.{}",
                    V::NAME,
                    socket,
                    iod,
                    lane,
                    rb_bus.segment(),
                    rb_bus.bus(),
                    devfn >> 3,
                    devfn & 7
                );
                let logical_bridge = layout
                    .default_devfns()
                    .iter()
                    .position(|&d| d == devfn)
                    .unwrap_or(usize::from(slot)) as u8;
                let binding = Binding {
                    ask: usize::MAX,
                    port: usize::MAX,
                    iod: map.iod,
                    rb_on_iod: map.rb_on_iod,
                    iohc: map.iohc,
                    core: map.core,
                    rc: map.rc,
                    local_port: port,
                    pcie_bridge_id: slot,
                    remap_reg: layout.remap_reg_index(slot).unwrap_or(0),
                    devfn,
                    logical_bridge,
                    refused: None,
                };
                lanes.push((socket, lane, address, true));
                bindings.push((socket, binding));
                visible.push((socket, map.iod, map.rb_on_iod, logical_bridge));
            }
        }
    }

    // Phase 1, keyed by the vendor's instance, `socket * 2 + die`.
    let mut sessions: Vec<(u8, Session<Smn<GlobalEcam>>), MAX_NODES> = Vec::new();
    let mut idle: Vec<Smn<GlobalEcam>, MAX_NODES> = Vec::new();
    for &(socket, iod) in state.nodes() {
        let ports = ports_of(topology, socket);
        let node = state.node_root(socket, iod).ok_or("WH MPIO die bus")?;
        // SAFETY: BSP owns the MPIO mailbox of this IOD on `node`.
        let smn = unsafe { Smn::on_root(GlobalEcam, node) };
        let prepared = Prepared::for_iod(layout, socket, iod, ports, &cfg.global, cfg.ancillary)
            .map_err(|e| {
                if let dxio_wh::Error::Port(i, _) = e {
                    fstart_log::error!("{}: S{} MPIO port {} malformed", V::NAME, socket, i);
                }
                "WH MPIO ports"
            })?;
        let Some((prepared, _)) = prepared else {
            // One per node: `MAX_NODES` at most.
            let _ = idle.push(smn);
            continue;
        };
        let dma_buf = crate::memory::dma_alloc(DMA_SIZE, 64)?;
        let (ptr, phys) = (dma_buf.as_mut_ptr(), dma_buf.as_ptr() as u64);
        // SAFETY: fresh DMA allocation, permanently reserved, identity-mapped below 4 GiB.
        let dma = unsafe { Dma::new(ptr, DMA_SIZE, phys) }.map_err(|_| "WH MPIO DMA")?;
        // SAFETY: BSP owns the MPIO mailbox on `bus` for this IOD.
        let mut ctrl = unsafe {
            Session::new(
                smn,
                dma,
                prepared,
                fstart_arch::x86::writeback_cache_range,
                poll,
                idle_reads,
            )
        }
        .map_err(|_| "WH MPIO session")?;

        ctrl.submit(timer).map_err(|_| "WH MPIO submit")?;
        ctrl.advance(timer).map_err(|_| "WH MPIO map")?;

        // Bind the mapped ports, remap their devfns on their IOHC, and write
        // their straps before the reconfiguration.
        let mapped = dxio_wh::bind(layout, iod, ports, ctrl.asks().map_err(|_| "WH MPIO asks")?)
            .map_err(|_| "WH MPIO map bind")?;
        for w in layout.wrappers() {
            if mapped.iter().all(|b| b.rb_on_iod != w.rb) {
                continue;
            }
            if let Some(bus) = rb_bus(state, socket, iod, w.rb)
                && let Some(nb) = iohc(layout.iohc(w.rb), bus)
            {
                dxio_wh::program_remaps(&nb, layout, iod, w.rb, &mapped);
            }
        }
        for b in &mapped {
            if let Some(want) = b.refused {
                fstart_log::warn!(
                    "{}: S{} IOD{} core {} port {}: devfn {:#x} refused, {:#x} kept",
                    V::NAME,
                    socket,
                    iod,
                    b.core,
                    b.local_port,
                    want,
                    b.devfn
                );
            }
        }
        // `MpioCfgCoreBeforeBifurcationWh` per wrapper
        // (`bifurcation::core_path`): a marked core with the early-BMC
        // lane its register path, a marked one without nothing; every other
        // core the SLT path, its ports' straps, then its own lists.
        let early_core = cfg
            .early_bmc
            .filter(|bmc| bmc.socket == socket)
            .and_then(|bmc| dxio_wh::translate_socket_lane(layout, bmc.lane, bmc.lane).ok())
            .filter(|map| map.iod == iod)
            .map(|map| map.core);
        let (_, bonus, _, _) = layout.bonus_core();
        let a0 = layout.is_a0();
        let mut none = |_: u16, _: u32, _: u32, _: bool| {};
        for w in layout.wrappers() {
            let on_core = || mapped.iter().filter(move |b| b.core == w.core);
            let marked = w.core == bonus && marked.contains(&(socket, iod));
            match bifurcation::core_path(marked, early_core == Some(w.core)) {
                bifurcation::CorePath::Early => {
                    let ops = bifurcation::before_bifurcation_early(
                        w.core,
                        w.rc,
                        &cfg.cores,
                        &cfg.core_policy,
                        a0,
                    )
                    .map_err(|_| "WH MPIO core straps")?;
                    ctrl.run(&ops, &mut none, timer)
                        .map_err(|_| "WH MPIO core straps")?;
                    continue;
                }
                bifurcation::CorePath::Skip => continue,
                bifurcation::CorePath::Normal => {}
            }
            let ops = bifurcation::before_ports(w.core, w.rc, &cfg.cores, &cfg.core_policy, a0)
                .map_err(|_| "WH MPIO core straps")?;
            ctrl.run(&ops, &mut none, timer)
                .map_err(|_| "WH MPIO core straps")?;
            let mut holds = bifurcation::CorePorts::default();
            for b in on_core() {
                let desc = &ports[b.port];
                holds.cxl |= desc.cxl || cfg.ports.cxl_on_all_ports;
                holds.pcie |= desc.engine.is_pcie();
                let s = settings(desc, b, cfg);
                let ops = straps::before_reconfig(&s, &cfg.cores, &cfg.ports, a0)
                    .map_err(|_| "WH MPIO straps")?;
                ctrl.run(&ops, &mut none, timer)
                    .map_err(|_| "WH MPIO straps")?;
            }
            let ops = bifurcation::before_bifurcation(
                w.core,
                w.rc,
                holds,
                &cfg.cores,
                &cfg.core_policy,
                a0,
            )
            .map_err(|_| "WH MPIO core straps")?;
            ctrl.run(&ops, &mut none, timer)
                .map_err(|_| "WH MPIO core straps")?;
        }
        ctrl.advance(timer).map_err(|_| "WH MPIO reconfigure")?;
        // `MpioCfgCoreAfterBifurcationWh`: the ports, then the core.
        for w in layout.wrappers() {
            for b in mapped.iter().filter(|b| b.core == w.core) {
                let s = settings(&ports[b.port], b, cfg);
                let ops = straps::after_reconfig(&s, &cfg.ports, a0)
                    .map_err(|_| "WH MPIO reconfigured ports")?;
                ctrl.run(&ops, &mut none, timer)
                    .map_err(|_| "WH MPIO reconfigured ports")?;
            }
            let ops = bifurcation::after_bifurcation(w.core, w.rc, &cfg.core_policy, a0)
                .map_err(|_| "WH MPIO core straps")?;
            ctrl.run(&ops, &mut none, timer)
                .map_err(|_| "WH MPIO core straps")?;
        }
        ctrl.advance(timer).map_err(|_| "WH MPIO pre-train")?;
        // One per node: `MAX_NODES` at most. The key is the vendor's
        // instance, the DF node index `socket * 2 + die`.
        let key = fstart_driver_amd::df::wh::node_index(socket, iod).ok_or("WH MPIO node")?;
        let _ = sessions.push((key, ctrl));
    }

    // With PCIe device authentication MPIO waits for each socket's SMU to
    // clear the links to train, sent once every instance finished phase 1
    // (`MpioCfgAfterAllInstancesWh`, SilVenicePei `0xf658c0`).
    if cfg.global.device_auth {
        for socket in 0..state.sockets() {
            let smu = state.smu_on(socket, 0, ctx.clock);
            if smu_v14::msg::clear_to_train(&smu).is_err() {
                fstart_log::error!("{}: S{} SMU PcieLinkClearToTrain refused", V::NAME, socket);
                return Err("WH PCIe clear to train");
            }
        }
    }

    // Phase 2: PERST on the primary and every verified secondary FCH. A
    // secondary that fails is reported and the boot goes on
    // ([`super::fch`]'s policy): the ports behind it do not train, and the
    // board's required devices there stop the boot below.
    if !qilian::release_perst(&ctx.hw.fch, cfg.perst) {
        return Err("PERST release");
    }
    for &(socket, iod, sec_base) in state.secondary_fchs() {
        // SAFETY: `sec_base` was relocated and readback-verified in `init_fch`
        // inside `below_pools.non_pci` (identity-mapped uncached MMIO).
        let sec_acpi = unsafe { Mmio::new(sec_base as usize) };
        if !qilian::release_perst_window(&sec_acpi, cfg.perst) {
            fstart_log::error!(
                "{}: S{} IOD{} FCH PERST release failed",
                V::NAME,
                socket,
                iod
            );
        }
    }
    if cfg.after_reset_delay_ms != 0 {
        ctx.clock.delay_ms(cfg.after_reset_delay_ms.into());
    }

    // Phase 3: early train.
    for (_, ctrl) in sessions.iter_mut() {
        if ctrl.prepared().early_train() {
            ctrl.early_train(timer).map_err(|_| "WH MPIO early train")?;
        }
    }

    // Phase 4.
    dxio_wh::train_all(&mut sessions, &idle, poll, timer).map_err(|_| "WH MPIO train/wait")?;
    let mut table = hotplug::Table::default();
    for (key, ctrl) in &mut sessions {
        let (socket, iod) = (*key >> 1, *key & 1);
        let ports = ports_of(topology, socket);
        ctrl.collect_train(timer).map_err(|_| "WH MPIO results")?;
        let (iod_bindings, link_up, logical) = {
            let asks = ctrl.asks().map_err(|_| "WH MPIO asks")?;
            let iod_bindings =
                dxio_wh::bind(layout, iod, ports, asks).map_err(|_| "WH MPIO train bind")?;
            let mut logical: alloc::vec::Vec<Option<u8>> = alloc::vec::Vec::new();
            let mut link_up: alloc::vec::Vec<bool> = alloc::vec::Vec::new();
            for ask in asks {
                logical.push(None);
                link_up.push(ask.status().read(Status::STATE) == TRAINED);
            }
            for binding in &iod_bindings {
                logical[binding.ask] = Some(binding.logical_bridge);
            }
            (iod_bindings, link_up, logical)
        };
        ctrl.update(&logical, cfg.ubm_hotplug, timer)
            .map_err(|_| "WH MPIO update")?;

        for b in &iod_bindings {
            let desc = &ports[b.port];
            let up = link_up[b.ask];
            let bus = rb_bus(state, socket, b.iod, b.rb_on_iod).ok_or("WH MPIO root bridge")?;
            let port = on_root(bus, b.devfn);
            let (lo, hi) = desc.lanes();
            fstart_log::info!(
                "{}: MPIO S{} IOD{} RB{} core {} port {} lanes {}-{} {}:{}.{} {}",
                V::NAME,
                socket,
                b.iod,
                b.rb_on_iod,
                b.core,
                b.local_port,
                lo,
                hi,
                port.bus(),
                port.device(),
                port.function(),
                if up { "trained" } else { "down" }
            );
            let s = settings(desc, b, cfg);
            // Link Capabilities and Status through SMN, as the vendor reads
            // them (`nbio::wh::pcie`'s access rule).
            let speed_cap = ctrl
                .smn()
                .reg(pcie::LINK_CAPABILITIES.at(b.port_id()))
                .read(pcie::LinkCapabilities::MAX_SPEED);
            let ops = straps::after_training(&s, &cfg.ports, up, speed_cap as u8)
                .map_err(|_| "WH MPIO trained ports")?;
            let mut config = |o, c, v, w| config_rmw(port, o, c, v, w);
            ctrl.run(&ops, &mut config, timer)
                .map_err(|_| "WH MPIO trained ports")?;
            if desc.hotplug != Hotplug::Disabled
                && let Some(hp) = &cfg.hotplug
            {
                let endpoint = ctrl
                    .smn()
                    .reg(pcie::LINK_CONTROL_STATUS.at(b.port_id()))
                    .is_set(pcie::LinkControlStatus::DLL_ACTIVE);
                let npem = desc.npem.map(|n| (n.enable, n.capability));
                let mut ops = straps::hotplug_pre_init(&s).map_err(|_| "WH MPIO hotplug ports")?;
                let init =
                    hotplug::init(&s, &hp.root_ports, b.iohc, b.logical_bridge, endpoint, npem)
                        .map_err(|_| "WH MPIO hotplug ports")?;
                ops.extend_from_slice(&init)
                    .map_err(|_| "WH MPIO hotplug ports")?;
                ctrl.run(&ops, &mut config, timer)
                    .map_err(|_| "WH MPIO hotplug ports")?;
                for slot in hp
                    .slots
                    .iter()
                    .filter(|s| s.socket == socket && (s.start_lane, s.end_lane) == desc.lanes())
                {
                    let slot_port = hotplug::SlotPort {
                        socket,
                        die: b.iod,
                        core: b.core,
                        port: b.local_port,
                        logical_bridge: b.logical_bridge,
                        hotplug: desc.hotplug,
                        legacy_i2c: desc.legacy_i2c,
                    };
                    if table.add(slot, &slot_port, hp.service.slot_base).is_err() {
                        fstart_log::warn!(
                            "{}: hotplug slot {} does not fit its port",
                            V::NAME,
                            slot.slot_num
                        );
                    }
                }
            }
            if cfg.ubm_hotplug && desc.hotplug == Hotplug::Ubm {
                ctrl.release_port(b.iohc, b.core, b.local_port, timer)
                    .map_err(|_| "WH MPIO UBM release")?;
            }
            lanes.push((socket, desc.start_lane, port, up));
            let expose = desc.always_expose == Some(true) || desc.hotplug != Hotplug::Disabled;
            if up || expose {
                visible.push((socket, b.iod, b.rb_on_iod, b.logical_bridge));
            }
            if up {
                bindings.push((socket, *b));
            }
        }

        // The ABL-trained link's early-link information.
        if let Some(bmc) = cfg.early_bmc.filter(|bmc| bmc.socket == socket)
            && let Ok(map) = dxio_wh::translate_socket_lane(layout, bmc.lane, bmc.lane)
            && map.iod == iod
        {
            match ctrl.deli_info(map.start, timer) {
                Ok(deli) => fstart_log::info!(
                    "{}: early BMC link DELI valid {} port {}",
                    V::NAME,
                    deli.valid,
                    deli.port
                ),
                Err(_) => fstart_log::warn!("{}: early BMC link DELI failed", V::NAME),
            }
        }
    }

    // MPIO's hotplug service, on every IOD with ports: the table (0x13) to
    // each, the I2C switch addresses (0x16), the flags (0x19) and the
    // enable (0x14) to the first one (socket 0's IOD 0) only. So does the
    // vendor on 2P (ServerHotplugInitV14Dxe, AmdNbioDxe `0xcf80`,
    // 2P_COMPLETE §2.17): 0x16/0x19/0x14 to the first GnbHandle only, 0x13
    // to every GnbHandle of every socket (the same IOD's mailbox once per
    // root bridge), each engine with its socket and die; it also sends 0x13
    // to IODs without ports, which fstart skips.
    if let Some(hp) = &cfg.hotplug
        && !table.is_empty()
    {
        if let Some((_, first)) = sessions.first_mut() {
            for address in hotplug::expander_addresses(hp.service.i2c_address, table.second_switch)
            {
                first
                    .hotplug_i2c_address(address, timer)
                    .map_err(|_| "WH MPIO hotplug I2C")?;
            }
        }
        for (_, ctrl) in sessions.iter_mut() {
            ctrl.hotplug_table(&table, timer)
                .map_err(|_| "WH MPIO hotplug table")?;
        }
        if let Some((_, first)) = sessions.first_mut() {
            // About 100 ms on the first PCIe core's counter, as the vendor
            // waits (`hotplug::settle`).
            let counter = layout
                .wrappers()
                .first()
                .map(|w| pcie::CORE_COUNTER.of_core(w.core, w.rc).address())
                .ok_or("WH MPIO hotplug wait")?;
            hotplug::settle(|| first.proxy_read(counter, dxio_wh::Width::Dword, timer))
                .map_err(|_| "WH MPIO hotplug wait")?;
            first
                .hotplug_enable(hp.service.flags, hp.service.enable_argument(), timer)
                .map_err(|_| "WH MPIO hotplug enable")?;
        }
    }

    // Keep the bridges of trained (or exposed) ports visible and hide the
    // rest of every root bridge so PCI scan never hangs. Bridge control is
    // per logical bridge (`0x13B3_1004 + lbid * 0x400`, Dell `0xf622b0`)
    // on the root bridge's IOHC.
    for rb in state.roots() {
        let (die, rb_on_iod) = fabric::rb_location(rb.index);
        if !layout.wrappers().iter().any(|w| w.rb == rb_on_iod) {
            continue;
        }
        let Some(nb) = iohc(
            layout.iohc(rb_on_iod),
            fabric::root_bus(rb.segment, rb.bus_base),
        ) else {
            continue;
        };
        for bridge in layout.first_slot()..dxio_wh::PORT_SLOTS as u8 {
            let show = visible.contains(&(rb.socket, die, rb_on_iod, bridge));
            nb.set_bridge_visible(usize::from(bridge), show);
        }
    }

    for s in links::missing(&cfg.expected, &lanes) {
        fstart_log::warn!(
            "{}: soldered link down on S{} lane {}",
            V::NAME,
            s.socket,
            s.lane
        );
    }
    let missing = links::missing(&cfg.required, &lanes)
        .inspect(|s| {
            fstart_log::error!(
                "{}: required soldered link down on S{} lane {}",
                V::NAME,
                s.socket,
                s.lane
            )
        })
        .count();
    if missing != 0 {
        return Err("WH MPIO endpoints");
    }
    Ok(Trained { lanes, bindings })
}
