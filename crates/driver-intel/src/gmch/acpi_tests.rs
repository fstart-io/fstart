//! Compare the resources Linux evaluates with the northbridge's runtime facts.
extern crate std;

use crate::IntelNorthbridgeDriver;
use fstart_acpi::device::AcpiDevice;
use fstart_core::services::memory_detect::{E820State, build_pc_compatible_e820};
use fstart_pci::PciRootInfo;
use fstart_pci::PciWindowKind;
use std::{format, fs, process::Command, string::String};

/// What ACPICA makes of the `_CRS` of `device` in a DSDT holding `aml`.
fn resources(label: &str, device: &str, aml: &[u8]) -> String {
    let dir = std::env::temp_dir().join(format!(
        "fstart-root-{label}-{device}-{}",
        std::process::id()
    ));
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("dsdt.aml"), fstart_acpi::platform::build_dsdt(aml)).unwrap();
    let output = Command::new("acpiexec")
        .current_dir(&dir)
        .args([
            "-dt",
            "-b",
            &format!("resources \\_SB.{device}"),
            "dsdt.aml",
        ])
        .output()
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.status.success() && text.contains("EndTag Resource"),
        "{text}"
    );
    assert!(!text.contains("AcpiGetCurrentResources failed"), "{text}");
    fs::remove_dir_all(dir).unwrap();
    text
}

fn check_mcfg(tables: &[alloc::vec::Vec<u8>], root: PciRootInfo) {
    let mcfg = tables
        .iter()
        .find(|table| table.starts_with(b"MCFG"))
        .expect("missing MCFG");
    assert_eq!(mcfg.len(), 60);
    assert_eq!(
        mcfg.iter().fold(0u8, |sum, byte| sum.wrapping_add(*byte)),
        0
    );
    assert_eq!(&mcfg[44..52], &root.ecam_base.to_le_bytes());
    assert_eq!(&mcfg[52..54], &root.segment.to_le_bytes());
    assert_eq!(&mcfg[54..56], &[root.bus_start, root.bus_end]);
}

/// The northbridge's namespace with PCI0's `_CRS` and `PDRC` from its root
/// bridge, as the platform assembles it.
fn dsdt_aml<N>(north: &N) -> alloc::vec::Vec<u8>
where
    N: IntelNorthbridgeDriver + AcpiDevice<Config = <N as IntelNorthbridgeDriver>::Config>,
{
    let root = north.pci_root().unwrap();
    let mut aml = north.dsdt_aml(IntelNorthbridgeDriver::config(north));
    aml.extend(crate::namespace::pci_root_resources::<N>(&root).unwrap());
    aml
}

fn check_low_memory_root<N>(label: &str, mut north: N)
where
    N: IntelNorthbridgeDriver + AcpiDevice<Config = <N as IntelNorthbridgeDriver>::Config>,
{
    let config = IntelNorthbridgeDriver::config(&north);
    let root = north.pci_root().unwrap().info();
    check_mcfg(&north.extra_tables(config), root);
    for tolud in [0x2000_0000u32, 0x4000_0000, 0x8000_0000, 0xc000_0000] {
        let mut memory = E820State::new();
        let count = build_pc_compatible_e820(
            memory.entries_mut(),
            tolud - 0x0090_0000,
            u64::from(tolud),
            tolud,
        )
        .unwrap();
        memory.set_detected(count, u64::from(tolud)).unwrap();
        north.memory_detected(&memory);
        let described = north.pci_root().unwrap();
        // The window PCI devices are allocated in.
        let mmio = described
            .allocatable()
            .iter()
            .find(|window| window.kind == PciWindowKind::Mmio)
            .unwrap();
        assert_eq!(mmio.base, u64::from(tolud));
        let aml = dsdt_aml(&north);
        let text = resources(label, "PCI0", &aml);
        // Two windows from TOLUD to the I/O APIC, around the ECAM window.
        let (ecam, ecam_end) = (root.ecam_base, root.ecam_base + root.ecam_size());
        for (base, end) in [(mmio.base, ecam), (ecam_end, 0xfec0_0000)] {
            for field in [
                format!("Address Minimum : {base:08X}"),
                format!("Address Maximum : {:08X}", end - 1),
                format!("Address Length : {:08X}", end - base),
            ] {
                assert!(text.contains(&field), "{field}\n{text}");
            }
        }
        // The ECAM window is the chipset's own: not in PCI0's _CRS, but a
        // motherboard resource in \_SB_.PDRC, with MCHBAR.
        assert!(
            !text.contains(&format!("Address Minimum : {ecam:08X}")),
            "{text}"
        );
        let board = resources(label, "PDRC", &aml);
        for field in [
            format!("Address : {ecam:08X}"),
            format!("Address Length : {:08X}", ecam_end - ecam),
            format!("Address : {:08X}", 0xfed1_4000u32),
        ] {
            assert!(board.contains(&field), "{field}\n{board}");
        }
        // What the allocator may use is in what `_CRS` reports.
        for usable in described.allocatable() {
            assert!(described.forwarded().iter().any(|whole| {
                whole.kind == usable.kind
                    && whole.base <= usable.base
                    && usable.end() <= whole.end()
            }));
        }
        // The legacy ranges the bridge forwards and the firmware keeps.
        for legacy in ["0CF7", "FFFF", "000FFFFF", "FED44FFF"] {
            assert!(
                text.contains(&format!("Address Maximum : {legacy}")),
                "{text}"
            );
        }
        assert!(
            text.contains(&format!("Address Maximum : {:04X}", root.bus_end)),
            "{text}"
        );
    }
}

#[test]
fn i945_root_covers_low_ram_bars_and_describes_pcie_config_space() {
    use crate::i945::{IntelI945, IntelI945Config};
    // Exercise a non-default bus range too: _CRS used to hardcode 64 buses.
    static CONFIG: IntelI945Config = IntelI945Config {
        ecam_buses: 128,
        ..IntelI945Config::new()
    };
    check_low_memory_root("i945", IntelI945::new_from_config(&CONFIG).unwrap());
}

#[test]
fn gm965_root_covers_low_ram_bars_and_matches_ecam_topology() {
    use crate::gm965::{IntelGm965, IntelGm965Config};
    static CONFIG: IntelGm965Config = IntelGm965Config {
        ecam_buses: 256,
        ..IntelGm965Config::new()
    };
    check_low_memory_root("gm965", IntelGm965::new_from_config(&CONFIG).unwrap());
}

#[test]
fn pineview_root_includes_the_aperture_above_4g() {
    use crate::pineview::{IntelPineview, IntelPineviewConfig};
    static CONFIG: IntelPineviewConfig = IntelPineviewConfig::new();
    let north = IntelPineview::new_from_config(&CONFIG).unwrap();
    check_mcfg(
        &north.extra_tables(&CONFIG),
        north.pci_root().unwrap().info(),
    );
    let text = resources("pineview", "PCI0", &dsdt_aml(&north));
    let high = text
        .split("64-Bit QWORD Address Space Resource")
        .nth(1)
        .expect("missing high MMIO window");
    assert!(
        high.contains("Address Minimum : 0000000100000000"),
        "{high}"
    );
    assert!(
        high.contains(&format!(
            "Address Maximum : {:016X}",
            fstart_arch::x86::physical_address_limit() - 1
        )),
        "{high}"
    );
}
