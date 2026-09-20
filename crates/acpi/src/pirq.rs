//! ACPI `_PRT` (PCI routing table) emission.
//!
//! Entries come from the platform's PIRQ routing data, so the GSIs an OS
//! derives are the ones the chipset's interrupt router delivers on.

use alloc::vec::Vec;

use acpi_tables::Aml;
use acpi_tables::aml::{Name, PackageBuilder};

/// AML `Name(_PRT, Package(...))` for a root-bus scope, one entry per route.
///
/// This is the table the OS reads: every entry is derived from the same route
/// data the chipset's interrupt router is programmed with, so the two cannot
/// disagree.
#[must_use]
pub fn prt_name_aml(routes: impl Iterator<Item = (u8, u8, u8)>) -> Vec<u8> {
    let mut entries = PackageBuilder::new();
    for (slot, pin, gsi) in routes {
        let address = (u32::from(slot) << 16) | 0xFFFF;
        let mut entry = PackageBuilder::new();
        entry.add_element(&address);
        entry.add_element(&u32::from(pin));
        entry.add_element(&0u32);
        entry.add_element(&u32::from(gsi));
        entries.add_element(&entry);
    }

    let mut bytes = Vec::new();
    Name::new("_PRT".into(), &entries).to_aml_bytes(&mut bytes);
    bytes
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use super::*;

    #[test]
    fn prt_aml_encodes_entries_in_order() {
        let bytes = prt_name_aml([(0x02u8, 0u8, 16u8), (0x1Du8, 0u8, 23u8)].into_iter());
        assert_eq!(
            bytes,
            vec![
                0x08, b'_', b'P', b'R', b'T', // Name(_PRT,
                0x12, 0x1A, 0x02, // Package(2),
                0x12, 0x0Bu8, 0x04, // Package(4),
                0x0C, 0xFF, 0xFF, 0x02, 0x00, // addr 0x0002FFFF
                0x00, // pin 0
                0x00, // source 0
                0x0A, 0x10, // gsi 16
                0x12, 0x0Bu8, 0x04, // Package(4),
                0x0C, 0xFF, 0xFF, 0x1D, 0x00, // addr 0x001DFFFF
                0x00, 0x00, 0x0A, 0x17, // pin 0, source 0, gsi 23
            ]
        );
    }
}
