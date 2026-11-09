//! D510MO ACPI and SMBIOS tables. Ramstage-only: they need the heap.

extern crate alloc;

use crate::mainboard::Acpi;
use alloc::vec::Vec;

impl fstart_acpi::device::AcpiDevice for Acpi {
    type Config = ();
    fn dsdt_aml(&self, _: &Self::Config) -> Vec<u8> {
        use fstart_acpi_macros::acpi_dsl;
        let mut aml = acpi_dsl! {
            Scope("\\_SB_") {
                Device("PWRB") { Name("_HID", "PNP0C0C"); Name("_PRW", Package(0x1du32, 0x04u32)); }
                Device("SLPB") { Name("_HID", "PNP0C0E"); Name("_PRW", Package(0x1du32, 0x04u32)); }
            }
        }
        .to_vec();
        let sio = fstart_driver_superio::superio_dsdt_aml(&crate::d510mo_superio_config());
        aml.extend(
            fstart_acpi::aml_linker::scope_vec(
                <crate::config::Hardware as fstart_acpi::PcNamespace>::LPC.as_str(),
                &sio,
            )
            .expect("D510MO Super I/O AML"),
        );
        aml
    }
}
pub static D510MO_SMBIOS_IDENTITY: fstart_acpi::smbios::SmbiosIdentity<'static> =
    fstart_acpi::smbios::SmbiosIdentity {
        bios_vendor: "fstart",
        bios_version: "0.1.0",
        bios_release_date: fstart_platform_intel::SMBIOS_RELEASE_DATE,
        sys_manufacturer: "Intel",
        sys_product: "D510MO",
        sys_version: "",
        sys_serial: None,
        sys_uuid: None,
        bb_manufacturer: "Intel",
        bb_product: "D510MO",
        bb_version: "",
        bb_serial: None,
        chassis_type: 0x03,
        chassis_manufacturer: "Intel",
        processor_sockets: &["FCBGA559"],
        oem_string: None,
    };
