//! ThinkPad X201 ACPI and SMBIOS tables. Ramstage-only: they need the heap.

extern crate alloc;

use crate::ec as x201;
use crate::mainboard::{Acpi, Identity};
use alloc::vec::Vec;
use fstart_acpi::PcNamespace;
use fstart_acpi::devices::tpm::{TpmFamily, TpmTisAcpi};
use fstart_driver_tpm::Family;
use fstart_platform_intel::ironlake::IronlakeIbexPeak;

impl Identity {
    /// The SMBIOS identity: like coreboot (`at24rf08c/lenovo_serials.c`),
    /// the EEPROM fields describe both system and board.
    pub fn smbios(&self) -> fstart_acpi::smbios::SmbiosIdentity<'_> {
        let mut identity = X201_SMBIOS_IDENTITY;
        if let Some(eeprom) = &self.eeprom {
            identity.sys_product = eeprom.part_number();
            identity.sys_version = eeprom.version();
            identity.sys_serial = Some(eeprom.serial_number());
            identity.sys_uuid = Some(eeprom.uuid);
            identity.bb_product = eeprom.part_number();
            identity.bb_version = eeprom.version();
            identity.bb_serial = Some(eeprom.serial_number());
        }
        identity.oem_string = self.ec_oem_string.as_ref().map(|oem| oem.as_str());
        identity
    }
}

impl fstart_acpi::device::AcpiDevice for Acpi {
    type Config = ();

    fn dsdt_aml(&self, _: &Self::Config) -> Vec<u8> {
        let mut aml = x201_mainboard_dsdt_aml(self.bluetooth, self.digitizer);
        if let Some(family) = self.tpm {
            // coreboot `lpc_tpm_fill_ssdt()` places it under `\_SB_.PCI0`.
            let tpm = TpmTisAcpi {
                family: match family {
                    Family::Tpm12 => TpmFamily::Tpm12,
                    Family::Tpm20 => TpmFamily::Tpm20,
                },
                server: false,
                log: None,
            };
            aml.extend(
                fstart_acpi::aml_linker::scope_vec("\\_SB_.PCI0", &tpm.dsdt_aml())
                    .expect("X201 TPM emission"),
            );
        }
        aml
    }
}

/// Assemble the X201 DSDT.
pub fn x201_mainboard_dsdt_aml(bluetooth: bool, digitizer: bool) -> Vec<u8> {
    let h8 = fstart_driver_lenovo::h8::H8Config {
        has_bluetooth: bluetooth,
        ..crate::config::X201_H8
    };
    let mut aml = x201::acpi::dsdt_aml::<IronlakeIbexPeak>(
        &h8,
        fstart_platform_intel::ironlake::IBEXPEAK_GPIOBASE,
        digitizer,
    );
    // Kconfig PS2K_EISAID/PS2M_EISAID: the TrackPoint is LEN0018.
    aml.extend(
        fstart_acpi::aml_linker::scope_vec(
            IronlakeIbexPeak::LPC.as_str(),
            &fstart_driver_intel::southbridge::acpi::ps2_controller_nodes_with(
                "PNP0303", "LEN0018",
            ),
        )
        .expect("X201 PS/2 emission"),
    );
    aml
}

static X201_SMBIOS_PROCESSOR_SOCKETS: [&str; 1] = ["BGA1288"];

pub static X201_SMBIOS_IDENTITY: fstart_acpi::smbios::SmbiosIdentity<'static> =
    fstart_acpi::smbios::SmbiosIdentity {
        bios_vendor: "fstart",
        // Non-OEM firmware ID, following coreboot and the X200 binding;
        // Linux thinkpad_acpi validates this before the ThinkPad model string.
        bios_version: "CBET4000 fstart 0.1.0",
        bios_release_date: fstart_platform_intel::SMBIOS_RELEASE_DATE,
        sys_manufacturer: "LENOVO",
        sys_product: "ThinkPad X201",
        sys_version: "ThinkPad X201",
        sys_serial: None,
        sys_uuid: None,
        bb_manufacturer: "LENOVO",
        bb_product: "ThinkPad X201",
        bb_version: "1.0",
        bb_serial: None,
        chassis_type: 0x0a,
        chassis_manufacturer: "LENOVO",
        processor_sockets: &X201_SMBIOS_PROCESSOR_SOCKETS,
        oem_string: None,
    };

#[cfg(test)]
mod tests {
    use super::*;
    use fstart_acpi::device::AcpiDevice;
    use fstart_driver_intel::ibexpeak::IntelIbexPeak;
    use fstart_driver_intel::ironlake::IntelIronlake;
    use fstart_driver_intel::namespace::SouthbridgeAcpi;
    use fstart_driver_intel::{IntelNorthbridgeDriver, IntelSouthbridgeDriver};
    use std::{fs, process::Command};

    fn iasl(dir: &std::path::Path, arguments: &[&str]) {
        let output = Command::new("iasl")
            .current_dir(dir)
            .args(arguments)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "iasl {arguments:?} failed: {}{}",
            std::string::String::from_utf8_lossy(&output.stdout),
            std::string::String::from_utf8_lossy(&output.stderr)
        );
    }

    /// The chipset and board DSDT parts disassemble and recompile, and
    /// carry coreboot's X201 surface (dock, EC wake, PS/2 IDs).
    #[test]
    fn complete_dsdt_iasl_round_trip() {
        let north_config = &crate::X201_PLATFORM.northbridge;
        let south_config = &crate::X201_PLATFORM.southbridge;
        let north = IntelIronlake::new_from_config(north_config).unwrap();
        let south = IntelIbexPeak::new_from_config(south_config).unwrap();

        let mut aml = north.dsdt_aml(north_config);
        aml.extend(south.dsdt_aml(
            south_config,
            &fstart_driver_intel::namespace::pci_root::<IntelIronlake>(),
        ));
        aml.extend(
            Acpi {
                bluetooth: true,
                digitizer: false,
                tpm: Some(Family::Tpm12),
            }
            .dsdt_aml(&()),
        );
        let dsdt = fstart_acpi::platform::build_dsdt(&aml);

        let dir =
            std::env::temp_dir().join(std::format!("fstart-x201-dsdt-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("dsdt.aml"), &dsdt).unwrap();
        iasl(&dir, &["-d", "dsdt.aml"]);
        let source = fs::read_to_string(dir.join("dsdt.dsl")).unwrap();
        for method in [
            "_Q18", "_Q37", "_Q45", "_Q50", "_Q58", "_DCK", "_L18", "UCMS",
        ] {
            assert!(
                source.contains(&std::format!("Method ({method},")),
                "missing {method}"
            );
        }
        assert!(source.contains("EisaId (\"LEN0018\")"));
        assert!(source.contains("EisaId (\"PNP0C31\")"));
        assert!(source.contains("GFX0.INCB ()"));
        assert!(source.contains("Name (_GPE, 0x11)"));
        fs::rename(dir.join("dsdt.aml"), dir.join("original.aml")).unwrap();
        iasl(&dir, &["-oa", "-tc", "dsdt.dsl"]);
        fs::remove_dir_all(dir).unwrap();
    }
}
