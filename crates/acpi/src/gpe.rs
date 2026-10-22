//! General-purpose event wiring: a device's `_PRW` and the `\_GPE._Lxx` /
//! `_Exx` method that notifies the devices sharing one GPE (ACPI 6.5
//! §5.6.4, §7.3.13).
//!
//! The usual chipset pattern: PCIe/PME wake of several bridges lands on one
//! GPE, each bridge declares it in `_PRW`, and the GPE handler sends Device
//! Wake (`Notify(dev, 2)`) to all of them so the OS checks their PME status.

extern crate alloc;

use alloc::format;
use alloc::vec::Vec;

use acpi_tables::Aml;
use acpi_tables::aml::{Method, Name, Notify, PackageBuilder, Path, Scope};

/// Notify value for Device Wake.
const DEVICE_WAKE: u8 = 2;

/// `Name(_PRW, Package { gpe, sleep_state })`: the device can wake the
/// system through `gpe` from `sleep_state` or shallower.
pub fn prw(gpe: u8, sleep_state: u8) -> Vec<u8> {
    let mut package = PackageBuilder::new();
    package.add_element(&gpe);
    package.add_element(&sleep_state);
    let mut bytes = Vec::new();
    Name::new(Path::new("_PRW"), &package).to_aml_bytes(&mut bytes);
    bytes
}

/// `Scope(\_GPE) { Method(_Lgg) { Notify(target, 2) ... } }` (`_Egg` when
/// `edge`). `targets` are absolute ACPI paths. Empty without targets.
pub fn notify_wake(gpe: u8, edge: bool, targets: &[&str]) -> Vec<u8> {
    if targets.is_empty() {
        return Vec::new();
    }
    let method = format!("_{}{:02X}", if edge { 'E' } else { 'L' }, gpe);
    let paths: Vec<Path> = targets.iter().map(|t| Path::new(t)).collect();
    let notifies: Vec<Notify> = paths.iter().map(|p| Notify::new(p, &DEVICE_WAKE)).collect();
    let body: Vec<&dyn Aml> = notifies.iter().map(|n| n as &dyn Aml).collect();
    let handler = Method::new(Path::new(&method), 0, false, body);
    let mut bytes = Vec::new();
    Scope::new(Path::new("\\_GPE"), alloc::vec![&handler]).to_aml_bytes(&mut bytes);
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prw_is_a_two_element_package() {
        // NameOp "_PRW" PackageOp, 2 elements: BytePrefix 0x0B, BytePrefix 4.
        let b = prw(0x0B, 4);
        assert_eq!(&b[..5], b"\x08_PRW");
        assert_eq!(b[5], 0x12);
        assert!(b.ends_with(&[0x02, 0x0A, 0x0B, 0x0A, 0x04]));
    }

    #[test]
    fn notify_handler_names_the_gpe() {
        let b = notify_wake(0x0B, false, &["\\_SB_.PCI0.RP71", "\\_SB_.PCI0.RP81"]);
        assert!(b.windows(4).any(|w| w == b"_L0B"));
        assert_eq!(b.iter().filter(|&&x| x == 0x86).count(), 2, "two NotifyOps");
        assert!(
            notify_wake(0x0B, true, &["\\_SB_.PCI0"])
                .windows(4)
                .any(|w| w == b"_E0B")
        );
        assert!(notify_wake(0x0B, false, &[]).is_empty());
    }
}
