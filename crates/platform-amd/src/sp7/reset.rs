// SPDX-License-Identifier: MIT OR Apache-2.0
//! Resets on the SP7 SoCs (reports/research/WARM_RESET.md): what the last
//! one was, the warm resets the firmware itself asks for, and the boot
//! steps that differ after a warm reset.
//!
//! Every reset the SP7 flow makes is one of Qilian's recipes
//! ([`fstart_driver_amd::fch::qilian::reset`]): a firmware warm reset goes
//! through [`firmware_warm_reset`], which the guard bounds; the SMM S5
//! fallback is Qilian's full reset (`qilian::sx`). The sync-flood reset
//! is the SMU's (`hwacpi::sync_flood`), with no x86 step. The OS resets
//! through the FADT's CF9h 06h with PMx10\[1\] clear (a warm reset). The
//! payload's reset service resets as the vendor's ResetSystem does
//! ([`SYSTEM_RESET`], WARM_RESET G15): a cold reset is Qilian's full one
//! (PMx10\[1\] set, CF9h 0Eh), a warm one Qilian's warm one, both while
//! the payload runs on its boot services ([`payload_reset`]) and from its
//! run-time services after the OS took over (the same registers through
//! the ACPI-MMIO page, `RuntimeReset::AmdFch`).
//!
//! | Step | Here |
//! |---|---|
//! | `enter`, before the takeover | [`Boot::read`]: PMxC0, SLP_TYP and the guard's record |
//! | `identify` | [`log`]: the last reset, a firmware warm-reset series in progress |
//! | `init_fch`, APOB | [`log_apob`]: APOB `7/3`, `1/0x10 +0x4D` |
//! | `init_fch`, PSP BAR | [`psp`]: C2P recovery (POST 2Dh, halt), ResetRequired on an A/B flash (guarded warm reset) |
//! | `init_fch`, after the SMU | [`nvm_self_refresh`]: SMU `0x4C` when the DRAM survived the warm reset |
//! | `ready_to_boot` | [`ready_to_boot`]: CMOS bank 1 06h/07h = 55h (GIGABYTE's APCB-recovery handshake) |
//! | `boot_done`, after BootDone | [`boot_done`]: the guard's record erased |
//!
//! Nothing else differs on a warm boot: neither donor's x86 code has a
//! warm branch in CCX, DF, NBIO, MPIO, FCH, USB, IOMMU, SMM or the PSP's
//! SmmInfo/LockDFReg/SmmLock/BootDone (WARM_RESET §2.2); the ABL trains
//! memory again (fstart never saves the APOB for a context restore) and
//! the flow runs as after power-on. The FCH boot timer the vendor enables
//! (`PMx47`/`PMx93`) stays off: nothing in either donor's later code was
//! found to stop it, and an FCH-driven reset of a boot that runs long
//! would be a second reset loop the guard cannot see (WARM_RESET G16).
//!
//! POST codes: `2D` the PSP runs its recovery firmware (the boot halts),
//! `2E` a firmware warm reset the guard refused (the boot goes on), `2F`
//! a firmware warm reset.

use fstart_driver_amd::apob::v2::ApobV2;
use fstart_driver_amd::ccx::tsc::Tsc;
use fstart_driver_amd::fch::qilian::reset::{self as ql, Guard, LastReset, ResetKind};
use fstart_driver_amd::fch::qilian::scratch::{self, ApcbHandshake};
use fstart_driver_amd::fch::{FchHw, Native, post};
use fstart_driver_amd::psp::{self, C2pTransport, mailbox::C2P_CMD};
use fstart_driver_amd::smu::{SmuMessages, v14 as smu_v14};

use super::init::{Hw, State};
use super::{Sp7, Variant};
use crate::flow::Ctx;

pub use ql::Reason;

/// The payload's resets ([module documentation](self)).
pub const SYSTEM_RESET: fstart_core::description::SystemReset =
    fstart_core::description::SystemReset {
        reset: payload_reset,
        runtime: fstart_core::description::RuntimeReset::AmdFch {
            acpi_mmio: fstart_driver_amd::fch::acpimmio::BASE,
        },
    };

/// A reset the payload asks for: Qilian's full reset for a cold one (the
/// vendor's DXE sets PMx10\[1\] and writes CF9h 0Eh: GIGABYTE always,
/// Dell with `PcdFchFullHardReset` set, as it ships), its warm reset for a
/// warm one. The guard is not armed: it bounds the firmware's own resets,
/// and the PSP's BootDone erased its record.
pub fn payload_reset(request: fstart_core::description::ResetRequest) -> ! {
    use fstart_core::description::ResetRequest;
    use fstart_driver_amd::window::{Mmio, Ports};
    let kind = match request {
        ResetRequest::Cold => ResetKind::Full,
        ResetRequest::Warm => ResetKind::Warm,
    };
    // SAFETY: the payload runs on the BSP with FCH 0's ACPI-MMIO block and
    // its ports decoded and identity-mapped uncached, as the flow left
    // them; nothing runs after the reset.
    let (acpi, io) = unsafe {
        (
            Mmio::new(fstart_driver_amd::fch::acpimmio::BASE as usize),
            Ports::new(),
        )
    };
    ql::reset(&acpi, &io, kind)
}

/// What the boot found before fstart wrote the FCH.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Boot {
    pub last: LastReset,
    pub guard: Guard,
}

impl Boot {
    /// PMxC0 and SLP_TYP before `take_over` and the HW-ACPI tables write
    /// them, and the guard's record.
    pub(super) fn read(fch: &Native<Tsc>) -> Self {
        let last = LastReset::read(fch.acpi());
        Self {
            last,
            guard: Guard::load(fch.io()),
        }
    }
}

/// The last reset and the firmware warm-reset series it continues.
pub(super) fn log<V: Variant>(boot: &Boot) {
    fstart_log::info!(
        "{}: last reset: {} (PMxC0 {:#x}, SLP_TYP {})",
        V::NAME,
        boot.last.cause().name(),
        boot.last.status,
        boot.last.slp_typ
    );
    if let Some(record) = boot.guard.live() {
        // The record counts on its own (the PSP may have rewritten PMxC0);
        // PMxC0 is only logged.
        fstart_log::warn!(
            "{}: after firmware warm reset {} of at most {} ({}; PMxC0 shows a CF9 06h reset: {})",
            V::NAME,
            record.count,
            ql::TOTAL_LIMIT,
            record.reason.name(),
            boot.last.cf9_warm()
        );
    }
}

/// What the ABL says of this boot's memory (APOB `7/3`) and of the APCB
/// (`1/0x10 +0x4D`).
pub(super) fn log_apob<V: Variant>(apob: &ApobV2<'_>) {
    if let Ok(info) = apob.boot_info() {
        fstart_log::info!(
            "{}: APOB boot info: memory context restore {}, DRAM survives warm reset {}",
            V::NAME,
            info.memory_context_restore,
            info.dram_survives_warm_reset
        );
    }
    if apob.apcb_recovery() == Ok(true) {
        fstart_log::warn!(
            "{}: the ABL recovered the APCB this boot (memory context restore off)",
            V::NAME
        );
    }
}

/// A warm reset firmware asks for `reason`: the guard's record written,
/// POST 2Fh, Qilian's warm reset. Returns only when the guard refuses
/// (POST 2Eh, logged): the boot goes on without the reset.
pub(crate) fn firmware_warm_reset<V: Variant>(hw: &Hw, reason: Reason) {
    let fch = &hw.fch;
    match hw.boot.guard.arm(fch.io(), reason) {
        Ok(record) => {
            post(fch, 0x2f);
            fstart_log::warn!(
                "{}: warm reset for {} ({} of at most {})",
                V::NAME,
                reason.name(),
                record.count,
                reason.limit()
            );
            fstart_log::flush();
            ql::reset(fch.acpi(), fch.io(), ResetKind::Warm)
        }
        Err(refused) => {
            post(fch, 0x2e);
            let (why, count) = match refused {
                ql::Refused::Total { count } => ("firmware warm resets in a row", count),
                ql::Refused::Reason { count, .. } => ("warm resets for it in a row", count),
                ql::Refused::Write => ("CMOS bank 1 did not read back", 0),
            };
            fstart_log::error!(
                "{}: warm reset for {} refused after {} {}: booting on",
                V::NAME,
                reason.name(),
                count,
                why
            );
        }
    }
}

/// The PSP before its session: its recovery firmware halts the boot (POST
/// 2Dh; a warm reset does not leave it); a ResetRequired on an A/B flash
/// is a guarded warm reset, as the vendor's PEI makes it unguarded
/// (AmdPspPeiV2Wh `0xe4fde0`/`0xe4fef0`), on any other flash ignored, as
/// the vendor does too.
pub(super) fn psp<V: Variant>(
    ctx: &Ctx<'_, Sp7<V>>,
    c2p: psp::MmioTransport,
) -> Result<(), &'static str> {
    let cmd = c2p.read(psp::mailbox::C2pReg::Cmd);
    if C2P_CMD::RECOVERY.is_set(cmd) {
        post(&ctx.hw.fch, 0x2d);
        fstart_log::error!(
            "{}: the PSP runs its recovery firmware (C2P {:#x}): reflash both ISH slots",
            V::NAME,
            cmd
        );
        return Err("PSP recovery firmware");
    }
    // The vendor waits for READY without bound; a PSP not ready within a
    // second is reported and the boot goes on (its session fails if the
    // PSP stays away).
    match psp::Mailbox::new(c2p, psp::Variant::Wh).reset_required(&ctx.clock) {
        Ok(false) => return Ok(()),
        Ok(true) => {}
        Err(error) => {
            fstart_log::warn!("{}: PSP ResetRequired check: {}", V::NAME, error.parts().0);
            return Ok(());
        }
    }
    let cmd = c2p.read(psp::mailbox::C2pReg::Cmd);
    if !super::flash::ab_layout::<V>(&ctx.hw.fch) {
        fstart_log::warn!(
            "{}: the PSP asks for a warm reset (C2P {:#x}), not made: the flash has no A/B ISH slots",
            V::NAME,
            cmd
        );
        return Ok(());
    }
    fstart_log::warn!(
        "{}: the PSP asks for a warm reset (C2P {:#x})",
        V::NAME,
        cmd
    );
    firmware_warm_reset::<V>(ctx.hw, Reason::PspResetRequired);
    Ok(())
}

/// SMU v14 `0x4C` EnableNvmSelfRefresh, argument 1, once per socket, when
/// APOB `7/3 +0x50` (DramSurvivesWarmReset) is set: the one warm-boot
/// step of the vendor's x86 code (GIGABYTE AmdNbioPei `0x709dbebf` per
/// socket; Dell `AmdNbioSmuEarlyInit 0xecf500` per NBIO, WARM_RESET §3).
/// Both donors' APCBs leave the token at 0: dormant on them.
pub(super) fn nvm_self_refresh<V: Variant>(ctx: &Ctx<'_, Sp7<V>>, state: &State) {
    let survives = ctx
        .hw
        .apob()
        .ok()
        .and_then(|apob| apob.boot_info().ok())
        .is_some_and(|info| info.dram_survives_warm_reset != 0);
    if !survives {
        return;
    }
    for socket in 0..state.sockets() {
        let smu = state.smu_on(socket, 0, ctx.clock);
        let mut args = [1, 0, 0, 0, 0, 0];
        match smu.send(smu_v14::msg::ENABLE_NVM_SELF_REFRESH, &mut args) {
            Ok(()) => fstart_log::info!("{}: S{} NVM self-refresh on", V::NAME, socket),
            Err(_) => {
                fstart_log::error!("{}: S{} SMU EnableNvmSelfRefresh failed", V::NAME, socket)
            }
        }
    }
}

/// The APCB-recovery handshake as GIGABYTE's EndOfDxe makes it (PCD
/// `PcdAmdPspApcbRecoveryEnable` 0): 55h/55h, "the x86 ignores APCB
/// recovery", every boot. Dell's (PCD 1) restores the APCB from its backup,
/// writes 5Ah/A5h and warm-resets ([`scratch::ignore_apcb_recovery`]);
/// fstart does not rewrite the APCB.
pub(super) fn ready_to_boot<V: Variant>(hw: &Hw) {
    match scratch::ignore_apcb_recovery(hw.fch.io()) {
        ApcbHandshake::Ignored => {}
        ApcbHandshake::Restored => {
            fstart_log::info!("{}: CMOS bank 1 06h/07h held 5Ah/A5h", V::NAME)
        }
        ApcbHandshake::Invalid(word) => fstart_log::warn!(
            "{}: CMOS bank 1 06h/07h held {:#x}: the PSP saw a cleared CMOS",
            V::NAME,
            word
        ),
    }
}

/// After the PSP took BootDone: the guard's record erased, so that the
/// OS's resets never count.
pub(super) fn boot_done(hw: &Hw) {
    Guard::clear(hw.fch.io());
}
