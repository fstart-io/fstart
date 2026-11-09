//! MB899 ACPI and SMBIOS tables. Ramstage-only: they need the heap.

extern crate alloc;

use crate::mainboard::Acpi;
use alloc::vec::Vec;
impl fstart_acpi::device::AcpiDevice for Acpi {
    type Config = ();
    fn dsdt_aml(&self, _: &Self::Config) -> Vec<u8> {
        let sio = fstart_driver_superio::superio_dsdt_aml(&crate::mb899_superio_config());
        fstart_acpi::aml_linker::scope_vec(
            <crate::config::Hardware as fstart_acpi::PcNamespace>::LPC.as_str(),
            &sio,
        )
        .expect("MB899 Super I/O AML")
    }
}

pub static MB899_SMBIOS_IDENTITY: fstart_acpi::smbios::SmbiosIdentity<'static> =
    fstart_acpi::smbios::SmbiosIdentity {
        bios_vendor: "fstart",
        bios_version: "0.1.0",
        bios_release_date: fstart_platform_intel::SMBIOS_RELEASE_DATE,
        sys_manufacturer: "IBASE",
        sys_product: "MB899",
        sys_version: "",
        sys_serial: None,
        sys_uuid: None,
        bb_manufacturer: "IBASE",
        bb_product: "MB899",
        bb_version: "",
        bb_serial: None,
        chassis_type: 0x03,
        chassis_manufacturer: "IBASE",
        processor_sockets: &["Socket M"],
        oem_string: None,
    };
