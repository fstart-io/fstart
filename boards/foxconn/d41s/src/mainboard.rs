//! Foxconn D41S mainboard hooks.

#[cfg(any(
    fstart_stage_env = "car",
    fstart_stage_env = "postcar",
    fstart_stage_env = "ram"
))]
use fstart_core::services::ServiceError;
#[cfg(any(
    fstart_stage_env = "car",
    fstart_stage_env = "postcar",
    fstart_stage_env = "ram"
))]
use fstart_core::services::device::BusDevice;
#[cfg(any(
    fstart_stage_env = "car",
    fstart_stage_env = "postcar",
    fstart_stage_env = "ram"
))]
use fstart_driver_intel::generic::ck505::I2cCk505;
#[cfg(any(
    fstart_stage_env = "car",
    fstart_stage_env = "postcar",
    fstart_stage_env = "ram"
))]
use fstart_driver_superio::ite8721f::Ite8721f;
#[cfg(any(
    fstart_stage_env = "car",
    fstart_stage_env = "postcar",
    fstart_stage_env = "ram"
))]
use fstart_platform_intel::pineview::PineviewIch7;
#[cfg(any(
    fstart_stage_env = "car",
    fstart_stage_env = "postcar",
    fstart_stage_env = "ram"
))]
use fstart_platform_intel::{IntelEarlyBoardHooks, IntelEarlyCtx};

/// Board-specific D41S hooks for the Pineview/ICH7 flow.
#[cfg(any(
    fstart_stage_env = "car",
    fstart_stage_env = "postcar",
    fstart_stage_env = "ram"
))]
pub struct D41SMainboard {
    superio: Option<Ite8721f>,
}

#[cfg(fstart_stage_env = "ram")]
mod mainboard_acpi_device {
    extern crate alloc;

    use super::{D41SMainboard, d41s_mainboard_dsdt_aml};

    impl fstart_acpi::device::AcpiDevice for D41SMainboard {
        type Config = fstart_platform_intel::pineview::PineviewIch7AcpiContext;

        fn dsdt_aml(&self, config: &Self::Config) -> alloc::vec::Vec<u8> {
            d41s_mainboard_dsdt_aml(*config)
        }
    }
}

#[cfg(any(
    fstart_stage_env = "car",
    fstart_stage_env = "postcar",
    fstart_stage_env = "ram"
))]
impl D41SMainboard {
    #[must_use]
    pub const fn new() -> Self {
        Self { superio: None }
    }
}

#[cfg(any(
    fstart_stage_env = "car",
    fstart_stage_env = "postcar",
    fstart_stage_env = "ram"
))]
impl IntelEarlyBoardHooks<PineviewIch7> for D41SMainboard {
    fn before_console(
        &mut self,
        _ctx: &mut IntelEarlyCtx<PineviewIch7>,
    ) -> Result<(), ServiceError> {
        let mut superio =
            Ite8721f::new_at_base(crate::d41s_superio_config().0, crate::SUPERIO_PNP_BASE)
                .map_err(ServiceError::from)?;
        superio.init().map_err(ServiceError::from)?;
        self.superio = Some(superio);
        Ok(())
    }

    fn after_memory(&mut self, ctx: &mut IntelEarlyCtx<PineviewIch7>) -> Result<(), ServiceError> {
        let southbridge = ctx.southbridge();
        let mut ck505 = I2cCk505::new_at_address(crate::d41s_ck505_config(), crate::CK505_ADDR)
            .map_err(ServiceError::from)?;
        ck505.init_on_smbus(southbridge).map_err(ServiceError::from)
    }
}

#[cfg(fstart_stage_env = "ram")]
mod acpi_impl {
    extern crate alloc;

    use alloc::vec::Vec;
    use fstart_platform_intel::pineview::PineviewIch7AcpiContext;

    pub fn d41s_mainboard_dsdt_aml(context: PineviewIch7AcpiContext) -> Vec<u8> {
        let config = crate::d41s_superio_config();
        let sio = fstart_driver_superio::superio_dsdt_aml(&config);
        fstart_acpi::aml_linker::scope_vec(context.lpc_scope(), &sio)
            .expect("D41S required Super I/O scope emission failed")
    }
}

#[cfg(fstart_stage_env = "ram")]
pub use acpi_impl::d41s_mainboard_dsdt_aml;

static D41S_SMBIOS_PROCESSORS: [fstart_acpi::smbios::ProcessorDesc<'static>; 1] =
    [fstart_acpi::smbios::ProcessorDesc {
        socket: "FCBGA559",
        manufacturer: "Intel",
        family: 0x28,
        max_speed_mhz: 0,
        core_count: 0,
        thread_count: 0,
        caches: &[],
    }];

const BIOS_RELEASE_DATE: &str = match option_env!("FSTART_SMBIOS_DATE") {
    Some(date) => date,
    None => "04/15/2026",
};

pub static D41S_SMBIOS_DESC: fstart_acpi::smbios::SmbiosDesc<'static> = fstart_acpi::smbios::SmbiosDesc {
    bios_vendor: "fstart",
    bios_version: "0.1.0",
    bios_release_date: BIOS_RELEASE_DATE,
    sys_manufacturer: "Foxconn",
    sys_product: "D41S",
    sys_version: "1.0",
    sys_serial: None,
    bb_manufacturer: "Foxconn",
    bb_product: "D41S",
    chassis_type: 0x03,
    chassis_manufacturer: "Foxconn",
    processors: &D41S_SMBIOS_PROCESSORS,
    memory_devices: &[],
    ram_base: 0x0010_0000,
    ram_end: 0x3fff_ffff,
};
