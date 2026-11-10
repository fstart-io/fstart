// SPDX-License-Identifier: MIT OR Apache-2.0
//! SMM on the Rs IOD, only for the PSP: the handler registers its SMRAM
//! buffers with the PSP (SmmInfo), opens the PSP's ACPI error-injection
//! service (RasEinj ENABLE, which the PSP takes only from SMM: status 11h
//! from normal mode) and answers the PSP's P2C requests.
//! No other SMI source: the OS runs ACPI natively (FADT SMI_CMD 0), so
//! SCI_EN, SLP_TYP and the power button stay with normal mode
//! ([`fstart_driver_amd::fch::smi::SoftwareSmi`]).
//!
//! AGESA's order (AmdPspSmmV2Rs, AmdPlatformRasRsSmm `RasEnablePspEinj`):
//! SmmInfo from the SMM driver's entry, RasEinj ENABLE after it. Here both
//! run in the handler on software SMI [`sw::PSP`], so the P2C buffer and the
//! C2P page are the handler's own SMRAM statics. Installation is the arch
//! crate's AMD64 path: every thread programs SMM_ADDR/SMM_MASK/SMBASE and
//! sets HWCR.SmmLock, no relocation SMI.

/// TSEG: 8 MiB, as SP3; NFX carves 128 MiB at `A8000000`.
pub const TSEG_SIZE: u64 = 8 << 20;

/// fstart's software SMI commands.
pub mod sw {
    /// SmmInfo, then RasEinj ENABLE; once per boot.
    pub const PSP: u8 = 0xd0;
}

/// What the installer writes into SMRAM for [`handler::RsSmmHandler`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub struct Config {
    /// The PSP BAR ([`super::rs_psp`]), below 4 GiB.
    pub psp_bar: u64,
    /// The TSC rate, for the PSP mailbox's bounded waits.
    pub tsc_mhz: u32,
}

/// TSEG in the highest RAM below `limit` clear of `avoid`.
pub fn place_tseg(
    regions: &[fstart_driver_amd::apob::sysmap::Region],
    limit: u64,
    avoid: &[core::ops::Range<u64>],
) -> Option<core::ops::Range<u64>> {
    fstart_driver_amd::apob::sysmap::place_tseg(regions, limit, TSEG_SIZE, avoid)
}

#[cfg(all(target_os = "none", feature = "rs-stage"))]
mod install {
    use super::{Config, sw};
    use fstart_arch::x86::{
        cpu::amd::smm::{AmdSmm, InstalledTseg},
        mp::MpHandle,
    };
    use fstart_driver_amd::fch::smi::{APM_CNT, SoftwareSmi};

    /// The board's SMM image that fbuild built from its `smm.rs`.
    #[cfg(fstart_amd_has_smm_image)]
    const IMAGE: Option<&[u8]> = Some(include_bytes!(env!("FSTART_SMM_IMAGE")));
    #[cfg(not(fstart_amd_has_smm_image))]
    const IMAGE: Option<&[u8]> = None;

    /// Install the handler in `tseg` on every CPU and lock SMM, then raise
    /// [`sw::PSP`]: afterwards the PSP's EINJ mailbox answers if it took
    /// RasEinj ENABLE.
    ///
    /// # Safety
    /// BSP with all APs parked under `mp`; `tseg` is RAM no one else uses,
    /// reserved from the OS; the PSP BAR in `config` is mapped below 4 GiB.
    pub unsafe fn install(
        mp: &MpHandle,
        tseg: core::ops::Range<u64>,
        config: Config,
    ) -> Result<InstalledTseg, ()> {
        let image = IMAGE.ok_or_else(|| {
            fstart_log::warn!("rs: no SMM image in this build");
        })?;
        if config.psp_bar == 0 || config.psp_bar >= 1 << 32 {
            return Err(());
        }
        let size = u32::try_from(tseg.end - tseg.start).map_err(|_| ())?;
        // SAFETY: the caller owns the FCH SMI registers; ACPI MMIO decodes.
        let smi = unsafe { SoftwareSmi::new(config) };
        let installed = AmdSmm::new("rs", &smi)
            .install(mp, image, tseg.start, size)
            .map_err(|error| {
                fstart_log::error!("rs: SMM installation {}", error.name());
            })?;
        // SAFETY: the FCH decodes the SMI command port, SMIs now enabled.
        unsafe { fstart_core::pio::outb(APM_CNT, sw::PSP) };
        Ok(installed)
    }
}
#[cfg(all(target_os = "none", feature = "rs-stage"))]
pub use install::install;

#[cfg(feature = "rs-smm")]
pub mod handler {
    use super::{Config, sw};
    use core::cell::UnsafeCell;
    use core::sync::atomic::{AtomicBool, Ordering};
    use fstart_arch::x86::cpu::amd::msr::{SMM_ADDR, SMM_MASK};
    use fstart_arch::x86::cpu::msr::{MsrAccess, NativeMsr};
    use fstart_driver_amd::{
        ccx::tsc::Tsc,
        fch::{
            Acpimmio, Fch, SmmBus,
            smi::{APM_CNT, SmiTrigger0, TRIGGER0, source, status},
        },
        psp::{
            Variant,
            command::{C2pBuffer, P2C_BUFFER_SIZE, SMM_FLAG_OFFSET, SmmInfo, SmmRegister},
            einj::RasEinj,
            mailbox::{Mailbox, MmioTransport},
            p2c,
        },
    };
    use fstart_smm::SmmContext;
    use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};

    #[repr(C, align(4096))]
    struct Page<const N: usize>(UnsafeCell<[u8; N]>);
    // SAFETY: only the SMM rendezvous owner touches the pages.
    unsafe impl<const N: usize> Sync for Page<N> {}

    /// The P2C buffer and the C2P page (command at +0, SMM flag at +C00h),
    /// registered with SmmInfo.
    static P2C: Page<{ P2C_BUFFER_SIZE as usize }> =
        Page(UnsafeCell::new([0; P2C_BUFFER_SIZE as usize]));
    static C2P: Page<0x1000> = Page(UnsafeCell::new([0; 0x1000]));
    const PSP_TIMEOUT_US: u64 = 100_000;

    /// The PSP's SMI dispatcher: the board's `smm_bin!(handler: ...)`.
    pub fn smi(_ctx: &mut SmmContext<'_>, config: &Config) {
        // SAFETY: SMM owns the FCH; ACPI MMIO and the SMI command port
        // decode; no config space is used.
        let fch = Fch::new(unsafe { SmmBus::without_config() }, Acpimmio::DIE0);
        let command = fch.reg(status(usize::from(source::SMI_COMMAND / 32)));
        let command_bit = 1 << (source::SMI_COMMAND % 32);
        if command.get() & command_bit != 0 {
            // SAFETY: the FCH decodes the SMI command port.
            if unsafe { fstart_core::pio::inb(APM_CNT) } == sw::PSP {
                psp(config);
            }
            command.set(command_bit);
        }
        let psp_status = fch.reg(status(usize::from(source::PSP / 32)));
        let psp_bit = 1 << (source::PSP % 32);
        if psp_status.get() & psp_bit != 0 {
            // SAFETY: the SmmInfo-registered P2C static, owned by the
            // rendezvous owner.
            unsafe { p2c::answer(&*P2C.0.get().cast()) };
            // Answer, then drop SmiTrig0[25] (AmdPspP2CmboxV2Ssp
            // `0x944`): a trigger left set keeps the source asserted.
            fch.reg(TRIGGER0).modify(SmiTrigger0::PSP::CLEAR);
            psp_status.set(psp_bit);
        }
        fch.reg(TRIGGER0).modify(SmiTrigger0::EOS::SET);
    }

    /// SmmInfo with this handler's buffers, then RasEinj ENABLE through
    /// the registered C2P page with the SMM flag. Once per boot.
    #[inline(never)]
    fn psp(config: &Config) {
        static SENT: AtomicBool = AtomicBool::new(false);
        if SENT.swap(true, Ordering::AcqRel) {
            return;
        }
        let Some(mhz) = core::num::NonZeroU32::new(config.tsc_mhz) else {
            return;
        };
        let timer = Tsc::new(mhz);
        // SAFETY: the PSP BAR the boot flow mapped below 4 GiB.
        let mailbox = Mailbox::new(unsafe { MmioTransport::new(config.psp_bar) }, Variant::Rs)
            .with_timeout(PSP_TIMEOUT_US);
        // SAFETY: SMM, ring 0.
        let cpu = unsafe { NativeMsr::new() };
        // SAFETY: the SMRAM static of the buffer's size, owned by the
        // rendezvous owner.
        p2c::ready(unsafe { &*P2C.0.get().cast() });
        let mut info = SmmInfo::new(
            cpu.reg(SMM_ADDR).get(),
            cpu.reg(SMM_MASK).get(),
            P2C.0.get() as u64,
            C2P.0.get() as u64,
        );
        // The Rs donors zero the five reserved descriptors.
        info.reserved = [SmmRegister::default(); 5];
        if mailbox.send(&timer, &mut C2pBuffer::new(info)).is_err() {
            return;
        }
        let page = C2P.0.get().cast::<u8>();
        let buffer = page.cast::<C2pBuffer<RasEinj>>();
        // SAFETY: the C2P page is SMRAM owned by the rendezvous owner and
        // registered with SmmInfo; the flag byte is inside it.
        unsafe {
            buffer.write(C2pBuffer::new(RasEinj::ENABLE));
            let _ = mailbox.send_in_smm(&timer, &mut *buffer, page.add(SMM_FLAG_OFFSET as usize));
        }
    }
}
