//! ICH SMI routing for SMM installation (coreboot `southbridge/intel/common/smi.c`).
//!
//! One implementation covers ICH7 through ICH10: the register layout only
//! differs in the GPE0 block, which [`Gpe0Block`] describes.

use super::pmio_ich::{self as pmio, PmIo};
pub use fstart_arch::x86::cpu::intel::smm::SmiControl;

const APM_CNT: u16 = 0x00b2;

/// Configuration the installer writes into SMRAM for [`IchSmmHandler`].
///
/// Both sides are built from this crate, so the type itself is the contract;
/// the generic SMM layer only checks its size and alignment.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IchSmmConfig {
    pm_base: u16,
    gpe0: Gpe0Block,
}

/// ICH GPE0 register geometry, owned by the Intel southbridge driver.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Gpe0Block {
    pub sts_offset: u16,
    pub wide: bool,
}

/// Chipset event enables saved while relocation admits only LAPIC self-SMIs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SmiEnableState {
    pub pm1: u16,
    pub gpe0_low: u32,
    pub gpe0_high: u32,
    pub alt_gp: u16,
}

/// ICH7/NM10: one 32-bit GPE0 status word at PMBASE+0x28.
pub const ICH7_GPE0: Gpe0Block = Gpe0Block {
    sts_offset: 0x28,
    wide: false,
};
/// ICH8 through ICH10: 64-bit GPE0 status at PMBASE+0x20.
pub const ICH8_GPE0: Gpe0Block = Gpe0Block {
    sts_offset: 0x20,
    wide: true,
};

/// SMI controller view of one ICH PM I/O block.
#[derive(Debug, Clone, Copy)]
pub struct IchSmi {
    pm: PmIo,
    gpe0: Gpe0Block,
}

impl IchSmi {
    #[must_use]
    pub const fn new(pm_base: u16, gpe0: Gpe0Block) -> Self {
        Self {
            pm: PmIo::new(pm_base),
            gpe0,
        }
    }

    /// coreboot `smm_southbridge_clear_state()`: clear every W1C status
    /// register so no stale event fires once SMIs are enabled.
    fn clear_status(&self) {
        self.pm.reset_smi_status();
        self.pm.reset_pm1_status();
        self.pm.tco().reset_tco_status();
        self.pm.write32(self.gpe0.sts_offset, 0xffff_ffff);
        if self.gpe0.wide {
            self.pm.write32(self.gpe0.sts_offset + 4, 0xffff_ffff);
        }
    }
}

impl SmiControl for IchSmi {
    type EnableState = SmiEnableState;
    type HandlerConfig = IchSmmConfig;

    fn smm_handler_config(&self) -> IchSmmConfig {
        IchSmmConfig {
            pm_base: self.pm.base(),
            gpe0: self.gpe0,
        }
    }

    fn set_acpi_mode(&self, enabled: bool) {
        if enabled {
            self.pm.setbits16(pmio::PM1_CNT, pmio::SCI_EN as u16);
        } else {
            self.pm.clrbits16(pmio::PM1_CNT, pmio::SCI_EN as u16);
        }
    }

    fn quiesce_for_relocation(&self) -> SmiEnableState {
        // Block chipset-originated SMIs before changing individual source
        // enables. PM1_CNT is deliberately untouched so SCI_EN is preserved.
        self.pm.write32(pmio::SMI_EN, 0);
        let gpe0_en = self.gpe0.sts_offset + if self.gpe0.wide { 8 } else { 4 };
        let previous = SmiEnableState {
            pm1: self.pm.read16(pmio::PM1_EN),
            gpe0_low: self.pm.read32(gpe0_en),
            gpe0_high: if self.gpe0.wide {
                self.pm.read32(gpe0_en + 4)
            } else {
                0
            },
            alt_gp: self.pm.read16(pmio::ALT_GP_SMI_EN),
        };
        self.pm.write16(pmio::PM1_EN, 0);
        self.pm.write32(gpe0_en, 0);
        if self.gpe0.wide {
            self.pm.write32(gpe0_en + 4, 0);
        }
        self.pm.write16(pmio::ALT_GP_SMI_EN, 0);
        self.clear_status();
        // LAPIC self-SMIs do not depend on a chipset source bit. Keep only the
        // global gate and EOS set while the shared relocation stub is live.
        self.pm.write32(pmio::SMI_EN, pmio::GBL_SMI_EN | pmio::EOS);
        previous
    }

    fn enable_permanent_smi(&self, previous: SmiEnableState) {
        self.clear_status();
        let gpe0_en = self.gpe0.sts_offset + if self.gpe0.wide { 8 } else { 4 };
        self.pm
            .write16(pmio::PM1_EN, previous.pm1 | pmio::PWRBTN_EN | pmio::GBL_EN);
        self.pm.write32(gpe0_en, previous.gpe0_low);
        if self.gpe0.wide {
            self.pm.write32(gpe0_en + 4, previous.gpe0_high);
        }
        self.pm.write16(pmio::ALT_GP_SMI_EN, previous.alt_gp);
        // No `SLP_SMI_EN`: this SMM has no sleep-transition work to do, and
        // intercepting the OS's sleep write would run a handler during the
        // machine's most delicate transition. coreboot's ICH7 boards without
        // SMM do not intercept it either (their SMM sleep handler exists only
        // to gate the memory reset on newer PCHs).
        self.pm.write32(
            pmio::SMI_EN,
            pmio::TCO_EN | pmio::APMC_EN | pmio::GBL_SMI_EN | pmio::EOS,
        );
    }
}

// ---------------------------------------------------------------------------
// SMI handler (runs inside the SMM image)
// ---------------------------------------------------------------------------

use core::marker::PhantomData;
use fstart_smm::{SmmContext, SmmHandler};

pub trait IchBoardSmmHandler {
    /// # Safety
    /// Called only by [`IchSmmHandler`] while it owns the SMM rendezvous.
    unsafe fn on_apmc(_ctx: &mut SmmContext<'_>, _command: u8) {}
    /// # Safety
    /// Called only by [`IchSmmHandler`] while it owns the SMM rendezvous.
    unsafe fn on_gpe(_ctx: &mut SmmContext<'_>, _gpe_status: u64) {}
    /// # Safety
    /// Called only by [`IchSmmHandler`] while it owns the SMM rendezvous.
    unsafe fn on_tco_command(_ctx: &mut SmmContext<'_>, _command: u8) -> Option<u8> {
        None
    }
}

pub struct NoIchBoardSmmHandler;
impl IchBoardSmmHandler for NoIchBoardSmmHandler {}

const APM_CNT_ACPI_DISABLE: u8 = 0x1e;
const APM_CNT_ACPI_ENABLE: u8 = 0xe1;
const APM_CNT_FINALIZE: u8 = 0xcb;

/// SMI handler for every ICH PM I/O layout.
///
/// PMBASE and the GPE0 block come from the [`IchSmmConfig`] the installer
/// wrote (see [`IntelSmm`](fstart_arch::x86::cpu::intel::smm::IntelSmm)), so
/// the same handler serves ICH7 and ICH8+ boards.
pub struct IchSmmHandler<B = NoIchBoardSmmHandler>(PhantomData<B>);

impl<B: IchBoardSmmHandler> SmmHandler for IchSmmHandler<B> {
    type Config = IchSmmConfig;

    /// Inlined into the board SMM entry: the installed blob is a raw copy
    /// with no dynamic loader, so no cross-crate PLT call may survive here.
    #[inline(always)]
    unsafe fn handle(ctx: &mut SmmContext<'_>, config: &IchSmmConfig) {
        unsafe {
            let gpe0 = config.gpe0;
            let pm = PmIo::new(config.pm_base);
            // The APM command port keeps its last written value, so a command
            // may only be consumed when this SMI came from the APM port.
            // Otherwise the install-time ACPI-disable would replay on every
            // unrelated SMI (TCO, GPE, sleep) and drop the chipset out of
            // ACPI mode in the middle of a suspend.
            let apm_command = (pm.read32(pmio::SMI_STS) & pmio::APM_STS != 0)
                .then(|| fstart_core::pio::inb(APM_CNT));
            match apm_command {
                // 16-bit PM1a access: QEMU TCG mishandles 32-bit PIO to
                // ACPI-core PM registers (writes vanish), while 16-bit works
                // on every engine; SCI_EN is a PM1a bit either way.
                Some(APM_CNT_ACPI_DISABLE) => pm.clrbits16(pmio::PM1_CNT, pmio::SCI_EN as u16),
                Some(APM_CNT_ACPI_ENABLE) => pm.setbits16(pmio::PM1_CNT, pmio::SCI_EN as u16),
                _ => {}
            }
            // FINALIZE has no chipset work yet; never forward it to the board.
            if let Some(command) = apm_command.filter(|command| *command != APM_CNT_FINALIZE) {
                B::on_apmc(ctx, command);
            }

            handle_tco::<B>(ctx, &pm);
            pm.write16(pmio::PM1_STS, 0xffff);
            let mut gpe_status = u64::from(pm.read32(gpe0.sts_offset));
            if gpe0.wide {
                gpe_status |= u64::from(pm.read32(gpe0.sts_offset + 4)) << 32;
            }
            B::on_gpe(ctx, gpe_status);
            pm.write32(gpe0.sts_offset, 0xffff_ffff);
            if gpe0.wide {
                pm.write32(gpe0.sts_offset + 4, 0xffff_ffff);
            }
            pm.write32(pmio::SMI_STS, 0xffff_ffff);
            pm.write16(pmio::ALT_GP_SMI_STS, 0xffff);
            pm.setbits32(pmio::SMI_EN, pmio::EOS);
        }
    }
}

#[inline(always)]
unsafe fn handle_tco<B: IchBoardSmmHandler>(ctx: &mut SmmContext<'_>, pm: &PmIo) {
    let tco = pm.tco();
    let sts = tco.read32(pmio::TCO1_STS);
    if sts & pmio::SW_TCO_SMI != 0 {
        let command = tco.read_dat_in();
        // SAFETY: SMM runtime supplied a valid SMI context to this handler.
        if let Some(response) = unsafe { B::on_tco_command(ctx, command) } {
            tco.write_dat_out(response);
        }
    }
    tco.reset_tco_status();
}
