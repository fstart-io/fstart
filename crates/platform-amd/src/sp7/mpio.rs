// SPDX-License-Identifier: MIT OR Apache-2.0
//! WH dual-IOD MPIO v2 link training on SP7:
//! - Resolves the WH silicon PCIe complex layout (`Sp7A0`, `Sp7B0`, `Sp8`).
//! - Splits non-PSP-trained board lanes across live local IODs (`0` and `1`)
//!   and runs WH's dedicated MPIO v2 session (`DOORBELL_V2 = 0x0C91_0954`,
//!   `Encoding::Retained`, `MSG_WAIT_COMPLETE = 0x3a`).
//! - Binds MPIO-returned ASKs by `(local_start_lane, kind)` (never by array
//!   position), programs `NB_PROG_DEVICE_REMAP` between `MAP` and `CONFIGURE`,
//!   releases PERST# on primary AND every verified secondary FCH aperture,
//!   propagates `UPDATE_LINK` errors, preserves PSP-trained ports strictly on
//!   the layout's bonus-core slots, enables AER on active cores, and hides
//!   inactive IOHC bridges.

extern crate alloc;

use core::num::NonZeroU32;

use fstart_core::services::Timer;
use fstart_driver_amd::dxio::rs::ask::{LinkSpeed, Port, Status};
use fstart_driver_amd::dxio::rs::mailbox::Poll;
use fstart_driver_amd::dxio::wh::{
    self as dxio_wh, Binding, Config, DMA_SIZE, Dma, Layout, Prepared, Session,
};
use fstart_driver_amd::fch::qilian;
use fstart_driver_amd::nbio::wh as nbio_wh;
use fstart_driver_amd::smn::Smn;
use fstart_driver_amd::window::Mmio;
use fstart_pci::{EcamDevice, PciAddress, ecam::GlobalEcam};
use heapless::Vec;

use super::init::State;
use super::{LaneDescriptor, Sp7, Variant, links};
use crate::flow::Ctx;

/// PCIe link state of a trained port in its returned MPIO ASK.
const TRAINED: u32 = 6;

/// Trained WH root ports, verified bindings, and lane-to-port associations.
pub struct Trained {
    /// `(first_socket_lane, root_port_bdf, link_up)` for trained & PSP-trained ports.
    pub lanes: Vec<(u8, PciAddress, bool), 32>,
    /// Verified root-port identity bindings (used by `after_resources` for requester IDs).
    pub bindings: Vec<Binding, 32>,
}

const fn speed_from_gen(max_gen: u8) -> LinkSpeed {
    match max_gen {
        1 => LinkSpeed::Gen1,
        2 => LinkSpeed::Gen2,
        3 => LinkSpeed::Gen3,
        4 => LinkSpeed::Gen4,
        5 => LinkSpeed::Gen5,
        6 => LinkSpeed::Gen6,
        _ => LinkSpeed::Auto,
    }
}

/// Convert board [`LaneDescriptor`] entries (skipping links already trained by
/// the PSP/ABL) into WH MPIO v2 port configs.
fn build_configs(descriptors: &[LaneDescriptor]) -> Vec<Config<'static>, 32> {
    let mut configs = Vec::new();
    for desc in descriptors {
        if desc.psp_trained {
            continue;
        }
        let max_speed = speed_from_gen(desc.max_gen);
        let port = Port {
            max_speed,
            target_speed: max_speed,
            ..Port::pcie(desc.start_lane, desc.end_lane)
        };
        let _ = configs.push(Config {
            port,
            phy: &[],
            speed: &[],
        });
    }
    configs
}

/// The donor selects complex tables by CPUID, not the count of unfused roots.
pub(super) fn layout() -> Layout {
    let (eax, ebx, _, _) = fstart_arch::x86::cpuid(0x8000_0001);
    Layout::from_cpuid(eax, ebx)
}

/// Run WH dual-IOD MPIO v2 link training across every live local IOD, program
/// IOHC bridge remaps, release PERST# across primary + verified secondary FCHs,
/// enable AER, hide inactive bridges, and match PSP-trained bonus-core links.
pub fn train<V: Variant>(
    ctx: &Ctx<'_, Sp7<V>>,
    state: &State,
    descriptors: &[LaneDescriptor],
) -> Result<Trained, &'static str> {
    let layout = layout();
    let mpio_cfg = &ctx.config.mpio;
    let configs = build_configs(descriptors);
    let ports: Vec<Port, 32> = configs.iter().map(|c| c.port).collect();
    let timer = &ctx.clock;
    let poll = Poll {
        reads: const { NonZeroU32::new(300_000).unwrap() },
        interval_us: const { NonZeroU32::new(100).unwrap() },
    };
    let idle_reads = const { NonZeroU32::new(100_000).unwrap() };

    let mut sessions: Vec<(u8, Session<Smn<GlobalEcam>>), 2> = Vec::new();
    for &iod in state.dies() {
        let Some((prepared, _board_indices)) =
            Prepared::for_iod(layout, iod, &configs).map_err(|_| "WH MPIO ports")?
        else {
            continue;
        };
        let bus = state.die_bus(iod).ok_or("WH MPIO die bus")?;
        let dma_buf = crate::memory::dma_alloc(DMA_SIZE, 64)?;
        let (ptr, phys) = (dma_buf.as_mut_ptr(), dma_buf.as_ptr() as u64);
        // SAFETY: fresh DMA allocation, permanently reserved, identity-mapped below 4 GiB.
        let dma = unsafe { Dma::new(ptr, DMA_SIZE, phys) }.map_err(|_| "WH MPIO DMA")?;
        // SAFETY: BSP owns MPIO v2 mailbox on `bus` for this IOD.
        let smn = unsafe { Smn::on_bus(GlobalEcam, bus) };
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

        // Phase 1: submit ASK/ancillary and run MAP.
        ctrl.submit(timer).map_err(|_| "WH MPIO submit")?;
        ctrl.advance(timer).map_err(|_| "WH MPIO map")?;

        // Bind mapped ASKs by (lane, kind) and program NB_PROG_DEVICE_REMAP on
        // every root bridge of this IOD before CONFIGURE.
        let mapped_asks = ctrl.asks().map_err(|_| "WH MPIO mapped asks")?;
        let mapped =
            dxio_wh::bind(layout, iod, &ports, mapped_asks).map_err(|_| "WH MPIO map bind")?;
        for rb in state.roots() {
            if rb.index / nbio_wh::ROOT_BRIDGES_PER_IOD != iod {
                continue;
            }
            let rb_on_iod = rb.index % nbio_wh::ROOT_BRIDGES_PER_IOD;
            // SAFETY: routed root bridge on this IOD, BSP owns SMN.
            if let Some(nb) =
                unsafe { nbio_wh::RootBridge::new(GlobalEcam, rb_on_iod, rb.bus_base) }
            {
                dxio_wh::program_remaps(&nb, layout, iod, rb_on_iod, &mapped);
            }
        }

        // Phases 2..3: CONFIGURE and PERST prepare.
        ctrl.advance(timer).map_err(|_| "WH MPIO configure")?;
        ctrl.advance(timer).map_err(|_| "WH MPIO PERST")?;
        let _ = sessions.push((iod, ctrl));
    }

    // Release board PCIe slot PERST# on the primary FCH and on every verified
    // secondary FCH aperture (matching Qilian donor `decomp/00f67c80.c`), then
    // wait 200 ms for endpoint reset recovery.
    if !qilian::release_perst(&ctx.hw.fch, mpio_cfg.perst) {
        return Err("PERST release");
    }
    for &(iod, sec_base) in state.secondary_fchs() {
        // SAFETY: `sec_base` was relocated and readback-verified in `init_fch`
        // inside `below_pools.non_pci` (identity-mapped uncached MMIO).
        let sec_acpi = unsafe { Mmio::new(sec_base as usize) };
        if !qilian::release_perst_window(&sec_acpi, mpio_cfg.perst) {
            fstart_log::error!("{}: secondary FCH{} PERST release failed", V::NAME, iod);
            return Err("secondary PERST release");
        }
    }
    ctx.clock.delay_ms(200);

    let mut lanes: Vec<(u8, PciAddress, bool), 32> = Vec::new();
    let mut bindings: Vec<Binding, 32> = Vec::new();
    let mut trained_slots: Vec<(u8, usize), 32> = Vec::new();

    // Phase 4: train links (`SETUP_LINK(TRAIN)` + `WAIT_COMPLETE(0x3a)` + `GET_ASK`),
    // re-bind by (lane, kind), and commit `LogicalBridgeId` via `UPDATE_LINK(0x322)`.
    dxio_wh::train_all(&mut sessions, timer).map_err(|_| "WH MPIO train/wait")?;
    for (iod, ctrl) in &mut sessions {
        ctrl.collect_train(timer).map_err(|_| "WH MPIO results")?;
        let (iod_bindings, link_up, logical) = {
            let asks = ctrl.asks().map_err(|_| "WH MPIO asks")?;
            let iod_bindings =
                dxio_wh::bind(layout, *iod, &ports, asks).map_err(|_| "WH MPIO train bind")?;
            let mut logical: Vec<Option<u8>, 32> = Vec::new();
            let mut link_up: Vec<bool, 32> = Vec::new();
            for ask in asks {
                let _ = logical.push(None);
                let _ = link_up.push(ask.status().read(Status::STATE) == TRAINED);
            }
            for binding in &iod_bindings {
                logical[binding.ask] = Some(binding.logical_bridge);
            }
            (iod_bindings, link_up, logical)
        };
        ctrl.update(&logical, mpio_cfg.update_link, timer)
            .map_err(|_| "WH MPIO update")?;

        for binding in &iod_bindings {
            let port_cfg = &ports[binding.port];
            let up = link_up[binding.ask];
            let rb_index = binding.iod * nbio_wh::ROOT_BRIDGES_PER_IOD + binding.rb_on_iod;
            let Some(rb) = state.roots().iter().find(|r| r.index == rb_index) else {
                return Err("WH MPIO root bridge");
            };
            let (dev, func) = (binding.devfn >> 3, binding.devfn & 7);
            let port = PciAddress::new(0, rb.bus_base, dev, func);
            fstart_log::info!(
                "{}: MPIO IOD{} RB{} core {} slot {} lanes {}-{} port {}:{}.{} {}",
                V::NAME,
                binding.iod,
                binding.rb_on_iod,
                binding.core,
                binding.pcie_bridge_id,
                port_cfg.start,
                port_cfg.end,
                port.bus(),
                port.device(),
                port.function(),
                if up { "trained" } else { "down" }
            );
            let _ = lanes.push((port_cfg.start, port, up));
            if up {
                let _ = trained_slots.push((rb_index, usize::from(binding.pcie_bridge_id)));
                let _ = bindings.push(*binding);
                // SAFETY: routed root bridge, BSP owns SMN.
                if let Some(nb) =
                    unsafe { nbio_wh::RootBridge::new(GlobalEcam, binding.rb_on_iod, rb.bus_base) }
                {
                    nb.enable_aer(binding.core);
                }
            }
        }
    }

    // Sweep every WH root bridge's PCIe bridge slots (`0..20`): keep trained
    // ports visible, preserve active PSP-trained ports ONLY on the layout's
    // verified bonus-core slots, and hide down/unused bridges so PCI scan
    // never hangs.
    let (bonus_rb, bonus_core, bonus_slot_start, bonus_slot_end) = layout.bonus_core();
    let mut psp_found: Vec<(u8, PciAddress, Binding), 4> = Vec::new();
    for rb in state.roots() {
        let iod = rb.index / nbio_wh::ROOT_BRIDGES_PER_IOD;
        let rb_on_iod = rb.index % nbio_wh::ROOT_BRIDGES_PER_IOD;
        // SAFETY: routed root bridge, BSP owns SMN.
        let Some(nb) = (unsafe { nbio_wh::RootBridge::new(GlobalEcam, rb_on_iod, rb.bus_base) })
        else {
            continue;
        };
        let bonus_lane = if rb_on_iod == bonus_rb {
            links::psp_trained_on_core(mpio_cfg.psp_trained, layout, iod, bonus_core)
        } else {
            None
        };
        for slot in usize::from(layout.first_slot())..dxio_wh::PORT_SLOTS {
            let Some((dev, func)) = layout.slot_devfn(&nb, slot as u8) else {
                continue;
            };
            if trained_slots.contains(&(rb.index, slot)) {
                nb.set_bridge_visible(slot, true);
                continue;
            }
            let is_bonus_slot = rb_on_iod == bonus_rb
                && (usize::from(bonus_slot_start)..usize::from(bonus_slot_end)).contains(&slot);
            if is_bonus_slot && let Some(lane) = bonus_lane {
                let ecam = EcamDevice::new(rb.bus_base, dev, func);
                if link_active(&ecam) {
                    let addr = PciAddress::new(0, rb.bus_base, dev, func);
                    nb.enable_aer(bonus_core);
                    nb.set_bridge_visible(slot, true);
                    let devfn = (dev << 3) | (func & 7);
                    let binding = Binding {
                        ask: 0,
                        port: 0,
                        iod,
                        rb_on_iod,
                        core: bonus_core,
                        local_port: (slot - usize::from(bonus_slot_start)) as u8,
                        pcie_bridge_id: slot as u8,
                        remap_reg: layout.remap_reg_index(slot as u8).unwrap_or(0),
                        devfn,
                        logical_bridge: slot as u8,
                    };
                    let _ = psp_found.push((lane, addr, binding));
                    continue;
                }
            }
            nb.set_bridge_visible(slot, false);
        }
    }

    for &(lane, port, binding) in &psp_found {
        if psp_found
            .iter()
            .filter(|&&(other, _, _)| other == lane)
            .count()
            == 1
        {
            fstart_log::info!(
                "{}: PSP-trained bonus lane {} at {}:{}.{} (slot {})",
                V::NAME,
                lane,
                port.bus(),
                port.device(),
                port.function(),
                binding.pcie_bridge_id
            );
            let _ = lanes.push((lane, port, true));
            let _ = bindings.push(binding);
        }
    }

    let missing = links::missing(&mpio_cfg.required, &lanes)
        .inspect(|s| {
            fstart_log::error!(
                "{}: required soldered link down on lane {}",
                V::NAME,
                s.lane
            )
        })
        .count();
    if missing != 0 {
        return Err("WH MPIO endpoints");
    }

    Ok(Trained { lanes, bindings })
}

/// True if the PCIe root port at `port` is present and reports Data Link Layer
/// Link Active (`DLL_ACTIVE`).
#[inline]
fn link_active(port: &EcamDevice) -> bool {
    use fstart_pci::config::PCI_EXPRESS_LINK_STATUS as LinkStatus;
    port.is_present()
        && port.find_capability(0x10).is_some_and(|cap| {
            tock_registers::LocalRegisterCopy::<u16, LinkStatus::Register>::new(
                port.read16(cap + 0x12),
            )
            .is_set(LinkStatus::DLL_ACTIVE)
        })
}
