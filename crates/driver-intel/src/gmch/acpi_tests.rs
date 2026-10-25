//! Compare the resources Linux evaluates with the northbridge's runtime facts.
extern crate std;

use crate::IntelNorthbridgeDriver;
use fstart_acpi::device::AcpiDevice;
#[cfg(any(feature = "i945", feature = "gm965"))]
use fstart_core::services::memory_detect::{E820State, build_pc_compatible_e820};
#[cfg(any(feature = "i945", feature = "gm965"))]
use fstart_pci::PciWindowKind;
use fstart_pci::{PciRootInfo, PciRootProvider};
use std::{format, fs, process::Command, string::String};

fn resources(label: &str, aml: &[u8]) -> String {
    let dir = std::env::temp_dir().join(format!("fstart-root-{label}-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("dsdt.aml"), fstart_acpi::platform::build_dsdt(aml)).unwrap();
    let output = Command::new("acpiexec")
        .current_dir(&dir)
        .args(["-dt", "-b", "resources \\_SB.PCI0", "dsdt.aml"])
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

#[cfg(any(feature = "i945", feature = "gm965"))]
fn check_low_memory_root<N>(label: &str, mut north: N)
where
    N: IntelNorthbridgeDriver + AcpiDevice<Config = <N as IntelNorthbridgeDriver>::Config>,
{
    let config = IntelNorthbridgeDriver::config(&north);
    let root = PciRootProvider::root_info(&north);
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
        let windows = north.resource_windows().unwrap();
        let mmio = windows
            .iter()
            .find(|window| window.kind == PciWindowKind::Mmio)
            .unwrap();
        assert_eq!(mmio.base, u64::from(tolud));
        let text = resources(label, &north.dsdt_aml(config));
        assert!(
            text.contains(&format!("Address Minimum : {:08X}", mmio.base)),
            "{text}"
        );
        assert!(text.contains("Address Maximum : FEBFFFFF"), "{text}");
        assert!(
            text.contains(&format!("Address Length : {:08X}", 0xfec0_0000 - mmio.base)),
            "{text}"
        );
        assert!(
            text.contains(&format!("Address Maximum : {:04X}", root.bus_end)),
            "{text}"
        );
    }
}

#[cfg(feature = "i945")]
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

#[cfg(feature = "gm965")]
#[test]
fn gm965_root_covers_low_ram_bars_and_matches_ecam_topology() {
    use crate::gm965::{IntelGm965, IntelGm965Config};
    static CONFIG: IntelGm965Config = IntelGm965Config {
        ecam_buses: 256,
        ..IntelGm965Config::new()
    };
    check_low_memory_root("gm965", IntelGm965::new_from_config(&CONFIG).unwrap());
}

#[cfg(feature = "pineview")]
#[test]
fn pineview_root_includes_the_high_prefetchable_aperture() {
    use crate::pineview::{IntelPineview, IntelPineviewConfig};
    static CONFIG: IntelPineviewConfig = IntelPineviewConfig::new();
    let north = IntelPineview::new_from_config(&CONFIG).unwrap();
    check_mcfg(&north.extra_tables(&CONFIG), north.root_info());
    let text = resources("pineview", &north.dsdt_aml(&CONFIG));
    let high = text
        .split("64-Bit QWORD Address Space Resource")
        .nth(1)
        .expect("missing high MMIO window");
    assert!(high.contains("Caching : Prefetchable"), "{high}");
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
