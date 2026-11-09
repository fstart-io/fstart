//! ThinkPad X61 ACPI and SMBIOS tables. Ramstage-only: they need the heap.

extern crate alloc;

use crate::mainboard::{Acpi, Identity};
use alloc::vec::Vec;

impl Identity {
    /// The SMBIOS identity: like coreboot, the EEPROM fields describe both
    /// system and board.
    pub fn smbios(&self) -> fstart_acpi::smbios::SmbiosIdentity<'_> {
        let mut identity = X61_SMBIOS_IDENTITY;
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
        x61_mainboard_dsdt_aml()
    }
}

/// Assemble the X61 DSDT.
pub fn x61_mainboard_dsdt_aml() -> Vec<u8> {
    fstart_driver_lenovo::x6::acpi::dsdt_aml::<crate::config::Hardware>(&crate::config::X61_H8)
}

static X61_SMBIOS_PROCESSOR_SOCKETS: [&str; 1] = ["Socket M"];

pub static X61_SMBIOS_IDENTITY: fstart_acpi::smbios::SmbiosIdentity<'static> =
    fstart_acpi::smbios::SmbiosIdentity {
        bios_vendor: "fstart",
        // Linux thinkpad_acpi requires a Lenovo-shaped firmware ID before
        // inspecting the product version. Use coreboot's non-OEM ID, retaining
        // our real firmware identity rather than impersonating a Lenovo BIOS.
        bios_version: "CBET4000 fstart 0.1.0",
        bios_release_date: fstart_platform_intel::SMBIOS_RELEASE_DATE,
        sys_manufacturer: "LENOVO",
        sys_product: "ThinkPad X61",
        sys_version: "ThinkPad X61",
        sys_serial: None,
        sys_uuid: None,
        bb_manufacturer: "LENOVO",
        bb_product: "ThinkPad X61",
        bb_version: "",
        bb_serial: None,
        chassis_type: 0x0a,
        chassis_manufacturer: "LENOVO",
        processor_sockets: &X61_SMBIOS_PROCESSOR_SOCKETS,
        oem_string: None,
    };

#[cfg(test)]
mod tests {
    use super::*;
    use fstart_acpi::device::AcpiDevice;
    use fstart_driver_intel::gm965::IntelGm965;
    use fstart_driver_intel::ich8::IntelIch8;
    use fstart_driver_intel::{IntelNorthbridgeDriver, IntelSouthbridgeDriver};
    use std::fs;
    use std::process::Command;

    #[test]
    fn complete_dsdt_iasl_round_trip() {
        let north_config = &crate::X61_PLATFORM.northbridge;
        let south_config = &crate::X61_PLATFORM.southbridge;
        let north = IntelGm965::new_from_config(north_config).unwrap();
        let south = IntelIch8::new_from_config(south_config).unwrap();

        let mut aml = north.dsdt_aml(north_config);
        aml.extend(fstart_driver_intel::namespace::SouthbridgeAcpi::dsdt_aml(
            &south,
            south_config,
            &fstart_driver_intel::namespace::pci_root::<IntelGm965>(),
        ));
        aml.extend(x61_mainboard_dsdt_aml());
        let dsdt = fstart_acpi::platform::build_dsdt(&aml);

        let dir = std::env::temp_dir().join(std::format!("fstart-x61-dsdt-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("dsdt.aml"), &dsdt).unwrap();

        let disassemble = Command::new("iasl")
            .current_dir(&dir)
            .args(["-d", "dsdt.aml"])
            .output()
            .unwrap();
        assert!(
            disassemble.status.success(),
            "iasl -d failed: {}{}",
            std::string::String::from_utf8_lossy(&disassemble.stdout),
            std::string::String::from_utf8_lossy(&disassemble.stderr)
        );
        let source = fs::read_to_string(dir.join("dsdt.dsl")).unwrap();
        for method in ["_Q18", "_Q37", "_Q50", "_Q58", "BRTU", "BRTD"] {
            assert!(source.contains(&std::format!("Method ({method},")));
        }
        for (method, event) in [("_Q37", "Zero"), ("_Q58", "Zero"), ("_Q50", "0x03")] {
            let body = source
                .split(&std::format!("Method ({method},"))
                .nth(1)
                .unwrap()
                .split('}')
                .next()
                .unwrap();
            assert!(body.contains(&std::format!("Notify (\\_SB.DOCK, {event})")));
        }
        assert!(source.contains("HKEY.RHK (0x09)"));
        assert!(source.contains("GFX0.INCB ()"));
        assert!(source.contains("GFX0.DECB ()"));
        assert!(!source.contains("SMIF"));
        assert!(!source.contains("Method (TRAP,"));
        fs::rename(dir.join("dsdt.aml"), dir.join("original.aml")).unwrap();
        let compile = Command::new("iasl")
            .current_dir(&dir)
            .args(["-oa", "-tc", "dsdt.dsl"])
            .output()
            .unwrap();
        assert!(
            compile.status.success(),
            "iasl -tc failed: {}{}",
            std::string::String::from_utf8_lossy(&compile.stdout),
            std::string::String::from_utf8_lossy(&compile.stderr)
        );

        // ACPICA narrows pre-existing shared Intel integer encodings on
        // the first pass. Once canonicalized, require stable AML bytes.
        let canonical = fs::read(dir.join("dsdt.aml")).unwrap();
        fs::rename(dir.join("dsdt.dsl"), dir.join("original.dsl")).unwrap();
        for arguments in [&["-d", "dsdt.aml"][..], &["-oa", "-tc", "dsdt.dsl"][..]] {
            let output = Command::new("iasl")
                .current_dir(&dir)
                .args(arguments)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "canonical roundtrip failed: {}{}",
                std::string::String::from_utf8_lossy(&output.stdout),
                std::string::String::from_utf8_lossy(&output.stderr)
            );
        }
        let recompiled = fs::read(dir.join("dsdt.aml")).unwrap();
        // Ignore only compiler-identification fields in the table header.
        assert_eq!(&recompiled[36..], &canonical[36..]);
        fs::remove_dir_all(dir).unwrap();
    }
}
