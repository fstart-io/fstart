//! SMM on SP3: TSEG size, the SoC's part of the SMM configuration and its
//! handler (ACPI_SMM §4, FCH_SP3 §5, PSP_APOB §3.3) around the FCH SMI
//! dispatcher every SoC uses ([`crate::smm`]).
//!
//! Installation is `crate::tseg` with `crate::smm::Smi`: SMIs off while
//! TSEG moves, then the FCH's sources and Rome's CF9 trap.
//!
//! The handler ([`handle`], feature `smm`; the board binds it as its SMI
//! handler) adds to the FCH's ACPI mode, sleep and power button the NBIO
//! USB SMI, Milan's PSP FCH lock (SW SMI D2), the PSP's SMM side
//! (`crate::smm::psp`: SmmInfo on [`crate::smm::sw::PSP_SMM_INFO`], the P2C
//! answers), Rome's EINJ and the machine-check handler every SoC with
//! PFEH shares ([`crate::smm::ras`], per the board's [`Policy`], with
//! [`enter_cpu`] and the board's BMC hook). There is no DXE or UEFI
//! dependency.
//!
//! The image is built by the host plan's `smm` unit from the board crate
//! (`smm_bin!`, feature `bundle-smm`); `build.rs` checks that it fits
//! [`TSEG_SIZE`] for the board's thread count.

use crate::sp3::silicon::Generation;

/// TSEG size: 8 MiB, as the donors' SMM_MASK (PSP_APOB §3.3 example
/// `0xffff_ff80_0000`-style masks); a power of two the MSR pair describes.
pub const TSEG_SIZE: u64 = 8 << 20;

/// The SP3 part of the SMM configuration ([`crate::smm::SmmConfig`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub struct Sp3 {
    /// Socket 0's PSP BAR (Naples: die 0), for SmmInfo and the FCH lock.
    pub psp_bar: u32,
    pub einj: EinjConfig,
    /// Root buses whose IOHC acknowledges the USB SMI (every die on Naples,
    /// socket 0 on Rome/Milan) and, on Naples, takes the S3 handshake.
    pub usb_buses: [u8; 8],
    pub usb_bus_count: u8,
    pub generation: Generation,
    /// Where the handler reports SmmInfo ([`crate::smm::status`]): a word
    /// of the firmware's reserved DRAM below 4 GiB, which normal mode reads
    /// before the PSP's DF lock.
    pub smm_info: u64,
}

/// Endpoints and owned cache-line targets, frozen by the normal installer.
/// Absent sockets have no target and are never accessed by SMM.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[repr(C)]
pub struct EinjConfig {
    pub df: [fstart_pci::PciAddress; 2],
    pub buses: [u8; 2],
    pub sockets: u8,
    pub targets: [u64; 2],
}

impl Sp3 {
    pub fn usb_buses(&self) -> &[u8] {
        &self.usb_buses[..usize::from(self.usb_bus_count).min(self.usb_buses.len())]
    }
}

/// The FCH SMI setup of `generation`'s handler: the ACPI blocks ResetHwAcpi
/// programs and the donors' ACPI enable and disable values.
pub const fn fch_smi(generation: Generation) -> fstart_driver_amd::fch::smi::Config {
    use fstart_driver_amd::fch::{acpimmio, smi};
    smi::Config {
        ports: acpimmio::AcpiPorts {
            event: acpimmio::PM1_STS as u16,
            control: acpimmio::PM1_CNT as u16,
            timer: acpimmio::PM_TMR as u16,
            gpe: acpimmio::GPE0_STS as u16,
        },
        acpi_enable: smi::sw::ACPI_ENABLE,
        acpi_disable: smi::sw::ACPI_DISABLE,
        gevents: 0,
        version: generation.fch(),
    }
}

/// The SoC's own SMI sources after the FCH's: Rome's CF9 trap.
#[cfg(feature = "bundle")]
pub fn sources(fch: &crate::sp3::silicon::hw::PrimaryFch, config: &crate::smm::SmmConfig<Sp3>) {
    if config.soc.generation == Generation::Rome {
        fstart_driver_amd::fch::huashan::smm::arm_cf9_trap(fch);
    }
}

/// A board's SMM policy on SP3: its RAS ([`crate::board::Board::RAS`])
/// and Rome's restricted correctable-memory EINJ in platform-owned
/// scratch RAM.
#[derive(Debug, Clone, Copy)]
pub struct Policy {
    pub ras: crate::smm::ras::Policy,
    pub memory_einj: bool,
}

impl Policy {
    /// OS-first: no MCA observation, EINJ or SEL reporting.
    pub const OS_FIRST: Self = Self {
        ras: crate::smm::ras::Policy::OS_FIRST,
        memory_einj: false,
    };
}

#[cfg(feature = "smm")]
pub use handler::{enter_cpu, handle};

#[cfg(feature = "smm")]
mod einj;

#[cfg(feature = "smm")]
mod handler {
    use crate::smm::ras::{self, Delivery, Event};
    use crate::smm::{Ecam, SmmConfig, SmmFch, psp as smm_psp, sw};
    use crate::sp3::silicon::Generation;
    use fstart_driver_amd::fch::smi::{self, Pending, source};
    use fstart_driver_amd::fch::{FchHw, sx};
    use fstart_driver_amd::psp::mailbox::{Mailbox, MmioTransport};
    use fstart_driver_amd::smn::Smn;
    use fstart_driver_amd::smu::Smu;
    use fstart_driver_amd::window::Window;
    use fstart_smm::SmmContext;
    use tock_registers::interfaces::Readable;

    use super::{Policy, Sp3};
    use crate::sp3::silicon::hw::Tsc;

    /// CPU-local work on every CPU that enters SMM, before the rendezvous:
    /// the MCA work of `policy`, which the owner reports ([`handle`]). Bind
    /// it as the board's `enter_cpu`.
    #[inline(always)]
    pub fn enter_cpu(ctx: &mut SmmContext<'_>, config: &SmmConfig<Sp3>, policy: &Policy) {
        ras::enter(&policy.ras, ctx.cpu(), config.soc.generation.silicon());
    }

    /// The SP3 SMI handler on the rendezvous owner: the owner's RAS work
    /// (`ras::own`: the storm's periodic SMI, the journal sent to the
    /// board's BMC through `bmc`), the FCH's sources ([`crate::smm`]), the
    /// SMI commands of the PSP's SMM info and EINJ, and the USB and PSP
    /// SMIs.
    #[inline(always)]
    pub fn handle(
        ctx: &mut SmmContext<'_>,
        config: &SmmConfig<Sp3>,
        policy: &Policy,
        bmc: impl FnMut(&Event<'_>, Tsc) -> Delivery,
    ) {
        crate::smm::with(config, |fch, smu| {
            let first = ctx.first_owner();
            ras::own(&policy.ras, fch.acpi(), first, || ctx.cpus_held(), bmc);
            let handler = Handler {
                fch,
                smu,
                config,
                policy,
            };
            smi::dispatch(fch.acpi(), fch.io(), &config.fch.ports, &handler);
            // After the SMI's end: the PSP may send its next request.
            smm_psp::release();
        });
    }

    struct Handler<'a> {
        fch: &'a SmmFch,
        smu: Option<&'a Smu<Ecam, Tsc>>,
        config: &'a SmmConfig<Sp3>,
        policy: &'a Policy,
    }

    impl smi::Handler for Handler<'_> {
        fn command(&self, command: u8) {
            let (config, timer) = (self.config, &self.fch.timer);
            let fch = &config.fch;
            match command {
                _ if command == fch.acpi_enable || command == fch.acpi_disable => {
                    sx::acpi_mode(self.fch, &fch.ports, command == fch.acpi_enable)
                }
                sw::PSP_SMM_INFO => smm_info(config, timer, self.policy.memory_einj),
                crate::sp3::einj::SW_BEGIN
                | crate::sp3::einj::SW_END
                | crate::sp3::einj::SW_EXECUTE => {
                    // SAFETY: shared dispatch owner; registered SMRAM page.
                    unsafe {
                        super::einj::dispatch(
                            command,
                            &psp(config).with_timeout(20_000),
                            timer,
                            |address| einj_target(config, address),
                            &config.soc.einj.targets,
                            &config.soc.einj.buses.map(|bus| Smn::on_bus(Ecam, bus)),
                        );
                    }
                }
                // The local SMI status, not this forgeable command, authorizes
                // MCA capture and native #MC handoff in `enter_cpu`.
                ras::MCE_SMI_COMMAND | ras::DIAG_COMMAND => {}
                smi::sw::FCH_LOCK if config.soc.generation == Generation::Milan => {
                    fch_lock(config, timer)
                }
                command => {
                    if let Some(bmc) = config.bmc {
                        // SAFETY: the board's KCS ports; SMM owns the CPUs.
                        unsafe { bmc.report(command, *timer) };
                    }
                }
            }
        }

        fn sources(&self, pending: &Pending) {
            let (fch, config) = (self.fch, self.config);
            let acpi = fch.acpi();
            if pending.has(source::USB_NBIO) {
                sx::usb_smi(fch, config.fch.version, config.soc.usb_buses());
            }
            // The PSP's fake SMI, routed by `fch::smi::route_psp`, raised by
            // the PSP setting SmiTrig0[25] (SmmInfo `trigger`;
            // AmdPspP2CmboxV2Ssp registers `{0x84, 2}` on the FCH misc
            // dispatcher, PSP_APOB §3.4).
            if pending.has(source::PSP) {
                smm_psp::answer(acpi, pending);
            }
            if config.soc.generation == Generation::Rome
                && fstart_driver_amd::fch::huashan::smm::take_cf9_trap(fch)
            {
                // The trapped write was a reset request: let it through.
                fstart_driver_amd::fch::huashan::smm::disarm_cf9_trap(fch);
                fch.io().write8(0xcf9, 0x06);
            }
        }

        fn power_button(&self) {
            let fch = &self.config.fch;
            if !sx::sci_enabled(self.fch.io(), &fch.ports) {
                sx::power_button(self.fch, &fch.ports, fch.version, self.smu);
            }
        }

        fn sleep(&self, slp_typ: u8) {
            let (fch, soc) = (&self.config.fch, &self.config.soc);
            sx::enter(
                self.fch,
                &fch.ports,
                fch.version,
                slp_typ,
                self.smu,
                soc.usb_buses(),
            );
        }
    }

    fn psp(config: &SmmConfig<Sp3>) -> Mailbox<MmioTransport> {
        // SAFETY: the PSP BAR the boot flow programmed and left mapped.
        unsafe {
            Mailbox::mmio(
                u64::from(config.soc.psp_bar),
                config.soc.generation.silicon().into(),
            )
        }
    }

    /// SmmInfo with this handler's buffers (`crate::smm::psp`), then
    /// Rome's EINJ backend. Once per boot, as AGESA's driver entry: the OS
    /// can raise SW SMI D0 too.
    #[inline(never)]
    fn smm_info(config: &SmmConfig<Sp3>, timer: &Tsc, memory_einj: bool) {
        let Some(registration) = smm_psp::register(&psp(config), timer, |info| info) else {
            return;
        };
        // SAFETY: the word the installer reserved for it; only this
        // handler writes it.
        unsafe {
            fstart_core::mmio::write32(
                config.soc.smm_info as *mut u32,
                crate::smm::status::of(registration),
            );
        }
        // Bounded read-only backend diagnostics in the fixed comm page.
        unsafe {
            fstart_core::mmio::write64(
                (crate::sp3::einj::BASE + 224) as *mut u64,
                registration.map_or_else(super::einj::status, |_| 0),
            );
            fstart_core::mmio::write64((crate::sp3::einj::BASE + 232) as *mut u64, 0);
            fstart_core::mmio::write64(
                (crate::sp3::einj::BASE + 240) as *mut u64,
                u64::from(memory_einj),
            );
        }
        let supported = registration.is_ok()
            && memory_einj
            && config.soc.generation == Generation::Rome
            && (1..=2).contains(&config.soc.einj.sockets);
        // SAFETY: SmmInfo registered the static C2P page. Enabling PSP
        // injection support does not inject; target validation follows it
        // in vendor order and is mandatory before publishing EINJ.
        unsafe {
            super::einj::init(
                supported,
                &psp(config).with_timeout(20_000),
                timer,
                config.soc.einj.sockets,
                &config.soc.einj.targets,
                || {
                    config
                        .soc
                        .einj
                        .targets
                        .iter()
                        .enumerate()
                        .any(|(s, &address)| {
                            address != 0
                                && einj_target(config, address)
                                    .is_some_and(|t| usize::from(t.socket) == s)
                        })
                },
            );
        }
    }

    fn einj_target(
        config: &SmmConfig<Sp3>,
        address: u64,
    ) -> Option<fstart_driver_amd::psp::ras::MemoryTarget> {
        let config = &config.soc;
        if config.generation == Generation::Rome {
            use fstart_driver_amd::df::{FICAA, FICAD_LO, Ficaa, ficaa};
            use fstart_pci::ConfigRegister;
            use tock_registers::interfaces::Writeable;
            // PCI addresses and endpoint buses are bound by the installer.
            if !(1..=2).contains(&config.einj.sockets) {
                return None;
            }
            let count = core::cell::Cell::new(0u64);
            let trace = |tag: u32, value: u32| {
                let n = count.get();
                if n < 32 {
                    // Fixed output-only trace, never used as input.
                    unsafe {
                        fstart_core::mmio::write64(
                            (crate::sp3::einj::BASE + 256 + n * 8) as *mut u64,
                            u64::from(tag) | (u64::from(value) << 32),
                        );
                        fstart_core::mmio::write64(
                            (crate::sp3::einj::BASE + 232) as *mut u64,
                            n + 1,
                        );
                    }
                    count.set(n + 1);
                }
                value
            };
            let result = fstart_driver_amd::psp::ras::memory_target(
                address,
                config.einj.sockets,
                |socket, instance, reg| {
                    let Some(&function) = config.einj.df.get(usize::from(socket)) else {
                        return u32::MAX;
                    };
                    if function.device() != 0x18 + socket || function.function() != 4 {
                        return u32::MAX;
                    }
                    let index =
                        unsafe { ConfigRegister::<_, Ficaa::Register>::new(Ecam, function, FICAA) };
                    let data = unsafe { ConfigRegister::<_, ()>::new(Ecam, function, FICAD_LO) };
                    let saved = index.get();
                    index.write(ficaa(
                        reg,
                        instance,
                        instance != fstart_driver_amd::psp::ras::BROADCAST,
                    ));
                    let value = data.get();
                    index.set(saved);
                    trace(
                        (u32::from(socket) << 30)
                            | (u32::from(instance & 0x7f) << 20)
                            | (u32::from(reg.function()) << 16)
                            | u32::from(reg.register()),
                        value,
                    )
                },
                |socket, address| {
                    let Some(&bus) = config.einj.buses.get(usize::from(socket)) else {
                        return u32::MAX;
                    };
                    // SAFETY: the handler owns this bus's SMN index/data pair while it runs.
                    let smn = unsafe { Smn::on_bus(Ecam, bus) };
                    let value = smn.with_saved_index(|s| s.read32(address));
                    trace(address | 0x8000_0000 | (u32::from(socket) << 30), value)
                },
            );
            return result;
        }
        let _ = (config, address);
        None
    }

    /// SW SMI D2 (Milan ReadyToBoot): LockFCHReg (C2P 0x30) with the SMM
    /// flag set, through the C2P page while it is the handler's. The
    /// donor's second LockFCHReg (0x3A) has an untraced gate and is not
    /// sent.
    fn fch_lock(config: &SmmConfig<Sp3>, timer: &Tsc) {
        use fstart_driver_amd::psp::command::LockFchReg;
        // SAFETY: the rendezvous owner.
        let _ = unsafe { smm_psp::send(&psp(config), timer, LockFchReg) };
    }
}
