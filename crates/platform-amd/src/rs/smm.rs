// SPDX-License-Identifier: MIT OR Apache-2.0
//! SMM on the RS SoCs, only for the PSP (AmdPspSmmV2Rs and
//! AmdPlatformRasRsSmm `RasEnablePspEinj` in the vendors' firmware): the
//! handler registers its SMRAM buffers with the PSP (SmmInfo), opens the
//! PSP's ACPI error injection (RasEinj ENABLE, which the PSP takes only
//! from SMM with the SMM flag set) and answers the PSP's P2C requests
//! (`crate::smm::psp`, as SP3's handler).
//!
//! The OS runs ACPI directly (FADT SMI_CMD 0, SCI_EN set by
//! `Init::finalize`): SLP_TYP, the power button and the GEVENTs stay with
//! it. The FCH raises SMIs for the SMI command port, which the boot flow
//! uses once for [`crate::smm::sw::PSP_SMM_INFO`] and then turns off, and
//! for the PSP ([`Smi`]).
//!
//! The handler reports to normal mode in a [`Report`] in the firmware's
//! reserved DRAM, which it never reads: the statuses of SmmInfo and
//! RasEinj ENABLE, and the PSP SMIs it answered.

use fstart_driver_amd::fch::acpimmio::AcpiPorts;

/// What the installer writes into SMRAM for [`handle`].
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Config {
    /// The FCH's ACPI blocks.
    pub ports: AcpiPorts,
    /// IOS0's PSP BAR, below 4 GiB: the handler's page tables map the low
    /// 4 GiB only.
    pub psp_bar: u32,
    /// Open the PSP's ACPI EINJ service after SmmInfo.
    pub einj: bool,
    /// The [`Report`], in the firmware's reserved DRAM below 4 GiB.
    pub report: u64,
}

/// What the handler leaves for normal mode (and a debugger reading
/// `/dev/mem`). A status is 0 when the PSP took the command, its C2P
/// status if it refused it, [`Report::TIMEOUT`] or [`Report::FAILED`]
/// otherwise, [`Report::NOT_SENT`] before the handler sent it.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Report {
    pub smm_info: u32,
    pub einj: u32,
    /// The PSP SMIs answered, and the PSP's last P2C command.
    pub p2c_requests: u32,
    pub p2c_command: u32,
}

impl Report {
    pub const NOT_SENT: u32 = u32::MAX;
    pub const TIMEOUT: u32 = 0x1_0000;
    pub const FAILED: u32 = 0x2_0000;
    pub const NONE: Self = Self {
        smm_info: Self::NOT_SENT,
        einj: Self::NOT_SENT,
        p2c_requests: 0,
        p2c_command: 0,
    };

    /// The report status of `result`.
    pub const fn status(result: Result<(), fstart_driver_amd::psp::PspError>) -> u32 {
        use fstart_driver_amd::psp::PspError;
        match result {
            Ok(()) => 0,
            Err(PspError::Status { status, .. }) => status,
            Err(PspError::Timeout(_)) => Self::TIMEOUT,
            Err(_) => Self::FAILED,
        }
    }
}

/// The FCH side of the installation: SMIs off while TSEG moves, then only
/// the SMI command port and the PSP's SMI (`fch::smi::enable_psp`).
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
        fstart_driver_amd::fch::smi::enable_psp(self.fch.acpi());
    }
}

#[cfg(feature = "smm")]
pub use handler::handle;

#[cfg(feature = "smm")]
mod handler {
    use super::{Config, Report};
    use crate::smm::{SmmFch, psp as smm_psp, sw};
    use fstart_driver_amd::ccx::tsc::Tsc;
    use fstart_driver_amd::fch::FchHw;
    use fstart_driver_amd::fch::smi::{self, Pending, source};
    use fstart_driver_amd::psp::Variant;
    use fstart_driver_amd::psp::command::SmmInfo;
    use fstart_driver_amd::psp::einj::RasEinj;
    use fstart_driver_amd::psp::mailbox::{Mailbox, MmioTransport};
    use fstart_smm::SmmContext;

    /// The RS SMI handler on the rendezvous owner: SW SMI D0 and the
    /// PSP's SMI, on the FCH's dispatcher.
    pub fn handle(_ctx: &mut SmmContext<'_>, config: &Config) {
        crate::smm::with_fch(crate::rs::ECAM_BASE, |fch| {
            smi::dispatch(
                fch.acpi(),
                fch.io(),
                &config.ports,
                &Handler { fch, config },
            );
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
                let command = smm_psp::answer(self.fch.acpi(), pending);
                let report = self.report();
                // SAFETY: the report the installer placed in reserved
                // firmware DRAM; only this handler writes it.
                unsafe {
                    let requests = &raw mut (*report).p2c_requests;
                    requests.write_volatile(requests.read_volatile().wrapping_add(1));
                    (&raw mut (*report).p2c_command).write_volatile(command);
                }
            }
        }

        /// Not routed: the OS owns ACPI.
        fn power_button(&self) {}

        /// Not routed: the OS owns ACPI and the FCH enters S5 itself.
        fn sleep(&self, _: u8) {}
    }

    /// SmmInfo, the reserved descriptors zeroed as the RS donors send it
    /// (sp5 `reports/PSP_SMM_ABI.md` §2), then RasEinj ENABLE with the SMM
    /// flag if the board publishes EINJ. Once per boot.
    #[inline(never)]
    fn psp(config: &Config, timer: &Tsc, report: *mut Report) {
        // SAFETY: the PSP BAR the boot flow mapped below 4 GiB.
        let mailbox = Mailbox::new(
            unsafe { MmioTransport::new(config.psp_bar.into()) },
            Variant::Gn,
        );
        let Some(registered) = smm_psp::register(&mailbox, timer, |info| SmmInfo {
            reserved: Default::default(),
            ..info
        }) else {
            return;
        };
        // SAFETY: as in `sources`.
        unsafe { (&raw mut (*report).smm_info).write_volatile(Report::status(registered)) };
        if registered.is_err() || !config.einj {
            return;
        }
        // SAFETY: SmmInfo registered the C2P page; the rendezvous owner.
        let enabled = unsafe { smm_psp::send(&mailbox, timer, RasEinj::ENABLE) };
        // SAFETY: as in `sources`.
        unsafe { (&raw mut (*report).einj).write_volatile(Report::status(enabled)) };
    }
}
