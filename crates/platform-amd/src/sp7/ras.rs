// SPDX-License-Identifier: MIT OR Apache-2.0
//! SP7's platform RAS beyond the MCA banks (DEVICES.md §2.12):
//!
//! - [`previous_boot`]: the last boot's errors that only the platform kept,
//!   for the BERT, as the vendors' AmdPlatformRasWhDxe finds them: a
//!   sync-flood reset in the S5 reset status (`PMxC0[27]`, Dell `0xf780`),
//!   the break events of every FCH (`PMx80[11:0]`, read and cleared, Dell
//!   `0x1eed0`), the "sync flood from pin" of every IOHC (`0x13B2_0800[2]`,
//!   GIGABYTE `0x9888`), read before NBIO's RAS setup clears it, and the
//!   PSP's S5-domain data (GetConfig `0x5C`, ID `0x19`,
//!   `psp::s5ras`): an entry with an error is a record of its own (the
//!   vendor makes accelerator SMN errors of those on sockets above 0 and
//!   no CPU record; fstart records every entry with an error, raw).
//! - [`aer_paths`]: `NbioPcieAerReportingPathExt` on every trained PCIe
//!   port before SMU message 9 announces the mechanism; with firmware-first
//!   (FIRMWARE_FIRST.md) the AER setup of every port and function below it
//!   and of the CXL root ports' RAS capability (`smm::aer::configure`, the
//!   vendor's `PlatformPcieAerEnablement`), then the IOHCs' RAS SMI.
//! - [`firmware_first`]: the GHES blocks the SMM handler fills and the HEST
//!   describes, in the firmware's reserved memory: the corrected errors'
//!   notified by SCI on event [`SCI_EVENT`] (GPE 14h, the AMD CPM's
//!   `_L14`), the uncorrected ones' by NMI (both donors; GIGABYTE's PCD
//!   `PcdCpmPcieUnCorrGhesNotifyType` 1, Dell `DellApeiLoggingSmm`).
//! - [`aer`]: the one AER mechanism in effect, which every consumer takes:
//!   the ports' routing ([`aer_paths`]), SMU message 9
//!   (`nbio::ready_to_boot`), the HEST/GHES and the DSDT's `AERR`/`_L14`
//!   and every `_OSC`, PCI and CXL ([`firmware_first`], `tables`), the
//!   AER and CXL RAS setup of the ports ([`aer_paths`]) and the SMM
//!   handler's sources (`init::install_smm`). The NBIO policy's, but
//!   OS-first when it asks for firmware-first and the SMM handler cannot
//!   reach the ECAM window (`smm::smm_reaches`): a split, the hardware
//!   routed to an SMI nobody handles while the OS is granted AER, loses
//!   the errors on both sides.
//! - [`smm_root_bridges`] (`smm::smm_reach`): what the handler reaches:
//!   the ECAM window, mapped in its page tables when above 4 GiB, and each
//!   root bridge's PCI windows, the only places it reaches a CXL register
//!   block in (`smm::aer::Reach`); the boot's setup of the CXL ports'
//!   RAS capability goes through the same check.

extern crate alloc;

use alloc::vec::Vec;

use fstart_acpi::cper::{Entry, Severity};
use fstart_driver_amd::fch::FchHw;
use fstart_driver_amd::fch::qilian::pm;
use fstart_driver_amd::window::Mmio;
use fstart_pci::PciWindowKind;
use fstart_smm::Region;

use super::fabric;
use super::init::State;
use super::nbio::{part, rb_root, root_bridge};
pub(super) use super::smm::Aer;
use super::smm::{RasRootBridge, SmmReach, smm_reach, smm_reaches};
use super::{Sp7, Variant};
use crate::acpi::apei::{PlatformEvent, platform_error};
use crate::flow::Ctx;
use crate::smm::aer::{FirmwareFirst, Ghes, notify};

/// The last boot's platform errors (each its BERT entries), worst first.
pub(super) fn previous_boot<V: Variant>(
    ctx: &Ctx<'_, Sp7<V>>,
    state: &mut State,
) -> Vec<Vec<Entry>> {
    let mut errors = s5_domain::<V>(ctx, state);
    let status = ctx.hw.boot.last.status;
    if ctx.hw.boot.last.sync_flood() {
        fstart_log::error!("{}: the last reset was a sync flood", V::NAME);
        errors.push(platform_error(
            Severity::Fatal,
            PlatformEvent::SyncFloodReset,
            0,
            0xff,
            status.into(),
        ));
    }
    let part = part();
    for rb in state.roots() {
        let Some(nb) = root_bridge(part, rb.index, rb_root(rb)) else {
            continue;
        };
        if nb.sync_flood_from_pin() {
            fstart_log::error!(
                "{}: S{} RB{}: sync flood from pin",
                V::NAME,
                rb.socket,
                rb.index
            );
            errors.push(platform_error(
                Severity::Fatal,
                PlatformEvent::IohcSyncFloodPin,
                rb.socket,
                rb.index,
                1 << 2,
            ));
        }
    }
    let mut fch = |socket: u8, die: u8, events: u16| {
        if events != 0 {
            fstart_log::error!(
                "{}: S{} IOD{} FCH break events {:#x}",
                V::NAME,
                socket,
                die,
                events
            );
            errors.push(platform_error(
                Severity::Recoverable,
                PlatformEvent::FchBreakEvent,
                socket,
                die,
                events.into(),
            ));
        }
    };
    fch(0, 0, pm::take_break_events(ctx.hw.fch.acpi()));
    for &(socket, die, base) in state.secondary_fchs() {
        // SAFETY: relocated and readback-verified in `init_fch` inside the
        // root bridge's non-PCI pool (identity-mapped uncached MMIO).
        let acpi = unsafe { Mmio::new(base as usize) };
        fch(socket, die, pm::take_break_events(&acpi));
    }
    errors
}

/// The PSP's S5-domain RAS entries with an error, each a record.
fn s5_domain<V: Variant>(ctx: &Ctx<'_, Sp7<V>>, state: &mut State) -> Vec<Vec<Entry>> {
    use fstart_driver_amd::psp::s5ras::{DATA_SIZE, GetConfig, parse};
    let mut errors = Vec::new();
    let Some(psp) = state.psp_mut() else {
        return errors;
    };
    let Ok(buffer) = crate::memory::dma_alloc(DATA_SIZE as usize, 64) else {
        return errors;
    };
    let address = buffer.as_ptr() as u64;
    // SAFETY: ring 0; the zeroed buffer goes to DRAM before the PSP
    // writes it, and what the CPU caches of it is dropped after.
    unsafe { fstart_arch::x86::writeback_invalidate_caches() };
    if let Err(error) = psp.send_unchecked(&ctx.clock, GetConfig::s5_domain_ras(address)) {
        let (why, value) = error.parts();
        fstart_log::warn!("{}: PSP S5 RAS data: {} {:#x}", V::NAME, why, value);
        return errors;
    }
    // SAFETY: as above.
    unsafe { fstart_arch::x86::writeback_invalidate_caches() };
    let (version, entries) = parse(buffer);
    for e in entries.iter().filter(|e| e.has_error()) {
        fstart_log::error!(
            "{}: PSP S5 RAS data v{}: S{} die {} {:#x}",
            V::NAME,
            version,
            e.socket,
            e.die,
            e.ras_data[0]
        );
        errors.push(platform_error(
            Severity::Recoverable,
            PlatformEvent::S5DomainRas,
            e.socket as u8,
            e.die as u8,
            u64::from(e.ras_data[0]) | u64::from(e.ras_data[1]) << 32,
        ));
    }
    errors
}

/// The software SCI event of the corrected errors' GHES: 20, GPE 14h, the
/// AMD CPM's (GIGABYTE AmdPlatformRasWhSmm `0x5a13` [I], CPMRAS `_L14`;
/// Dell uses 22 and its own `_L16`).
pub const SCI_EVENT: u8 = 20;

/// Bytes of each GHES's error status block: the vendors' 0x1000.
pub const BLOCK_SIZE: usize = 0x1000;

/// The GHES memory: the two error status addresses (qwords holding the
/// blocks' addresses) in the first page, the corrected and the uncorrected
/// block in the next two.
#[repr(C, align(4096))]
struct GhesRegion([u8; 3 * BLOCK_SIZE]);

static mut GHES: GhesRegion = GhesRegion([0; 3 * BLOCK_SIZE]);
static GHES_READY: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// The AER mechanism in effect ([`Aer::decide`]), the one source of every
/// AER consumer (module doc).
pub(super) fn aer<V: Variant>(ctx: &Ctx<'_, Sp7<V>>) -> Aer {
    static LOGGED: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
    let policy = &ctx.config.nbio;
    let aer = Aer::decide(policy, smm_reaches(&ctx.hw.ecam));
    if aer.mechanism != policy.aer_mechanism
        && !LOGGED.swap(true, core::sync::atomic::Ordering::Relaxed)
    {
        fstart_log::error!(
            "{}: firmware-first AER configured but the SMM cannot reach the ECAM at {:#x}: OS-first",
            V::NAME,
            ctx.hw.ecam.base
        );
    }
    aer
}

/// The firmware-first error sources, if the AER mechanism in effect is
/// firmware-first ([`aer`]); [`FirmwareFirst::NONE`] otherwise. The first
/// call sets the blocks up (empty, their addresses in the status address
/// qwords); they live in the firmware's reserved memory, as the BERT's
/// region.
pub(super) fn firmware_first<V: Variant>(ctx: &Ctx<'_, Sp7<V>>) -> FirmwareFirst {
    if !aer(ctx).firmware_first() {
        return FirmwareFirst::NONE;
    }
    let region = &raw mut GHES;
    let base = region as u64;
    let ghes = |n: u64, notify| Ghes {
        status_address: base + 8 * n,
        block: base + (n + 1) * BLOCK_SIZE as u64,
        length: BLOCK_SIZE as u32,
        notify,
        _reserved: [0; 3],
    };
    let ff = FirmwareFirst {
        corrected: ghes(0, notify::SCI),
        uncorrected: ghes(1, notify::NMI),
        sci_event: SCI_EVENT,
        _reserved: [0; 7],
    };
    if !GHES_READY.swap(true, core::sync::atomic::Ordering::AcqRel) {
        // SAFETY: the BSP, once, before the SMM handler or the OS use it.
        let bytes = unsafe { &mut (*region).0 };
        bytes.fill(0);
        for g in [&ff.corrected, &ff.uncorrected] {
            let at = (g.status_address - base) as usize;
            bytes[at..at + 8].copy_from_slice(&g.block.to_le_bytes());
        }
    }
    ff
}

/// [`smm_reach`] of the root bridges whose IOHC RAS SMI the SMM handler
/// serves, with their PCI roots of the DF plan.
pub(super) fn smm_root_bridges<V: Variant>(ctx: &Ctx<'_, Sp7<V>>, state: &State) -> SmmReach {
    use crate::soc::Init;
    let ecam = Region::new(ctx.hw.ecam.base, ctx.hw.ecam.size());
    let roots = Sp7::<V>::fabric(ctx, state).roots().unwrap_or_default();
    let layout = part().layout();
    let entries = state.roots().iter().map(|rb| {
        let (_, rb_on_iod) = fabric::rb_location(rb.index);
        let entry = RasRootBridge {
            segment: rb.segment,
            bus: rb.bus_base,
            iohc: layout.iohc(rb_on_iod),
            ..RasRootBridge::default()
        };
        let windows = roots
            .iter()
            .find(|root| {
                let info = root.info();
                (info.segment, info.bus_start) == (rb.segment, rb.bus_base)
            })
            .map_or(&[][..], |root| root.allocatable());
        (entry, windows)
    });
    let reach = smm_reach(ecam, entries.clone());
    let windows: usize = entries
        .map(|(_, w)| w.iter().filter(|w| w.kind == PciWindowKind::Mmio).count())
        .sum();
    if usize::from(reach.window_count) < windows {
        fstart_log::warn!(
            "{}: SMM reaches {} of {} PCI windows for CXL RAS",
            V::NAME,
            reach.window_count,
            windows
        );
    }
    reach
}

/// Firmware-first at ready-to-boot, after enumeration: the AER setup of
/// every root port and every function below it (`smm::aer::configure`,
/// its CXL RAS blocks reached as the handler reaches them), then each
/// IOHC's RAS SMI on.
fn firmware_first_ports<V: Variant>(ctx: &Ctx<'_, Sp7<V>>, state: &State) {
    if !firmware_first(ctx).enabled() {
        return;
    }
    // SAFETY: the BSP owns config space at ready-to-boot.
    let errors = unsafe { fstart_pci::aer::Errors::new(fstart_pci::ecam::GlobalEcam) };
    let part = part();
    let reach = smm_root_bridges(ctx, state);
    let tseg = Region::new(ctx.memory.tseg, super::TSEG_SIZE.into());
    let mut ports = 0u32;
    for (rb, entry) in state.roots().iter().zip(&reach.root_bridges) {
        let root = rb_root(rb);
        let blocks = crate::smm::aer::Reach {
            windows: reach.windows_of(entry),
            protected: &[tseg],
        };
        crate::smm::aer::root_ports(&errors, root, |port| {
            crate::smm::aer::configure(&errors, port, |base| blocks.block(base));
            ports += 1;
        });
        if let Some(nb) = root_bridge(part, rb.index, root) {
            nb.enable_ras_smi();
        }
    }
    fstart_log::info!("{}: firmware-first AER on {} root ports", V::NAME, ports);
}

/// The AER routing of every trained PCIe port for the mechanism in effect
/// ([`aer`]).
pub(super) fn aer_paths<V: Variant>(ctx: &Ctx<'_, Sp7<V>>, state: &State) {
    firmware_first_ports(ctx, state);
    let Some(path) = aer(ctx).path() else {
        return;
    };
    let part = part();
    let a0 = super::mpio::layout().is_a0();
    for (socket, b) in state.bindings() {
        let index = fabric::rb_index(b.iod, b.rb_on_iod);
        let Some(rb) = state
            .roots()
            .iter()
            .find(|r| (r.socket, r.index) == (*socket, index))
        else {
            continue;
        };
        if let Some(nb) = root_bridge(part, rb.index, rb_root(rb)) {
            nb.aer_reporting_path(b.core, b.local_port, a0, &path);
        }
    }
}
