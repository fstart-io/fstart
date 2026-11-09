//! ASUS P5GC-MX ACPI and SMBIOS tables. Ramstage-only: they need the heap.

extern crate alloc;

use crate::mainboard::Acpi;
use alloc::vec::Vec;

impl fstart_acpi::device::AcpiDevice for Acpi {
    type Config = ();
    fn dsdt_aml(&self, _: &Self::Config) -> Vec<u8> {
        let sio = fstart_driver_superio::superio_dsdt_aml(&crate::p5gc_mx_superio_config());
        fstart_acpi::aml_linker::scope_vec(
            <crate::config::Hardware as fstart_acpi::PcNamespace>::LPC.as_str(),
            &sio,
        )
        .expect("P5GC-MX Super I/O AML")
    }
}

pub static P5GC_MX_SMBIOS_IDENTITY: fstart_acpi::smbios::SmbiosIdentity<'static> =
    fstart_acpi::smbios::SmbiosIdentity {
        bios_vendor: "fstart",
        bios_version: "0.1.0",
        bios_release_date: fstart_platform_intel::SMBIOS_RELEASE_DATE,
        sys_manufacturer: "ASUSTeK Computer Inc.",
        sys_product: "P5GC-MX",
        sys_version: "",
        sys_serial: None,
        sys_uuid: None,
        bb_manufacturer: "ASUSTeK Computer Inc.",
        bb_product: "P5GC-MX",
        bb_version: "",
        bb_serial: None,
        chassis_type: 0x03,
        chassis_manufacturer: "ASUSTeK Computer Inc.",
        processor_sockets: &["LGA775"],
        oem_string: None,
    };
