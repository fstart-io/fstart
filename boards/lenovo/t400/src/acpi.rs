//! T400-family DSDT glue around the H8 EC: sleep/wake hooks, GFX0 brightness
//! hotkeys, the ThinkLight `\UCMS` method, the EC wake GPE, and the dock
//! (coreboot `t400/dsdt.asl`, `t400/acpi/dock.asl`), plus the SMBIOS identity.
//! Ramstage-only: they need the heap.

extern crate alloc;

use crate::mainboard::{Acpi, Identity};
use alloc::vec::Vec;
use fstart_acpi::PcNamespace;
use fstart_acpi_macros::acpi_dsl;
use fstart_driver_lenovo::h8::H8Config;
use fstart_driver_lenovo::h8_acpi::ec;

impl Identity {
    pub fn smbios(&self) -> fstart_acpi::smbios::SmbiosIdentity<'_> {
        fstart_driver_lenovo::thinkpad::smbios_identity(
            T400_SMBIOS_IDENTITY,
            self.eeprom.as_ref(),
            self.ec_oem_string.as_ref(),
        )
    }
}

impl fstart_acpi::device::AcpiDevice for Acpi {
    type Config = ();

    fn dsdt_aml(&self, _: &Self::Config) -> Vec<u8> {
        t400_mainboard_dsdt_aml(self.bluetooth)
    }
}

impl fstart_platform_intel::acpi::BoardAcpi for crate::Board {
    type Acpi = Acpi;

    fn acpi_baseline() -> Self::Acpi {
        Acpi {
            bluetooth: crate::config::T400_H8.has_bluetooth,
        }
    }
}

/// Assemble the T400-family DSDT.
pub fn t400_mainboard_dsdt_aml(bluetooth: bool) -> Vec<u8> {
    let h8 = fstart_driver_lenovo::h8::H8Config {
        has_bluetooth: bluetooth,
        ..crate::config::T400_H8
    };
    dsdt_aml::<<crate::Board as fstart_platform_intel::IntelBoardFacts>::Platform>(
        &h8,
        fstart_driver_intel::ich9::ich9::DEFAULT_GPIOBASE,
    )
}

static T400_SMBIOS_PROCESSOR_SOCKETS: [&str; 1] = ["Socket P"];

pub static T400_SMBIOS_IDENTITY: fstart_acpi::smbios::SmbiosIdentity<'static> =
    fstart_acpi::smbios::SmbiosIdentity {
        bios_vendor: "fstart",
        // Non-OEM firmware ID, following coreboot; Linux thinkpad_acpi
        // validates this before the ThinkPad model string.
        bios_version: "CBET4000 fstart 0.1.0",
        bios_release_date: fstart_platform_intel::SMBIOS_RELEASE_DATE,
        sys_manufacturer: "LENOVO",
        sys_product: crate::config::PRODUCT_NAME,
        sys_version: crate::config::PRODUCT_NAME,
        sys_serial: None,
        sys_uuid: None,
        bb_manufacturer: "LENOVO",
        bb_product: crate::config::PRODUCT_NAME,
        bb_version: "1.0",
        bb_serial: None,
        chassis_type: 0x0a,
        chassis_manufacturer: "LENOVO",
        processor_sockets: &T400_SMBIOS_PROCESSOR_SOCKETS,
        oem_string: None,
    };

/// Board DSDT. The platform's IGD provides `INCB`/`DECB`, the Core 2 CPU
/// AML provides `\CNOT`; `gpio_base` is the ICH GPIOBASE.
#[must_use]
pub fn dsdt_aml<N: PcNamespace>(h8: &H8Config, gpio_base: u16) -> Vec<u8> {
    let mut aml = fstart_driver_lenovo::thinkpad::h8_dsdt_aml::<N>(h8);
    aml.extend(dock_aml::<N>(gpio_base));
    let dock_resources = fstart_acpi::devices::GenericAcpi {
        name: "DRES",
        hid: "PNP0C02",
        resources: &fstart_driver_lenovo::t6::dock::FIRMWARE_RESOURCES,
        gsiv: None,
        interrupt_flags: Default::default(),
    }
    .dsdt_aml();
    aml.extend(
        fstart_acpi::aml_linker::scope_vec(N::LPC.as_str(), &dock_resources)
            .expect("T400 dock resources"),
    );
    aml
}

/// Coreboot `t400/acpi/dock.asl`: dock device keyed on GPIO28 and EC DKR1,
/// plus the EC dock queries `_Q18`, `_Q37`, `_Q45`, `_Q50`, `_Q58`.
fn dock_aml<N: PcNamespace>(gpio_base: u16) -> Vec<u8> {
    let gpio_level = u32::from(gpio_base) + 0x0c;
    let mut aml: Vec<u8> = acpi_dsl! {
        Scope("\\") {
            OperationRegion("GPLV", SystemIO, #{dword gpio_level}, 0x04u32);
            Field("GPLV", ByteAcc, NoLock, Preserve) {
                , 28,
                GP28, 1,
            }
        }
    }
    .into();
    aml.extend_from_slice(&acpi_dsl! {
        Scope(#{const path 4 ec::<N>()}) {
            Method("DKSE", 1, NotSerialized) { DKR1 = Arg0; }
        }
    });
    aml.extend_from_slice(&acpi_dsl! {
        Scope(#{const "\\_SB_"}) {
            Device("DOCK") {
                // Standard ACPI docking station ID (ACPI0003 is AC adapter).
                Name("_HID", EisaId("PNP0C15"));
                Name("_PCL", Package(#{const "\\_SB_"}));
                Method("_DCK", 1, NotSerialized) {
                    GP28 = Arg0;
                    #{const path 5 ec::<N>().child("DKSE")}(Arg0);
                    Local1 = #{const path 5 ec::<N>().child("DKR1")};
                    If (Arg0 == Local1) { Return(1u32); }
                    Return(0u32);
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
            Method("_Q50", 0, NotSerialized) { Notify(#{const "\\_SB_.DOCK"}, 3u32); }
            Method("_Q58", 0, NotSerialized) { Notify(#{const "\\_SB_.DOCK"}, 0u32); }
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
    fn dock_and_thinklight_execute() {
        let h8 = H8Config::new(
            H8Resources::new(EcPorts::new(0x62, 0x66), 0x1600, 0x15e0),
            0x11,
            "IBM0068",
        );
        let mut aml: Vec<u8> = acpi_dsl! {
            Scope("\\_SB_") { Device("PCI0") { Device("LPCB") { } } }
        }
        .into();
        aml.extend(dsdt_aml::<crate::config::Hardware>(&h8, 0x580));
        let dir =
            std::env::temp_dir().join(std::format!("fstart-t400-glue-{}", std::process::id()));
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
                    "execute \\_SB.DOCK._DCK 1;execute \\_SB.DOCK._STA;",
                    "execute \\_SB.DOCK._DCK 0;execute \\_SB.PCI0.LPCB.EC._Q50;",
                    "execute \\UCMS 12;execute \\UCMS 13;execute \\BRTU;execute \\BRTD"
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
        fs::remove_dir_all(dir).unwrap();
        let text = std::format!(
            "{}{}",
            std::string::String::from_utf8_lossy(&result.stdout),
            std::string::String::from_utf8_lossy(&result.stderr)
        );
        assert!(
            result.status.success() && !text.contains("AE_") && !text.contains("ACPI Error"),
            "{text}"
        );
        assert_eq!(
            text.matches("[Integer] = 0000000000000001").count(),
            3,
            "_DCK(1), _STA while docked, and _DCK(0): {text}"
        );
        assert!(text.contains("Notify on [DOCK]"), "dock queries: {text}");
    }
}
