//! T60/R60/Z61t ACPI and SMBIOS tables. Ramstage-only: they need the heap.

extern crate alloc;

use crate::mainboard::{Acpi, Identity};
use alloc::vec::Vec;

impl Identity {
    pub fn smbios(&self) -> fstart_acpi::smbios::SmbiosIdentity<'_> {
        let mut result = T6_SMBIOS_IDENTITY;
        if let Some(eeprom) = &self.eeprom {
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
}

impl fstart_acpi::device::AcpiDevice for Acpi {
    type Config = ();
    fn dsdt_aml(&self, _: &Self::Config) -> Vec<u8> {
        fstart_driver_lenovo::t6::acpi::dsdt_aml::<crate::config::Hardware>(
            &crate::T6_H8,
            self.native_brightness,
        )
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
