extern crate std;

use std::{fs, process::Command};

use super::x200_mainboard_dsdt_aml;

/// Run `commands` in ACPICA's `acpiexec` against `aml` as the DSDT and
/// return its output, which must report no ACPI error.
fn acpiexec(aml: &[u8], commands: &str) -> std::string::String {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("dsdt.aml"),
        fstart_acpi::platform::build_dsdt(aml),
    )
    .unwrap();
    let result = Command::new("acpiexec")
        .current_dir(dir.path())
        .args(["-dt", "-b", commands, "dsdt.aml"])
        .output()
        .expect("acpiexec (ACPICA tools)");
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
    use fstart_driver_intel::southbridge::pirq::{PciPin, PinRoute, Pirq, PirqRouting, SlotPins};
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
    let south_config: &'static _ = std::boxed::Box::leak(std::boxed::Box::new(IntelIch9Config {
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
    assert!(!aml.windows(4).any(|name| name == b"GFX0"));
    // Simulate the assigned IGD BAR without running PCI enumeration on the host.
    let gfx = fstart_driver_intel::igd::gfx0_acpi_node::<IntelGm45>(0xf400_0000);
    aml.extend(
        fstart_acpi::aml_linker::scope_vec(
            const { namespace::pci_root::<IntelGm45>() }.as_str(),
            &gfx,
        )
        .unwrap(),
    );
    // PCI0's `_CRS` and `PDRC`, as the platform adds them from the root
    // bridge.
    let root = IntelNorthbridgeDriver::pci_root(&north).unwrap();
    aml.extend(namespace::pci_root_resources::<IntelGm45>(&root).unwrap());
    aml.extend(south.dsdt_aml(south_config, &namespace::pci_root::<IntelGm45>()));
    aml.extend(x200_mainboard_dsdt_aml(true, true));
    // The X200 does not support S3.
    assert!(!aml.windows(4).any(|bytes| bytes == b"_S3_"));
    let text = acpiexec(
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
