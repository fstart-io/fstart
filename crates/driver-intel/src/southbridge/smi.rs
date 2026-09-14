//! ICH SMI routing for SMM installation (coreboot `southbridge/intel/common/smi.c`).
//!
//! One implementation covers ICH7 through ICH10: the register layout only
//! differs in the GPE0 block, which [`Gpe0Block`] describes.

use super::pmio_ich::{self as pmio, PmIo};
pub use fstart_arch::cpu_intel::smm::{Gpe0Block, SmiControl};

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
    fn pm_base(&self) -> u16 {
        self.pm.base()
    }

    fn gpe0(&self) -> Gpe0Block {
        self.gpe0
    }

    fn enable_relocation_smi(&self) {
        self.pm.reset_smi_status();
        self.pm
            .write32(pmio::SMI_EN, pmio::APMC_EN | pmio::GBL_SMI_EN | pmio::EOS);
    }

    fn enable_permanent_smi(&self) {
        self.clear_status();
        self.pm
            .write16(pmio::PM1_EN, pmio::PWRBTN_EN | pmio::GBL_EN);
        self.pm.write32(
            pmio::SMI_EN,
            pmio::TCO_EN | pmio::APMC_EN | pmio::SLP_SMI_EN | pmio::GBL_SMI_EN | pmio::EOS,
        );
    }
}

// ---------------------------------------------------------------------------
// SMI handler (runs inside the SMM image)
// ---------------------------------------------------------------------------

use core::marker::PhantomData;
use fstart_smm::{
    NoBoardSmmHandler, SMM_PLATFORM_DATA_ICH_GPE0_STS_OFFSET, SMM_PLATFORM_DATA_ICH_PM_BASE,
    SMM_PLATFORM_FLAG_ICH_GPE0_64BIT, SMM_RUNTIME_FLAG_FINALIZED, SmmBoardHandler, SmmContext,
    SmmHandler,
};

const APM_CNT_ACPI_DISABLE: u8 = 0x1e;
const APM_CNT_ACPI_ENABLE: u8 = 0xe1;
const APM_CNT_FINALIZE: u8 = 0xcb;

/// SMI handler for every ICH PM I/O layout.
///
/// PMBASE and the GPE0 block come from the runtime parameters the installer
/// published (see [`IntelSmm`](fstart_arch::cpu_intel::smm::IntelSmm)), so
/// the same handler serves ICH7 and ICH8+ boards.
pub struct IchSmmHandler<B = NoBoardSmmHandler>(PhantomData<B>);

impl<B: SmmBoardHandler> SmmHandler for IchSmmHandler<B> {
    /// Inlined into the board SMM entry: the installed blob is a raw copy
    /// with no dynamic loader, so no cross-crate PLT call may survive here.
    #[inline(always)]
    unsafe fn handle(ctx: &mut SmmContext<'_>) {
        unsafe {
            let pm_base = ctx.params.platform_data[SMM_PLATFORM_DATA_ICH_PM_BASE] as u16;
            if pm_base == 0 {
                return;
            }
            let gpe0 = Gpe0Block {
                sts_offset: ctx.params.platform_data[SMM_PLATFORM_DATA_ICH_GPE0_STS_OFFSET] as u16,
                wide: ctx.params.platform_flags & SMM_PLATFORM_FLAG_ICH_GPE0_64BIT != 0,
            };

            let pm = PmIo::new(pm_base);
            match ctx.apm_command {
                // 16-bit PM1a access: QEMU TCG mishandles 32-bit PIO to
                // ACPI-core PM registers (writes vanish), while 16-bit works
                // on every engine; SCI_EN is a PM1a bit either way.
                APM_CNT_ACPI_DISABLE => pm.clrbits16(pmio::PM1_CNT, pmio::SCI_EN as u16),
                APM_CNT_ACPI_ENABLE => pm.setbits16(pmio::PM1_CNT, pmio::SCI_EN as u16),
                APM_CNT_FINALIZE => ctx.set_runtime_flags(SMM_RUNTIME_FLAG_FINALIZED),
                _ => {}
            }
            if ctx.apm_command != APM_CNT_FINALIZE {
                B::on_apmc(ctx, ctx.apm_command);
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
unsafe fn handle_tco<B: SmmBoardHandler>(ctx: &mut SmmContext<'_>, pm: &PmIo) {
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
