//! SMM: TSEG placement, the FCH SMI control the arch installer drives, and
//! the native FCH SMI handler (ACPI_SMM §4, FCH_SP3 §5, PSP_APOB §3.3).
//!
//! Installation is the arch crate's AMD64 path
//! (`fstart_arch::x86::cpu::amd::smm::AmdSmm`): every thread programs
//! SMM_ADDR/SMM_MASK/SMBASE and sets HWCR.SmmLock, no SMI relocation. The
//! FCH half is [`Sp3Smi`]: SMIs off while TSEG moves, then the boot
//! sources the donors leave routed (SW SMI, SLP_TYP, power button; Rome's
//! CF9 trap).
//!
//! The handler ([`Sp3SmmHandler`], feature `smm`) handles ACPI enable/disable,
//! Sx entry, power button,
//! the NBIO USB SMI, Milan's PSP FCH lock (SW SMI D2) and the PSP SmmInfo
//! registration ([`sw::PSP_SMM_INFO`]). Boards can opt into [`ras`]'s MCA
//! observer: CPU-local capture before native #MC, with OS-owned banks and
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

use fstart_driver_amd::sp3::apob::sysmap::Region;
use fstart_driver_amd::sp3::soc::Generation;

use crate::layout::Span;
use crate::memory::FOUR_GIB;

/// TSEG size: 8 MiB, as the donors' SMM_MASK (PSP_APOB §3.3 example
/// `0xffff_ff80_0000`-style masks); a power of two the MSR pair describes.
pub const TSEG_SIZE: u64 = 8 << 20;

/// fstart-private software SMI numbers besides the FCH ones
/// (`fch::smi::sw`).
pub mod sw {
    /// Register the handler's PSP buffers with the PSP (SmmInfo, C2P 0x02).
    pub const PSP_SMM_INFO: u8 = 0xD0;
}

/// The highest `size`-aligned window of `size` bytes inside one RAM region
/// below `tom` (and 4 GiB) that overlaps none of `avoid`.
pub fn place_tseg(regions: &[Region], tom: u64, size: u64, avoid: &[Span]) -> Option<Span> {
    let limit = tom.min(FOUR_GIB);
    let fit = |r: &Region| {
        let mut top = r.end().min(limit) & !(size - 1);
        while top >= r.base + size {
            let candidate = Span::new(top - size, size);
            match avoid.iter().find(|a| a.overlaps(&candidate)) {
                Some(a) => top = a.base & !(size - 1),
                None => return Some(candidate),
            }
        }
        None
    };
    regions
        .iter()
        .filter(|r| r.kind.is_ram() && r.base < limit)
        .filter_map(fit)
        .max_by_key(|s| s.base)
}

/// What the normal-mode installer writes into SMRAM for the handler.
///
/// The SMM entry borrows this immutable block in locked SMRAM rather than
/// copying it, including the per-socket EINJ endpoints and owned pages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub struct SmmConfig {
    /// ECAM base for the handler's config accesses (SMN on bus 0).
    pub ecam_base: u64,
    /// Socket 0's PSP BAR (Naples: die 0), for SmmInfo and the FCH lock.
    pub psp_bar: u32,
    pub einj: EinjConfig,
    /// Root buses whose IOHC acknowledges the USB SMI (every die on Naples,
    /// socket 0 on Rome/Milan) and, on Naples, takes the S3 handshake.
    pub usb_buses: [u8; 8],
    pub usb_bus_count: u8,
    pub generation: Generation,
    /// Sleep-state notification to the BMC, if the board has one.
    pub bmc: Option<BmcSxNotify>,
}

/// A board's BMC sleep-state notification: the DSDT writes `sw_smi_base +
/// Sx` to the SMI command port (`\_PTS`, `\_WAK`, `\_SB._INI`), and the
/// handler sends Set ACPI Power State over the KCS interface (AMI
/// BmcAcpi's SW SMIs 0xC0–0xC5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub struct BmcSxNotify {
    pub kcs_port: u16,
    pub kcs_spacing: u8,
    pub sw_smi_base: u8,
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

impl SmmConfig {
    pub fn usb_buses(&self) -> &[u8] {
        &self.usb_buses[..usize::from(self.usb_bus_count).min(self.usb_buses.len())]
    }
}

/// The FCH side of SMM installation on socket 0's FCH.
#[cfg(feature = "bundle-reset")]
pub struct Sp3Smi<'a> {
    pub fch: &'a crate::hw::PrimaryFch,
    pub config: SmmConfig,
}

#[cfg(feature = "bundle-reset")]
impl fstart_arch::x86::cpu::amd::smm::SmiControl for Sp3Smi<'_> {
    type HandlerConfig = SmmConfig;

    fn smm_handler_config(&self) -> SmmConfig {
        self.config
    }

    fn disable_smi(&self) {
        use fstart_driver_amd::sp3::fch::FchHw;
        use fstart_driver_amd::sp3::fch::Window;
        use fstart_driver_amd::sp3::fch::smi::TRIG0;
        use tock_registers::interfaces::ReadWriteable;
        // SmiEnB: the FCH generates no SMI until `enable_smi`.
        self.fch
            .acpi()
            .reg::<u32, TRIG0::Register>(fstart_driver_amd::sp3::fch::smi::TRIG0)
            .modify(TRIG0::SMI_EN_B::SET);
    }

    fn enable_smi(&self) {
        use fstart_driver_amd::sp3::fch::FchHw;
        fstart_driver_amd::sp3::fch::smi::enable_boot_sources(self.fch.acpi());
        #[cfg(any(feature = "rome", feature = "milan"))]
        if self.config.generation == Generation::Rome {
            fstart_driver_amd::sp3::fch::huashan::smm::arm_cf9_trap(self.fch);
        }
    }
}

/// The FCH bits AGESA sets before SmmInfo (PSP_APOB §3.3): SmiStatus1
/// bit 1 cleared, SmiTrig0 bit 25 cleared, PSP SMI control = SMI.
#[cfg(feature = "bundle-reset")]
pub fn prepare_psp_smi(fch: &crate::hw::PrimaryFch) {
    use fstart_driver_amd::sp3::fch::{FchHw, Window, acpimmio, update};
    let acpi = fch.acpi();
    update::<u32, _>(acpi, acpimmio::SMI + 0x84, 0, 1 << 1);
    update::<u32, _>(acpi, acpimmio::SMI + 0x98, 1 << 25, 0);
    update::<u32, _>(acpi, acpimmio::SMI + 0xa8, 0xc, 0x4);
    let _ = acpi.read32(acpimmio::SMI + 0xa8);
}

#[cfg(feature = "smm")]
pub use handler::Sp3SmmHandler;

#[cfg(feature = "smm")]
pub mod ras;

#[cfg(feature = "smm")]
mod einj;

#[cfg(feature = "smm")]
mod handler {
    use core::cell::UnsafeCell;

    use fstart_arch::x86::cpu::amd::msr::{SMM_ADDR, SMM_MASK};
    use fstart_arch::x86::cpu::msr::MsrAccess;
    use fstart_driver_amd::smn::Smn;
    use fstart_driver_amd::smu::Smu;
    use fstart_driver_amd::sp3::fch::common::sx;
    use fstart_driver_amd::sp3::fch::smi::Source;
    use fstart_driver_amd::sp3::fch::smi::{self, source};
    use fstart_driver_amd::sp3::fch::{Fch, FchHw, Mmio, Ports, Window, acpimmio, update};
    use fstart_driver_amd::sp3::psp::command::{C2pBuffer, P2C_BUFFER_SIZE, SmmInfo};
    use fstart_driver_amd::sp3::psp::mailbox::{Mailbox, MmioTransport};
    use fstart_driver_amd::sp3::soc::Generation;
    use fstart_pci::{ConfigRegionAccess, EcamDevice, PciAddress};
    use fstart_smm::{SmmContext, SmmHandler};
    use tock_registers::interfaces::Readable;

    use super::{SmmConfig, sw};
    use crate::hw::{Tsc, msr};

    /// Config space in SMM: the global ECAM window (`ecam::init` with the
    /// installer's base on every SMI) without `GlobalEcam`'s panic on an
    /// unmapped register, which the SMM image builder rejects. Such an
    /// access reads all ones and drops the write, like a master abort.
    #[derive(Debug, Clone, Copy, Default)]
    struct Ecam;

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

    /// Socket 0's FCH as `hw::primary_fch`, over [`Ecam`].
    type SmmFch = Fch<Mmio, Mmio, Ports, Ecam, Tsc>;

    fn primary_fch(timer: Tsc) -> SmmFch {
        // SAFETY: SMM owns the CPU; the FCH ACPI-MMIO block, MMIO and
        // ports are mapped by the handler's identity tables.
        unsafe {
            Fch {
                acpi: Mmio::new(acpimmio::BASE as usize),
                mmio: Mmio::new(0),
                io: Ports::new(),
                pci: Ecam,
                bus: 0,
                timer,
            }
        }
    }

    #[repr(C, align(4096))]
    struct Page<const N: usize>(UnsafeCell<[u8; N]>);
    // SAFETY: only the SMM rendezvous owner touches the pages.
    unsafe impl<const N: usize> Sync for Page<N> {}

    /// PSP-to-CPU buffer and the C2P page (command buffer at +0, SMM flag
    /// at +0xC00), in the handler's SMRAM.
    static P2C: Page<{ P2C_BUFFER_SIZE as usize }> =
        Page(UnsafeCell::new([0; P2C_BUFFER_SIZE as usize]));
    static C2P: Page<0x1000> = Page(UnsafeCell::new([0; 0x1000]));

    /// The PSP's fake SMI: SmiStat1\[1\], routed by SmiControl `0xA8[3:2]`
    /// (`prepare_psp_smi`), raised by the PSP setting SmiTrig0\[25\]
    /// (SmmInfo `trigger`; AmdPspP2CmboxV2Ssp registers `{0x84, 2}` on
    /// the FCH misc dispatcher, PSP_APOB §3.4).
    const PSP_SMI: Source = Source::new(0x84, 1);
    const PSP_TRIGGER: u32 = 1 << 25;

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

    /// The SP3 FCH SMI dispatcher.
    pub struct Sp3SmmHandler<P = super::ras::OsFirst>(core::marker::PhantomData<P>);

    impl<P: super::ras::Policy> SmmHandler for Sp3SmmHandler<P> {
        type Config = SmmConfig;

        unsafe fn enter_cpu(ctx: &mut SmmContext<'_>, config: &SmmConfig) {
            super::ras::enter::<P>(ctx.cpu(), config.generation);
        }

        unsafe fn handle(_ctx: &mut SmmContext<'_>, config: &SmmConfig) {
            fstart_pci::ecam::init(config.ecam_base as usize);
            // The OS may be in the middle of an SMN access on bus 0
            // (00:00.0, `PciAddress::default`).
            let host = PciAddress::default();
            let pci = Ecam;
            // SAFETY: SMM owns the CPU; the index is restored below.
            let saved = unsafe { pci.read(host, fstart_driver_amd::smn::SMN_INDEX) };
            super::ras::flush::<P>(crate::hw::tsc());
            dispatch::<P>(config);
            // SAFETY: as above.
            unsafe { pci.write(host, fstart_driver_amd::smn::SMN_INDEX, saved) };
        }
    }

    fn dispatch<P: super::ras::Policy>(config: &SmmConfig) {
        let timer = crate::hw::tsc();
        let hw = primary_fch(timer);
        let acpi = hw.acpi();
        let generation = config.generation;
        // The installer only writes a generation this image carries.
        let Some(smu) = smu(generation, timer) else {
            smi::eos(acpi);
            return;
        };

        if smi::is_pending(acpi, source::SMI_CMD) {
            let command = hw.io().read8(acpimmio::SMI_CMD);
            smi::clear(acpi, source::SMI_CMD);
            match command {
                smi::sw::ACPI_ENABLE => sx::acpi_enable(&hw),
                smi::sw::ACPI_DISABLE => sx::acpi_disable(&hw),
                sw::PSP_SMM_INFO => smm_info(config, &timer, P::MEMORY_EINJ),
                crate::einj::SW_BEGIN | crate::einj::SW_END | crate::einj::SW_EXECUTE => {
                    // SAFETY: shared dispatch owner; registered SMRAM page.
                    unsafe {
                        super::einj::dispatch(
                            command,
                            &psp(config).with_timeout(20_000),
                            &timer,
                            C2P.0.get().cast(),
                            |address| einj_target(config, address),
                            &config.einj.targets,
                            &config.einj.buses.map(|bus| Smn::on_bus(Ecam, bus)),
                        );
                    }
                }
                // The local SMI status, not this forgeable command, authorizes
                // MCA capture and native #MC handoff in `enter_cpu`.
                super::ras::MCE_SMI_COMMAND | super::ras::DIAG_COMMAND => {}
                #[cfg(feature = "milan")]
                smi::sw::FCH_LOCK if generation == Generation::Milan => fch_lock(config, &timer),
                command => {
                    if let Some(bmc) = config.bmc {
                        bmc_notify(&bmc, command, timer);
                    }
                }
            }
        }
        if smi::is_pending(acpi, source::SLP_TYP) {
            let slp_typ = (hw.io().read16(acpimmio::PM1_CNT) >> 10 & 7) as u8;
            // Returns only for S1/S2 or on an SMU error; the OS sees the
            // wake (or the failed sleep) either way.
            let _ = sx::enter(&hw, generation, slp_typ, &smu, config.usb_buses());
            smi::clear(acpi, source::SLP_TYP);
        }
        if smi::is_pending(acpi, source::PWR_BTN) {
            let _ = sx::power_button(&hw, generation, &smu);
        }
        if smi::is_pending(acpi, source::USB_NBIO) {
            sx::usb_smi(&hw, generation, config.usb_buses());
            smi::clear(acpi, source::USB_NBIO);
        }
        if smi::is_pending(acpi, PSP_SMI) {
            p2c_answer();
            // AmdPspP2CmboxV2Ssp `0x944` (instruction-checked): answer,
            // then drop SmiTrig0[25]; its dispatcher then clears the status.
            // A trigger left set keeps the source asserted.
            update::<u32, _>(acpi, smi::TRIG0, PSP_TRIGGER, 0);
            smi::clear(acpi, PSP_SMI);
        }
        #[cfg(any(feature = "rome", feature = "milan"))]
        if generation == Generation::Rome
            && fstart_driver_amd::sp3::fch::huashan::smm::take_cf9_trap(&hw)
        {
            // The trapped write was a reset request: let it through.
            fstart_driver_amd::sp3::fch::huashan::smm::disarm_cf9_trap(&hw);
            hw.io().write8(0xcf9, 0x06);
        }
        smi::eos(acpi);
    }

    /// Socket 0's (Naples: die 0's) SMU mailbox through bus 0.
    fn smu(generation: Generation, timer: Tsc) -> Option<Smu<Ecam, Tsc>> {
        // SAFETY: the handler owns this bus's SMN index/data pair while it runs.
        let smn = unsafe { Smn::on_bus(Ecam, 0) };
        let (regs, limit) = match generation {
            #[cfg(feature = "naples")]
            Generation::Naples => (
                fstart_driver_amd::smu::v9::MAILBOX,
                fstart_driver_amd::smu::v9::ID_LIMIT,
            ),
            #[cfg(any(feature = "rome", feature = "milan"))]
            Generation::Rome | Generation::Milan => {
                use fstart_driver_amd::smu::v11;
                (v11::MAILBOX, v11::id_limit(generation))
            }
            #[allow(unreachable_patterns)]
            _ => return None,
        };
        Some(
            Smu::new(smn, regs, timer, limit)
                .with_timeout_us(crate::hw::SMU_TIMEOUT_US)
                .with_poll_us(crate::hw::SMU_POLL_US),
        )
    }

    fn psp(config: &SmmConfig) -> Mailbox<MmioTransport> {
        // SAFETY: the PSP BAR the boot flow programmed and left mapped.
        Mailbox::new(
            unsafe { MmioTransport::new(u64::from(config.psp_bar)) },
            config.generation,
        )
    }

    /// The P2C buffer's dword `index`.
    #[inline(never)]
    fn p2c_word(index: usize) -> *mut u32 {
        // SAFETY: in bounds of the SMRAM static; the caller owns the
        // rendezvous.
        unsafe { P2C.0.get().cast::<u32>().add(index) }
    }

    /// Report sleep state `command - sw_smi_base` to the BMC. A KCS error
    /// is dropped: the OS's sleep goes on either way.
    fn bmc_notify(bmc: &super::BmcSxNotify, command: u8, timer: Tsc) {
        let Some((system, device)) = command
            .checked_sub(bmc.sw_smi_base)
            .and_then(fstart_driver_ipmi::sleep_power_states)
        else {
            return;
        };
        // SAFETY: the board's KCS ports; SMM owns the CPU.
        let mut kcs = unsafe {
            fstart_driver_ipmi::kcs::Kcs::new(bmc.kcs_port, bmc.kcs_spacing.into(), timer)
        };
        let _ = kcs.set_acpi_power_state(system, device);
    }

    /// SmmInfo (PSP_APOB §3.3) with this handler's buffers, the P2C
    /// mailbox idle and ready first as on every donor (see [`p2c::READY`];
    /// the driver's `P2C_HEADER1_MILAN` is the same bit). Once per boot, as
    /// AGESA's driver entry: the OS can raise SW SMI D0 too.
    #[inline(never)]
    fn smm_info(config: &SmmConfig, timer: &Tsc, memory_einj: bool) {
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
            ((crate::einj::BASE + 224) as *mut u64)
                .write_volatile(registration.map_or_else(super::einj::status, |_| 0));
            ((crate::einj::BASE + 232) as *mut u64).write_volatile(0);
            ((crate::einj::BASE + 240) as *mut u64).write_volatile(u64::from(memory_einj));
        }
        let supported = registration.is_ok()
            && memory_einj
            && config.generation == Generation::Rome
            && (1..=2).contains(&config.einj.sockets);
        // SAFETY: SmmInfo registered the static C2P page. Enabling PSP
        // injection support does not inject; target validation follows it
        // in vendor order and is mandatory before publishing EINJ.
        unsafe {
            super::einj::init(
                supported,
                &psp(config).with_timeout(20_000),
                timer,
                C2P.0.get().cast(),
                config.einj.sockets,
                &config.einj.targets,
                || {
                    config.einj.targets.iter().enumerate().any(|(s, &address)| {
                        address != 0
                            && einj_target(config, address)
                                .is_some_and(|t| usize::from(t.socket) == s)
                    })
                },
            );
        }
    }

    fn einj_target(
        config: &SmmConfig,
        address: u64,
    ) -> Option<fstart_driver_amd::sp3::psp::ras::MemoryTarget> {
        #[cfg(feature = "rome")]
        if config.generation == Generation::Rome {
            use fstart_driver_amd::sp3::df::{FICAA, regs};
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
                        ((crate::einj::BASE + 256 + n * 8) as *mut u64)
                            .write_volatile(u64::from(tag) | (u64::from(value) << 32));
                        ((crate::einj::BASE + 232) as *mut u64).write_volatile(n + 1);
                    }
                    count.set(n + 1);
                }
                value
            };
            let result = fstart_driver_amd::sp3::psp::ras::memory_target(
                address,
                config.einj.sockets,
                |socket, instance, reg| {
                    let Some(&function) = config.einj.df.get(usize::from(socket)) else {
                        return u32::MAX;
                    };
                    if function.device() != 0x18 + socket || function.function() != 4 {
                        return u32::MAX;
                    }
                    let index = unsafe {
                        ConfigRegister::<_, FICAA::Register>::new(Ecam, function, regs::FICAA)
                    };
                    let data =
                        unsafe { ConfigRegister::<_, ()>::new(Ecam, function, regs::FICAD_LO) };
                    let saved = index.get();
                    index.write(
                        FICAA::TARGET_INST.val(u32::from(
                            instance != fstart_driver_amd::sp3::psp::ras::BROADCAST,
                        )) + FICAA::FUNC.val(u32::from(reg.func))
                            + FICAA::REG.val(u32::from(reg.offset >> 2))
                            + FICAA::INST.val(u32::from(instance)),
                    );
                    let value = data.get();
                    index.set(saved);
                    trace(
                        (u32::from(socket) << 30)
                            | (u32::from(instance & 0x7f) << 20)
                            | (u32::from(reg.func) << 16)
                            | u32::from(reg.offset),
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
    #[cfg(feature = "milan")]
    fn fch_lock(config: &SmmConfig, timer: &Tsc) {
        use fstart_driver_amd::sp3::psp::command::LockFchReg;
        let page = C2P.0.get().cast::<u8>();
        let buffer = page.cast::<C2pBuffer<LockFchReg>>();
        // SAFETY: the C2P page is SMRAM owned by the rendezvous owner and
        // registered with SmmInfo; the flag byte is inside it.
        unsafe {
            buffer.write(C2pBuffer::new(LockFchReg));
            let _ = fstart_driver_amd::sp3::fch::huashan::smm::lock_fch_registers(
                &psp(config),
                timer,
                &mut *buffer,
                page.add(fstart_driver_amd::sp3::psp::command::SMM_FLAG_OFFSET as usize),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fstart_driver_amd::sp3::apob::sysmap::RegionKind;

    fn ram(base: u64, size: u64) -> Region {
        Region {
            base,
            size,
            kind: RegionKind::Ram,
        }
    }

    #[test]
    fn tseg_goes_to_the_top_of_low_ram_around_firmware() {
        let regions = [
            ram(0, 0x8000_0000),
            Region {
                base: 0x8000_0000,
                size: 0x8000_0000,
                kind: RegionKind::Mmio,
            },
            ram(FOUR_GIB, 0x1_0000_0000),
        ];
        let early = crate::layout::CACHE_WINDOW;
        let t = place_tseg(&regions, 0x8000_0000, TSEG_SIZE, &[early]).unwrap();
        assert_eq!(t, Span::new(0x7f80_0000, TSEG_SIZE));
        // TOM inside the firmware window: step below it.
        let t = place_tseg(&regions, 0x7600_0000, TSEG_SIZE, &[early]).unwrap();
        assert_eq!(t, Span::new(0x7480_0000, TSEG_SIZE));
        assert_eq!(place_tseg(&regions[1..], 0x8000_0000, TSEG_SIZE, &[]), None);
    }
}
