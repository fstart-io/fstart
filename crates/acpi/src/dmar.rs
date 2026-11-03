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
const RMRR: u16 = 1;
const DRHD_INCLUDE_PCI_ALL: u8 = 1;

/// DMAR flag: interrupt remapping supported.
pub const DMAR_INTR_REMAP: u8 = 1;

/// RMRR structure header.
#[repr(C)]
#[derive(IntoBytes, Immutable)]
struct RmrrHeader {
    kind: U16<LE>,
    length: U16<LE>,
    _reserved: U16<LE>,
    segment: U16<LE>,
    base: U64<LE>,
    limit: U64<LE>,
}

/// Device-scope types (VT-d 8.3.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeKind {
    PciEndpoint = 1,
    PciSubhierarchy = 2,
    IoApic = 3,
    MsiHpet = 4,
}

/// A device scope with a single-level path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Scope {
    pub kind: ScopeKind,
    /// I/O APIC or HPET ID; zero for PCI scopes.
    pub enumeration_id: u8,
    pub bus: u8,
    pub device: u8,
    pub function: u8,
}

impl Scope {
    /// A PCI endpoint `(bus, device, function)`.
    #[must_use]
    pub const fn endpoint(bus: u8, device: u8, function: u8) -> Self {
        Self {
            kind: ScopeKind::PciEndpoint,
            enumeration_id: 0,
            bus,
            device,
            function,
        }
    }
}

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

/// A remapping unit with arbitrary device scopes.
#[derive(Debug, Clone, Copy)]
pub struct DrhdUnit<'a> {
    pub include_pci_all: bool,
    pub segment: u16,
    pub base: u64,
    pub scopes: &'a [Scope],
}

/// A reserved memory region a device keeps using under remapping
/// (coreboot `acpi_create_dmar_rmrr`); `limit` is inclusive.
#[derive(Debug, Clone, Copy)]
pub struct Rmrr<'a> {
    pub segment: u16,
    pub base: u64,
    pub limit: u64,
    pub scopes: &'a [Scope],
}

fn append_scopes(table: &mut Sdt, scopes: &[Scope]) {
    for scope in scopes {
        table.append_slice(
            EndpointScope {
                kind: scope.kind as u8,
                length: core::mem::size_of::<EndpointScope>() as u8,
                _reserved: 0.into(),
                enumeration_id: scope.enumeration_id,
                start_bus: scope.bus,
                device: scope.device,
                function: scope.function,
            }
            .as_bytes(),
        );
    }
}

/// Build a DMAR table. `host_address_width` is the DMA physical address
/// width minus one, as the specification encodes it.
#[must_use]
pub fn dmar(host_address_width: u8, units: &[Drhd<'_>]) -> Vec<u8> {
    let scopes: Vec<Vec<Scope>> = units
        .iter()
        .map(|unit| {
            unit.endpoints
                .iter()
                .map(|&(bus, device, function)| Scope::endpoint(bus, device, function))
                .collect()
        })
        .collect();
    let units: Vec<DrhdUnit<'_>> = units
        .iter()
        .zip(scopes.iter())
        .map(|(unit, scopes)| DrhdUnit {
            include_pci_all: unit.include_pci_all,
            segment: unit.segment,
            base: unit.base,
            scopes,
        })
        .collect();
    dmar_with(host_address_width, 0, &units, &[])
}

/// Build a DMAR table with `flags` (e.g. [`DMAR_INTR_REMAP`]), remapping
/// units and reserved memory regions, in that order.
#[must_use]
pub fn dmar_with(
    host_address_width: u8,
    flags: u8,
    units: &[DrhdUnit<'_>],
    rmrrs: &[Rmrr<'_>],
) -> Vec<u8> {
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
            flags,
            _reserved: [0; 10],
        }
        .as_bytes(),
    );
    let scope_len = core::mem::size_of::<EndpointScope>();
    for unit in units {
        let length = core::mem::size_of::<DrhdHeader>() + unit.scopes.len() * scope_len;
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
        append_scopes(&mut table, unit.scopes);
    }
    for rmrr in rmrrs {
        let length = core::mem::size_of::<RmrrHeader>() + rmrr.scopes.len() * scope_len;
        table.append_slice(
            RmrrHeader {
                kind: RMRR.into(),
                length: u16::try_from(length).expect("RMRR length").into(),
                _reserved: 0.into(),
                segment: rmrr.segment.into(),
                base: rmrr.base.into(),
                limit: rmrr.limit.into(),
            }
            .as_bytes(),
        );
        append_scopes(&mut table, rmrr.scopes);
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
    }
}
