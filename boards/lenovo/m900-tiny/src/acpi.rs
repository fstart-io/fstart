// SPDX-License-Identifier: MIT OR Apache-2.0
//! The board's ACPI: the Super I/O's devices and decodes, the TPM and the
//! console's SPCR. The NCT6687D's EC is not an ACPI EC.
use alloc::vec::Vec;
use fstart_acpi::device::AcpiDevice;
use fstart_acpi::devices::tpm::{TpmFamily, TpmTisAcpi};
use fstart_acpi_macros::acpi_dsl;
use fstart_driver_superio::SuperIoConfig;

pub struct M900Acpi {
    config: SuperIoConfig,
    tpm: Option<TpmFamily>,
}
impl M900Acpi {
    /// `config` is what `mainboard::init_superio` set up; `tpm` the family
    /// of the TIS TPM that answered, if any.
    pub fn new(config: SuperIoConfig, tpm: Option<fstart_driver_tpm::Family>) -> Self {
        let tpm = tpm.map(|family| match family {
            fstart_driver_tpm::Family::Tpm12 => TpmFamily::Tpm12,
            fstart_driver_tpm::Family::Tpm20 => TpmFamily::Tpm20,
        });
        Self { config, tpm }
    }
}

pub static SMBIOS: fstart_acpi::smbios::SmbiosIdentity<'static> =
    fstart_acpi::smbios::SmbiosIdentity {
        bios_vendor: "fstart",
        bios_version: "0.1.0",
        bios_release_date: fstart_platform_intel::SMBIOS_RELEASE_DATE,
        sys_manufacturer: "LENOVO",
        sys_product: "ThinkCentre M700 / M900 Tiny",
        sys_version: "",
        sys_serial: None,
        sys_uuid: None,
        bb_manufacturer: "LENOVO",
        bb_product: "ThinkCentre M700 / M900 Tiny",
        bb_version: "",
        bb_serial: None,
        chassis_type: 0x23,
        chassis_manufacturer: "LENOVO",
        chassis_serial: None,
        processor_sockets: &["LGA1151"],
        oem_string: None,
        ipmi: None,
    };

/// The host ACPI baseline: what the ramstage describes on a board with its
/// TPM 1.2 fitted.
impl fstart_platform_intel::acpi::BoardAcpi for crate::Board {
    type Acpi = M900Acpi;

    fn acpi_baseline() -> Self::Acpi {
        M900Acpi::new(
            crate::superio::console_config(crate::mainboard::CONSOLE_BAUD),
            Some(fstart_driver_tpm::Family::Tpm12),
        )
    }
}

impl AcpiDevice for M900Acpi {
    type Config = ();
    fn dsdt_aml(&self, _: &()) -> Vec<u8> {
        let mut children = fstart_driver_superio::superio_dsdt_aml(&self.config);
        // Vendor-indexed EC is firmware-owned. Reserve ports, do not expose
        // them as a standard ACPI EC with nonexistent command/event methods.
        children.extend_from_slice(&acpi_dsl! {Device("SIRC") {
            Name("_HID",EisaId("PNP0C02"));Name("_UID",3u32);
            Name("_CRS",ResourceTemplate {
                IO(0x2eu16,0x2eu16,1u8,2u8);
                IO(0xa10u16,0xa10u16,1u8,8u8);
                IO(0xa20u16,0xa20u16,1u8,8u8);
            });
        }});
        let mut aml = fstart_acpi::aml_linker::scope_vec(
            fstart_driver_intel::namespace::lpc::<
                fstart_driver_intel::skylake::driver::IntelSkylake,
                fstart_driver_intel::sunrise_point::driver::IntelSunrisePoint,
            >()
            .as_str(),
            &children,
        )
        .expect("M900 LPC scope");
        if let Some(family) = self.tpm {
            // coreboot lpc_tpm_fill_ssdt() places it under \_SB_.PCI0.
            let tpm = TpmTisAcpi {
                family,
                server: false,
                log: None,
            };
            aml.extend(
                fstart_acpi::aml_linker::scope_vec(
                    fstart_driver_intel::namespace::pci_root::<
                        fstart_driver_intel::skylake::driver::IntelSkylake,
                    >()
                    .as_str(),
                    &tpm.dsdt_aml(),
                )
                .expect("M900 TPM scope"),
            );
        }
        aml
    }
    /// SPCR for the NCT6687D COM1 the firmware console uses, as native.
    fn extra_tables(&self, _: &()) -> Vec<Vec<u8>> {
        let com = self.config.com1.as_ref().expect("M900 COM1");
        let mut spcr = Vec::new();
        fstart_acpi::Aml::to_aml_bytes(
            &fstart_acpi::spcr::build_spcr_16550_io(com.io_base, com.irq),
            &mut spcr,
        );
        alloc::vec![spcr]
    }
}
