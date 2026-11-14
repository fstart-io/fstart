//! SMM on SP3: TSEG size, the SoC's part of the SMM configuration and its
//! handler (ACPI_SMM §4, FCH_SP3 §5, PSP_APOB §3.3) around the FCH SMI
//! dispatcher every SoC uses ([`crate::smm`]).
//!
//! Installation is `crate::tseg` with `crate::smm::Smi`: SMIs off while
//! TSEG moves, then the FCH's sources and Rome's CF9 trap.
//!
//! The handler ([`handle`], feature `smm`; the board binds it as its SMI
//! handler) adds to the FCH's ACPI mode, sleep and power button the NBIO
//! USB SMI, Milan's PSP FCH lock (SW SMI D2) and the PSP SmmInfo
//! registration ([`sw::PSP_SMM_INFO`]). Boards can opt into [`ras`]'s MCA
//! observer (a [`Policy`] value, with [`enter_cpu`]): CPU-local capture before native #MC, with OS-owned banks and
//! optional bounded SEL delivery. There is no DXE or UEFI dependency.
//! AGESA sends SmmInfo from its SMM
//! driver too; doing it from the handler puts the P2C buffer and the C2P
//! page in the handler's own SMRAM statics, whose addresses normal mode
//! cannot know before the image is placed.
//!
//! SmmInfo routes the PSP's "fake SMI" (SmiStat1\[1\], raised by the PSP
//! setting SmiTrig0\[25\]) to SMI, so the handler must answer every P2C
//! request and re-arm that trigger, or the source stays asserted and the
//! FCH raises SMIs forever. It answers like AGESA's P2C mailbox but
//! implements none of the PSP's SPI NV/RPMC/ARS services (status
//! "unsupported", as the donors answer RPMC).
//!
//! The image is built by the host plan's `smm` unit from the board crate
//! (`smm_bin!`, feature `bundle-smm`); `build.rs` checks that it fits
//! [`TSEG_SIZE`] for the board's thread count.

use crate::sp3::silicon::Generation;

/// TSEG size: 8 MiB, as the donors' SMM_MASK (PSP_APOB §3.3 example
/// `0xffff_ff80_0000`-style masks); a power of two the MSR pair describes.
pub const TSEG_SIZE: u64 = 8 << 20;

/// fstart-private software SMI numbers besides the FCH ones
/// (`fch::smi::sw`).
pub mod sw {
    /// Register the handler's PSP buffers with the PSP (SmmInfo, C2P 0x02).
    pub const PSP_SMM_INFO: u8 = 0xD0;
}

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

/// The FCH bits AGESA sets before SmmInfo (PSP_APOB §3.3): SmiStatus1
/// bit 1 cleared, SmiTrig0 bit 25 cleared, PSP SMI control = SMI.
#[cfg(feature = "bundle")]
pub fn prepare_psp_smi(fch: &crate::sp3::silicon::hw::PrimaryFch) {
    use fstart_driver_amd::fch::{FchHw, acpimmio};
    use fstart_driver_amd::window::{Window, update};
    let acpi = fch.acpi();
    update::<u32, _>(acpi, acpimmio::SMI + 0x84, 0, 1 << 1);
    update::<u32, _>(acpi, acpimmio::SMI + 0x98, 1 << 25, 0);
    update::<u32, _>(acpi, acpimmio::SMI + 0xa8, 0xc, 0x4);
    let _ = acpi.read32(acpimmio::SMI + 0xa8);
}

#[cfg(feature = "smm")]
pub use handler::{enter_cpu, handle};
#[cfg(feature = "smm")]
pub use ras::Policy;

#[cfg(feature = "smm")]
pub mod ras;

#[cfg(feature = "smm")]
mod einj;

#[cfg(feature = "smm")]
mod handler {
    use core::cell::UnsafeCell;

    use crate::smm::{Ecam, SmmConfig, SmmFch};
    use crate::sp3::silicon::Generation;
    use fstart_arch::x86::cpu::amd::msr::{SMM_ADDR, SMM_MASK};
    use fstart_arch::x86::cpu::msr::MsrAccess;
    use fstart_driver_amd::fch::smi::{self, Pending, source};
    use fstart_driver_amd::fch::{FchHw, sx};
    use fstart_driver_amd::psp::command::{C2pBuffer, P2C_BUFFER_SIZE, SmmInfo};
    use fstart_driver_amd::psp::mailbox::{Mailbox, MmioTransport};
    use fstart_driver_amd::smn::Smn;
    use fstart_driver_amd::smu::Smu;
    use fstart_driver_amd::window::{Window, update};
    use fstart_smm::SmmContext;
    use tock_registers::interfaces::Readable;

    use super::ras::Policy;
    use super::{Sp3, sw};
    use crate::sp3::silicon::hw::{Tsc, msr};

    #[repr(C, align(4096))]
    struct Page<const N: usize>(UnsafeCell<[u8; N]>);
    // SAFETY: only the SMM rendezvous owner touches the pages.
    unsafe impl<const N: usize> Sync for Page<N> {}

    /// PSP-to-CPU buffer and the C2P page (command buffer at +0, SMM flag
    /// at +0xC00), in the handler's SMRAM.
    static P2C: Page<{ P2C_BUFFER_SIZE as usize }> =
        Page(UnsafeCell::new([0; P2C_BUFFER_SIZE as usize]));
    static C2P: Page<0x1000> = Page(UnsafeCell::new([0; 0x1000]));

    /// P2C buffer: dword 0 the PSP's command, dword 1 flags, then the
    /// command buffer `{size, status, data}` (PSP_APOB §3.4; layout from
    /// AmdPspP2CmboxV2Ssp `0x720`, Rome a9724bd1, instruction-checked).
    mod p2c {
        pub const COMMAND: usize = 0;
        pub const FLAGS: usize = 1;
        pub const SIZE: usize = 2;
        pub const STATUS: usize = 3;
        /// x86 ready for the next request. Every donor sets it with
        /// COMMAND = 0 before SmmInfo (Naples d8a42c95 `0xbef`, Rome
        /// a9724bd1 `0xcef`), clears it while it works and sets it again
        /// when done.
        pub const READY: u32 = 1 << 31;
        /// The PSP asks for a byte-sum checksum in FLAGS\[7:0\].
        pub const CHECKSUM: u32 = 1 << 8;
        /// Largest command buffer that stays inside the P2C buffer (the
        /// donor accepts 0x2000, 8 bytes more than it has).
        pub const MAX_SIZE: u32 = super::P2C_BUFFER_SIZE as u32 - 8;
        /// Statuses the donor writes: done, invalid request, and what its
        /// RPMC services return ("unsupported").
        pub const OK: u32 = 0;
        pub const INVALID: u32 = 1;
        pub const UNSUPPORTED: u32 = 4;
    }

    /// CPU-local work on every CPU that enters SMM, before the rendezvous:
    /// MCA capture when `policy` observes MCEs. Bind it as the board's
    /// `enter_cpu`.
    pub fn enter_cpu(ctx: &mut SmmContext<'_>, config: &SmmConfig<Sp3>, policy: &Policy) {
        super::ras::enter(policy, ctx.cpu(), config.soc.generation);
    }

    /// The SP3 SMI handler on the rendezvous owner: the FCH's sources
    /// ([`crate::smm`]), the SMI commands of the PSP's SMM info and EINJ,
    /// the USB and PSP SMIs, and RAS reporting per `policy`.
    pub fn handle(_ctx: &mut SmmContext<'_>, config: &SmmConfig<Sp3>, policy: &Policy) {
        crate::smm::with(config, |fch, smu| {
            super::ras::flush(policy, fch.timer);
            let handler = Handler {
                fch,
                smu,
                config,
                policy,
            };
            smi::dispatch(fch.acpi(), fch.io(), &config.fch.ports, &handler);
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
                            C2P.0.get().cast(),
                            |address| einj_target(config, address),
                            &config.soc.einj.targets,
                            &config.soc.einj.buses.map(|bus| Smn::on_bus(Ecam, bus)),
                        );
                    }
                }
                // The local SMI status, not this forgeable command, authorizes
                // MCA capture and native #MC handoff in `enter_cpu`.
                super::ras::MCE_SMI_COMMAND | super::ras::DIAG_COMMAND => {}
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
            // The PSP's fake SMI, routed by `prepare_psp_smi`, raised by the
            // PSP setting SmiTrig0[25] (SmmInfo `trigger`; AmdPspP2CmboxV2Ssp
            // registers `{0x84, 2}` on the FCH misc dispatcher, PSP_APOB
            // §3.4).
            if pending.has(source::PSP) {
                p2c_answer();
                // AmdPspP2CmboxV2Ssp `0x944` (instruction-checked): answer,
                // then drop SmiTrig0[25]; its dispatcher then clears the
                // status. A trigger left set keeps the source asserted.
                update::<u32, _>(acpi, smi::TRIGGER0.offset, smi::Trigger0::PSP::SET.value, 0);
                smi::clear(acpi, source::PSP);
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
                let _ = sx::power_button(self.fch, &fch.ports, fch.version, self.smu);
            }
        }

        fn sleep(&self, slp_typ: u8) {
            let (fch, soc) = (&self.config.fch, &self.config.soc);
            let _ = sx::enter(
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
        Mailbox::new(
            unsafe { MmioTransport::new(u64::from(config.soc.psp_bar)) },
            config.soc.generation.silicon().into(),
        )
    }

    /// The P2C buffer's dword `index`.
    #[inline(never)]
    fn p2c_word(index: usize) -> *mut u32 {
        // SAFETY: in bounds of the SMRAM static; the caller owns the
        // rendezvous.
        unsafe { P2C.0.get().cast::<u32>().add(index) }
    }

    /// SmmInfo (PSP_APOB §3.3) with this handler's buffers, the P2C
    /// mailbox idle and ready first as on every donor (see [`p2c::READY`];
    /// the driver's `P2C_HEADER1_MILAN` is the same bit). Once per boot, as
    /// AGESA's driver entry: the OS can raise SW SMI D0 too.
    #[inline(never)]
    fn smm_info(config: &SmmConfig<Sp3>, timer: &Tsc, memory_einj: bool) {
        use core::sync::atomic::{AtomicBool, Ordering};
        static SENT: AtomicBool = AtomicBool::new(false);
        if SENT.swap(true, Ordering::AcqRel) {
            return;
        }
        let cpu = msr();
        let p2c = P2C.0.get() as u64;
        let c2p = C2P.0.get() as u64;
        // SAFETY: SMRAM static, owned by the rendezvous owner.
        unsafe {
            p2c_word(p2c::COMMAND).write_volatile(0);
            p2c_word(p2c::FLAGS).write_volatile(p2c::READY);
        }
        let info = SmmInfo::new(cpu.reg(SMM_ADDR).get(), cpu.reg(SMM_MASK).get(), p2c, c2p);
        let registration = psp(config).send(timer, &mut C2pBuffer::new(info));
        // Bounded read-only backend diagnostics in the fixed comm page.
        unsafe {
            ((crate::sp3::einj::BASE + 224) as *mut u64)
                .write_volatile(registration.map_or_else(super::einj::status, |_| 0));
            ((crate::sp3::einj::BASE + 232) as *mut u64).write_volatile(0);
            ((crate::sp3::einj::BASE + 240) as *mut u64).write_volatile(u64::from(memory_einj));
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
                C2P.0.get().cast(),
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
                        ((crate::sp3::einj::BASE + 256 + n * 8) as *mut u64)
                            .write_volatile(u64::from(tag) | (u64::from(value) << 32));
                        ((crate::sp3::einj::BASE + 232) as *mut u64).write_volatile(n + 1);
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

    /// Answer the PSP's P2C request the way AGESA's mailbox does
    /// (AmdPspP2CmboxV2Ssp `0x720`): busy, status, checksum when asked,
    /// write back, command 0, ready. 0x81/0x82 need nothing from x86; the
    /// SPI NV (0x83–0x86), RPMC (0x88/0x89) and ARS (0x8a) services are
    /// not implemented and get the donor's RPMC answer, anything else
    /// "invalid".
    fn p2c_answer() {
        // SAFETY: the P2C static is SMRAM owned by the rendezvous owner;
        // the PSP only writes it while READY is set.
        unsafe {
            let flags = p2c_word(p2c::FLAGS).read_volatile() & !p2c::READY;
            p2c_word(p2c::FLAGS).write_volatile(flags);
            let size = p2c_word(p2c::SIZE).read_volatile();
            let status = match p2c_word(p2c::COMMAND).read_volatile() {
                _ if size > p2c::MAX_SIZE => p2c::INVALID,
                0x81 | 0x82 => p2c::OK,
                0x83..=0x86 | 0x88..=0x8a => p2c::UNSUPPORTED,
                _ => p2c::INVALID,
            };
            p2c_word(p2c::STATUS).write_volatile(status);
            if status == p2c::OK && flags & p2c::CHECKSUM != 0 {
                let bytes = P2C.0.get().cast::<u8>().add(4 * p2c::SIZE);
                let sum = (0..size as usize)
                    .map(|i| bytes.add(i).read_volatile())
                    .fold(0u8, u8::wrapping_add);
                p2c_word(p2c::FLAGS).write_volatile(flags & !0xff | u32::from(sum));
            }
            fstart_arch::x86::writeback_invalidate_caches();
            p2c_word(p2c::COMMAND).write_volatile(0);
            let flags = p2c_word(p2c::FLAGS).read_volatile();
            p2c_word(p2c::FLAGS).write_volatile(flags | p2c::READY);
            fstart_arch::x86::writeback_invalidate_caches();
        }
    }

    /// SW SMI D2 (Milan ReadyToBoot): LockFCHReg through the C2P page.
    fn fch_lock(config: &SmmConfig<Sp3>, timer: &Tsc) {
        use fstart_driver_amd::psp::command::LockFchReg;
        let page = C2P.0.get().cast::<u8>();
        let buffer = page.cast::<C2pBuffer<LockFchReg>>();
        // SAFETY: the C2P page is SMRAM owned by the rendezvous owner and
        // registered with SmmInfo; the flag byte is inside it.
        unsafe {
            buffer.write(C2pBuffer::new(LockFchReg));
            let _ = fstart_driver_amd::fch::huashan::smm::lock_fch_registers(
                &psp(config),
                timer,
                &mut *buffer,
                page.add(fstart_driver_amd::psp::command::SMM_FLAG_OFFSET as usize),
            );
        }
    }
}
