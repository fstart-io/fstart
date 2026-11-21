//! T60/R60/Z61t ACPI and SMBIOS tables. Ramstage-only: they need the heap.

extern crate alloc;

use crate::mainboard::{Acpi, Identity};
use alloc::vec::Vec;

impl Identity {
    pub fn smbios(&self) -> fstart_acpi::smbios::SmbiosIdentity<'_> {
        fstart_driver_lenovo::thinkpad::smbios_identity(
            T6_SMBIOS_IDENTITY,
            self.eeprom.as_ref(),
            self.ec_oem.as_ref(),
        )
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
