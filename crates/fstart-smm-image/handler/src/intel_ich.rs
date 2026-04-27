use crate::abi::{
    SmmEntryParams, SMM_PLATFORM_DATA_ICH_GPE0_STS_OFFSET, SMM_PLATFORM_DATA_ICH_PM_BASE,
};
use crate::{pio_read32, pio_write16, pio_write32, set_runtime_flags, SMM_RUNTIME_FLAG_FINALIZED};

const APM_CNT_ACPI_DISABLE: u8 = 0x1e;
const APM_CNT_ACPI_ENABLE: u8 = 0xe1;
const APM_CNT_FINALIZE: u8 = 0xcb;

const PM1_STS: u16 = 0x00;
const PM1_CNT: u16 = 0x04;
const SMI_EN: u16 = 0x30;
const SMI_STS: u16 = 0x34;
const ALT_GP_SMI_STS: u16 = 0x3a;
const TCO1_STS: u16 = 0x64;
const TCO2_STS: u16 = 0x66;
const SCI_EN: u32 = 1;
const EOS: u32 = 2;

pub unsafe fn dispatch(params: &mut SmmEntryParams, apm_command: u8) {
    let pm_base = params.platform_data[SMM_PLATFORM_DATA_ICH_PM_BASE] as u16;
    if pm_base == 0 {
        return;
    }

    match apm_command {
        APM_CNT_ACPI_DISABLE => pm_read_modify_write32(pm_base, PM1_CNT, !SCI_EN, false),
        APM_CNT_ACPI_ENABLE => pm_read_modify_write32(pm_base, PM1_CNT, SCI_EN, true),
        APM_CNT_FINALIZE => set_runtime_flags(params, SMM_RUNTIME_FLAG_FINALIZED),
        _ => {}
    }

    pm_write16(pm_base, PM1_STS, 0xffff);

    let gpe0_sts_offset = params.platform_data[SMM_PLATFORM_DATA_ICH_GPE0_STS_OFFSET] as u16;
    pio_write32(pm_base.wrapping_add(gpe0_sts_offset), 0xffff_ffff);

    pm_write32(pm_base, SMI_STS, 0xffff_ffff);
    pm_write16(pm_base, ALT_GP_SMI_STS, 0xffff);
    pm_write16(pm_base, TCO1_STS, 0xffff);
    pm_write16(pm_base, TCO2_STS, 0xffff);

    pm_read_modify_write32(pm_base, SMI_EN, EOS, true);
}

unsafe fn pm_read_modify_write32(pm_base: u16, offset: u16, mask: u32, set: bool) {
    let port = pm_base.wrapping_add(offset);
    let value = pio_read32(port);
    let value = if set { value | mask } else { value & mask };
    pio_write32(port, value);
}

unsafe fn pm_write16(pm_base: u16, offset: u16, value: u16) {
    pio_write16(pm_base.wrapping_add(offset), value);
}

unsafe fn pm_write32(pm_base: u16, offset: u16, value: u32) {
    pio_write32(pm_base.wrapping_add(offset), value);
}
