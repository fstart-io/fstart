//! Foxconn D41S mainboard hooks.

#[cfg(feature = "stage")]
use fstart_core::services::device::{BusDevice, DeviceError};
#[cfg(feature = "stage")]
use fstart_core::services::ServiceError;
#[cfg(feature = "stage")]
use fstart_driver_intel::ck505::I2cCk505;
#[cfg(feature = "stage")]
use fstart_driver_superio::ite8721f::Ite8721f;
#[cfg(feature = "stage")]
use fstart_platform_intel::pineview::PineviewIch7;
#[cfg(feature = "stage")]
use fstart_platform_intel::{IntelEarlyBoardHooks, IntelEarlyCtx};

/// Board-specific D41S hooks for the Pineview/ICH7 flow.
#[cfg(feature = "stage")]
pub struct D41SMainboard {
    superio: Option<Ite8721f>,
}

#[cfg(feature = "acpi")]
mod mainboard_acpi_device {
    extern crate alloc;

    use super::{d41s_mainboard_dsdt_aml, D41SMainboard};

    impl fstart_acpi::device::AcpiDevice for D41SMainboard {
        type Config = fstart_platform_intel::pineview::PineviewIch7AcpiContext;

        fn dsdt_aml(&self, config: &Self::Config) -> alloc::vec::Vec<u8> {
            d41s_mainboard_dsdt_aml(*config)
        }
    }
}

#[cfg(feature = "stage")]
impl D41SMainboard {
    #[must_use]
    pub const fn new() -> Self {
        Self { superio: None }
    }
}

#[cfg(feature = "stage")]
impl IntelEarlyBoardHooks<PineviewIch7> for D41SMainboard {
    fn before_console(
        &mut self,
        _ctx: &mut IntelEarlyCtx<PineviewIch7>,
    ) -> Result<(), ServiceError> {
        let mut superio = Ite8721f::new_at_base(
            crate::d41s_superio_config().0,
            crate::SUPERIO_PNP_BASE,
        )
        .map_err(device_error_to_service_error)?;
        superio.init().map_err(device_error_to_service_error)?;
        self.superio = Some(superio);
        Ok(())
    }

    fn after_memory(&mut self, ctx: &mut IntelEarlyCtx<PineviewIch7>) -> Result<(), ServiceError> {
        let southbridge = ctx.southbridge();
        let mut ck505 = I2cCk505::new_at_address(crate::d41s_ck505_config(), crate::CK505_ADDR)
            .map_err(device_error_to_service_error)?;
        ck505
            .init_on_smbus(southbridge)
            .map_err(device_error_to_service_error)
    }

    fn before_handoff(
        &mut self,
        _ctx: &mut IntelEarlyCtx<PineviewIch7>,
    ) -> Result<(), ServiceError> {
        fstart_driver_superio::quiesce_i8042_for_os();
        Ok(())
    }
}

#[cfg(feature = "stage")]
fn device_error_to_service_error(_err: DeviceError) -> ServiceError {
    ServiceError::HardwareError
}

#[cfg(feature = "acpi")]
mod acpi_impl {
    extern crate alloc;

    use alloc::vec::Vec;
    use fstart_acpi::{Aml, AmlSink};
    use fstart_platform_intel::pineview::PineviewIch7AcpiContext;

    struct RawAml<'a>(&'a [u8]);

    impl Aml for RawAml<'_> {
        fn to_aml_bytes(&self, sink: &mut dyn AmlSink) {
            sink.vec(self.0);
        }
    }

    fn scope_aml(path: &str, children: &[u8]) -> Vec<u8> {
        let raw = RawAml(children);
        let scope = fstart_acpi::aml::Scope::new(
            fstart_acpi::aml::Path::new(path),
            alloc::vec![&raw as &dyn Aml],
        );
        let mut bytes = Vec::new();
        scope.to_aml_bytes(&mut bytes);
        bytes
    }

    pub fn d41s_mainboard_dsdt_aml(context: PineviewIch7AcpiContext) -> Vec<u8> {
        let config = crate::d41s_superio_config();
        let sio = fstart_driver_superio::superio_dsdt_aml(&config);
        scope_aml(context.lpc_scope(), &sio)
    }
}

#[cfg(feature = "acpi")]
pub use acpi_impl::d41s_mainboard_dsdt_aml;
