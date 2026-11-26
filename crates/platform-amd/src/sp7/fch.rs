// SPDX-License-Identifier: MIT OR Apache-2.0
//! The Qilian FCH steps of SP7 at the vendor's points of the boot (FCH.md;
//! Dell PR9XJ and GIGABYTE MD25-CG0 SilVenicePei), 1P and 2P:
//!
//! | Flow step | Vendor | Here |
//! |---|---|---|
//! | `enter` | `FchInitResetHwAcpiQl` start (takeover) | `qilian::take_over` (in `init`) |
//! | `init_fch`, after the PM blocks and the SMU | HW-ACPI reset (with its WA features), the ISA step, env, `FchInitResetAbSil`, `FchAbHandleSyncFloodQl` | [`primary`] |
//! | `init_fch`, per secondary FCH found | `FchMultiFchDispatchSecondaryInits`: HwAcpi, A-Link, SD | [`secondaries`] |
//! | `init_chipset` | USB on socket 0 and socket 1's die-0 FCHs | [`usb`] |
//! | measured boot | the board's SPI TPM routing | [`route_tpm`] |
//!
//! The eSPI link and its decodes are the ABL's (APCB `EspiInit`): they are
//! logged, never reset.
//!
//! A secondary FCH that fails is reported and the boot goes on, at every
//! step, as the vendor's dispatcher (`FchMultiFchDispatchSecondaryInits`)
//! goes on past a failing one: one whose ACPI MMIO relocation does not
//! read back is left out of `State::secondary_fchs` (no init here, no PERST
//! release in `sp7::mpio`); one whose init or PERST release fails is
//! logged. What the board needs behind it is the board's to say: a
//! required soldered device (`Mpio::required`) whose link does not come up
//! stops the boot. The primary FCH's failures stop it.

use fstart_driver_amd::ccx::tsc::Tsc;
use fstart_driver_amd::df::wh as df_wh;
use fstart_driver_amd::fch::qilian::{self, espi, hwacpi, secondary};
use fstart_driver_amd::fch::{FchHw, Native};
use fstart_driver_amd::smn::Smn;
use fstart_driver_amd::window::Mmio;
use fstart_pci::ecam::GlobalEcam;

use super::init::State;
use super::{Sp7, Variant};
use crate::board::TpmBus;
use crate::flow::Ctx;

/// Route the board's TPM: on SPI through the FCH's TPM chip select; an
/// LPC/eSPI TPM is decoded at reset.
pub(super) fn route_tpm<V: Variant>(fch: &Native<Tsc>, bus: TpmBus) -> bool {
    match bus {
        TpmBus::Lpc => true,
        TpmBus::Spi => {
            let routed = qilian::tpm::route_to_spi(fch);
            if !routed {
                fstart_log::error!("{}: TPM to SPI routing locked", V::NAME);
            }
            routed
        }
    }
}

/// The primary FCH after the PM blocks: the HW-ACPI tables and interrupt
/// routes, the A-Link decode and the sync-flood reset handed to the SMU;
/// the eSPI decodes the ABL left are logged.
pub(super) fn primary<V: Variant>(ctx: &Ctx<'_, Sp7<V>>, state: &State) {
    let fch = &ctx.hw.fch;
    let devices = &ctx.config.devices;
    if hwacpi::init(fch, &devices.fch_reset, &devices.fch_misc) {
        fstart_log::warn!("{}: RTC extension invalid, rewritten", V::NAME);
    }
    // The watchdog's period, before the OS reads it from the WDAT; stopped
    // first, in case the OS before left it running across a warm reset.
    if let Some(clock) = devices.wdat
        && devices.fch_reset.watchdog_decode
        && qilian::pm::watchdog_clock(fch.acpi(), clock)
    {
        fstart_log::warn!("{}: the FCH watchdog reset the boot before", V::NAME);
    }
    // `InitializeFchIsaQlTp1`, after the HW-ACPI step, if the board asks
    // for it; no flash access is in flight (they are synchronous) and the
    // reads after it stay valid (`qilian::isa`).
    if let Some(isa) = &devices.isa {
        qilian::isa::init(fch, isa);
    }
    hwacpi::ab_decode(fch.acpi());
    hwacpi::env(fch);
    if hwacpi::sync_flood(fch, &state.smu_on(0, 0, ctx.clock), ctx.config.sync_flood).is_err() {
        fstart_log::warn!("{}: SMU sync-flood reset not set", V::NAME);
    }
    for (i, decode) in espi::decodes(fch.mmio()).iter().enumerate() {
        let Some(decode) = decode else {
            continue;
        };
        fstart_log::info!(
            "{}: eSPI{}: 2Eh {} 60h {} 80h {}",
            V::NAME,
            i,
            decode.sio_2e,
            decode.kbc,
            decode.port80
        );
        for range in decode.io.iter().flatten() {
            fstart_log::info!(
                "{}: eSPI{}: I/O {:#x}+{:#x}",
                V::NAME,
                i,
                range.base,
                range.size
            );
        }
    }
}

/// Each secondary FCH the fabric step relocated ([`State::secondary_fchs`]):
/// HwAcpi, A-Link and SD off, in the vendor's order. One that fails is
/// reported and the boot goes on, as the vendor's dispatcher does.
pub(super) fn secondaries<V: Variant>(state: &State) -> Result<(), &'static str> {
    // SAFETY: BSP owns DF node (0, 0)'s I/O routing during cold init.
    let df = unsafe { df_wh::Node::new(GlobalEcam, 0, 0) }.ok_or("DF node")?;
    // SAFETY: the BSP's ports; legacy I/O goes to one secondary at a time.
    let io = unsafe { fstart_driver_amd::window::Ports::new() };
    for &(socket, die, base) in state.secondary_fchs() {
        let Some(root) = state.node_root(socket, die) else {
            fstart_log::error!("{}: S{} IOD{} FCH has no root bus", V::NAME, socket, die);
            continue;
        };
        // SAFETY: relocated and readback-verified in `init_fch` inside the
        // root bridge's non-PCI pool (identity-mapped uncached MMIO).
        let acpi = unsafe { Mmio::new(base as usize) };
        // SAFETY: BSP owns SMN on the IOD's root bus during cold init.
        let smn = unsafe { Smn::on_root(GlobalEcam, root) };
        let policy = secondary::Policy::DEFAULT;
        match secondary::init(&df, socket, die, &io, &acpi, &smn, policy) {
            Ok(()) => fstart_log::info!(
                "{}: S{} IOD{} FCH: HwAcpi, A-Link, SD",
                V::NAME,
                socket,
                die
            ),
            Err(_) => fstart_log::error!("{}: S{} IOD{} FCH init failed", V::NAME, socket, die),
        }
    }
    Ok(())
}

/// The board's FCH I2C controllers ([`super::Devices::i2c`]) powered and
/// set up at the vendor's standard-mode timing (`FchI2cMasterPei`).
pub(super) fn i2c<V: Variant>(ctx: &Ctx<'_, Sp7<V>>) {
    for &n in ctx.config.devices.i2c {
        let Some(base) = qilian::i2c::base(n) else {
            fstart_log::error!("{}: no FCH I2C controller {}", V::NAME, n);
            continue;
        };
        qilian::i2c::enable(&ctx.hw.fch, n);
        // SAFETY: the FCH's APB window is decoded since reset; the board
        // gave the controller to firmware.
        let up = unsafe {
            fstart_driver_i2c::designware::DesignWare::new(
                base as usize,
                &qilian::i2c::STANDARD,
                ctx.clock,
            )
        };
        if up.is_err() {
            fstart_log::warn!("{}: FCH I2C {} does not answer", V::NAME, n);
        }
    }
}

/// The xHCIs of the FCHs that have them (socket 0's, socket 1's die 0),
/// through each IOD's SMU, each socket with its own policy (`policies`,
/// [`super::Config::usb`]: socket 1 has its own PHY tables and enables,
/// SilVenicePei `0xf316a0`/`0xf31950`).
pub(super) fn usb<V: Variant>(
    ctx: &Ctx<'_, Sp7<V>>,
    state: &State,
    policies: &[Option<qilian::usb::Policy>],
) {
    for &(socket, die) in state.nodes() {
        if !secondary::has_usb(socket, die) {
            continue;
        }
        let Some(policy) = policies.get(usize::from(socket)).and_then(Option::as_ref) else {
            continue;
        };
        let Some(root) = state.node_root(socket, die) else {
            continue;
        };
        let smu = state.smu_on(socket, die, ctx.clock);
        // SAFETY: BSP owns SMN on the IOD's root bus during cold init.
        let smn = unsafe { Smn::on_root(GlobalEcam, root) };
        match qilian::usb::cold_boot(policy).and_then(|mut usb| usb.run(&smn, &smu)) {
            Ok(()) => fstart_log::info!("{}: S{} xHCI initialized", V::NAME, socket),
            Err(_) => fstart_log::warn!("{}: S{} xHCI SMU init skipped/rejected", V::NAME, socket),
        }
    }
}
