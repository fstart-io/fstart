//! ICH8/ICH9 SMM southbridge handler.

use core::marker::PhantomData;

use crate::southbridge::pmio_ich::{self as pmio, PmIo};
use fstart_smm::{
    NoBoardSmmHandler, SmmBoardHandler, SmmContext, SmmHandler, SMM_RUNTIME_FLAG_FINALIZED,
};

const APM_CNT_ACPI_DISABLE: u8 = 0x1e;
const APM_CNT_ACPI_ENABLE: u8 = 0xe1;
const APM_CNT_FINALIZE: u8 = 0xcb;
const ICH8_GPE0_STS: u16 = 0x20;

/// SMM handler for ICH8/ICH9-compatible PMBASE layout.
pub struct Ich8SmmHandler<B = NoBoardSmmHandler>(PhantomData<B>);

impl<B: SmmBoardHandler> SmmHandler for Ich8SmmHandler<B> {
    unsafe fn handle(ctx: &mut SmmContext<'_>) {
        let pm_base = ctx.params.platform_data[fstart_smm::SMM_PLATFORM_DATA_ICH_PM_BASE] as u16;
        if pm_base == 0 {
            return;
        }

        let pm = PmIo::new(pm_base);
        match ctx.apm_command {
            APM_CNT_ACPI_DISABLE => {
                pm.clrbits32(pmio::PM1_CNT, pmio::SCI_EN);
                B::on_apmc(ctx, ctx.apm_command);
            }
            APM_CNT_ACPI_ENABLE => {
                pm.setbits32(pmio::PM1_CNT, pmio::SCI_EN);
                B::on_apmc(ctx, ctx.apm_command);
            }
            APM_CNT_FINALIZE => ctx.set_runtime_flags(SMM_RUNTIME_FLAG_FINALIZED),
            _ => B::on_apmc(ctx, ctx.apm_command),
        }

        handle_tco::<B>(ctx, &pm);
        pm.write16(pmio::PM1_STS, 0xffff);
        let gpe_status =
            pm.read32(ICH8_GPE0_STS) as u64 | ((pm.read32(ICH8_GPE0_STS + 4) as u64) << 32);
        B::on_gpe(ctx, gpe_status);
        pm.write32(ICH8_GPE0_STS, 0xffff_ffff);
        pm.write32(ICH8_GPE0_STS + 4, 0xffff_ffff);
        pm.write32(pmio::SMI_STS, 0xffff_ffff);
        pm.write16(pmio::ALT_GP_SMI_STS, 0xffff);
        pm.setbits32(pmio::SMI_EN, pmio::EOS);
    }
}

unsafe fn handle_tco<B: SmmBoardHandler>(ctx: &mut SmmContext<'_>, pm: &PmIo) {
    let tco = pm.tco();
    let sts = tco.read32(pmio::TCO1_STS);
    if sts & pmio::SW_TCO_SMI != 0 {
        let command = tco.read_dat_in();
        if let Some(response) = B::on_tco_command(ctx, command) {
            tco.write_dat_out(response);
        }
    }
    tco.reset_tco_status();
}
