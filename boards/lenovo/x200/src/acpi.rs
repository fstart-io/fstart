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
use fstart_driver_lenovo::h8_acpi::{self, ec};

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
    let mut aml = h8_acpi::dsdt_aml_with_brightness::<N>(h8, &h8_acpi::igd_brightness::<N>());
    aml.extend(h8_acpi::thinklight::<N>());
    aml.extend_from_slice(&acpi_dsl! {
        Scope("\\") {
            // acpi/platform.asl.
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
    use std::{fs, process::Command};

    /// Run `commands` in ACPICA's `acpiexec` against `aml` as the DSDT and
    /// return its output, which must report no ACPI error.
    fn acpiexec(name: &str, aml: &[u8], commands: &str) -> std::string::String {
        let dir =
            std::env::temp_dir().join(std::format!("fstart-x200-{name}-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("dsdt.aml"), fstart_acpi::platform::build_dsdt(aml)).unwrap();
        let result = Command::new("acpiexec")
            .current_dir(&dir)
            .args(["-dt", "-b", commands, "dsdt.aml"])
            .output()
            .expect("acpiexec (ACPICA tools)");
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
        text
    }

    /// The X200's whole DSDT, as its facts build it: GM45/ICH9 with the
    /// board's C-states and PCI routing, the H8 and the board glue. Sleep
    /// states, thermal trips, CPU power states, PCI resources and the IGD
    /// resolve and run in one namespace.
    #[test]
    fn complete_dsdt_executes() {
        use fstart_acpi::device::AcpiDevice;
        use fstart_driver_intel::gm45::IntelGm45;
        use fstart_driver_intel::ich9::{IntelIch9, IntelIch9Config};
        use fstart_driver_intel::namespace::{self, SouthbridgeAcpi};
        use fstart_driver_intel::southbridge::pirq::{
            PciPin, PinRoute, Pirq, PirqRouting, SlotPins,
        };
        use fstart_driver_intel::{IntelNorthbridgeDriver, IntelSouthbridgeDriver};

        let north_config = &crate::X200_PLATFORM.northbridge;
        // The board keeps the strap PIRQ routing, which the ICH9 AML reads
        // from the live RCBA. The host has none: route LPC and the root
        // ports statically instead.
        const fn route(slot: u8, pin: PciPin, line: Pirq) -> PinRoute {
            PinRoute { slot, pin, line }
        }
        static PIN_ROUTES: [PinRoute; 5] = [
            route(0x1c, PciPin::A, Pirq::A),
            route(0x1c, PciPin::B, Pirq::B),
            route(0x1c, PciPin::C, Pirq::C),
            route(0x1c, PciPin::D, Pirq::D),
            route(0x1f, PciPin::A, Pirq::A),
        ];
        let south_config: &'static _ =
            std::boxed::Box::leak(std::boxed::Box::new(IntelIch9Config {
                preserve_hardware_pirq: false,
                pirq: Some(PirqRouting {
                    slot_pins: &[SlotPins {
                        slot: 0x1f,
                        functions: [Some(PciPin::A), None, None, None, None, None, None, None],
                    }],
                    pin_routes: &PIN_ROUTES,
                    bridge_routes: &[],
                }),
                ..crate::X200_PLATFORM.southbridge
            }));
        let north = IntelGm45::new_from_config(north_config).unwrap();
        let south = IntelIch9::new_from_config(south_config).unwrap();
        let mut aml = north.dsdt_aml(north_config);
        // PCI0's `_CRS` and `PDRC`, as the platform adds them from the root
        // bridge.
        let root = IntelNorthbridgeDriver::pci_root(&north).unwrap();
        aml.extend(namespace::pci_root_resources::<IntelGm45>(&root).unwrap());
        aml.extend(south.dsdt_aml(south_config, &namespace::pci_root::<IntelGm45>()));
        aml.extend(x200_mainboard_dsdt_aml(true, true));
        // The X200 does not support S3.
        assert!(!aml.windows(4).any(|bytes| bytes == b"_S3_"));
        let text = acpiexec(
            "dsdt",
            &aml,
            concat!(
                "execute \\_S5;execute \\_TZ.THM0._PSL;execute \\_TZ.THM1._PSL;",
                "execute \\_TZ.THM0._CRT;execute \\_TZ.THM0._PSV;execute \\_TZ.THM0._AC0;",
                "execute \\C000._UID;execute \\C001._UID;",
                "execute \\C000._PSS;execute \\C000._PCT;execute \\C000._CST;",
                "execute \\PNOT;execute \\_SB.PCI0._CRS;execute \\_SB.PDRC._CRS;",
                // PCIe _OSC UUID, revision 1, three dwords of control requests.
                "execute \\_SB.PCI0._OSC (5B 4D DB 33 F7 1F 1C 40 96 57 74 41 C0 3D D7 66) ",
                "1 3 (00 00 00 00 1F 00 00 00 1D 00 00 00);",
                "execute \\_SB.PCI0.LPCB.PS2K._CRS;execute \\_SB.PCI0.GFX0.LCD0._BCL;",
                "execute \\_SB.PCI0.GFX0._DOD"
            ),
        );
        assert!(text.contains("[Integer] = 0000000000000007"), "{text}");
        assert_eq!(
            text.matches("[Package] Contains 2 Elements").count(),
            3,
            "{text}"
        );
        assert!(text.contains("C000") && text.contains("C001"), "{text}");
        // PWRS is 0 (battery) before the AC device runs: the C6 MWAIT hint.
        assert!(text.contains("01 02 01 50 00"), "battery _CST: {text}");
        assert!(
            text.contains("[Integer] = 0000000080010410"),
            "_DOD: {text}"
        );
        assert!(text.contains("Value 0x81"), "PNOT: {text}");
        assert!(text.contains("[Integer] = 0000000000000A12"), "{text}");
        // 100 C critical, 90 C passive, the fan's 85 C.
        for decikelvin in ["0000000000000E94", "0000000000000E30", "0000000000000DCC"] {
            assert!(text.contains(decikelvin), "thermal trip: {text}");
        }
    }

    #[test]
    fn dock_thinklight_and_digitizer_execute() {
        let mut aml: Vec<u8> = acpi_dsl! {
            Scope("\\_SB_") { Device("PCI0") { Device("LPCB") { } } }
        }
        .into();
        aml.extend(dsdt_aml::<fstart_platform_intel::gm45::Gm45Ich9>(
            &crate::config::X200_H8,
            fstart_driver_intel::ich9::ich9::DEFAULT_GPIOBASE,
            true,
        ));
        assert_eq!(aml.windows(4).filter(|name| *name == b"DTR_").count(), 1);
        let text = acpiexec(
            "glue",
            &aml,
            concat!(
                "execute \\_SB.GGID;execute \\_SB.DOCK._DCK 1;execute \\_SB.DOCK._STA;",
                "execute \\_SB.DOCK._DCK 0;execute \\_SB.PCI0.LPCB.EC._Q50;",
                "execute \\_SB.PCI0.LPCB.EC._Q5A;execute \\UCMS 12;execute \\UCMS 13;",
                "execute \\_SB.PCI0.LPCB.DTR._CRS"
            ),
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
