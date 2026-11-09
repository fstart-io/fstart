//! ThinkPad X60 ACPI and SMBIOS tables. Ramstage-only: they need the heap.

extern crate alloc;

use crate::mainboard::{Acpi, Identity};
use alloc::vec::Vec;

impl Identity {
    pub fn smbios(&self) -> fstart_acpi::smbios::SmbiosIdentity<'_> {
        let mut identity = X60_SMBIOS_IDENTITY;
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
        x60_mainboard_dsdt_aml()
    }
}

pub fn x60_mainboard_dsdt_aml() -> Vec<u8> {
    fstart_driver_lenovo::x6::acpi::dsdt_aml::<crate::config::Hardware>(&crate::config::X60_H8)
}

static X60_SMBIOS_PROCESSOR_SOCKETS: [&str; 1] = ["Socket M"];

pub static X60_SMBIOS_IDENTITY: fstart_acpi::smbios::SmbiosIdentity<'static> =
    fstart_acpi::smbios::SmbiosIdentity {
        bios_vendor: "fstart",
        bios_version: "CBET4000 fstart 0.1.0",
        bios_release_date: fstart_platform_intel::SMBIOS_RELEASE_DATE,
        sys_manufacturer: "LENOVO",
        sys_product: "ThinkPad X60",
        sys_version: "ThinkPad X60",
        sys_serial: None,
        sys_uuid: None,
        bb_manufacturer: "LENOVO",
        bb_product: "ThinkPad X60",
        bb_version: "",
        bb_serial: None,
        chassis_type: 0x0a,
        chassis_manufacturer: "LENOVO",
        processor_sockets: &X60_SMBIOS_PROCESSOR_SOCKETS,
        oem_string: None,
    };

#[cfg(test)]
mod tests {
    use super::*;
    use fstart_acpi::device::AcpiDevice;
    use fstart_driver_intel::i945::IntelI945;
    use fstart_driver_intel::ich7::IntelIch7;
    use fstart_driver_intel::{IntelNorthbridgeDriver, IntelSouthbridgeDriver};
    use std::fs;
    use std::process::Command;

    fn iasl(dir: &std::path::Path, args: &[&str]) {
        let output = Command::new("iasl")
            .current_dir(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "iasl {args:?} failed: {}{}",
            std::string::String::from_utf8_lossy(&output.stdout),
            std::string::String::from_utf8_lossy(&output.stderr)
        );
    }

    /// The chipset and board fragments must form one namespace.
    #[test]
    fn complete_dsdt_iasl_round_trip() {
        let north_config = &crate::X60_PLATFORM.northbridge;
        let south_config = &crate::X60_PLATFORM.southbridge;
        let north = IntelI945::new_from_config(north_config).unwrap();
        let south = IntelIch7::new_from_config(south_config).unwrap();
        let mut aml = north.dsdt_aml(north_config);
        aml.extend(fstart_driver_intel::namespace::SouthbridgeAcpi::dsdt_aml(
            &south,
            south_config,
            &fstart_driver_intel::namespace::pci_root::<IntelI945>(),
        ));
        aml.extend(x60_mainboard_dsdt_aml());
        let dsdt = fstart_acpi::platform::build_dsdt(&aml);
        let dir = std::env::temp_dir().join(std::format!("fstart-x60-dsdt-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("dsdt.aml"), &dsdt).unwrap();
        iasl(&dir, &["-d", "dsdt.aml"]);
        let source = fs::read_to_string(dir.join("dsdt.dsl")).unwrap();
        for name in [
            "Device (PEGP)",
            "Device (GFX0)",
            "Device (LCD0)",
            "Device (DOCK)",
        ] {
            assert_eq!(source.matches(name).count(), 1, "{name}");
        }
        assert!(source.contains("Name (_GPE, 0x1C)"));
        assert!(source.contains("GFX0.INCB ()"));
        assert!(source.contains("0x0008FFFF"));
        fs::rename(dir.join("dsdt.aml"), dir.join("original.aml")).unwrap();
        iasl(&dir, &["-oa", "-tc", "dsdt.dsl"]);
        fs::remove_dir_all(dir).unwrap();
    }
}
