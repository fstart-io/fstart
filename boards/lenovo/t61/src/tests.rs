use crate::*;
use fstart_acpi::device::AcpiDevice;
use fstart_driver_intel::{
    IntelNorthbridgeDriver, IntelSouthbridgeDriver, gm965::IntelGm965, ich8::IntelIch8,
};
use std::{fs, process::Command};

#[test]
fn wiring_matches_t61_population() {
    assert!(T61_PLATFORM.northbridge.enable_peg);
    assert_eq!(
        T61_PLATFORM.southbridge.pcie_ports,
        [true, true, true, true, true, false]
    );
    assert_eq!(T61_PLATFORM.southbridge.sata.unwrap().ports, 5);
    assert_eq!(T61_PLATFORM.southbridge.spi_vscc, Some(0x2005));
    assert_eq!(T61_PLATFORM.southbridge.pirq.ip_value(0x1c), 0x0001_4321);
    assert_eq!(igd_config().default_pwm_freq, 220);
    let pins = gpio_pins();
    for (pin, blink) in [(18, true), (24, false)] {
        let gpio = pins.iter().find(|p| p.pin == pin).unwrap();
        assert_eq!(
            gpio.dir,
            fstart_driver_intel::southbridge::gpio_ich::GpioDir::Output
        );
        assert_eq!(gpio.blink, blink);
    }
    assert_eq!(&include_bytes!("../data.vbt")[..4], b"$VBT");
    let hda = T61_PLATFORM.southbridge.hda.unwrap();
    assert_eq!(hda.verbs.get(0).subsystem_id, 0x17aa_20d7);
    assert_eq!(hda.verbs.get(0).pins.get(0).encode(), 0x0221401f);
}

#[test]
fn complete_dsdt_round_trips_and_routes_t61_events() {
    let north_config = &T61_PLATFORM.northbridge;
    let south_config = &T61_PLATFORM.southbridge;
    let north = IntelGm965::new_from_config(north_config).unwrap();
    let south = IntelIch8::new_from_config(south_config).unwrap();
    let mut aml = north.dsdt_aml(north_config);
    aml.extend(fstart_driver_intel::namespace::SouthbridgeAcpi::dsdt_aml(
        &south,
        south_config,
        &fstart_driver_intel::namespace::pci_root::<IntelGm965>(),
    ));
    aml.extend(acpi::dsdt_aml());
    aml.extend(
        fstart_acpi_macros::acpi_dsl! {
            Scope("\\_SB_.PCI0.PEGP.DGPU.LCD0") {
                Method("TST0", 0, Serialized) {
                    Store(1u32, #{const "\\_SB_.PCI0.MCHC.PEGE"});
                    Store(0x042910deu32, #{const "\\_SB_.PCI0.PEGP.DGPU.GPID"});
                    Store(0x10000000u32, #{const "\\_SB_.PCI0.PEGP.DGPU.BAR0"});
                    _BCM(20u32);
                    If (PWMR() != 0x94u32) { Return(1u32); }
                    _BCM(25u32);
                    If (PWMR() != 0x139u32) { Return(2u32); }
                    Local0 = 0u32;
                    While (Local0 < 16u32) {
                        Local1 = DeRefOf(Index(BCLP, Local0));
                        _BCM(Local1);
                        If (_BQC() != Local1) { Return(3u32); }
                        Increment(Local0);
                    }
                    If (PWMR() != 0x401u32) { Return(4u32); }
                    _BCM(0u32);
                    If (PWMR() != 0x401u32) { Return(5u32); }
                    // An unrelated PCI display must not receive G8x PWM writes.
                    Store(0x123410deu32, #{const "\\_SB_.PCI0.PEGP.DGPU.GPID"});
                    _BCM(20u32);
                    If (PWMR() != 0u32) { Return(6u32); }
                    Store(0x042910deu32, #{const "\\_SB_.PCI0.PEGP.DGPU.GPID"});
                    If (PWMR() != 0x401u32) { Return(7u32); }
                    Store(0u32, #{const "\\_SB_.PCI0.MCHC.PEGE"});
                    _BCM(20u32);
                    Store(1u32, #{const "\\_SB_.PCI0.MCHC.PEGE"});
                    If (PWMR() != 0x401u32) { Return(8u32); }
                    Return(0u32);
                }
            }
        }
        .to_vec(),
    );
    let dir = std::env::temp_dir().join(std::format!("fstart-t61-dsdt-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("dsdt.aml"),
        fstart_acpi::platform::build_dsdt(&aml),
    )
    .unwrap();
    for args in [&["-d", "dsdt.aml"][..], &["-oa", "-tc", "dsdt.dsl"][..]] {
        let output = Command::new("iasl")
            .current_dir(&dir)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "iasl: {}{}",
            std::string::String::from_utf8_lossy(&output.stdout),
            std::string::String::from_utf8_lossy(&output.stderr)
        );
    }
    let source = fs::read_to_string(dir.join("dsdt.dsl")).unwrap();
    for name in ["DGPU", "CRT0", "DVI0", "DOCK", "PS2M"] {
        assert_eq!(
            source.matches(&std::format!("Device ({name})")).count(),
            1,
            "{name}"
        );
    }
    assert!(source.contains("HKEY.RHK (0x09)"));
    assert!(source.contains("Notify (\\_SB.PCI0.PEGP.DGPU.LCD0, 0x86)"));
    assert!(source.contains("Notify (\\_SB.PCI0.PEGP.DGPU.LCD0, 0x87)"));
    for method in ["_Q18", "_Q37", "_Q50", "BRTU", "BRTD"] {
        assert_eq!(
            source.matches(&std::format!("Method ({method},")).count(),
            1,
            "{method}"
        );
    }
    assert!(source.contains("0x0061C084"));
    let output = Command::new("acpiexec")
        .current_dir(&dir)
        .args(["-b", "execute \\_SB.PCI0.PEGP.DGPU.LCD0._BCL", "dsdt.aml"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let text = std::string::String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("[Package] Contains 18 Elements"), "{text}");
    let result = Command::new("acpiexec")
        .current_dir(&dir)
        .args([
            "-dt",
            "-fv",
            "0",
            "-b",
            "execute \\_SB.PCI0.PEGP.DGPU.LCD0.TST0",
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
    assert!(
        text.rsplit("[Integer] = ")
            .next()
            .is_some_and(|value| value.starts_with("0000000000000000")),
        "{text}"
    );
    fs::remove_dir_all(dir).unwrap();
}
