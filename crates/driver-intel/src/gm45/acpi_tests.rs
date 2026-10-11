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
    static NB: IntelGm45Config = IntelGm45Config::new();
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
    let dir = std::env::temp_dir().join(std::format!("fstart-x200-acpi-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("dsdt.aml"),
        fstart_acpi::platform::build_dsdt(&aml),
    )
    .unwrap();
    let result = Command::new("acpiexec")
        .current_dir(&dir)
        .args([
            "-dt",
            "-b",
            concat!(
                "execute \\_S5;execute \\_TZ.THM0._PSL;execute \\_TZ.THM1._PSL;",
                "execute \\CP00._UID;execute \\CP01._UID;",
                "execute \\CP00._PSS;execute \\CP00._PCT;execute \\CP00._CST"
            ),
            "dsdt.aml",
        ])
        .output()
        .unwrap();
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
    assert!(text.contains("[Integer] = 0000000000000A12"), "{text}");
    fs::remove_dir_all(dir).unwrap();
}
