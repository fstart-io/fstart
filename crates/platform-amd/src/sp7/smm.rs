// SPDX-License-Identifier: MIT OR Apache-2.0
//! The SP7 (Venice / WH) SMM handler: PSP `SmmInfo` registration, optional
//! `RasEinj::ENABLE`, PSP P2C SMI service, and per-CPU SMCA RAS thresholding
//! and BMC SEL delivery ([`crate::smm::ras`]).
//!
//! Layout and protocol match `AmdPspSmmV2` / `AmdPlatformRasWhDxe` on Venice:
//! - SW SMI `0xD0` (`PSP_SMM_INFO`) sends `SmmInfo` and, if `config.einj` is
//!   set, `RasEinj::ENABLE` over the PSP C2P mailbox (`MP0_C2PMSG_31` opens
//!   only after `RasEinj::ENABLE` with the SMM flag).
//! - PSP P2C SMI (`source::PSP`) answers PSP requests by the WH PSP's rules
//!   (`psp::p2c::answer_of`) and releases the lock.
//! - The OS's SLP_TYP write to S5 traps to SMM, which finishes the entry as
//!   the vendor's FCH SMM dispatcher does: SLP_TYP without SLP_EN, then the
//!   SMU v14 sleep entry (`fch::qilian::sx::enter_s5_posted`).
//! - Each CPU entering SMM runs `ras::enter` (supporting up to 1024 SP7
//!   threads); the rendezvous owner runs `ras::own` (storm control + SEL).

use fstart_driver_amd::fch::acpimmio::AcpiPorts;

/// What the installer gives the SP7 SMM handler, in the SMM header.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Config {
    /// The Qilian FCH ACPI I/O ports (`AcpiPorts`).
    pub ports: AcpiPorts,
    /// Socket 0 die 0 FCH root bridge PSP MMIO BAR below 4 GiB.
    pub psp_bar: u32,
    /// Send `RasEinj::ENABLE` right after `SmmInfo` (for the ACPI EINJ table).
    pub einj: bool,
    /// True if host-side IPMI KCS I/O decode was established during early init.
    /// Passed across the stage-to-SMM ELF boundary so the SMM handler does not
    /// rely on a separate per-ELF static.
    pub kcs_decoded: bool,
    /// Reserved DRAM address of [`Report`], written only by this handler.
    pub report: u64,
}

/// What the SP7 SMM handler writes back to boot code (and keeps for diagnostics).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Report {
    pub smm_info: u32,
    pub einj: u32,
    /// Count of PSP P2C SMIs answered, and the last P2C command code.
    pub p2c_requests: u32,
    pub p2c_command: u32,
}

impl Report {
    pub const NONE: Self = Self {
        smm_info: crate::smm::status::NOT_SENT,
        einj: crate::smm::status::NOT_SENT,
        p2c_requests: 0,
        p2c_command: 0,
    };
}

/// FCH SMI controller during TSEG installation: disables SMIs while relocating
/// SMBASE, then enables only the SMI command port and the PSP SMI.
#[cfg(feature = "bundle")]
pub struct Smi<'a, H> {
    pub fch: &'a H,
    pub config: Config,
}

#[cfg(feature = "bundle")]
impl<H: fstart_driver_amd::fch::FchHw> fstart_arch::x86::cpu::amd::smm::SmiControl for Smi<'_, H> {
    type HandlerConfig = Config;

    fn smm_handler_config(&self) -> Config {
        self.config
    }

    fn disable_smi(&self) {
        fstart_driver_amd::fch::smi::disable(self.fch.acpi());
    }

    fn enable_smi(&self) {
        fstart_driver_amd::fch::smi::route_sleep(self.fch.acpi());
        fstart_driver_amd::fch::smi::enable_psp(self.fch.acpi());
    }
}

#[cfg(feature = "smm")]
pub use handler::{enter_cpu, handle, handle_with};

#[cfg(feature = "smm")]
mod handler {
    use super::{Config, Report};
    use crate::smm::ras::{self, Delivery, Event, Policy};
    use crate::smm::{SmmFch, psp as smm_psp, sw};
    use fstart_core::mmio::{read32, write32};
    use fstart_driver_amd::ccx::tsc::Tsc;
    use fstart_driver_amd::fch::FchHw;
    use fstart_driver_amd::fch::smi::{self, Pending, source};
    use fstart_driver_amd::psp::Variant;
    use fstart_driver_amd::psp::command::SmmInfo;
    use fstart_driver_amd::psp::einj::RasEinj;
    use fstart_driver_amd::psp::mailbox::Mailbox;
    use fstart_smm::SmmContext;

    /// Per-CPU SMM entry hook before rendezvous: arms/reads SMCA RAS state
    /// according to `policy` (up to 1024 SP7 logical CPUs, 2P).
    ///
    /// `Silicon::Gn` only keeps PFEH off Naples B1 there (`ccx::ras::arm`);
    /// the Zen 6 PFEH, MCE-redirect and SMI-trigger MSRs are the ones it
    /// programs (AmdPlatformRasWhSmm `0x16940`, `0x114a0`).
    #[inline(always)]
    pub fn enter_cpu(ctx: &mut SmmContext<'_>, _: &Config, policy: &Policy) {
        ras::enter(policy, ctx.cpu(), fstart_driver_amd::cpu::Silicon::Gn);
    }

    /// SP7 SMI handler on the rendezvous owner for boards using OS-first MCA.
    pub fn handle(ctx: &mut SmmContext<'_>, config: &Config) {
        handle_with(ctx, config, &Policy::OS_FIRST, ras::no_bmc);
    }

    /// SP7 SMI handler on the rendezvous owner: drains the RAS journal to
    /// `bmc` under storm control, then dispatches SW SMI `0xD0` and PSP P2C.
    #[inline(always)]
    pub fn handle_with(
        ctx: &mut SmmContext<'_>,
        config: &Config,
        policy: &Policy,
        bmc: impl FnMut(&Event<'_>, Tsc) -> Delivery,
    ) {
        crate::smm::with_fch(crate::sp7::ECAM_BASE, |fch| {
            let first = ctx.first_owner();
            ras::own(policy, fch.acpi(), first, || ctx.cpus_held(), bmc);
            smi::dispatch(
                fch.acpi(),
                fch.io(),
                &config.ports,
                &Handler { fch, config },
            );
            // Release the PSP P2C lock after handling pending SMIs.
            smm_psp::release();
        });
    }

    struct Handler<'a> {
        fch: &'a SmmFch,
        config: &'a Config,
    }

    impl Handler<'_> {
        fn report(&self) -> *mut Report {
            self.config.report as *mut Report
        }
    }

    impl smi::Handler for Handler<'_> {
        fn command(&self, command: u8) {
            if command == sw::PSP_SMM_INFO {
                psp(self.config, &self.fch.timer, self.report());
            }
        }

        fn sources(&self, pending: &Pending) {
            if pending.has(source::PSP) {
                let command = smm_psp::answer_of(self.fch.acpi(), pending, Variant::Wh);
                let report = self.report();
                // SAFETY: reserved DRAM report owned exclusively by this handler.
                unsafe {
                    let requests = &raw mut (*report).p2c_requests;
                    write32(requests, read32(requests).wrapping_add(1));
                    write32(&raw mut (*report).p2c_command, command);
                }
            }
        }

        /// OS owns ACPI directly.
        fn power_button(&self) {}

        /// S5 as the vendor's FCH SMM dispatcher enters it; other types
        /// return (SP7 publishes S0 and S5 only).
        fn sleep(&self, slp_typ: u8) {
            fstart_driver_amd::fch::qilian::sx::enter_s5_posted(
                self.fch,
                &self.config.ports,
                slp_typ,
            );
        }
    }

    /// Register `SmmInfo` (zeroing reserved descriptors) and, when enabled,
    /// send `RasEinj::ENABLE` with the SMM flag. Runs once per boot.
    #[inline(never)]
    fn psp(config: &Config, timer: &Tsc, report: *mut Report) {
        // SAFETY: PSP BAR mapped below 4 GiB on die 0 RB 0 during cold init.
        let mailbox = unsafe { Mailbox::mmio(config.psp_bar.into(), Variant::Wh) };
        let Some(registered) = smm_psp::register(&mailbox, timer, |info| SmmInfo {
            reserved: Default::default(),
            ..info
        }) else {
            return;
        };
        // SAFETY: as in `sources`.
        unsafe {
            write32(
                &raw mut (*report).smm_info,
                crate::smm::status::of(registered),
            )
        };
        if registered.is_err() || !config.einj {
            return;
        }
        // SAFETY: SmmInfo registered the C2P SMM buffer; rendezvous owner only.
        let enabled = unsafe { smm_psp::send(&mailbox, timer, RasEinj::ENABLE) };
        // SAFETY: as in `sources`.
        unsafe { write32(&raw mut (*report).einj, crate::smm::status::of(enabled)) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_none_marks_both_psp_commands_unsent() {
        assert_eq!(Report::NONE.smm_info, crate::smm::status::NOT_SENT);
        assert_eq!(Report::NONE.einj, crate::smm::status::NOT_SENT);
        assert_eq!(Report::NONE.p2c_requests, 0);
        assert_eq!(Report::NONE.p2c_command, 0);
    }
}
