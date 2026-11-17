// SPDX-License-Identifier: MIT OR Apache-2.0
//! MPIO on the flow: the board's ports trained through the I/O die's one
//! MPIO controller, whose mailbox every IOS's root bus reaches. Its DMA
//! window is never freed: a failed or timed-out request does not prove the
//! firmware done with it.
extern crate alloc;

use super::init::State;
use super::{Rs, Variant};
use crate::flow::Ctx;
use core::num::NonZeroU32;
use fstart_driver_amd::dxio::rs::ask::{Kind, Status};
use fstart_driver_amd::dxio::rs::mailbox::Poll;
use fstart_driver_amd::dxio::rs::{ports, session};
use fstart_driver_amd::nbio::rs::{pcie_bridges, set_bridge_visible};
use fstart_pci::{EcamDevice, ecam::GlobalEcam};
use heapless::Vec;

/// PCIe link state of a trained port in its ASK.
const TRAINED: u32 = 6;

/// Train the board's `configs` and return the ports placed in PCI.
pub fn train<V: Variant>(
    ctx: &Ctx<'_, Rs<V>>,
    state: &State,
    configs: &[session::Config<'_>],
) -> Result<Vec<ports::Binding, 32>, &'static str> {
    use fstart_core::services::Timer;
    let mpio = ctx.config.mpio;
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
    let mut delay = ctx.clock;
    controller.submit(&mut delay).map_err(|_| "MPIO submit")?;
    controller.advance(&mut delay).map_err(|_| "MPIO map")?;
    let mapped = ports::bind(&ios, &board, controller.asks().map_err(|_| "MPIO")?)
        .map_err(|_| "MPIO map results")?;
    for n in state.ios() {
        ports::program(&n, n.index, &mapped);
    }
    controller
        .advance(&mut delay)
        .map_err(|_| "MPIO configure")?;
    controller.advance(&mut delay).map_err(|_| "MPIO PERST")?;
    if !fstart_driver_amd::fch::songshan::release_perst(&ctx.hw.fch) {
        return Err("PERST");
    }
    ctx.clock.delay_ms(200);
    controller.advance(&mut delay).map_err(|_| "MPIO train")?;
    let asks = controller.asks().map_err(|_| "MPIO")?;
    let trained = ports::bind(&ios, &board, asks).map_err(|_| "MPIO train results")?;
    let mut logical: Vec<Option<u8>, 32> = asks.iter().map(|_| None).collect();
    let mut required = 0;
    for b in &trained {
        logical[b.ask] = Some(b.logical_bridge);
        let up = asks[b.ask].status().read(Status::STATE) == TRAINED;
        let rb = state
            .roots()
            .iter()
            .find(|rb| rb.index == b.ios)
            .ok_or("IOS")?;
        let port = EcamDevice::new(rb.bus_base, b.devfn >> 3, b.devfn & 7);
        fstart_log::info!(
            "rs: MPIO lanes {}-{} IOS {} devfn {:#x} {}",
            board[b.port].start,
            board[b.port].end,
            b.ios,
            b.devfn,
            if up { "trained" } else { "down" }
        );
        if up
            && let Some(vendor) = endpoint(&port, rb.bus_limit)
            && mpio
                .required
                .iter()
                .any(|&(lane, v)| lane == board[b.port].start && v == vendor)
        {
            required += 1;
        }
    }
    let asks: alloc::vec::Vec<_> = asks
        .iter()
        .map(|ask| (ask.status().read(Status::STATE), ask.kind()))
        .collect();
    controller
        .update(&logical, &mut delay)
        .map_err(|_| "MPIO update")?;
    // Only the bridges of ports with a link stay visible: a configuration
    // request to a port whose link and core are down never completes. A
    // port with an active link MPIO did not train (a PSP-trained BMC link)
    // stays too.
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
                None => link_active(&EcamDevice::new(
                    rb.bus_base,
                    ports::NATIVE_DEVFNS[usize::from(lbid)] >> 3,
                    ports::NATIVE_DEVFNS[usize::from(lbid)] & 7,
                )),
            };
            set_bridge_visible(&n, lbid, visible);
        }
    }
    if required != mpio.required.len() {
        fstart_log::error!(
            "rs: {} of the board's {} soldered endpoints are there",
            required,
            mpio.required.len()
        );
        return Err("MPIO endpoints");
    }
    Ok(trained)
}

/// The PCI vendor of the device behind root port `port`, through a free
/// bus of its root (`last`) for the probe.
#[inline]
fn endpoint(port: &EcamDevice, last: u8) -> Option<u16> {
    // SAFETY: a root port with DMA off; the probe restores its buses.
    unsafe { fstart_pci::register::probe_direct_child(&GlobalEcam, port.address(), last) }
        .ok()
        .flatten()
        .map(|id| id as u16)
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
