//! ACPI `_PRT` (PCI routing table) emission.
//!
//! Entries come from the platform's PIRQ routing data, so the GSIs an OS
//! derives are the ones the chipset's interrupt router delivers on.

use alloc::vec;
use alloc::vec::Vec;

/// AML `Name(_PRT, Package(...))` for a root-bus scope, one entry per route.
///
/// This is the table the OS reads: every entry is derived from the same
/// [`PinRoute`] data the router registers are programmed with, so the two
/// cannot disagree.
#[must_use]
pub fn prt_name_aml(routes: impl Iterator<Item = (u8, u8, u8)>) -> Vec<u8> {
    let entries: Vec<Vec<u8>> = routes
        .map(|(slot, pin, gsi)| {
            package(&[
                dword_aml((u32::from(slot) << 16) | 0xFFFF),
                integer_aml(u64::from(pin)),
                integer_aml(0),
                integer_aml(u64::from(gsi)),
            ])
        })
        .collect();
    name_aml(b"_PRT", &package(&entries))
}

/// AML `Name` opcode with a four-character name and a single body object.
fn name_aml(name: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(body.len() + 5);
    out.push(0x08); // NameOp
    out.extend_from_slice(name);
    out.extend_from_slice(body);
    out
}

/// AML `Package` opcode over already-serialized elements.
fn package(elements: &[Vec<u8>]) -> Vec<u8> {
    let count = u8::try_from(elements.len()).expect("package holds at most 255 elements");
    let content_len: usize = elements.iter().map(Vec::len).sum::<usize>() + 1;
    let (length, width) = fstart_acpi::aml_linker::package_length(content_len)
        .expect("package length fits in a PkgLength field");
    let mut out = Vec::with_capacity(content_len + width + 1);
    out.push(0x12); // PackageOp
    out.extend_from_slice(&length[..width]);
    out.push(count); // NumElements
    for element in elements {
        out.extend_from_slice(element);
    }
    out
}

/// AML integer in its smallest encoding (bare `Zero` for zero).
fn integer_aml(value: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity(9);
    match value {
        0 => out.push(0x00), // ZeroOp
        value if value <= u64::from(u8::MAX) => {
            out.push(0x0A); // BytePrefix
            out.push(value as u8);
        }
        value if value <= u64::from(u16::MAX) => {
            out.push(0x0B); // WordPrefix
            out.extend_from_slice(&(value as u16).to_le_bytes());
        }
        value if value <= u64::from(u32::MAX) => {
            out.push(0x0C); // DwordPrefix
            out.extend_from_slice(&(value as u32).to_le_bytes());
        }
        value => {
            out.push(0x0E); // QwordPrefix
            out.extend_from_slice(&value.to_le_bytes());
        }
    }
    out
}

/// AML dword constant, as `_PRT` address fields expect.
fn dword_aml(value: u32) -> Vec<u8> {
    let mut out = vec![0x0C]; // DwordPrefix
    out.extend_from_slice(&value.to_le_bytes());
    out
}

#[cfg(test)]
mod tests {
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
