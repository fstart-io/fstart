// SPDX-License-Identifier: MIT OR Apache-2.0
//! MPIO on the flow: the board's ports trained through the I/O die's one
//! MPIO controller, whose mailbox every IOS's root bus reaches. Its DMA
//! window is never freed: a failed or timed-out request does not prove the
//! firmware done with it.
extern crate alloc;

use super::init::State;
use super::{Rs, Variant, links};
use crate::flow::Ctx;
use core::num::NonZeroU32;
use fstart_driver_amd::dxio::rs::ask::{Kind, Status};
use fstart_driver_amd::dxio::rs::mailbox::Poll;
use fstart_driver_amd::dxio::rs::{ports, session, strap};
use fstart_driver_amd::nbio::rs::{enable_aer, pcie_bridges, set_bridge_visible};
use fstart_pci::{EcamDevice, PciAddress};
use heapless::Vec;

/// PCIe link state of a trained port in its ASK.
const TRAINED: u32 = 6;

/// The board's ports in PCI after training.
pub struct Trained {
    /// MPIO's ports.
    pub ports: Vec<ports::Binding, 32>,
    /// The root port of every board port by its first lane, MPIO's and the
    /// PSP's, and whether its link is up.
    pub lanes: Vec<(u8, PciAddress, bool), 32>,
}

/// Train the board's `configs` and return the ports placed in PCI.
pub fn train<V: Variant>(
    ctx: &Ctx<'_, Rs<V>>,
    state: &State,
    configs: &[session::Config<'_>],
) -> Result<Trained, &'static str> {
    use fstart_core::services::Timer;
    let mpio = &ctx.config.mpio;
    let ios: Vec<u8, 4> = state.roots().iter().map(|rb| rb.index).collect();
    let board: Vec<_, 32> = configs.iter().map(|c| c.port).collect();
    let prepared = session::Prepared::new(0, &ios, configs)
        .map_err(|_| "MPIO ports")?
        .with_psp_vetting(mpio.psp_vetting)
        .with_encoding(mpio.encoding);
    let dma = crate::memory::dma_alloc(session::DMA_SIZE, 64)?;
    let (ptr, phys) = (dma.as_mut_ptr(), dma.as_ptr() as u64);
    // SAFETY: a fresh allocation, never freed, identity mapped below 4 GiB.
    let dma = unsafe { session::Dma::new(ptr, session::DMA_SIZE, phys) }.map_err(|_| "MPIO DMA")?;
    let primary = state.primary();
    // SAFETY: the BSP owns the controller; its DMA stays reserved.
    let mut controller = unsafe {
        session::Session::new(
            *primary.smn(),
            dma,
            prepared,
            fstart_arch::x86::writeback_cache_range,
            Poll {
                reads: NonZeroU32::new(300_000).unwrap(),
                interval_us: NonZeroU32::new(100).unwrap(),
            },
            NonZeroU32::new(100_000).unwrap(),
        )
    }
    .map_err(|_| "MPIO")?;
    let timer = &ctx.clock;
    controller.submit(timer).map_err(|_| "MPIO submit")?;
    controller.advance(timer).map_err(|_| "MPIO map")?;
    let mapped = ports::bind(&ios, &board, controller.asks().map_err(|_| "MPIO")?)
        .map_err(|_| "MPIO map results")?;
    for n in state.ios() {
        ports::program(&n, n.index, &mapped);
    }
    // The AER capability on the root ports of every wrapper a port went
    // to, before configure latches the straps. The OS owns AER (`_OSC`).
    let mut wrappers: Vec<(u8, u8), 12> = Vec::new();
    for b in &mapped {
        let core = (b.ios, ports::wrapper_port(b.slot).0);
        if !wrappers.contains(&core) {
            wrappers.push(core).map_err(|_| "MPIO wrappers")?;
        }
    }
    for &(ios, wrapper) in &wrappers {
        for (index, value) in strap::AER {
            controller
                .write_strap(ios, wrapper, index, value, timer)
                .map_err(|_| "MPIO straps")?;
        }
    }
    controller.advance(timer).map_err(|_| "MPIO configure")?;
    controller.advance(timer).map_err(|_| "MPIO PERST")?;
    if !fstart_driver_amd::fch::songshan::release_perst(&ctx.hw.fch, mpio.perst) {
        return Err("PERST");
    }
    ctx.clock.delay_ms(200);
    controller.advance(timer).map_err(|_| "MPIO train")?;
    let asks = controller.asks().map_err(|_| "MPIO")?;
    let trained = ports::bind(&ios, &board, asks).map_err(|_| "MPIO train results")?;
    let mut logical: Vec<Option<u8>, 32> = asks.iter().map(|_| None).collect();
    let mut lanes: Vec<(u8, PciAddress, bool), 32> = Vec::new();
    for b in &trained {
        logical[b.ask] = Some(b.logical_bridge);
        let up = asks[b.ask].status().read(Status::STATE) == TRAINED;
        let rb = state
            .roots()
            .iter()
            .find(|rb| rb.index == b.ios)
            .ok_or("IOS")?;
        let (device, function) = fstart_pci::devfn_parts(b.devfn);
        let port = PciAddress::new(0, rb.bus_base, device, function);
        fstart_log::info!(
            "rs: MPIO lanes {}-{} IOS {} devfn {:#x} {}",
            board[b.port].start,
            board[b.port].end,
            b.ios,
            b.devfn,
            if up { "trained" } else { "down" }
        );
        lanes
            .push((board[b.port].start, port, up))
            .map_err(|_| "MPIO ports")?;
    }
    let asks: alloc::vec::Vec<_> = asks
        .iter()
        .map(|ask| (ask.status().read(Status::STATE), ask.kind()))
        .collect();
    controller
        .update(&logical, timer)
        .map_err(|_| "MPIO update")?;
    // Only the bridges of ports with a link stay visible: a configuration
    // request to a port whose link and core are down never completes. A
    // port with an active link MPIO did not train (a PSP-trained BMC link)
    // stays too, and is the board's PSP-trained link on its wrapper.
    let mut psp: Vec<(u8, PciAddress), 4> = Vec::new();
    for n in state.ios() {
        let rb = state
            .roots()
            .iter()
            .find(|rb| rb.index == n.index)
            .ok_or("IOS")?;
        for lbid in 0..pcie_bridges(n.index) {
            let ours = trained
                .iter()
                .find(|b| b.ios == n.index && b.logical_bridge == lbid);
            let visible = match ours {
                Some(b) => asks[b.ask] == (TRAINED, Some(Kind::Pcie)),
                None => {
                    let devfn = ports::NATIVE_DEVFNS[usize::from(lbid)];
                    let port = EcamDevice::new(rb.bus_base, devfn >> 3, devfn & 7);
                    let up = link_active(&port);
                    // MPIO's straps missed its core.
                    if up {
                        enable_aer(&n, ports::wrapper_port(lbid).0);
                        let wrapper = links::wrapper(lbid);
                        if let Some(lane) =
                            links::psp_trained(&ios, mpio.psp_trained, n.index, wrapper)
                        {
                            psp.push((lane, port.address())).map_err(|_| "MPIO ports")?;
                        }
                    }
                    up
                }
            };
            set_bridge_visible(&n, lbid, visible);
        }
    }
    // Two live ports on one PSP-trained link's wrapper: neither is known
    // to be its.
    for &(lane, port) in &psp {
        if psp.iter().filter(|&&(other, _)| other == lane).count() == 1 {
            fstart_log::info!(
                "rs: PSP-trained lane {} at {}:{}.{}",
                lane,
                port.bus(),
                port.device(),
                port.function()
            );
            lanes.push((lane, port, true)).map_err(|_| "MPIO ports")?;
        }
    }
    let missing = links::missing(&mpio.required, &lanes)
        .inspect(|soldered| fstart_log::error!("rs: no link on lane {}", soldered.lane))
        .count();
    if missing != 0 {
        return Err("MPIO endpoints");
    }
    Ok(Trained {
        ports: trained,
        lanes,
    })
}

/// The PCIe link of `port` is up (Data Link Layer Link Active).
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
