//! DMAR (DMA Remapping) table builder, Intel VT-d specification.
//!
//! Covers the remapping hardware units (DRHD) and their PCI endpoint scopes,
//! as coreboot's `acpi_create_dmar()`/`acpi_create_dmar_drhd_4k()` emit them.

use acpi_tables::sdt::Sdt;
use alloc::vec::Vec;
use zerocopy::byteorder::{LE, U16, U64};
use zerocopy::{Immutable, IntoBytes};

/// DMAR fields after the 36-byte SDT header.
#[repr(C)]
#[derive(IntoBytes, Immutable)]
struct DmarHeader {
    host_address_width: u8,
    flags: u8,
    _reserved: [u8; 10],
}

/// DRHD remapping-structure header.
#[repr(C)]
#[derive(IntoBytes, Immutable)]
struct DrhdHeader {
    kind: U16<LE>,
    length: U16<LE>,
    flags: u8,
    /// Register-set size as 2^n 4 KiB pages.
    size: u8,
    segment: U16<LE>,
    base: U64<LE>,
}

/// Device scope with a single-level PCI path.
#[repr(C)]
#[derive(IntoBytes, Immutable)]
struct EndpointScope {
    kind: u8,
    length: u8,
    _reserved: U16<LE>,
    enumeration_id: u8,
    start_bus: u8,
    device: u8,
    function: u8,
}

const DRHD: u16 = 0;
const DRHD_INCLUDE_PCI_ALL: u8 = 1;
const SCOPE_PCI_ENDPOINT: u8 = 1;

/// One remapping unit with a 4 KiB register set.
#[derive(Debug, Clone, Copy)]
pub struct Drhd<'a> {
    /// Covers every device on the segment not claimed by another unit.
    pub include_pci_all: bool,
    pub segment: u16,
    pub base: u64,
    /// PCI endpoints as `(bus, device, function)`.
    pub endpoints: &'a [(u8, u8, u8)],
}

/// Reserved memory requiring identity mapping for these PCI endpoints.
/// Bounds are inclusive, page aligned (limit ends at the final page's last
/// byte). This record does not reserve E820 backing or authorize firmware DMA.
#[derive(Debug, Clone, Copy)]
pub struct Rmrr<'a> {
    pub segment: u16,
    pub base: u64,
    pub limit: u64,
    pub endpoints: &'a [(u8, u8, u8)],
}

#[repr(C)]
#[derive(IntoBytes, Immutable)]
struct RmrrHeader {
    kind: U16<LE>,
    length: U16<LE>,
    reserved: U16<LE>,
    segment: U16<LE>,
    base: U64<LE>,
    limit: U64<LE>,
}

/// Build a DMAR table. `host_address_width` is the DMA physical address
/// width minus one, as the specification encodes it.
#[must_use]
pub fn dmar(host_address_width: u8, units: &[Drhd<'_>]) -> Vec<u8> {
    dmar_with_reserved(host_address_width, units, &[])
}

/// DMAR with actual reserved identity mappings. Interrupt remapping remains
/// unadvertised; callers must not infer protection from these descriptions.
#[must_use]
pub fn dmar_with_reserved(
    host_address_width: u8,
    units: &[Drhd<'_>],
    reserved: &[Rmrr<'_>],
) -> Vec<u8> {
    assert!((31..=63).contains(&host_address_width));
    let mut table = Sdt::new(
        *b"DMAR",
        36,
        1,
        crate::OEM_ID,
        crate::OEM_TABLE_ID,
        crate::OEM_REVISION,
    );
    table.append_slice(
        DmarHeader {
            host_address_width,
            flags: 0,
            _reserved: [0; 10],
        }
        .as_bytes(),
    );
    for unit in units {
        let scope_len = core::mem::size_of::<EndpointScope>();
        let length = core::mem::size_of::<DrhdHeader>() + unit.endpoints.len() * scope_len;
        table.append_slice(
            DrhdHeader {
                kind: DRHD.into(),
                length: u16::try_from(length).expect("DRHD length").into(),
                flags: if unit.include_pci_all {
                    DRHD_INCLUDE_PCI_ALL
                } else {
                    0
                },
                size: 0,
                segment: unit.segment.into(),
                base: unit.base.into(),
            }
            .as_bytes(),
        );
        for &(bus, device, function) in unit.endpoints {
            table.append_slice(
                EndpointScope {
                    kind: SCOPE_PCI_ENDPOINT,
                    length: scope_len as u8,
                    _reserved: 0.into(),
                    enumeration_id: 0,
                    start_bus: bus,
                    device,
                    function,
                }
                .as_bytes(),
            );
        }
    }
    for region in reserved {
        assert!(
            region.base <= region.limit
                && region.base & 0xfff == 0
                && region.limit & 0xfff == 0xfff
                && !region.endpoints.is_empty(),
            "invalid reserved DMA range"
        );
        let scope_len = core::mem::size_of::<EndpointScope>();
        let length = core::mem::size_of::<RmrrHeader>() + region.endpoints.len() * scope_len;
        table.append_slice(
            RmrrHeader {
                kind: 1.into(),
                length: u16::try_from(length).expect("RMRR length").into(),
                reserved: 0.into(),
                segment: region.segment.into(),
                base: region.base.into(),
                limit: region.limit.into(),
            }
            .as_bytes(),
        );
        for &(bus, device, function) in region.endpoints {
            assert!(device < 32 && function < 8);
            table.append_slice(
                EndpointScope {
                    kind: SCOPE_PCI_ENDPOINT,
                    length: scope_len as u8,
                    _reserved: 0.into(),
                    enumeration_id: 0,
                    start_bus: bus,
                    device,
                    function,
                }
                .as_bytes(),
            );
        }
    }
    table.as_slice().to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dmar_layout_matches_the_vtd_wire_format() {
        let table = dmar(
            35,
            &[
                Drhd {
                    include_pci_all: false,
                    segment: 0,
                    base: 0xfed9_0000,
                    endpoints: &[(0, 0x1b, 0)],
                },
                Drhd {
                    include_pci_all: true,
                    segment: 0,
                    base: 0xfed9_3000,
                    endpoints: &[],
                },
            ],
        );
        assert_eq!(&table[..4], b"DMAR");
        assert_eq!(table.len(), 48 + 24 + 16);
        assert_eq!(u32::from_le_bytes(table[4..8].try_into().unwrap()), 88);
        assert_eq!(
            table.iter().fold(0u8, |sum, byte| sum.wrapping_add(*byte)),
            0
        );
        assert_eq!(table[36], 35);
        // First DRHD: type 0, length 24, base, then the 0:1b.0 endpoint.
        assert_eq!(&table[48..52], &[0, 0, 24, 0]);
        assert_eq!(&table[56..64], &0xfed9_0000u64.to_le_bytes());
        assert_eq!(&table[64..72], &[1, 8, 0, 0, 0, 0, 0x1b, 0]);
        // Catch-all DRHD with INCLUDE_PCI_ALL.
        assert_eq!(&table[72..77], &[0, 0, 16, 0, 1]);
        let table = dmar_with_reserved(
            38,
            &[Drhd {
                include_pci_all: true,
                segment: 0,
                base: 0xfed91000,
                endpoints: &[],
            }],
            &[Rmrr {
                segment: 0,
                base: 0xbe000000,
                limit: 0xbfffffff,
                endpoints: &[(0, 2, 0)],
            }],
        );
        // OS-visible identity range is inclusive and bound to ONLY the IGD,
        // not every PCI device covered by the preceding catch-all DRHD.
        assert_eq!(&table[64..72], &[1, 0, 32, 0, 0, 0, 0, 0]);
        assert_eq!(
            u64::from_le_bytes(table[72..80].try_into().unwrap()),
            0xbe000000
        );
        assert_eq!(
            u64::from_le_bytes(table[80..88].try_into().unwrap()),
            0xbfffffff
        );
        assert_eq!(&table[88..96], &[1, 8, 0, 0, 0, 0, 2, 0]);
        assert_eq!(table[37], 0); // no invented interrupt-remapping support
        assert_eq!(table.iter().fold(0u8, |s, b| s.wrapping_add(*b)), 0);
    }
}
