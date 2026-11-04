//! The Intel southbridges' DSDT fragments have to be well-formed AML.
//!
//! Every chipset driver composes its PCI root namespace from the fragments
//! in `fstart_driver_intel::southbridge::acpi`, and a board build only checks
//! that the bytes fit. `iasl` is what proves they decode, so render both ICH
//! generations here and let it disassemble them.

use fstart_acpi::AcpiPath;
use fstart_driver_intel::IntelSouthbridgeDriver;
use fstart_driver_intel::ich7::{IntelIch7, IntelIch7Config};
use fstart_driver_intel::ich8::{IntelIch8, IntelIch8Config};
use fstart_driver_intel::namespace::SouthbridgeAcpi;
use std::path::Path;
use std::process::Command;

/// The PCI root every Intel northbridge names.
const PCI_ROOT: AcpiPath = AcpiPath::SB.child("PCI0");

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
    let aml = south.dsdt_aml(&CONFIG, &PCI_ROOT);
    assert_legacy_resources(&aml, &iasl_round_trip("ich7", &aml));
}

#[test]
fn ich8_dsdt_disassembles() {
    static CONFIG: IntelIch8Config = IntelIch8Config::new();
    let south = IntelIch8::new_from_config(&CONFIG).expect("ICH8 config is valid");
    let aml = south.dsdt_aml(&CONFIG, &PCI_ROOT);
    assert_legacy_resources(&aml, &iasl_round_trip("ich8", &aml));
}

#[cfg(feature = "baytrail")]
#[test]
fn baytrail_dsdt_disassembles() {
    use fstart_acpi::device::AcpiDevice;
    use fstart_driver_intel::IntelNorthbridgeDriver;
    use fstart_driver_intel::baytrail::{BaytrailNorth, BaytrailSouth, NorthConfig, SouthConfig};
    use fstart_driver_intel::namespace::{SouthbridgeAcpi, pci_root, pci_root_resources};
    static NORTH: NorthConfig = NorthConfig::new();
    static SOUTH: SouthConfig = SouthConfig::new();
    let north = BaytrailNorth::new_from_config(&NORTH).expect("north cluster config");
    let south = BaytrailSouth::new_from_config(&SOUTH).expect("south cluster config");
    // What the platform assembles: the north's devices, the root bridge's
    // resources from the allocator's description, then the south cluster.
    let mut aml = north.dsdt_aml(&NORTH);
    let root = north.pci_root().expect("Bay Trail root bridge");
    aml.extend(pci_root_resources::<BaytrailNorth>(&root).expect("root bridge resources"));
    aml.extend(south.dsdt_aml(&SOUTH, &pci_root::<BaytrailNorth>()));
    let dsl = iasl_round_trip("baytrail", &aml);
    assert_legacy_resources(&aml, &dsl);
    // PCI memory runs from TOLM (2 GiB) to where the B-unit stops forwarding.
    let mmio: std::vec::Vec<_> = root
        .forwarded()
        .iter()
        .filter(|window| window.kind == fstart_pci::PciWindowKind::Mmio)
        .map(|window| (window.base, window.end()))
        .collect();
    assert!(
        mmio.iter()
            .any(|(base, end)| *base <= 0x8000_0000 && *end > 0x8000_0000 && *end <= 0xfe00_0000),
        "forwarded MMIO {mmio:x?} must start at TOLM and stop at 0xfe000000"
    );
    // Neither the OS description nor the allocator may reach into the
    // aperture the B-unit does not decode.
    for window in root
        .forwarded()
        .iter()
        .chain(root.allocatable())
        .filter(|window| window.kind == fstart_pci::PciWindowKind::Mmio)
    {
        assert!(
            window.end() <= 0xfe00_0000 || window.base >= 0xfec0_0000,
            "window {:#x}..{:#x} reaches into the aperture the B-unit drops",
            window.base,
            window.end()
        );
    }
    // Bay Trail SATA is 00:13.0, SMBus 00:1f.3, and the three GPIO
    // communities enumerate.
    assert!(dsl.contains("Name (_ADR, 0x00130000)"), "{dsl}");
    assert!(dsl.contains("Name (_ADR, 0x001F0003)"), "{dsl}");
    assert_eq!(dsl.matches("\"INT33FC\"").count(), 6, "{dsl}");
}
