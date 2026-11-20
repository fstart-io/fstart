//! ThinkPad X220 ACPI and SMBIOS tables. Ramstage-only: they need the heap.

extern crate alloc;

use crate::config::X220_H8;
use crate::mainboard::{Acpi, Identity};
use alloc::vec::Vec;
use fstart_acpi::PcNamespace;
use fstart_acpi::devices::tpm::{TpmFamily, TpmTisAcpi};
use fstart_acpi_macros::acpi_dsl;
use fstart_driver_tpm::Family;
use fstart_platform_intel::sandybridge::SandybridgeBd82x6x;

impl Identity {
    /// The SMBIOS identity: like coreboot (`at24rf08c/lenovo_serials.c`),
    /// the EEPROM fields describe both system and board.
    pub fn smbios(&self) -> fstart_acpi::smbios::SmbiosIdentity<'_> {
        let mut identity = X220_SMBIOS_IDENTITY;
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
        let mut aml = x220_mainboard_dsdt_aml(self.wwan);
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
                    .expect("X220 TPM emission"),
            );
        }
        aml
    }
}

/// Assemble the X220 board DSDT content (coreboot `dsdt.asl`,
/// `acpi/platform.asl`, `acpi/ec.asl`, `acpi/superio.asl`).
pub fn x220_mainboard_dsdt_aml(wwan: bool) -> Vec<u8> {
    let h8 = fstart_driver_lenovo::h8::H8Config {
        has_wwan: wwan,
        ..X220_H8
    };
    // Hotkey brightness goes through GFX0 (BRIGHTNESS_UP/DOWN).
    let brightness = acpi_dsl! {
        Scope("\\") {
            Method("BRTU", 0, NotSerialized) { #{const "\\_SB_.PCI0.GFX0.INCB"}(); }
            Method("BRTD", 0, NotSerialized) { #{const "\\_SB_.PCI0.GFX0.DECB"}(); }
        }
    };
    let mut aml = fstart_driver_lenovo::h8_acpi::dsdt_aml_with_brightness::<SandybridgeBd82x6x>(
        &h8,
        &brightness,
    );
    aml.extend_from_slice(&acpi_dsl! {
        Scope("\\") {
            // ec/lenovo/h8/acpi/thinklight.asl.
            Method("UCMS", 1, Serialized) {
                If (Arg0 == 0x0Cu32) {
                    #{const "\\_SB_.PCI0.LPCB.EC__.LGHT"}(1u32);
                }
                If (Arg0 == 0x0Du32) {
                    #{const "\\_SB_.PCI0.LPCB.EC__.LGHT"}(0u32);
                }
            }
            Method("_PTS", 1, NotSerialized) {
                #{const "\\_SB_.PCI0.LPCB.EC__.MUTE"}(1u32);
                #{const "\\_SB_.PCI0.LPCB.EC__.USBP"}(0u32);
                #{const "\\_SB_.PCI0.LPCB.EC__.RADI"}(0u32);
            }
            Method("_WAK", 1, NotSerialized) {
                #{const "\\_SB_.PCI0.LPCB.EC__.HKEY.WAKE"}(Arg0);
                Return(Package(0u32, 0u32));
            }
        }
    });
    // Kconfig PS2K_EISAID/PS2M_EISAID: the TrackPoint is LEN0020.
    aml.extend(
        fstart_acpi::aml_linker::scope_vec(
            SandybridgeBd82x6x::LPC.as_str(),
            &fstart_driver_intel::southbridge::acpi::ps2_controller_nodes_with(
                "PNP0303", "LEN0020",
            ),
        )
        .expect("X220 PS/2 emission"),
    );
    aml.extend(usb_ports_aml());
    aml
}

/// `variants/x220/overridetree.cb` `drivers/usb/acpi` ports behind the
/// rate-matching hubs.
fn usb_ports_aml() -> Vec<u8> {
    use fstart_acpi::usb::{Pld, PldHorizontal as H, PldPanel as P, UpcType, port_aml};
    const fn a(
        description: &'static str,
        panel: P,
        horizontal: H,
        group: u8,
    ) -> (&'static str, UpcType, Pld) {
        (
            description,
            UpcType::A,
            Pld::type_a(panel, horizontal, (group, 0)),
        )
    }
    const fn internal(description: &'static str) -> (&'static str, UpcType, Pld) {
        (
            description,
            UpcType::Internal,
            Pld::for_upc(UpcType::Internal, (0, 0)),
        )
    }
    let ports: [(&str, u8, (&str, UpcType, Pld)); 11] = [
        ("EHC1", 0, a("USB2.0 A Near VGA", P::Left, H::Left, 1)),
        (
            "EHC1",
            1,
            a("USB2.0 A Near ExpressCard", P::Left, H::Right, 1),
        ),
        ("EHC1", 2, internal("USB2 WLAN")),
        ("EHC1", 3, internal("USB2 WWAN")),
        ("EHC1", 5, internal("USB2 ExpressCard")),
        ("EHC2", 0, a("Docking 2", P::Left, H::Left, 3)),
        ("EHC2", 1, a("System Port 1", P::Right, H::Left, 2)),
        ("EHC2", 2, internal("FPR")),
        ("EHC2", 3, internal("BDC")),
        ("EHC2", 4, a("Docking 1", P::Left, H::Right, 3)),
        ("EHC2", 5, internal("Camera")),
    ];
    let mut aml = Vec::new();
    for (controller, port, (description, upc, pld)) in &ports {
        let scope = alloc::format!("\\_SB_.PCI0.{controller}.HUB7.URMH.PRT{port}");
        aml.extend(
            fstart_acpi::aml_linker::scope_vec(&scope, &port_aml(Some(description), *upc, pld))
                .expect("USB port AML"),
        );
    }
    aml
}

static X220_SMBIOS_PROCESSOR_SOCKETS: [&str; 1] = ["rPGA988B"];

pub static X220_SMBIOS_IDENTITY: fstart_acpi::smbios::SmbiosIdentity<'static> =
    fstart_acpi::smbios::SmbiosIdentity {
        bios_vendor: "fstart",
        // Non-OEM firmware ID, following coreboot and the X200/X201
        // bindings; Linux thinkpad_acpi validates this before the ThinkPad
        // model string.
        bios_version: "CBET4000 fstart 0.1.0",
        bios_release_date: fstart_platform_intel::SMBIOS_RELEASE_DATE,
        sys_manufacturer: "LENOVO",
        sys_product: "ThinkPad X220",
        sys_version: "ThinkPad X220",
        sys_serial: None,
        sys_uuid: None,
        bb_manufacturer: "LENOVO",
        bb_product: "ThinkPad X220",
        bb_version: "1.0",
        bb_serial: None,
        chassis_type: 0x0a,
        chassis_manufacturer: "LENOVO",
        processor_sockets: &X220_SMBIOS_PROCESSOR_SOCKETS,
        oem_string: None,
    };

#[cfg(test)]
mod tests {
    use super::*;
    use fstart_acpi::device::AcpiDevice;
    use fstart_driver_intel::bd82x6x::IntelBd82x6x;
    use fstart_driver_intel::namespace::SouthbridgeAcpi;
    use fstart_driver_intel::sandybridge::IntelSandybridge;
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
    /// carry coreboot's X220 surface (EC, sleep hooks, PS/2 IDs, USB ports).
    #[test]
    fn complete_dsdt_iasl_round_trip() {
        let north_config = &crate::X220_PLATFORM.northbridge;
        let south_config = &crate::X220_PLATFORM.southbridge;
        let north = IntelSandybridge::new_from_config(north_config).unwrap();
        let south = IntelBd82x6x::new_from_config(south_config).unwrap();

        let mut aml = north.dsdt_aml(north_config);
        aml.extend(south.dsdt_aml(
            south_config,
            &fstart_driver_intel::namespace::pci_root::<IntelSandybridge>(),
        ));
        aml.extend(
            Acpi {
                wwan: true,
                tpm: Some(Family::Tpm12),
            }
            .dsdt_aml(&()),
        );
        let dsdt = fstart_acpi::platform::build_dsdt(&aml);

        let dir =
            std::env::temp_dir().join(std::format!("fstart-x220-dsdt-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("dsdt.aml"), &dsdt).unwrap();
        iasl(&dir, &["-d", "dsdt.aml"]);
        let source = fs::read_to_string(dir.join("dsdt.dsl")).unwrap();
        for method in ["_PTS", "_WAK", "UCMS", "_L01"] {
            assert!(
                source.contains(&std::format!("Method ({method},")),
                "missing {method}"
            );
        }
        assert!(source.contains("EisaId (\"LEN0020\")"));
        assert!(source.contains("EisaId (\"PNP0C31\")"));
        assert!(source.contains("GFX0.INCB ()"));
        assert!(source.contains("Name (_PLD,"));
        fs::rename(dir.join("dsdt.aml"), dir.join("original.aml")).unwrap();
        iasl(&dir, &["-oa", "-tc", "dsdt.dsl"]);
        fs::remove_dir_all(dir).unwrap();
    }
}
