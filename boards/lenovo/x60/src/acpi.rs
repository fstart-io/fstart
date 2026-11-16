//! ThinkPad X60 ACPI and SMBIOS tables. Ramstage-only: they need the heap.

extern crate alloc;

use crate::dock;
use crate::mainboard::{Acpi, Identity};
use alloc::vec::Vec;
use fstart_acpi::PcNamespace;
use fstart_acpi_macros::acpi_dsl;
use fstart_driver_lenovo::h8::H8Config;
use fstart_driver_lenovo::h8_acpi::{self, ec};

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
        x60_mainboard_dsdt_aml(self.bluetooth)
    }
}

/// Assemble the X60 DSDT.
pub fn x60_mainboard_dsdt_aml(bluetooth: bool) -> Vec<u8> {
    let h8 = H8Config {
        has_bluetooth: bluetooth,
        ..crate::config::X60_H8
    };
    dsdt_aml::<crate::config::Hardware>(&h8, fstart_platform_intel::i945::ICH7_PMBASE as u16 + 0x60)
}

/// Board DSDT: sleep/wake hooks, the UltraBase dock, the EC wake GPE and
/// the H8 EC surface, with hotkey brightness through the IGD. `tco_base` is
/// the TCO mailbox the dock's `_DCK` asks the SMI handler through.
pub fn dsdt_aml<N: PcNamespace>(h8: &H8Config, tco_base: u16) -> Vec<u8> {
    // Dock commands use the TCO mailbox, not an unbacked AML SMIF variable
    // or an I/O trap/GNVS channel.
    let mut out: Vec<u8> = acpi_dsl! {
        Scope("\\") {
            Method("_PTS", 1, NotSerialized) {
                #{const path 5 ec::<N>().child("MUTE")}(1u32);
                #{const path 5 ec::<N>().child("USBP")}(0u32);
                #{const path 5 ec::<N>().child("RADI")}(0u32);
                #{const path 6 ec::<N>().child("HKEY").child("MHKC")}(0u32);
            }
            Method("_WAK", 1, NotSerialized) {
                #{const path 6 ec::<N>().child("HKEY").child("MHKC")}(1u32);
                #{const path 6 ec::<N>().child("HKEY").child("WAKE")}(Arg0);
                Return(Package(0u32, 0u32));
            }
        }
    }
    .into();
    out.extend(dock_aml(tco_base));
    // The EC wake events (H8_WAKE#, GPIO8: GPE 0x18).
    out.extend_from_slice(&acpi_dsl! {
        Scope(#{const "\\_GPE"}) {
            Method("_L18", 0, NotSerialized) {
                Local0 = #{const path 5 ec::<N>().child("WAKE")};
                If (Local0 & 0x04u32) {
                    Notify(#{const path 5 ec::<N>().child("LID_")}, 0x02u32);
                }
                If (Local0 & 0x08u32) {
                    Notify(#{const "\\_SB_.DOCK"}, 0x03u32);
                    Notify(#{const path 5 ec::<N>().child("SLPB")}, 0x02u32);
                }
                If (Local0 & 0x10u32) {
                    Notify(#{const path 5 ec::<N>().child("SLPB")}, 0x02u32);
                }
                If (Local0 & 0x80u32) {
                    Notify(#{const path 5 ec::<N>().child("SLPB")}, 0x02u32);
                }
            }
        }
    });
    out.extend(h8_acpi::dsdt_aml_with_brightness::<N>(
        h8,
        &h8_acpi::igd_brightness::<N>(),
    ));
    out.extend(ec_queries_aml::<N>());
    out
}

/// The dock's ACPI device: `\_SB.DOCK` reports the DLPC switch state, and
/// its `_DCK` asks the board's SMI handler through the ICH TCO mailbox at
/// `tco_base` (PMBASE + 0x60), which [`dock::smm_dock_command`] answers.
fn dock_aml(tco_base: u16) -> Vec<u8> {
    // `_DCK` writes the mailbox commands as literals.
    const _: () = assert!(dock::SMI_DOCK_CONNECT == 1 && dock::SMI_DOCK_DISCONNECT == 2);
    let dlpc_switch = dock::DLPC_SWITCH;
    acpi_dsl! {
        Scope(#{const "\\_SB_"}) {
            OperationRegion("DLPC", SystemIO, #{word dlpc_switch}, 0x01u32);
            Field("DLPC", ByteAcc, NoLock, Preserve) {
                , 3,
                DSTA, 1,
            }
            OperationRegion("TCOX", SystemIO, #{word tco_base}, 0x20u32);
            Field("TCOX", ByteAcc, NoLock, Preserve) {
                Offset(0x02),
                TDIN, 8,
                TDOT, 8,
            }
            Device("DOCK") {
                Name("_HID", "ACPI0003");
                Name("_UID", 0u32);
                Name("_PCL", Package(#{const "\\_SB_"}));
                Method("_DCK", 1, Serialized) {
                    If (Arg0) {
                        TDIN = 1u32;
                    } Else {
                        TDIN = 2u32;
                    }
                    Return(TDOT);
                }
                Method("_PSR", 0, NotSerialized) {
                    Return(DSTA);
                }
                Method("_STA", 0, NotSerialized) {
                    Return(DSTA);
                }
            }
        }
    }
    .into()
}

/// The EC queries of the vendor DSDT around the dock: `_Q18` is the Fn-F9
/// undock hotkey, `_Q37`/`_Q58` dock attach, `_Q50` undock. They open the
/// EC's scope, so they follow the H8's own AML.
fn ec_queries_aml<N: PcNamespace>() -> Vec<u8> {
    acpi_dsl! {
        Scope(#{const path 4 ec::<N>()}) {
            Method("_Q18", 0, NotSerialized) { #{const "^HKEY.RHK_"}(0x09u32); }
            Method("_Q37", 0, NotSerialized) { Notify(#{const "\\_SB_.DOCK"}, 0u32); }
            Method("_Q50", 0, NotSerialized) { Notify(#{const "\\_SB_.DOCK"}, 3u32); }
            Method("_Q58", 0, NotSerialized) { Notify(#{const "\\_SB_.DOCK"}, 0u32); }
        }
    }
    .into()
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
        aml.extend(x60_mainboard_dsdt_aml(false));
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
