//! Foxconn D41S ACPI and SMBIOS tables. Ramstage-only: they need the heap.

extern crate alloc;

use crate::mainboard::Acpi;
use alloc::vec::Vec;

impl fstart_acpi::device::AcpiDevice for Acpi {
    type Config = ();

    fn dsdt_aml(&self, _: &Self::Config) -> Vec<u8> {
        d41s_mainboard_dsdt_aml()
    }
}

pub fn d41s_mainboard_dsdt_aml() -> Vec<u8> {
    let config = crate::d41s_superio_config();
    let sio = fstart_driver_superio::superio_dsdt_aml(&config);
    fstart_acpi::aml_linker::scope_vec(
        <crate::config::Hardware as fstart_acpi::PcNamespace>::LPC.as_str(),
        &sio,
    )
    .expect("D41S required Super I/O scope emission failed")
}

static D41S_SMBIOS_PROCESSOR_SOCKETS: [&str; 1] = ["FCBGA559"];

pub static D41S_SMBIOS_IDENTITY: fstart_acpi::smbios::SmbiosIdentity<'static> =
    fstart_acpi::smbios::SmbiosIdentity {
        bios_vendor: "fstart",
        bios_version: "0.1.0",
        bios_release_date: fstart_platform_intel::SMBIOS_RELEASE_DATE,
        sys_manufacturer: "Foxconn",
        sys_product: "D41S",
        sys_version: "1.0",
        sys_serial: None,
        sys_uuid: None,
        bb_manufacturer: "Foxconn",
        bb_product: "D41S",
        bb_version: "",
        bb_serial: None,
        chassis_type: 0x03,
        chassis_manufacturer: "Foxconn",
        processor_sockets: &D41S_SMBIOS_PROCESSOR_SOCKETS,
        oem_string: None,
    };
