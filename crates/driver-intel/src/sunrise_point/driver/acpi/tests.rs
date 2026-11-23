// SPDX-License-Identifier: MIT OR Apache-2.0
//! Pure routing/resources and real AML interpreter checks, NOT PCI discovery,
//! initialized-hardware fixtures, memory-training or SMM-protection evidence.
extern crate std;
use super::*;

#[test]
fn skip_init_uart2_retains_its_native_route() {
    let address = PciAddress::new(0, 0, 0x19, 0);
    assert_eq!(root_gsi(address, PciPin::A, false), Some(32));
}

#[test]
fn coalesced_pin_and_fixed_resources_survive_acpi_publication() {
    let address = PciAddress::new(0, 0, 0x1c, 0); // physical RP7 coalesced to function0
    assert_eq!(root_gsi(address, PciPin::C, true), Some(18));
    assert_eq!(root_gsi(address, PciPin::C, false), None);
    use fstart_pci::{
        PciRole::{Forwarded, Own, Window},
        PciRoot,
    };
    let root = PciRoot::new(
        PciRootInfo {
            segment: 0,
            ecam_base: 0xe0000000,
            bus_start: 0,
            bus_end: 255,
        },
        [
            Forwarded.mmio(0xd0000000, 0x2ec00000),
            Window.mmio(0xd0000000, 0x10000000),
            Own.mmio(0xe0000000, 0x10000000),
        ],
    )
    .unwrap();
    // An inherited LPSS BAR remains forwarded even outside the firmware's
    // allocatable arena; ECAM is exclusively a motherboard resource.
    assert!(root.mmio().any(|r| r.contains(&0xfe034000)));
    assert!(!root.mmio().any(|r| r.contains(&0xe0000000)));
    assert!(
        !root
            .allocatable()
            .iter()
            .any(|r| r.base <= 0xfe034000 && r.end() > 0xfe034000)
    );
}

#[test]
fn aml_interpreter_observes_physical_swizzle_and_bounded_osc_grants() {
    let root = AcpiPath::SB.child("PCI0");
    let mut port =
        fstart_acpi::pirq::prt_modes_aml((0..4).map(|pin| (0, pin, 16 + (2 + pin) % 4)), &root);
    port.extend_from_slice(&acpi_dsl! {Method("RCHK",0,NotSerialized) {
        Local0=_PRT();
        Local1=DeRefOf(Index(Local0,0u32));
        Return(DeRefOf(Index(Local1,3u32))==18u32);
    }});
    let mut pci = crate::southbridge::acpi::pci_irq_links(&[11; 8]);
    pci.extend_from_slice(&pci_osc());
    pci.extend_from_slice(&fstart_acpi::aml_linker::device_vec("RP07", &port).unwrap());
    pci.extend_from_slice(&acpi_dsl! {Method("OSCT",0,Serialized) {
        Name("OBUF",Buffer(0u8,0u8,0u8,0u8,0u8,0u8,0u8,0u8,0xffu8,0u8,0u8,0u8));
        Local1=_OSC(ToUUID("33db4d5b-1ff7-401c-9657-7441c03dd766"),1u32,3u32,OBUF);
        CreateDwordField(Local1,0u32,"STAT");
        CreateDwordField(Local1,8u32,"CTRL");
        Return((STAT==16u32)&&(CTRL==0x3du32));
    }});
    let mut aml: Vec<u8> = acpi_dsl! {Scope("\\") {
        Name("PICM",0u32);Method("_PIC",1,NotSerialized){PICM=Arg0;}
    }}
    .into();
    aml.extend_from_slice(&fstart_acpi::aml_linker::device_vec("PCI0", &pci).unwrap());
    let directory = tempfile::tempdir().unwrap();
    let table = directory.path().join("native.aml");
    std::fs::write(&table, fstart_acpi::platform::build_dsdt(&aml)).unwrap();
    let output = std::process::Command::new("acpiexec")
        .args([
            // Execute AML semantics, not ACPICA's shutdown allocation tracker.
            "-dt",
            "-b",
            r"execute \_PIC 1;execute \_SB.PCI0.RP07.RCHK;execute \_SB.PCI0.OSCT",
        ])
        .arg(table)
        .output()
        .expect("acpica-tools required");
    let text = std::format!(
        "{}{}",
        std::string::String::from_utf8_lossy(&output.stdout),
        std::string::String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.status.success() && !text.contains("ACPI Error"),
        "{text}"
    );
    assert_eq!(
        text.matches("[Integer] = FFFFFFFFFFFFFFFF").count(),
        2,
        "{text}"
    );
}
