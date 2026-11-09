//! D945GCLF ACPI and SMBIOS tables. Ramstage-only: they need the heap.

extern crate alloc;

use crate::mainboard::Acpi;
use alloc::vec::Vec;
use fstart_acpi_macros::acpi_dsl;

impl fstart_acpi::device::AcpiDevice for Acpi {
    type Config = ();

    fn dsdt_aml(&self, _: &Self::Config) -> Vec<u8> {
        d945gclf_mainboard_dsdt_aml()
    }
}

static D945GCLF_SMBIOS_PROCESSOR_SOCKETS: [&str; 1] = ["Socket 441"];

pub static D945GCLF_SMBIOS_IDENTITY: fstart_acpi::smbios::SmbiosIdentity<'static> =
    fstart_acpi::smbios::SmbiosIdentity {
        bios_vendor: "fstart",
        bios_version: "0.1.0",
        bios_release_date: fstart_platform_intel::SMBIOS_RELEASE_DATE,
        sys_manufacturer: "Intel",
        sys_product: "D945GCLF",
        sys_version: "1.0",
        sys_serial: None,
        sys_uuid: None,
        bb_manufacturer: "Intel",
        bb_product: "D945GCLF",
        bb_version: "",
        bb_serial: None,
        chassis_type: 0x03,
        chassis_manufacturer: "Intel",
        processor_sockets: &D945GCLF_SMBIOS_PROCESSOR_SOCKETS,
        oem_string: None,
    };

/// Board DSDT fragment: sleep/power buttons (coreboot `mainboard.asl`)
/// plus the SuperIO scope (coreboot `superio.asl`) under LPCB.
pub fn d945gclf_mainboard_dsdt_aml() -> Vec<u8> {
    let mut out = Vec::new();

    out.extend_from_slice(&acpi_dsl! {
        Scope("\\_SB_") {
            Device("SLPB") {
                Name("_HID", "PNP0C0E");
                Name("_PRW", Package(0x1du32, 0x04u32));
            }
            Device("PWRB") {
                Name("_HID", "PNP0C0C");
                Name("_PRW", Package(0x1du32, 0x04u32));
            }
        }
    });

    let config = crate::d945gclf_superio_config();
    let sio = fstart_driver_superio::superio_dsdt_aml(&config);
    out.extend(
        fstart_acpi::aml_linker::scope_vec(
            <crate::config::Hardware as fstart_acpi::PcNamespace>::LPC.as_str(),
            &sio,
        )
        .expect("D945GCLF required Super I/O scope emission failed"),
    );
    out
}
