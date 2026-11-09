//! GA-945GCM-S2L ACPI and SMBIOS tables. Ramstage-only: they need the heap.

extern crate alloc;

use crate::mainboard::Acpi;
use alloc::vec::Vec;

impl fstart_acpi::device::AcpiDevice for Acpi {
    type Config = ();
    fn dsdt_aml(&self, _: &Self::Config) -> Vec<u8> {
        let sio = fstart_driver_superio::superio_dsdt_aml(&crate::ga945gcm_superio_config());
        fstart_acpi::aml_linker::scope_vec(
            <crate::config::Hardware as fstart_acpi::PcNamespace>::LPC.as_str(),
            &sio,
        )
        .expect("GA-945GCM Super I/O AML")
    }
}

pub static GA945GCM_SMBIOS_IDENTITY: fstart_acpi::smbios::SmbiosIdentity<'static> =
    fstart_acpi::smbios::SmbiosIdentity {
        bios_vendor: "fstart",
        bios_version: "0.1.0",
        bios_release_date: fstart_platform_intel::SMBIOS_RELEASE_DATE,
        sys_manufacturer: "Gigabyte Technology Co., Ltd.",
        sys_product: crate::PRODUCT_NAME,
        sys_version: "",
        sys_serial: None,
        sys_uuid: None,
        bb_manufacturer: "Gigabyte Technology Co., Ltd.",
        bb_product: crate::PRODUCT_NAME,
        bb_version: "",
        bb_serial: None,
        chassis_type: 0x03,
        chassis_manufacturer: "Gigabyte Technology Co., Ltd.",
        processor_sockets: &["LGA775"],
        oem_string: None,
    };
