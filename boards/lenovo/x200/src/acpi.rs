//! X200 DSDT glue around the H8 EC: sleep/wake hooks, GFX0
//! brightness hotkeys, the ThinkLight `\UCMS` method, the EC wake GPE, the
//! dock (coreboot x200 `dsdt.asl`, `acpi/*.asl`) and the X200t digitizer,
//! plus the SMBIOS identity. Ramstage-only: they need the heap.

extern crate alloc;

use crate::mainboard::{Acpi, Identity};
use alloc::vec::Vec;
use fstart_acpi::PcNamespace;
use fstart_acpi_macros::acpi_dsl;
use fstart_driver_lenovo::h8::H8Config;
use fstart_driver_lenovo::h8_acpi::ec;

impl Identity {
    /// The SMBIOS identity: like coreboot (`at24rf08c/lenovo_serials.c`),
    /// the EEPROM fields describe both system and board.
    pub fn smbios(&self) -> fstart_acpi::smbios::SmbiosIdentity<'_> {
        let mut identity = X200_SMBIOS_IDENTITY;
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
        x200_mainboard_dsdt_aml(self.bluetooth, self.digitizer)
    }
}

/// Assemble the X200 DSDT.
pub fn x200_mainboard_dsdt_aml(bluetooth: bool, digitizer: bool) -> Vec<u8> {
    let h8 = fstart_driver_lenovo::h8::H8Config {
        has_bluetooth: bluetooth,
        ..crate::config::X200_H8
    };
    dsdt_aml::<<crate::Board as fstart_platform_intel::IntelBoardFacts>::Platform>(
        &h8,
        fstart_driver_intel::ich9::ich9::DEFAULT_GPIOBASE,
        digitizer,
    )
}

static X200_SMBIOS_PROCESSOR_SOCKETS: [&str; 1] = ["BGA956"];

pub static X200_SMBIOS_IDENTITY: fstart_acpi::smbios::SmbiosIdentity<'static> =
    fstart_acpi::smbios::SmbiosIdentity {
        bios_vendor: "fstart",
        // Non-OEM firmware ID, following coreboot and the X61 binding;
        // Linux thinkpad_acpi validates this before the ThinkPad model string.
        bios_version: "CBET4000 fstart 0.1.0",
        bios_release_date: fstart_platform_intel::SMBIOS_RELEASE_DATE,
        sys_manufacturer: "LENOVO",
        sys_product: "ThinkPad X200",
        sys_version: "ThinkPad X200",
        sys_serial: None,
        sys_uuid: None,
        bb_manufacturer: "LENOVO",
        bb_product: "ThinkPad X200",
        bb_version: "1.0",
        bb_serial: None,
        chassis_type: 0x0a,
        chassis_manufacturer: "LENOVO",
        processor_sockets: &X200_SMBIOS_PROCESSOR_SOCKETS,
        oem_string: None,
    };

/// Board DSDT. The platform's IGD provides `INCB`/`DECB`, the Core 2 CPU
/// AML provides `\CNOT`; `gpio_base` is the ICH GPIOBASE.
#[must_use]
pub fn dsdt_aml<N: PcNamespace>(h8: &H8Config, gpio_base: u16, digitizer: bool) -> Vec<u8> {
    // Coreboot x200 dsdt.asl: hotkey brightness goes through GFX0.
    let brightness = acpi_dsl! {
        Scope("\\") {
            Method("BRTU", 0, NotSerialized) { #{const path 4 N::IGD.child("INCB")}(); }
            Method("BRTD", 0, NotSerialized) { #{const path 4 N::IGD.child("DECB")}(); }
        }
    };
    let mut aml = fstart_driver_lenovo::h8_acpi::dsdt_aml_with_brightness::<N>(h8, &brightness);
    aml.extend_from_slice(&acpi_dsl! {
        Scope("\\") {
            // ec/lenovo/h8/acpi/thinklight.asl, for thinkpad_acpi.
            Method("UCMS", 1, Serialized) {
                If (Arg0 == 0x0Cu32) {
                    #{const path 5 ec::<N>().child("LGHT")}(1u32);
                }
                If (Arg0 == 0x0Du32) {
                    #{const path 5 ec::<N>().child("LGHT")}(0u32);
                }
            }
            Method("_PTS", 1, NotSerialized) {
                #{const path 5 ec::<N>().child("MUTE")}(1u32);
                #{const path 5 ec::<N>().child("USBP")}(0u32);
                #{const path 5 ec::<N>().child("RADI")}(0u32);
            }
            Method("_WAK", 1, NotSerialized) {
                #{const path 6 ec::<N>().child("HKEY").child("WAKE")}(Arg0);
                Return(Package(0u32, 0u32));
            }
        }
        Scope("\\_GPE") {
            Method("_L18", 0, NotSerialized) {
                Local0 = #{const path 5 ec::<N>().child("WAKE")};
            }
        }
    });
    aml.extend(dock_aml::<N>(gpio_base));
    if digitizer {
        aml.extend(
            fstart_acpi::aml_linker::scope_vec(
                N::LPC.as_str(),
                &fstart_driver_lenovo::wacom::digitizer_aml(),
            )
            .expect("X200 digitizer emission"),
        );
    }
    aml
}

/// Coreboot x200 `acpi/dock.asl`: the dock device keyed on the GPIO2-4 dock
/// ID and EC DKR1, plus the EC dock queries. Only the GPIO level bits the
/// dock methods use are declared (GPIOBASE + 0x0c).
fn dock_aml<N: PcNamespace>(gpio_base: u16) -> Vec<u8> {
    let gpio_level = u32::from(gpio_base) + 0x0c;
    let mut aml: Vec<u8> = acpi_dsl! {
        Scope("\\") {
            OperationRegion("GPLV", SystemIO, #{dword gpio_level}, 0x04u32);
            Field("GPLV", ByteAcc, NoLock, Preserve) {
                , 2,
                GP02, 1,
                GP03, 1,
                GP04, 1,
                , 23,
                GP28, 1,
            }
        }
    }
    .into();
    // DKR1 lives in the EC; DOCK switches it through this setter.
    aml.extend_from_slice(&acpi_dsl! {
        Scope(#{const path 4 ec::<N>()}) {
            Method("DKSE", 1, NotSerialized) { DKR1 = Arg0; }
        }
    });
    aml.extend_from_slice(&acpi_dsl! {
        Scope(#{const "\\_SB_"}) {
            // Dock ID cache, coreboot DOCK.G_ID: 7 dock absent, 3 present.
            Name("G_ID", 0xFFFFFFFFu32);
            Method("GGID", 0, NotSerialized) {
                Local0 = G_ID;
                If (Local0 == 0xFFFFFFFFu32) {
                    Local0 = GP02 | (GP03 << 1u32) | (GP04 << 2u32);
                    If (Local0 == 0u32) {
                        Local0 = 3u32;
                    }
                    G_ID = Local0;
                }
                Return(Local0);
            }
            Device("DOCK") {
                Name("_HID", "ACPI0003");
                Name("_UID", 0u32);
                Name("_PCL", Package(#{const "\\_SB_"}));
                Method("_DCK", 1, NotSerialized) {
                    GP28 = Arg0;
                    #{const path 5 ec::<N>().child("DKSE")}(Arg0);
                    Local1 = #{const path 5 ec::<N>().child("DKR1")};
                    Local0 = Arg0 ^ Local1;
                    Return(Local0);
                }
                Method("_STA", 0, NotSerialized) {
                    Local0 = #{const path 5 ec::<N>().child("DKR1")};
                    Return(Local0);
                }
            }
        }
    });
    aml.extend_from_slice(&acpi_dsl! {
        Scope(#{const path 4 ec::<N>()}) {
            Method("_Q18", 0, NotSerialized) { Notify(#{const "\\_SB_.DOCK"}, 3u32); }
            Method("_Q37", 0, NotSerialized) { Notify(#{const "\\_SB_.DOCK"}, 0u32); }
            Method("_Q45", 0, NotSerialized) { Notify(#{const "\\_SB_.DOCK"}, 3u32); }
            // Undock button on the dock.
            Method("_Q50", 0, NotSerialized) {
                If (GGID() != 7u32) {
                    Notify(#{const "\\_SB_.DOCK"}, 3u32);
                }
            }
            Method("_Q58", 0, NotSerialized) { Notify(#{const "\\_SB_.DOCK"}, 0u32); }
            // Power unplugged: only disconnect the dock on force eject.
            Method("_Q5A", 0, NotSerialized) {
                Local0 = GGID();
                If (Local0 == 7u32) {
                    Notify(#{const "\\_SB_.DOCK"}, 3u32);
                }
                If (Local0 == 3u32) {
                    Sleep(100u32);
                    If (DKR1 == 1u32) {
                        Notify(#{const "\\_SB_.DOCK"}, 0u32);
                    }
                }
            }
        }
    });
    aml
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use fstart_driver_lenovo::{ec::EcPorts, h8::H8Resources};
    use std::{fs, process::Command};

    #[test]
    fn dock_thinklight_and_digitizer_execute() {
        let h8 = H8Config::new(
            H8Resources::new(EcPorts::new(0x62, 0x66), 0x1600, 0x15e0),
            0x11,
            "IBM0068",
        );
        let mut aml: Vec<u8> = acpi_dsl! {
            Scope("\\_SB_") { Device("PCI0") { Device("LPCB") { } } }
        }
        .into();
        aml.extend(dsdt_aml::<fstart_platform_intel::gm45::Gm45Ich9>(
            &h8, 0x580, true,
        ));
        assert_eq!(aml.windows(4).filter(|name| *name == b"DTR_").count(), 1);
        let dir =
            std::env::temp_dir().join(std::format!("fstart-x200-glue-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("dsdt.aml"),
            fstart_acpi::platform::build_dsdt(&aml),
        )
        .unwrap();
        let result = match Command::new("acpiexec")
            .current_dir(&dir)
            .args([
                "-dt",
                "-b",
                concat!(
                    "execute \\_SB.GGID;execute \\_SB.DOCK._DCK 1;execute \\_SB.DOCK._STA;",
                    "execute \\_SB.DOCK._DCK 0;execute \\_SB.PCI0.LPCB.EC._Q50;",
                    "execute \\_SB.PCI0.LPCB.EC._Q5A;execute \\UCMS 12;execute \\UCMS 13;",
                    "execute \\_SB.PCI0.LPCB.DTR._CRS"
                ),
                "dsdt.aml",
            ])
            .output()
        {
            Ok(result) => result,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                std::eprintln!("skipping: acpiexec not installed");
                fs::remove_dir_all(dir).unwrap();
                return;
            }
            Err(error) => panic!("acpiexec: {error}"),
        };
        fs::remove_dir_all(&dir).unwrap();
        let text = std::format!(
            "{}{}",
            std::string::String::from_utf8_lossy(&result.stdout),
            std::string::String::from_utf8_lossy(&result.stderr)
        );
        assert!(
            result.status.success() && !text.contains("AE_") && !text.contains("ACPI Error"),
            "{text}"
        );
        // Simulated GPIO levels read zero: no dock ID, so GGID reports 3.
        assert!(
            text.contains("[Integer] = 0000000000000003"),
            "GGID: {text}"
        );
        // _DCK sets DKR1 and returns Arg0 ^ DKR1, so _STA then reads 1.
        assert!(
            text.contains("[Integer] = 0000000000000001"),
            "_STA: {text}"
        );
        // Undocked (GGID 3, not 7): the dock button requests an eject.
        assert!(text.contains("Notify on [DOCK]"), "dock queries: {text}");
        // Digitizer: I/O 0x200-0x207 and IRQ 5.
        assert!(text.contains("47 01 00 02 00 02 01 08 22 20 00"), "{text}");
    }
}
