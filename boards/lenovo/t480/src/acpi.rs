// SPDX-License-Identifier: MIT OR Apache-2.0
//! ThinkPad T480 ACPI and SMBIOS tables. Ramstage-only: they need the heap.

use crate::mainboard::{Acpi, H8_CONFIG, Identity};
use alloc::vec::Vec;
use fstart_acpi::PcNamespace;
use fstart_acpi::devices::tpm::{TpmFamily, TpmTisAcpi};
use fstart_acpi_macros::acpi_dsl;
use fstart_driver_lenovo::h8::H8Config;
use fstart_driver_lenovo::h8_acpi::{self, ec};
use fstart_driver_tpm::Family;
use fstart_platform_intel::skylake::SkylakeSunrisePoint;

impl Identity {
    pub fn smbios(&self) -> fstart_acpi::smbios::SmbiosIdentity<'_> {
        fstart_driver_lenovo::thinkpad::smbios_identity(
            T480_SMBIOS_IDENTITY,
            None,
            self.ec_oem_string.as_ref(),
        )
    }
}

impl Acpi {
    fn tpm_acpi(&self) -> Option<TpmTisAcpi> {
        let family = self.tpm?;
        Some(TpmTisAcpi {
            family: match family {
                Family::Tpm12 => TpmFamily::Tpm12,
                Family::Tpm20 => TpmFamily::Tpm20,
            },
            server: false,
            log: None,
        })
    }
}

impl fstart_acpi::device::AcpiDevice for Acpi {
    type Config = ();

    fn dsdt_aml(&self, _: &Self::Config) -> Vec<u8> {
        let mut aml = t480_mainboard_dsdt_aml(self.keyboard_backlight);
        if let Some(tpm) = self.tpm_acpi() {
            // coreboot `lpc_tpm_fill_ssdt()` places the TPM under `\_SB_.PCI0`.
            aml.extend(
                fstart_acpi::aml_linker::scope_vec(
                    fstart_driver_intel::namespace::pci_root::<
                        fstart_driver_intel::skylake::driver::IntelSkylake,
                    >()
                    .as_str(),
                    &tpm.dsdt_aml(),
                )
                .expect("T480 TPM emission"),
            );
        }
        aml
    }

    fn extra_tables(&self, _: &Self::Config) -> Vec<Vec<u8>> {
        self.tpm_acpi().map_or_else(Vec::new, |tpm| tpm.tables())
    }
}

impl fstart_platform_intel::acpi::BoardAcpi for crate::Board {
    type Acpi = Acpi;

    fn acpi_baseline() -> Self::Acpi {
        Acpi {
            keyboard_backlight: H8_CONFIG.has_keyboard_backlight,
            tpm: Some(Family::Tpm20),
        }
    }
}

/// Assemble the ThinkPad T480 DSDT: H8 EC + HKEY (`LEN0268`), brightness
/// hooks (`GFX0.INCB`/`DECB`), `_PTS`/`_WAK`, and PS/2 (`LEN0071`/`LEN0094`).
pub fn t480_mainboard_dsdt_aml(keyboard_backlight: bool) -> Vec<u8> {
    let h8 = H8Config {
        has_keyboard_backlight: keyboard_backlight,
        ..H8_CONFIG
    };
    let brightness = acpi_dsl! {
        Scope("\\") {
            Method("BRTU", 0, NotSerialized) {
                If (CondRefOf(#{const path 4 SkylakeSunrisePoint::IGD.child("INCB")})) {
                    #{const path 4 SkylakeSunrisePoint::IGD.child("INCB")}();
                }
            }
            Method("BRTD", 0, NotSerialized) {
                If (CondRefOf(#{const path 4 SkylakeSunrisePoint::IGD.child("DECB")})) {
                    #{const path 4 SkylakeSunrisePoint::IGD.child("DECB")}();
                }
            }
        }
    };
    let mut aml = h8_acpi::dsdt_aml_with_brightness::<SkylakeSunrisePoint>(&h8, &brightness);
    aml.extend_from_slice(&acpi_dsl! {
        Scope("\\") {
            Method("_PTS", 1, NotSerialized) {
                #{const path 5 ec::<SkylakeSunrisePoint>().child("MUTE")}(1u32);
                #{const path 5 ec::<SkylakeSunrisePoint>().child("USBP")}(0u32);
                #{const path 5 ec::<SkylakeSunrisePoint>().child("RADI")}(0u32);
            }
            Method("_WAK", 1, NotSerialized) {
                #{const path 6 ec::<SkylakeSunrisePoint>().child("HKEY").child("WAKE")}(Arg0);
                Return(Package(0u32, 0u32));
            }
        }
    });
    // Kconfig PS2K_EISAID="LEN0071", PS2M_EISAID="LEN0094".
    aml.extend(
        fstart_acpi::aml_linker::scope_vec(
            SkylakeSunrisePoint::LPC.as_str(),
            &fstart_driver_intel::southbridge::acpi::ps2_controller_nodes_with(
                "LEN0071", "LEN0094",
            ),
        )
        .expect("T480 PS/2 emission"),
    );
    aml
}

static T480_SMBIOS_PROCESSOR_SOCKETS: [&str; 1] = ["BGA1356"];

pub static T480_SMBIOS_IDENTITY: fstart_acpi::smbios::SmbiosIdentity<'static> =
    fstart_acpi::smbios::SmbiosIdentity {
        bios_vendor: "fstart",
        // Non-OEM firmware ID matching coreboot `smbios_mainboard_bios_version`:
        // Linux thinkpad_acpi validates this prefix before the ThinkPad model string.
        bios_version: "CBET4000 fstart 0.1.0",
        bios_release_date: fstart_platform_intel::SMBIOS_RELEASE_DATE,
        sys_manufacturer: "LENOVO",
        sys_product: "T480",
        sys_version: "ThinkPad T480",
        sys_serial: None,
        sys_uuid: None,
        bb_manufacturer: "LENOVO",
        bb_product: "T480",
        bb_version: "ThinkPad T480",
        bb_serial: None,
        chassis_type: 0x0a,
        chassis_manufacturer: "LENOVO",
        processor_sockets: &T480_SMBIOS_PROCESSOR_SOCKETS,
        oem_string: None,
    };

#[cfg(test)]
mod tests {
    use super::*;
    use fstart_acpi::device::AcpiDevice;
    use fstart_driver_intel::{
        namespace::SouthbridgeAcpi, skylake::driver::IntelSkylake,
        sunrise_point::driver::IntelSunrisePoint,
    };
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

    #[test]
    fn complete_dsdt_iasl_round_trip() {
        let north_config = crate::profile::PLATFORM.northbridge;
        let south_config = crate::profile::PLATFORM.southbridge;
        let mut north = IntelSkylake::new(north_config).unwrap();
        north.set_host_acpi_baseline();
        let mut south =
            <IntelSunrisePoint as fstart_driver_intel::IntelSouthbridgeDriver>::new_from_config(
                south_config,
            )
            .unwrap();
        south.set_host_acpi_baseline();

        let mut aml = north.dsdt_aml(north_config);
        aml.extend(south.dsdt_aml(
            south_config,
            &fstart_driver_intel::namespace::pci_root::<IntelSkylake>(),
        ));
        aml.extend(
            Acpi {
                keyboard_backlight: true,
                tpm: Some(Family::Tpm20),
            }
            .dsdt_aml(&()),
        );
        let dsdt = fstart_acpi::platform::build_dsdt(&aml);

        let dir =
            std::env::temp_dir().join(std::format!("fstart-t480-dsdt-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("dsdt.aml"), &dsdt).unwrap();
        iasl(&dir, &["-d", "dsdt.aml"]);
        let source = fs::read_to_string(dir.join("dsdt.dsl")).unwrap();
        assert!(source.contains("EisaId (\"LEN0268\")"));
        assert!(source.contains("EisaId (\"LEN0071\")"));
        assert!(source.contains("EisaId (\"LEN0094\")"));
        assert!(source.contains("\"MSFT0101\""));
        assert!(source.contains("Name (_GPE, 0x16)"));
        assert!(source.contains("GFX0.INCB ()"));
        assert!(source.contains("GFX0.DECB ()"));
        assert!(source.contains("Method (_Q60,"));
        assert!(source.contains("Method (RHKX,"));
        assert!(source.contains("_OSI (\"Windows 2012\")"));
        assert!(source.contains("EisaId (\"PNP0C01\")"));
        assert!(!source.contains("Method (WLSW"));
        fs::rename(dir.join("dsdt.aml"), dir.join("original.aml")).unwrap();
        iasl(&dir, &["-we", "-oa", "-tc", "dsdt.dsl"]);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn tpm2_extra_table_emitted_when_tpm20_present() {
        let acpi = Acpi {
            keyboard_backlight: true,
            tpm: Some(Family::Tpm20),
        };
        let tables = acpi.extra_tables(&());
        assert_eq!(tables.len(), 1);
        let tpm2 = &tables[0];
        assert_eq!(&tpm2[..4], b"TPM2");
        assert_eq!(tpm2.iter().fold(0u8, |a, b| a.wrapping_add(*b)), 0);
        // Start method 6 = Memory-mapped I/O (TIS/FIFO).
        assert_eq!(&tpm2[48..52], &6u32.to_le_bytes());

        let absent = Acpi {
            keyboard_backlight: true,
            tpm: None,
        };
        assert!(absent.extra_tables(&()).is_empty());
    }
}
