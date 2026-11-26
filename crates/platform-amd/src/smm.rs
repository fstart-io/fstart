// SPDX-License-Identifier: MIT OR Apache-2.0
//! SMM for every SoC: the configuration the installer writes into SMRAM,
//! the FCH as the owner of the SMI sources ([`Smi`]), and the handler on
//! the driver's SMI dispatcher (`fch::smi::dispatch`) and sleep path
//! (`fch::sx`).
//!
//! A SoC with SMM work of its own adds it as its part of the configuration
//! and a handler around the same dispatcher (SP3: the PSP's SMM info and
//! mailbox, EINJ, RAS, Rome's CF9 trap and Milan's FCH lock, in
//! [`crate::sp3::smm`]; RS: the PSP's SMM info, mailbox and EINJ, in
//! [`crate::rs::smm`]); Picasso and Zeppelin have none ([`handle`]). What
//! the PSP needs of SMM is the same for every SoC that registers with it
//! (`psp`). The board binds the handler in its `smm.rs` with
//! [`smm_bin!`], and adds its own sources there ([`handle_with`]).

use fstart_driver_amd::fch::smi;
pub use fstart_driver_ipmi::SleepNotify;

/// fstart-private software SMI numbers besides the FCH ones
/// (`fch::smi::sw`).
pub mod sw {
    /// Register the handler's PSP buffers with the PSP (SmmInfo, C2P 0x02).
    pub const PSP_SMM_INFO: u8 = 0xD0;
}

/// How a handler reports a PSP command to normal mode, in a word of the
/// firmware's reserved DRAM that only the handler writes: 0 when the PSP
/// took it, its C2P status if it refused it, [`status::TIMEOUT`] or
/// [`status::FAILED`] otherwise, and [`status::NOT_SENT`], what normal
/// mode puts there first, until the handler sent it.
pub mod status {
    use fstart_driver_amd::psp::PspError;

    pub const NOT_SENT: u32 = u32::MAX;
    pub const TIMEOUT: u32 = 0x1_0000;
    pub const FAILED: u32 = 0x2_0000;

    /// The report status of `result`.
    pub const fn of(result: Result<(), PspError>) -> u32 {
        match result {
            Ok(()) => 0,
            Err(PspError::Status { status, .. }) => status,
            Err(PspError::Timeout(_)) => TIMEOUT,
            Err(_) => FAILED,
        }
    }

    /// How long normal mode waits for the handler's report after the SW
    /// SMI: the handler's PSP commands wait at most 20 ms each.
    pub const WAIT_US: u64 = 1_000_000;

    /// The status `read` returns once it is no longer [`NOT_SENT`], or
    /// [`NOT_SENT`] after [`WAIT_US`] by `timer`: the SW SMI is taken at
    /// the OUT on every part fstart knows, but a report is read only once
    /// it is there, and never waited for without end.
    pub fn wait(timer: &impl fstart_core::services::Timer, read: impl Fn() -> u32) -> u32 {
        let start = timer.timestamp_us();
        loop {
            let status = read();
            if status != NOT_SENT || timer.timestamp_us().wrapping_sub(start) >= WAIT_US {
                return status;
            }
            core::hint::spin_loop();
        }
    }

    /// The handler's answers on RS: SmmInfo's status, then, if the board
    /// publishes EINJ (`einj`) and the PSP took SmmInfo, RasEinj ENABLE's,
    /// each as [`wait`] returned it. Neither is read again: a report that
    /// comes after its deadline stays [`NOT_SENT`], which halts the boot
    /// (SmmInfo) or leaves EINJ out.
    pub fn answers(
        timer: &impl fstart_core::services::Timer,
        einj: bool,
        smm_info: impl Fn() -> u32,
        einj_status: impl Fn() -> u32,
    ) -> (u32, Option<u32>) {
        let smm_info = wait(timer, smm_info);
        let einj = (einj && smm_info == 0).then(|| wait(timer, einj_status));
        (smm_info, einj)
    }

    /// SmmInfo as the handler reported it on [`super::sw::PSP_SMM_INFO`],
    /// read back by normal mode before anything that needs it: the PSP's
    /// DF lock breaks device DMA without it, and BootDone must not leave
    /// the DF unlocked. An error unless the PSP took it.
    pub fn smm_info(soc: &str, status: u32) -> Result<(), &'static str> {
        match status {
            0 => {
                fstart_log::info!("{}: PSP SmmInfo status 0", soc);
                Ok(())
            }
            NOT_SENT => {
                fstart_log::error!("{}: PSP SmmInfo not sent: no SMM handler answered", soc);
                Err("PSP SmmInfo")
            }
            status => {
                fstart_log::error!("{}: PSP SmmInfo status {:#x}", soc, status);
                Err("PSP SmmInfo")
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use fstart_driver_amd::psp::mailbox::Wait;

        /// No handler answered, the PSP refused, it timed out: each halts
        /// the boot before the DF lock; only the PSP's 0 lets it go on.
        #[test]
        fn only_a_registration_the_psp_took_goes_on() {
            assert_eq!(smm_info("test", NOT_SENT), Err("PSP SmmInfo"));
            let refused = of(Err(PspError::Status {
                command: 2,
                status: 5,
            }));
            assert_eq!(refused, 5);
            assert_eq!(smm_info("test", refused), Err("PSP SmmInfo"));
            let timeout = of(Err(PspError::Timeout(Wait::Done)));
            assert_eq!(smm_info("test", timeout), Err("PSP SmmInfo"));
            assert_eq!(
                smm_info("test", of(Err(PspError::Unavailable))),
                Err("PSP SmmInfo")
            );
            assert_eq!(smm_info("test", of(Ok(()))), Ok(()));
        }

        /// The report is read once the handler wrote it, however late
        /// within the deadline; never written, the wait ends at the
        /// deadline with NOT_SENT, which halts the boot.
        #[test]
        fn the_report_is_waited_for_within_a_deadline() {
            use core::cell::Cell;
            use fstart_core::services::{ManualTimer, Timer};
            let timer = ManualTimer::ticking();
            let reads = Cell::new(0);
            let late = || {
                reads.set(reads.get() + 1);
                if reads.get() < 50 { NOT_SENT } else { 0 }
            };
            assert_eq!(wait(&timer, late), 0);
            assert_eq!(reads.get(), 50);
            let timer = ManualTimer::ticking();
            assert_eq!(wait(&timer, || NOT_SENT), NOT_SENT);
            assert!(timer.timestamp_us() >= WAIT_US);
            assert_eq!(smm_info("test", NOT_SENT), Err("PSP SmmInfo"));
        }

        /// A success that comes after the deadline does not count: the
        /// status is the one the wait returned. RasEinj ENABLE is waited
        /// for only with EINJ and SmmInfo taken, and its late success
        /// leaves EINJ out the same way.
        #[test]
        fn a_report_after_its_deadline_stays_not_sent() {
            use fstart_core::services::{ManualTimer, Timer};
            let timer = ManualTimer::ticking();
            let start = timer.timestamp_us();
            let after = |deadline: u64| {
                let timer = &timer;
                move || {
                    if timer.timestamp_us().wrapping_sub(start) > deadline {
                        0
                    } else {
                        NOT_SENT
                    }
                }
            };
            assert_eq!(
                answers(&timer, true, after(WAIT_US), after(0)),
                (NOT_SENT, None)
            );
            let timer = ManualTimer::ticking();
            let start = timer.timestamp_us();
            let late = || {
                if timer.timestamp_us().wrapping_sub(start) > 3 * WAIT_US {
                    0
                } else {
                    NOT_SENT
                }
            };
            assert_eq!(answers(&timer, true, || 0, late), (0, Some(NOT_SENT)));
            assert_eq!(answers(&timer, false, || 0, || 0), (0, None));
            assert_eq!(answers(&timer, true, || 5, || 0), (5, None));
        }
    }
}

/// What the normal-mode installer writes into SMRAM for the handler: the
/// FCH's SMI setup, the ECAM the handler reaches config space through, the
/// BMC's sleep-state notification and the SoC's own part `S`.
///
/// The SMM entry borrows this immutable block in locked SMRAM rather than
/// copying it.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SmmConfig<S = ()> {
    pub fch: smi::Config,
    /// ECAM base for the handler's config accesses (SMN on bus 0); 0 for
    /// CF8/CFC when the window is not below 4 GiB ([`Ecam`]).
    pub ecam_base: u64,
    /// Sleep-state notification to the BMC, if the board has one.
    pub bmc: Option<SleepNotify>,
    pub soc: S,
}

/// The FCH side of SMM installation, for every SoC: SMIs off while TSEG
/// moves, then the FCH's sources and the SoC's own (`sources`).
#[cfg(feature = "bundle")]
pub struct Smi<'a, H, S> {
    pub fch: &'a H,
    pub config: SmmConfig<S>,
    pub sources: fn(&H, &SmmConfig<S>),
}

#[cfg(feature = "bundle")]
impl<H: fstart_driver_amd::fch::FchHw, S: Copy> fstart_arch::x86::cpu::amd::smm::SmiControl
    for Smi<'_, H, S>
{
    type HandlerConfig = SmmConfig<S>;

    fn smm_handler_config(&self) -> SmmConfig<S> {
        self.config
    }

    fn disable_smi(&self) {
        smi::disable(self.fch.acpi());
    }

    fn enable_smi(&self) {
        smi::enable(self.fch, &self.config.fch);
        (self.sources)(self.fch, &self.config);
    }
}

#[cfg(feature = "smm")]
pub use fstart_smm::SmmContext;
#[cfg(feature = "bundle-smm")]
pub use fstart_smm::smm_bin;
#[cfg(feature = "smm")]
pub use handler::{Ecam, SmmFch, handle, handle_with, with, with_fch, with_fch_in};

pub mod aer;
#[cfg(feature = "smm")]
pub(crate) mod psp;
pub mod ras;

#[cfg(feature = "smm")]
mod handler {
    use fstart_driver_amd::ccx::tsc::Tsc;
    use fstart_driver_amd::fch::smi::{self, Pending};
    use fstart_driver_amd::fch::sx::{self, Version};
    use fstart_driver_amd::fch::{Fch, FchHw, acpimmio};
    use fstart_driver_amd::smn::Smn;
    use fstart_driver_amd::smu::{Smu, v9, v11};
    use fstart_driver_amd::window::{Mmio, Ports};
    use fstart_pci::{ConfigRegionAccess, EcamDevice, PciAddress};

    use super::{SmmConfig, SmmContext};

    /// SMU message timing in SMM: 10-us polls, each wait bounded by 1 s.
    const SMU_POLL_US: u32 = 10;
    const SMU_TIMEOUT_US: u32 = 1_000_000;

    /// Config space in SMM: the global ECAM window (`ecam::init` with the
    /// installer's base on every SMI) without `GlobalEcam`'s panic on an
    /// unmapped register, which the SMM image builder rejects. Such an
    /// access reads all ones and drops the write, like a master abort.
    ///
    /// Without an ECAM base (0: the SoC's window is above the 4 GiB the
    /// handler maps, SP7) segment 0's standard headers go through CF8/CFC
    /// ([`with_fch`] keeps the interrupted code's selector).
    #[derive(Debug, Clone, Copy, Default)]
    pub struct Ecam;

    /// The CF8 dword selector of `offset` in `address`, if CF8 is the way
    /// there (no ECAM, segment 0, a dword of the first 256 bytes).
    fn legacy(address: PciAddress, offset: u16) -> Option<(u8, u8, u8, u8)> {
        (fstart_pci::ecam::base() == 0
            && address.segment() == 0
            && offset < 0x100
            && offset & 3 == 0)
            .then(|| {
                (
                    address.bus(),
                    address.device(),
                    address.function(),
                    offset as u8,
                )
            })
    }

    impl ConfigRegionAccess for Ecam {
        unsafe fn read(&self, address: PciAddress, offset: u16) -> u32 {
            if let Some((bus, device, function, offset)) = legacy(address, offset) {
                // SAFETY: SMM owns the CPU and the CF8 selector (saved).
                return unsafe { fstart_pci::cf8::read32(bus, device, function, offset) };
            }
            EcamDevice::new_address(address)
                .try_read32(offset)
                .unwrap_or(u32::MAX)
        }

        unsafe fn write(&self, address: PciAddress, offset: u16, value: u32) {
            if let Some((bus, device, function, offset)) = legacy(address, offset) {
                // SAFETY: as for `read`.
                return unsafe { fstart_pci::cf8::write32(bus, device, function, offset, value) };
            }
            let _ = EcamDevice::new_address(address).try_write32(offset, value);
        }
    }

    /// FCH 0 in SMM, over [`Ecam`].
    pub type SmmFch = Fch<Mmio, Mmio, Ports, Ecam, Tsc>;

    /// The handler's view of the machine on the rendezvous owner: config
    /// space through the installer's ECAM, FCH 0 and the SMU of die 0 or
    /// socket 0 ([`Version`]). The interrupted code may be in the middle of
    /// an SMN access on bus 0: its index is saved around `f`.
    pub fn with<S, R>(
        config: &SmmConfig<S>,
        f: impl FnOnce(&SmmFch, Option<&Smu<Ecam, Tsc>>) -> R,
    ) -> R {
        with_fch(config.ecam_base, |fch| {
            let smu = smu(config.fch.version, fch.timer);
            f(fch, smu.as_ref())
        })
    }

    /// [`with`] without the SMU, for a handler that does not take the
    /// sleep path: config space through the ECAM at `ecam_base` and FCH 0,
    /// the SMN index of bus 0 saved around `f`.
    #[inline(always)]
    pub fn with_fch<R>(ecam_base: u64, f: impl FnOnce(&SmmFch) -> R) -> R {
        with_fch_in(ecam_base, 1, f)
    }

    /// [`with_fch`] with an ECAM window of `segments` PCI segments at
    /// `ecam_base` (SP7's 2P bus maps), each of them reached.
    #[inline(always)]
    pub fn with_fch_in<R>(ecam_base: u64, segments: u16, f: impl FnOnce(&SmmFch) -> R) -> R {
        fstart_pci::ecam::init_segments(ecam_base as usize, segments);
        // SAFETY: ring 0 in SMM; the P-state MSRs.
        let timer = Tsc::native(&unsafe { fstart_arch::x86::cpu::msr::NativeMsr::new() });
        // SAFETY: SMM owns the CPU; the FCH ACPI-MMIO block, MMIO and ports
        // are mapped by the handler's identity tables.
        let fch = unsafe {
            Fch {
                acpi: Mmio::new(acpimmio::BASE as usize),
                mmio: Mmio::new(0),
                io: Ports::new(),
                pci: Ecam,
                bus: 0,
                timer,
            }
        };
        // SAFETY: SMM owns the CPU; the interrupted code's CF8 selector is
        // restored below.
        let selector = (ecam_base == 0)
            .then(|| unsafe { fstart_core::pio::inl(fstart_pci::cf8::ADDRESS_PORT) });
        let host = PciAddress::default();
        // SAFETY: SMM owns the CPU; the index is restored below.
        let saved = unsafe { Ecam.read(host, fstart_driver_amd::smn::SMN_INDEX) };
        let result = f(&fch);
        // SAFETY: as above.
        unsafe { Ecam.write(host, fstart_driver_amd::smn::SMN_INDEX, saved) };
        if let Some(selector) = selector {
            // SAFETY: as above.
            unsafe { fstart_core::pio::outl(fstart_pci::cf8::ADDRESS_PORT, selector) };
        }
        result
    }

    /// The SMU of die 0 or socket 0 through bus 0, for the sleep entry;
    /// Sandstone sends it nothing.
    fn smu(version: Version, timer: Tsc) -> Option<Smu<Ecam, Tsc>> {
        let (regs, limit) = match version {
            Version::Sandstone => return None,
            Version::Taishan => (v9::MAILBOX, v9::ID_LIMIT),
            Version::Huashan => (
                v11::MAILBOX,
                v11::id_limit(fstart_driver_amd::cpu::Silicon::Ssp),
            ),
            Version::Milan => (
                v11::MAILBOX,
                v11::id_limit(fstart_driver_amd::cpu::Silicon::Gn),
            ),
        };
        // SAFETY: the handler owns this bus's SMN index/data pair while it
        // runs ([`with`] restores the index).
        let smn = unsafe { Smn::on_bus(Ecam, 0) };
        Some(
            Smu::new(smn, regs, timer, limit)
                .with_timeout_us(SMU_TIMEOUT_US)
                .with_poll_us(SMU_POLL_US),
        )
    }

    /// The FCH's own SMI work: ACPI mode, the BMC's sleep notification,
    /// power button and sleep; the board's `sources` besides.
    struct Fch0<'a, F> {
        fch: &'a SmmFch,
        smu: Option<&'a Smu<Ecam, Tsc>>,
        config: &'a SmmConfig,
        sources: F,
    }

    impl<F: Fn(&SmmFch, &Pending)> smi::Handler for Fch0<'_, F> {
        fn command(&self, command: u8) {
            let fch = &self.config.fch;
            if command == fch.acpi_enable || command == fch.acpi_disable {
                sx::acpi_mode(self.fch, &fch.ports, command == fch.acpi_enable);
            } else if let Some(bmc) = self.config.bmc {
                // SAFETY: the board's KCS ports; SMM owns the CPUs.
                unsafe { bmc.report(command, self.fch.timer) };
            }
        }

        fn sources(&self, pending: &Pending) {
            (self.sources)(self.fch, pending);
        }

        fn power_button(&self) {
            let fch = &self.config.fch;
            if !sx::sci_enabled(self.fch.io(), &fch.ports) {
                sx::power_button(self.fch, &fch.ports, fch.version, self.smu);
            }
        }

        fn sleep(&self, slp_typ: u8) {
            let fch = &self.config.fch;
            sx::enter(self.fch, &fch.ports, fch.version, slp_typ, self.smu, &[]);
        }
    }

    /// The SMI handler of a SoC without SMM work of its own (Picasso,
    /// Zeppelin) on a board with SMI sources of its own: `sources` gets
    /// every SMI's sources before the power button and sleep.
    pub fn handle_with(
        _ctx: &mut SmmContext<'_>,
        config: &SmmConfig,
        sources: impl Fn(&SmmFch, &Pending),
    ) {
        with(config, |fch, smu| {
            let handler = Fch0 {
                fch,
                smu,
                config,
                sources,
            };
            smi::dispatch(fch.acpi(), fch.io(), &config.fch.ports, &handler);
        });
    }

    /// The SMI handler of a SoC without SMM work of its own on a board
    /// without SMI sources of its own.
    pub fn handle(ctx: &mut SmmContext<'_>, config: &SmmConfig) {
        handle_with(ctx, config, |_, _| {});
    }
}
