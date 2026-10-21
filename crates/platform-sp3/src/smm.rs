//! SMM: TSEG placement, the FCH SMI control the arch installer drives, and
//! the OS-first FCH SMI handler (ACPI_SMM §4, FCH_SP3 §5, PSP_APOB §3.3).
//!
//! Installation is the arch crate's AMD64 path
//! (`fstart_arch::x86::cpu::amd::smm::AmdSmm`): every thread programs
//! SMM_ADDR/SMM_MASK/SMBASE and sets HWCR.SmmLock, no SMI relocation. The
//! FCH half is [`Sp3Smi`]: SMIs off while TSEG moves, then the boot
//! sources the donors leave routed (SW SMI, SLP_TYP, power button; Rome's
//! CF9 trap).
//!
//! The handler ([`Sp3SmmHandler`], feature `smm`) is OS-first like the
//! donors' SP3 dispatchers: ACPI enable/disable, Sx entry, power button,
//! the NBIO USB SMI, Milan's PSP FCH lock (SW SMI D2) and the PSP SmmInfo
//! registration ([`sw::PSP_SMM_INFO`]). AGESA sends SmmInfo from its SMM
//! driver too; doing it from the handler puts the P2C buffer and the C2P
//! page in the handler's own SMRAM statics, whose addresses normal mode
//! cannot know before the image is placed.

use fstart_driver_zen::apob::sysmap::Region;
use fstart_driver_zen::soc::Generation;

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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub struct SmmConfig {
    pub generation: Generation,
    /// ECAM base for the handler's config accesses (SMN on bus 0).
    pub ecam_base: u64,
    /// Socket 0's PSP BAR (Naples: die 0), for SmmInfo and the FCH lock.
    pub psp_bar: u64,
    /// Root buses whose IOHC acknowledges the USB SMI (every die on Naples,
    /// socket 0 on Rome/Milan) and, on Naples, takes the S3 handshake.
    pub usb_buses: [u8; 8],
    pub usb_bus_count: u8,
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
        use fstart_driver_zen::fch::FchHw;
        use fstart_driver_zen::fch::Window;
        use fstart_driver_zen::fch::smi::TRIG0;
        use tock_registers::interfaces::ReadWriteable;
        // SmiEnB: the FCH generates no SMI until `enable_smi`.
        self.fch
            .acpi()
            .reg::<u32, TRIG0::Register>(fstart_driver_zen::fch::smi::TRIG0)
            .modify(TRIG0::SMI_EN_B::SET);
    }

    fn enable_smi(&self) {
        use fstart_driver_zen::fch::FchHw;
        fstart_driver_zen::fch::smi::enable_boot_sources(self.fch.acpi());
        #[cfg(any(feature = "rome", feature = "milan"))]
        if self.config.generation == Generation::Rome {
            fstart_driver_zen::fch::huashan::smm::arm_cf9_trap(self.fch);
        }
    }
}

/// The FCH bits AGESA sets before SmmInfo (PSP_APOB §3.3): SmiStatus1
/// bit 1 cleared, SmiTrig0 bit 25 cleared, PSP SMI control = SMI.
#[cfg(feature = "bundle-reset")]
pub fn prepare_psp_smi(fch: &crate::hw::PrimaryFch) {
    use fstart_driver_zen::fch::{FchHw, Window, acpimmio, update};
    let acpi = fch.acpi();
    update::<u32, _>(acpi, acpimmio::SMI + 0x84, 0, 1 << 1);
    update::<u32, _>(acpi, acpimmio::SMI + 0x98, 1 << 25, 0);
    update::<u32, _>(acpi, acpimmio::SMI + 0xa8, 0xc, 0x4);
    let _ = acpi.read32(acpimmio::SMI + 0xa8);
}

#[cfg(feature = "smm")]
pub use handler::Sp3SmmHandler;

#[cfg(feature = "smm")]
mod handler {
    use core::cell::UnsafeCell;

    use fstart_arch::x86::cpu::amd::msr::{SMM_ADDR, SMM_MASK};
    use fstart_arch::x86::cpu::msr::MsrAccess;
    use fstart_driver_zen::fch::common::sx;
    use fstart_driver_zen::fch::smi::{self, source};
    use fstart_driver_zen::fch::{FchHw, Window, acpimmio};
    use fstart_driver_zen::psp::command::{C2pBuffer, P2C_BUFFER_SIZE, SmmInfo};
    use fstart_driver_zen::psp::mailbox::{Mailbox, MmioTransport};
    use fstart_driver_zen::smn::Smn;
    use fstart_driver_zen::smu::mailbox::Mailbox as Smu;
    use fstart_driver_zen::soc::Generation;
    use fstart_pci::{ConfigRegionAccess, PciAddress};
    use fstart_smm::{SmmContext, SmmHandler};
    use tock_registers::interfaces::Readable;

    use super::{SmmConfig, sw};
    use crate::hw::{Pci, Tsc, msr, primary_fch};

    #[repr(C, align(4096))]
    struct Page<const N: usize>(UnsafeCell<[u8; N]>);
    // SAFETY: only the SMM rendezvous owner touches the pages.
    unsafe impl<const N: usize> Sync for Page<N> {}

    /// PSP-to-CPU buffer and the C2P page (command buffer at +0, SMM flag
    /// at +0xC00), in the handler's SMRAM.
    static P2C: Page<{ P2C_BUFFER_SIZE as usize }> =
        Page(UnsafeCell::new([0; P2C_BUFFER_SIZE as usize]));
    static C2P: Page<0x1000> = Page(UnsafeCell::new([0; 0x1000]));

    /// The SP3 FCH SMI dispatcher.
    pub struct Sp3SmmHandler;

    impl SmmHandler for Sp3SmmHandler {
        type Config = SmmConfig;

        unsafe fn handle(_ctx: &mut SmmContext<'_>, config: &SmmConfig) {
            fstart_pci::ecam::init(config.ecam_base as usize);
            // The OS may be in the middle of an SMN access on bus 0.
            let host = PciAddress::new(0, 0, 0, 0);
            let pci = Pci::default();
            // SAFETY: SMM owns the CPU; the index is restored below.
            let saved = unsafe { pci.read(host, fstart_driver_zen::smn::SMN_INDEX) };
            dispatch(config);
            // SAFETY: as above.
            unsafe { pci.write(host, fstart_driver_zen::smn::SMN_INDEX, saved) };
        }
    }

    fn dispatch(config: &SmmConfig) {
        let timer = Tsc::new();
        let hw = primary_fch(timer);
        let acpi = hw.acpi();
        let generation = config.generation;
        let smu = smu(generation, timer);

        if smi::is_pending(acpi, source::SMI_CMD) {
            let command = hw.io().read8(acpimmio::SMI_CMD);
            smi::clear(acpi, source::SMI_CMD);
            match command {
                smi::sw::ACPI_ENABLE => sx::acpi_enable(&hw),
                smi::sw::ACPI_DISABLE => sx::acpi_disable(&hw),
                sw::PSP_SMM_INFO => smm_info(config, &timer),
                #[cfg(feature = "milan")]
                smi::sw::FCH_LOCK if generation == Generation::Milan => fch_lock(config, &timer),
                _ => {}
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
        #[cfg(any(feature = "rome", feature = "milan"))]
        if generation == Generation::Rome
            && fstart_driver_zen::fch::huashan::smm::take_cf9_trap(&hw)
        {
            // The trapped write was a reset request: let it through.
            fstart_driver_zen::fch::huashan::smm::disarm_cf9_trap(&hw);
            hw.io().write8(0xcf9, 0x06);
        }
        smi::eos(acpi);
    }

    /// Socket 0's (Naples: die 0's) SMU mailbox through bus 0.
    fn smu(generation: Generation, timer: Tsc) -> Smu<Pci, Tsc> {
        let smn = Smn::new(Pci::default(), 0);
        let (regs, limit) = match generation {
            #[cfg(feature = "naples")]
            Generation::Naples => (
                fstart_driver_zen::smu::v9::REGS,
                fstart_driver_zen::smu::v9::ID_LIMIT,
            ),
            #[cfg(any(feature = "rome", feature = "milan"))]
            Generation::Rome | Generation::Milan => {
                use fstart_driver_zen::smu::v11;
                (v11::REGS, v11::id_limit(generation))
            }
            #[allow(unreachable_patterns)]
            _ => unreachable!("the flow only installs SMM for a compiled-in generation"),
        };
        // SAFETY: SMM owns bus 0's SMN pair (saved by the caller).
        unsafe { Smu::new(smn, regs, timer, limit) }
    }

    fn psp(config: &SmmConfig) -> Mailbox<MmioTransport> {
        // SAFETY: the PSP BAR the boot flow programmed and left mapped.
        Mailbox::new(
            unsafe { MmioTransport::new(config.psp_bar) },
            config.generation,
        )
    }

    /// SmmInfo (PSP_APOB §3.3) with this handler's buffers.
    fn smm_info(config: &SmmConfig, timer: &Tsc) {
        let cpu = msr();
        let p2c = P2C.0.get() as u64;
        let c2p = C2P.0.get() as u64;
        if config.generation == Generation::Milan {
            // SAFETY: SMRAM static, owned by the rendezvous owner.
            unsafe {
                (P2C.0.get().cast::<u32>().add(1))
                    .write_volatile(fstart_driver_zen::psp::command::P2C_HEADER1_MILAN)
            };
        }
        let info = SmmInfo::new(cpu.reg(SMM_ADDR).get(), cpu.reg(SMM_MASK).get(), p2c, c2p);
        let _ = psp(config).send(timer, &mut C2pBuffer::new(info));
    }

    /// SW SMI D2 (Milan ReadyToBoot): LockFCHReg through the C2P page.
    #[cfg(feature = "milan")]
    fn fch_lock(config: &SmmConfig, timer: &Tsc) {
        use fstart_driver_zen::psp::command::LockFchReg;
        let page = C2P.0.get().cast::<u8>();
        let buffer = page.cast::<C2pBuffer<LockFchReg>>();
        // SAFETY: the C2P page is SMRAM owned by the rendezvous owner and
        // registered with SmmInfo; the flag byte is inside it.
        unsafe {
            buffer.write(C2pBuffer::new(LockFchReg));
            let _ = fstart_driver_zen::fch::huashan::smm::lock_fch_registers(
                &psp(config),
                timer,
                &mut *buffer,
                page.add(fstart_driver_zen::psp::command::SMM_FLAG_OFFSET as usize),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fstart_driver_zen::apob::sysmap::RegionKind;

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
