// SPDX-License-Identifier: MIT OR Apache-2.0
//! WH dual-IOD MPIO v2 link training on SP7:
//! - Splits non-PSP-trained board lanes across live local IODs (`0` and `1`).
//! - Runs the 4-phase WH MPIO v2 session (`DOORBELL_V2 = 0x0C91_0954`,
//!   `Encoding::LowByte`) on each IOD's primary root bus, releasing board
//!   PERST# between the PERST and train phases.
//! - Probes active root ports (including PSP-trained links such as the AST2700
//!   BMC VGA link on lane 129), enables AER on active cores, and hides
//!   inactive IOHC bridges (`BRIDGE_DIS | CFG_DIS | CRS_ENABLE`).

extern crate alloc;

use core::num::NonZeroU32;

use fstart_core::services::Timer;
use fstart_driver_amd::dxio::rs::ask::{LinkSpeed, Port, Status};
use fstart_driver_amd::dxio::rs::mailbox::Poll;
use fstart_driver_amd::dxio::rs::session;
use fstart_driver_amd::dxio::wh as dxio_wh;
use fstart_driver_amd::fch::qilian;
use fstart_driver_amd::nbio::wh as nbio_wh;
use fstart_driver_amd::smn::Smn;
use fstart_pci::{EcamDevice, PciAddress, ecam::GlobalEcam};
use heapless::Vec;

use super::init::State;
use super::{LaneDescriptor, Sp7, Variant, links};
use crate::flow::Ctx;

/// PCIe link state of a trained port in its returned MPIO ASK.
const TRAINED: u32 = 6;

/// Trained WH root ports and lane-to-port associations.
pub struct Trained {
    /// `(first_socket_lane, root_port_bdf, link_up)` for trained & PSP-trained ports.
    pub lanes: Vec<(u8, PciAddress, bool), 32>,
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
/// the PSP/ABL) into MPIO v2 session port configs.
fn build_configs(descriptors: &[LaneDescriptor]) -> Vec<session::Config<'static>, 32> {
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
        let _ = configs.push(session::Config {
            port,
            phy: &[],
            speed: &[],
        });
    }
    configs
}

/// Run WH dual-IOD MPIO v2 link training across every live local IOD, release
/// PERST#, enable AER, hide inactive bridges, and match PSP-trained links.
pub fn train<V: Variant>(
    ctx: &Ctx<'_, Sp7<V>>,
    state: &State,
    descriptors: &[LaneDescriptor],
) -> Result<Trained, &'static str> {
    let mpio_cfg = &ctx.config.mpio;
    let configs = build_configs(descriptors);
    let timer = &ctx.clock;
    let poll = Poll {
        reads: const { NonZeroU32::new(300_000).unwrap() },
        interval_us: const { NonZeroU32::new(100).unwrap() },
    };
    let idle_reads = const { NonZeroU32::new(100_000).unwrap() };

    let mut sessions: Vec<(u8, session::Session<Smn<GlobalEcam>>, Vec<usize, 32>), 2> = Vec::new();
    for &iod in state.dies() {
        let Some((prepared, board_indices)) =
            dxio_wh::prepare_iod(iod, &configs).map_err(|_| "WH MPIO ports")?
        else {
            continue;
        };
        let bus = state.die_bus(iod).ok_or("WH MPIO die bus")?;
        let dma_buf = crate::memory::dma_alloc(session::DMA_SIZE, 64)?;
        let (ptr, phys) = (dma_buf.as_mut_ptr(), dma_buf.as_ptr() as u64);
        // SAFETY: fresh DMA allocation, permanently reserved, identity-mapped below 4 GiB.
        let dma = unsafe { session::Dma::new(ptr, session::DMA_SIZE, phys) }
            .map_err(|_| "WH MPIO DMA")?;
        // SAFETY: BSP owns MPIO v2 mailbox on `bus` for this IOD.
        let smn = unsafe { Smn::on_bus(GlobalEcam, bus) };
        let mut ctrl = unsafe {
            session::Session::new(
                smn,
                dma,
                prepared,
                fstart_arch::x86::writeback_cache_range,
                poll,
                idle_reads,
            )
        }
        .map_err(|_| "WH MPIO session")?;
        // Phases 1..3: submit ASK/ancillary, map, configure, PERST prepare.
        ctrl.submit(timer).map_err(|_| "WH MPIO submit")?;
        ctrl.advance(timer).map_err(|_| "WH MPIO map")?;
        ctrl.advance(timer).map_err(|_| "WH MPIO configure")?;
        ctrl.advance(timer).map_err(|_| "WH MPIO PERST")?;
        let _ = sessions.push((iod, ctrl, board_indices));
    }

    // Release board PCIe slot PERST# and wait 200 ms for endpoint reset recovery.
    if !qilian::release_perst(&ctx.hw.fch, mpio_cfg.perst) {
        return Err("PERST release");
    }
    ctx.clock.delay_ms(200);

    let mut lanes: Vec<(u8, PciAddress, bool), 32> = Vec::new();
    let mut trained_bdfs: Vec<PciAddress, 32> = Vec::new();

    // Phase 4: train links and update firmware on each IOD.
    for (iod, ctrl, board_indices) in &mut sessions {
        ctrl.advance(timer).map_err(|_| "WH MPIO train")?;
        let asks = ctrl.asks().map_err(|_| "WH MPIO asks")?;
        let mut logical: Vec<Option<u8>, 32> = Vec::new();
        for (ask_idx, ask) in asks.iter().enumerate() {
            let cfg = &configs[board_indices[ask_idx]];
            let up = ask.status().read(Status::STATE) == TRAINED;
            let local_port = ask.status().read(Status::LOCAL_PORT_ID) as u8;
            let _ = logical.push(Some(local_port));
            // Resolve the root bridge and programmed DevFn for this port.
            if let Ok((port_iod, map)) =
                dxio_wh::translate_socket_lane(cfg.port.start, cfg.port.end)
                && port_iod == *iod
            {
                let rb_index = (*iod) * nbio_wh::ROOT_BRIDGES_PER_IOD + map.root_index;
                if let Some(rb) = state.roots().iter().find(|r| r.index == rb_index) {
                    // SAFETY: routed root bridge, BSP owns SMN.
                    if let Some(nb) = unsafe {
                        nbio_wh::RootBridge::new(
                            GlobalEcam,
                            rb.index % nbio_wh::ROOT_BRIDGES_PER_IOD,
                            rb.bus_base,
                        )
                    } {
                        let (dev, func) = nb.bridge_devfn(usize::from(local_port));
                        let port = PciAddress::new(0, rb.bus_base, dev, func);
                        fstart_log::info!(
                            "{}: MPIO IOD{} lanes {}-{} port {}:{}.{} {}",
                            V::NAME,
                            iod,
                            cfg.port.start,
                            cfg.port.end,
                            port.bus(),
                            port.device(),
                            port.function(),
                            if up { "trained" } else { "down" }
                        );
                        let _ = lanes.push((cfg.port.start, port, up));
                        if up {
                            let _ = trained_bdfs.push(port);
                            nb.enable_aer(map.wrapper);
                        }
                    }
                }
            }
        }
        let _ = ctrl.update(&logical, timer);
    }

    // Sweep every WH root bridge's IOHC slots: keep live ports (including
    // PSP-trained links such as the AST2700 BMC VGA link) visible, enable AER
    // on PSP-trained cores, and hide down bridges so PCI scan never hangs.
    let mut psp_found: Vec<(u8, PciAddress), 4> = Vec::new();
    for rb in state.roots() {
        let iod = rb.index / nbio_wh::ROOT_BRIDGES_PER_IOD;
        let rb_on_iod = rb.index % nbio_wh::ROOT_BRIDGES_PER_IOD;
        // SAFETY: routed root bridge, BSP owns SMN.
        let Some(nb) = (unsafe { nbio_wh::RootBridge::new(GlobalEcam, rb_on_iod, rb.bus_base) })
        else {
            continue;
        };
        for slot in 0..usize::from(nbio_wh::PCIE_BRIDGES_PER_RB) {
            let (dev, func) = nb.bridge_devfn(slot);
            if dev == 0 && func == 0 {
                continue;
            }
            let addr = PciAddress::new(0, rb.bus_base, dev, func);
            let ecam = EcamDevice::new(rb.bus_base, dev, func);
            let active = trained_bdfs.contains(&addr) || link_active(&ecam);
            if active && !trained_bdfs.contains(&addr) {
                // Active link not trained by MPIO -> PSP-trained (bonus core 4 on RB0).
                if rb_on_iod == 0 {
                    nb.enable_aer(4);
                    if let Some(lane) = links::psp_trained_on_core(mpio_cfg.psp_trained, iod, 4) {
                        let _ = psp_found.push((lane, addr));
                    }
                }
            }
            nb.set_bridge_visible(slot, active);
        }
    }

    for &(lane, port) in &psp_found {
        if psp_found
            .iter()
            .filter(|&&(other, _)| other == lane)
            .count()
            == 1
        {
            fstart_log::info!(
                "{}: PSP-trained lane {} at {}:{}.{}",
                V::NAME,
                lane,
                port.bus(),
                port.device(),
                port.function()
            );
            let _ = lanes.push((lane, port, true));
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

    Ok(Trained { lanes })
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
