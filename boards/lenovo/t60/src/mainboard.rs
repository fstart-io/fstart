//! T6 board hooks. Reusable drivers own register encodings; the family owns
//! dock/EC sequencing. No S3, ATI initialization/brightness, TI CardBus vendor
//! setup, IR, parallel-port or thermal-safety support is claimed.
#[cfg(any(fstart_stage_env = "car", fstart_stage_env = "ram"))]
use fstart_core::services::ServiceError;
#[cfg(any(fstart_stage_env = "car", fstart_stage_env = "ram"))]
#[derive(Default)]
pub struct T6Mainboard {
    #[cfg(fstart_stage_env = "ram")]
    identity: Option<fstart_driver_lenovo::eeprom::Identity>,
    #[cfg(fstart_stage_env = "ram")]
    ec_oem: Option<fstart_driver_lenovo::h8::EcOemString>,
    #[cfg(fstart_stage_env = "ram")]
    native_brightness: bool,
    #[cfg(fstart_stage_env = "ram")]
    console_failed: bool,
}
#[cfg(any(fstart_stage_env = "car", fstart_stage_env = "ram"))]
impl<S> fstart_platform_intel::IntelSmbusRouting<S> for T6Mainboard {}

#[cfg(all(not(test), fstart_stage_env = "car"))]
impl fstart_platform_intel::IntelEarlyBoardHooks<crate::Hardware> for T6Mainboard {
    fn before_console(
        &mut self,
        ctx: &mut fstart_platform_intel::IntelEarlyCtx<crate::Hardware>,
    ) -> Result<(), ServiceError> {
        let _ = fstart_driver_lenovo::t6::dock::setup_console(ctx.southbridge(), &crate::T6_H8);
        Ok(())
    }
    fn before_memory(
        &mut self,
        ctx: &mut fstart_platform_intel::IntelEarlyCtx<crate::Hardware>,
    ) -> Result<(), ServiceError> {
        if ctx.boot_path == fstart_platform_intel::BootPath::S3Resume {
            return Err(ServiceError::NotSupported);
        }
        Ok(())
    }
}

#[cfg(fstart_stage_env = "ram")]
impl fstart_acpi::device::AcpiDevice for T6Mainboard {
    type Config = fstart_platform_intel::i945::I945Ich7AcpiContext;
    fn dsdt_aml(&self, ctx: &Self::Config) -> alloc::vec::Vec<u8> {
        fstart_driver_lenovo::t6::acpi::dsdt_aml(
            &crate::T6_H8,
            ctx.lpc_scope(),
            self.native_brightness,
        )
    }
}
#[cfg(fstart_stage_env = "ram")]
extern crate alloc;

#[cfg(all(not(test), fstart_stage_env = "ram"))]
fn os_ec() -> fstart_driver_lenovo::ec::Ec {
    fstart_driver_lenovo::ec::Ec::new(crate::T6_H8.resources.os)
}

#[cfg(all(not(test), fstart_stage_env = "ram"))]
impl fstart_platform_intel::IntelMainstageBoardHooks<crate::Hardware> for T6Mainboard {
    fn smbios_identity<'a>(
        &'a self,
        configured: &fstart_platform_intel::tables::SmbiosIdentity<'a>,
    ) -> fstart_platform_intel::tables::SmbiosIdentity<'a> {
        let mut result = *configured;
        if let Some(eeprom) = &self.identity {
            result.sys_product = eeprom.part_number();
            result.sys_version = eeprom.version();
            result.sys_serial = Some(eeprom.serial_number());
            result.sys_uuid = Some(eeprom.uuid);
            result.bb_product = eeprom.part_number();
            result.bb_version = eeprom.version();
            result.bb_serial = Some(eeprom.serial_number());
        }
        result.oem_string = self.ec_oem.as_ref().map(|oem| oem.as_str());
        result
    }
    fn before_console(
        &mut self,
        ctx: &mut fstart_platform_intel::IntelMainstageBoardCtx<crate::Hardware>,
    ) -> Result<(), ServiceError> {
        self.console_failed =
            fstart_driver_lenovo::t6::dock::setup_console(ctx.southbridge(), &crate::T6_H8)
                .is_err();
        Ok(())
    }
    fn firmware_resources(&self) -> impl Iterator<Item = fstart_core::acpi::AcpiResource> {
        fstart_driver_lenovo::t6::firmware_resources(&crate::T6_H8)
    }

    fn before_devices(
        &mut self,
        ctx: &mut fstart_platform_intel::IntelMainstageBoardCtx<crate::Hardware>,
    ) -> Result<(), ServiceError> {
        use fstart_driver_lenovo::{
            h8::H8,
            t6::{self, dock},
        };
        if ctx.resume {
            return Err(ServiceError::NotSupported);
        }
        if !t6::ec_init(crate::MODEL, ctx.southbridge(), &crate::T6_H8, false) {
            fstart_log::error!("T6: EC/thermal initialization incomplete");
        }
        let legacy = dock::legacy_io_present(ctx.southbridge())?;
        let bay = H8::new(os_ec()).ultrabay_device_present();
        if let Some(present) = bay {
            ctx.southbridge()
                .set_ide_primary_enabled(dock::ide_primary_enabled(legacy, present));
        } else {
            fstart_log::error!("T6: Ultrabay presence unavailable; PATA remains disabled");
        }
        if !dock::mainstage_power_policy(os_ec(), &crate::T6_H8, legacy) {
            fstart_log::error!("T6: Ultrabay power/LED policy incomplete");
        }
        self.native_brightness = fstart_pci::EcamDevice::new(0, 2, 0).vendor_id() == 0x8086;
        fstart_log::info!(
            "T6: experimental, no hardware/thermal validation; S3, ATI, TI CardBus setup, IR and parallel unsupported"
        );
        if self.console_failed {
            fstart_log::error!("T6: dock/module setup failed; serial console may be unavailable");
        }
        Ok(())
    }
    fn after_devices(
        &mut self,
        ctx: &mut fstart_platform_intel::IntelMainstageBoardCtx<crate::Hardware>,
    ) -> Result<(), ServiceError> {
        use fstart_driver_intel::generic::ck505::I2cCk505;
        let mut clock = I2cCk505::new_at_address(crate::ck505_config(), 0x69)?;
        if clock.init_on_smbus(ctx.southbridge()).is_err() {
            fstart_log::error!("T6: CK505 initialization failed");
        }
        self.identity = fstart_driver_lenovo::eeprom::Identity::read(ctx.southbridge()).ok();
        self.ec_oem = fstart_driver_lenovo::h8::H8::new(os_ec()).smbios_oem_string();
        Ok(())
    }
    fn before_handoff(
        &mut self,
        _ctx: &mut fstart_platform_intel::IntelMainstageBoardCtx<crate::Hardware>,
    ) -> Result<(), ServiceError> {
        use fstart_driver_intel::southbridge::smi::{APM_CNT, APM_CNT_ACPI_DISABLE};
        // SAFETY: permanent board SMM has been installed and owns APMC.
        unsafe { fstart_core::pio::outb(APM_CNT, APM_CNT_ACPI_DISABLE) };
        Ok(())
    }
}

static SOCKETS: [&str; 1] = ["Socket M"];
pub static T6_SMBIOS_IDENTITY: fstart_acpi::smbios::SmbiosIdentity<'static> =
    fstart_acpi::smbios::SmbiosIdentity {
        bios_vendor: "fstart",
        bios_version: "CBET4000 fstart 0.1.0",
        bios_release_date: fstart_platform_intel::SMBIOS_RELEASE_DATE,
        sys_manufacturer: "LENOVO",
        sys_product: crate::PRODUCT_NAME,
        sys_version: crate::PRODUCT_NAME,
        sys_serial: None,
        sys_uuid: None,
        bb_manufacturer: "LENOVO",
        bb_product: crate::PRODUCT_NAME,
        bb_version: "",
        bb_serial: None,
        chassis_type: 0x0a,
        chassis_manufacturer: "LENOVO",
        processor_sockets: &SOCKETS,
        oem_string: None,
    };
