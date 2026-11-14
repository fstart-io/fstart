// SPDX-License-Identifier: MIT OR Apache-2.0
//! SMM for every SoC: the configuration the installer writes into SMRAM,
//! the FCH as the owner of the SMI sources ([`Smi`]), and the handler on
//! the driver's SMI dispatcher (`fch::smi::dispatch`) and sleep path
//! (`fch::sx`).
//!
//! A SoC with SMM work of its own adds it as its part of the configuration
//! and a handler around the same dispatcher (SP3: the PSP's SMM info and
//! mailbox, EINJ, RAS, Rome's CF9 trap and Milan's FCH lock, in
//! [`crate::sp3::smm`]); Picasso and Zeppelin have none ([`handle`]). The
//! board binds the handler in its `smm.rs` with [`smm_bin!`], and adds its
//! own sources there ([`handle_with`]).

use fstart_driver_amd::fch::smi;
pub use fstart_driver_ipmi::SleepNotify;

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
    /// ECAM base for the handler's config accesses (SMN on bus 0).
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
pub use handler::{Ecam, SmmFch, handle, handle_with, with};

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
    #[derive(Debug, Clone, Copy, Default)]
    pub struct Ecam;

    impl ConfigRegionAccess for Ecam {
        unsafe fn read(&self, address: PciAddress, offset: u16) -> u32 {
            EcamDevice::new_address(address)
                .try_read32(offset)
                .unwrap_or(u32::MAX)
        }

        unsafe fn write(&self, address: PciAddress, offset: u16, value: u32) {
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
        fstart_pci::ecam::init(config.ecam_base as usize);
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
        let host = PciAddress::default();
        // SAFETY: SMM owns the CPU; the index is restored below.
        let saved = unsafe { Ecam.read(host, fstart_driver_amd::smn::SMN_INDEX) };
        let smu = smu(config.fch.version, timer);
        let result = f(&fch, smu.as_ref());
        // SAFETY: as above.
        unsafe { Ecam.write(host, fstart_driver_amd::smn::SMN_INDEX, saved) };
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
                let _ = sx::power_button(self.fch, &fch.ports, fch.version, self.smu);
            }
        }

        fn sleep(&self, slp_typ: u8) {
            let fch = &self.config.fch;
            let _ = sx::enter(self.fch, &fch.ports, fch.version, slp_typ, self.smu, &[]);
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
