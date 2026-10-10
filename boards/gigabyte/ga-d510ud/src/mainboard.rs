//! IT8720F board policy through semantic driver operations.
#[cfg(any(fstart_stage_env = "car", fstart_stage_env = "ram"))]
use fstart_core::services::{ServiceError, device::BusDevice};
#[cfg(any(fstart_stage_env = "car", fstart_stage_env = "ram"))]
#[derive(Default)]
pub struct GaD510udMainboard;

#[cfg(any(fstart_stage_env = "car", fstart_stage_env = "ram"))]
impl<S> fstart_platform_intel::IntelSmbusRouting<S> for GaD510udMainboard {}

#[cfg(any(fstart_stage_env = "car", fstart_stage_env = "ram"))]
impl GaD510udMainboard {
    fn setup_superio(&mut self) -> Result<(), ServiceError> {
        use fstart_driver_superio::ite8720f::Ite8720f;
        let mut sio =
            Ite8720f::new_at_base(crate::ga_d510ud_superio_config(), crate::SUPERIO_PNP_BASE)?;
        sio.init()?;
        sio.configure_gpio(&crate::GA_D510UD_SUPERIO_GPIO, crate::SUPERIO_GPIO_BASE);
        sio.use_internal_power_good();
        sio.disable_watchdog();
        sio.preset_bus_select(crate::GA_D510UD_BSEL_PRESET);
        sio.select_main_flash();
        sio.disconnect_hwmon_irq();
        sio.disable_parallel_ecp_decode();
        Ok(())
    }
}
#[cfg(fstart_stage_env = "car")]
impl fstart_platform_intel::IntelEarlyBoardHooks<crate::Hardware> for GaD510udMainboard {
    fn before_console(
        &mut self,
        _: &mut fstart_platform_intel::IntelEarlyCtx<crate::Hardware>,
    ) -> Result<(), ServiceError> {
        self.setup_superio()
    }
}
#[cfg(fstart_stage_env = "ram")]
impl fstart_platform_intel::IntelMainstageBoardHooks<crate::Hardware> for GaD510udMainboard {
    fn before_console(
        &mut self,
        _: &mut fstart_platform_intel::IntelMainstageBoardCtx<crate::Hardware>,
    ) -> Result<(), ServiceError> {
        self.setup_superio()
    }
}
#[cfg(fstart_stage_env = "ram")]
impl fstart_acpi::device::AcpiDevice for GaD510udMainboard {
    type Config = fstart_platform_intel::pineview::PineviewIch7AcpiContext;
    fn dsdt_aml(&self, config: &Self::Config) -> alloc::vec::Vec<u8> {
        let sio = fstart_driver_superio::superio_dsdt_aml(&crate::ga_d510ud_superio_config());
        fstart_acpi::aml_linker::scope_vec(config.lpc_scope(), &sio)
            .expect("GA-D510UD Super I/O AML")
    }
}
#[cfg(fstart_stage_env = "ram")]
extern crate alloc;

pub static GA_D510UD_SMBIOS_IDENTITY: fstart_acpi::smbios::SmbiosIdentity<'static> =
    fstart_acpi::smbios::SmbiosIdentity {
        bios_vendor: "fstart",
        bios_version: "0.1.0",
        bios_release_date: fstart_platform_intel::SMBIOS_RELEASE_DATE,
        sys_manufacturer: "Gigabyte",
        sys_product: "GA-D510UD",
        sys_version: "",
        sys_serial: None,
        sys_uuid: None,
        bb_manufacturer: "Gigabyte",
        bb_product: "GA-D510UD",
        bb_version: "",
        bb_serial: None,
        chassis_type: 0x03,
        chassis_manufacturer: "Gigabyte",
        processor_sockets: &["FCBGA559"],
        oem_string: None,
    };
