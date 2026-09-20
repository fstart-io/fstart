//! The Intel southbridges' DSDT fragments have to be well-formed AML.
//!
//! Every chipset driver composes its `\_SB.PCI0` namespace from the fragments
//! in `fstart_driver_intel::southbridge::acpi`, and a board build only checks
//! that the bytes fit. `iasl` is what proves they decode, so render both ICH
//! generations here and let it disassemble them.

use fstart_acpi::device::AcpiDevice;
use fstart_driver_intel::IntelSouthbridgeDriver;
use fstart_driver_intel::ich7::{IntelIch7, IntelIch7Config};
use fstart_driver_intel::ich8::{IntelIch8, IntelIch8Config};
use std::path::Path;
use std::process::Command;

/// Disassemble `aml` as the DSDT `iasl` would see it on a booted machine.
fn iasl_round_trip(chipset: &str, aml: &[u8]) -> String {
    let dsdt = fstart_acpi::platform::build_dsdt(aml);
    let dir =
        std::env::temp_dir().join(std::format!("fstart-{chipset}-dsdt-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let input = dir.join("input.aml");
    std::fs::write(&input, &dsdt).expect("write DSDT");

    let output = run_iasl(&dir, &["-d", "input.aml"]);
    let diagnostics = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        output.status.success(),
        "iasl failed to disassemble the {chipset} DSDT\n{diagnostics}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !diagnostics.contains("Error"),
        "iasl reported errors in the {chipset} DSDT:\n{diagnostics}"
    );
    std::fs::read_to_string(dir.join("input.dsl")).expect("read disassembled DSDT")
}

fn assert_legacy_resources(aml: &[u8], dsl: &str) {
    // Validate the resource bytes directly: ACPICA versions disagree on
    // whether this buffer is disassembled as bytes or symbolic resource DSL.
    const RTC_RESOURCES: &[u8] = &[
        0x47, 0x01, 0x70, 0x00, 0x70, 0x00, 0x01, 0x08, // fixed I/O 0x70..0x77
        0x23, 0x00, 0x01, 0x01, // IRQ 8, edge/high/exclusive
        0x79, 0x00, // end tag
    ];
    assert!(
        aml.windows(RTC_RESOURCES.len())
            .any(|window| window == RTC_RESOURCES),
        "RTC must advertise an edge-triggered, active-high, exclusive IRQ 8"
    );
    assert!(!dsl.contains("OperationRegion (PMIO"));
    assert!(!dsl.contains("OperationRegion (GPIO"));
}

fn run_iasl(dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new("iasl")
        .current_dir(dir)
        .args(args)
        .output()
        .expect("iasl must be installed (acpica-tools)")
}

#[test]
fn ich7_dsdt_disassembles() {
    static CONFIG: IntelIch7Config = IntelIch7Config::new();
    let south = IntelIch7::new_from_config(&CONFIG).expect("ICH7 config is valid");
    let aml = south.dsdt_aml(&CONFIG);
    assert_legacy_resources(&aml, &iasl_round_trip("ich7", &aml));
}

#[test]
fn ich8_dsdt_disassembles() {
    static CONFIG: IntelIch8Config = IntelIch8Config::new();
    let south = IntelIch8::new_from_config(&CONFIG).expect("ICH8 config is valid");
    let aml = south.dsdt_aml(&CONFIG);
    assert_legacy_resources(&aml, &iasl_round_trip("ich8", &aml));
}
