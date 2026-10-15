//! Exercise the combined GM45/ICH9/ThinkPad namespace with ACPICA.
extern crate std;

use super::{IntelGm45, IntelGm45Config};
use crate::ich9::{IntelIch9, IntelIch9Config};
use crate::southbridge::pirq::{PciPin, PinRoute, Pirq, PirqRouting, SlotPins};
use crate::{IntelNorthbridgeDriver, IntelSouthbridgeDriver};
use fstart_acpi::device::AcpiDevice;
use fstart_driver_lenovo::{
    ec::EcPorts,
    h8::{H8BatteryThresholdBackend, H8Config, H8Resources},
};
use std::{fs, process::Command};

#[test]
fn x200_sleep_and_passive_cooling_namespace_resolves() {
    const fn c3(mwait_hint: u8) -> [crate::cpu::CState; 2] {
        [
            crate::cpu::CState {
                ctype: 1,
                latency: 1,
                power: 1000,
                mwait_hint: 0,
            },
            crate::cpu::CState {
                ctype: 3,
                latency: 0x37,
                power: 250,
                mwait_hint,
            },
        ]
    }
    static NB: IntelGm45Config = IntelGm45Config {
        cstates: crate::cpu::Core2Cstates {
            ac: &c3(0x20),
            battery: &c3(0x50),
        },
        ..IntelGm45Config::new()
    };
    static SB: IntelIch9Config = IntelIch9Config {
        pcie_ports: [false; 6],
        pirq: Some(PirqRouting {
            slot_pins: &[SlotPins {
                slot: 0x1f,
                functions: [Some(PciPin::A), None, None, None, None, None, None, None],
            }],
            pin_routes: &[PinRoute {
                slot: 0x1f,
                pin: PciPin::A,
                line: Pirq::A,
            }],
            bridge_routes: &[],
        }),
        ..IntelIch9Config::new()
    };
    let nb = IntelGm45::new_from_config(&NB).unwrap();
    let sb = IntelIch9::new_from_config(&SB).unwrap();
    let mut aml = nb.dsdt_aml(&NB);
    aml.extend(sb.dsdt_aml(&SB));
    let h8 = H8Config {
        second_thermal_zone: true,
        critical_temp_celsius: 100,
        passive_temp_celsius: 90,
        bat_thresholds: true,
        bat_threshold_backend: H8BatteryThresholdBackend::Registers24 { bit7: false },
        ..H8Config::new(
            H8Resources::new(EcPorts::new(0x62, 0x66), 0x1600, 0x15e0),
            0x11,
            "IBM0068",
        )
    };
    aml.extend(fstart_driver_lenovo::h8_acpi::dsdt_aml(
        &h8,
        "\\_SB_.PCI0.LPCB",
    ));
    assert!(!aml.windows(4).any(|bytes| bytes == b"_S3_"));
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("dsdt.aml"),
        fstart_acpi::platform::build_dsdt(&aml),
    )
    .unwrap();
    let result = match Command::new("acpiexec")
        .current_dir(dir.path())
        .args([
            "-dt",
            "-b",
            concat!(
                "execute \\_S5;execute \\_TZ.THM0._PSL;execute \\_TZ.THM1._PSL;",
                "execute \\_TZ.THM0._CRT;execute \\_TZ.THM0._PSV;execute \\_TZ.THM0._AC0;",
                "execute \\CP00._UID;execute \\CP01._UID;",
                "execute \\CP00._PSS;execute \\CP00._PCT;execute \\CP00._CST;",
                "execute \\PNOT;execute \\_SB.PCI0._CRS;execute \\_SB.PCI0.PDRC._CRS;",
                // PCIe _OSC UUID, revision 1, three dwords of control requests.
                "execute \\_SB.PCI0._OSC (5B 4D DB 33 F7 1F 1C 40 96 57 74 41 C0 3D D7 66) ",
                "1 3 (00 00 00 00 1F 00 00 00 1D 00 00 00);",
                "execute \\_SB.PCI0.LPCB.PS2K._CRS;execute \\_SB.PCI0.GFX0.LCD0._BCL"
            ),
            "dsdt.aml",
        ])
        .output()
    {
        Ok(result) => result,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            std::eprintln!("skipping: acpiexec not installed");
            return;
        }
        Err(error) => panic!("acpiexec: {error}"),
    };
    let text = std::format!(
        "{}{}",
        std::string::String::from_utf8_lossy(&result.stdout),
        std::string::String::from_utf8_lossy(&result.stderr)
    );
    assert!(
        result.status.success() && !text.contains("AE_") && !text.contains("ACPI Error"),
        "{text}"
    );
    assert!(text.contains("[Integer] = 0000000000000007"), "{text}");
    assert_eq!(
        text.matches("[Package] Contains 2 Elements").count(),
        3,
        "{text}"
    );
    assert!(text.contains("CP00") && text.contains("CP01"), "{text}");
    // PWRS is 0 (battery) before the AC device runs: the C6 MWAIT hint.
    assert!(text.contains("01 02 01 50 00"), "battery _CST: {text}");
    assert!(text.contains("Value 0x81"), "PNOT: {text}");
    assert!(text.contains("[Integer] = 0000000000000A12"), "{text}");
    for decikelvin in ["0000000000000E94", "0000000000000E30", "0000000000000DCC"] {
        assert!(text.contains(decikelvin), "thermal trip: {text}");
    }
}
