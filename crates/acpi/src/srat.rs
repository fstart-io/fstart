//! SRAT (System Resource Affinity Table), ACPI 6.5 §5.2.16.
//!
//! Processor affinity uses the Local APIC structure (type 0) for IDs an
//! xAPIC can hold and the x2APIC structure (type 2) otherwise, the same rule
//! the x86 MADT builder applies, so every processor appears in both tables
//! with matching structure kinds.

use acpi_tables::sdt::Sdt;
use alloc::vec::Vec;
use zerocopy::{
    Immutable, IntoBytes,
    little_endian::{U16, U32, U64},
};

/// One SRAT affinity structure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SratEntry {
    /// A processor by APIC ID; type 0 when the ID is below 0xFF, else type 2.
    Processor { domain: u32, apic_id: u32 },
    /// A memory range `[base, base + length)`.
    Memory {
        domain: u32,
        base: u64,
        length: u64,
        hot_pluggable: bool,
        non_volatile: bool,
    },
}

const ENABLED: u32 = 1 << 0;
const HOT_PLUGGABLE: u32 = 1 << 1;
const NON_VOLATILE: u32 = 1 << 2;

#[derive(IntoBytes, Immutable)]
#[repr(C, packed)]
struct LocalApicAffinity {
    kind: u8,
    length: u8,
    domain_lo: u8,
    apic_id: u8,
    flags: U32,
    sapic_eid: u8,
    domain_hi: [u8; 3],
    clock_domain: U32,
}

#[derive(IntoBytes, Immutable)]
#[repr(C, packed)]
struct MemoryAffinity {
    kind: u8,
    length: u8,
    domain: U32,
    reserved: U16,
    base: U64,
    length_bytes: U64,
    reserved2: U32,
    flags: U32,
    reserved3: U64,
}

#[derive(IntoBytes, Immutable)]
#[repr(C, packed)]
struct X2ApicAffinity {
    kind: u8,
    length: u8,
    reserved: U16,
    domain: U32,
    x2apic_id: U32,
    flags: U32,
    clock_domain: U32,
    reserved2: U32,
}

fn entry(e: &SratEntry) -> Vec<u8> {
    match *e {
        SratEntry::Processor { domain, apic_id } if apic_id < 0xff => {
            let [lo, hi @ ..] = domain.to_le_bytes();
            LocalApicAffinity {
                kind: 0,
                length: 16,
                domain_lo: lo,
                apic_id: apic_id as u8,
                flags: ENABLED.into(),
                sapic_eid: 0,
                domain_hi: hi,
                clock_domain: U32::ZERO,
            }
            .as_bytes()
            .to_vec()
        }
        SratEntry::Processor { domain, apic_id } => X2ApicAffinity {
            kind: 2,
            length: 24,
            reserved: U16::ZERO,
            domain: domain.into(),
            x2apic_id: apic_id.into(),
            flags: ENABLED.into(),
            clock_domain: U32::ZERO,
            reserved2: U32::ZERO,
        }
        .as_bytes()
        .to_vec(),
        SratEntry::Memory {
            domain,
            base,
            length,
            hot_pluggable,
            non_volatile,
        } => MemoryAffinity {
            kind: 1,
            length: 40,
            domain: domain.into(),
            reserved: U16::ZERO,
            base: base.into(),
            length_bytes: length.into(),
            reserved2: U32::ZERO,
            flags: (ENABLED
                | if hot_pluggable { HOT_PLUGGABLE } else { 0 }
                | if non_volatile { NON_VOLATILE } else { 0 })
            .into(),
            reserved3: U64::ZERO,
        }
        .as_bytes()
        .to_vec(),
    }
}

/// Build the SRAT (revision 3).
pub fn build_srat(entries: &[SratEntry]) -> Vec<u8> {
    let mut sdt = Sdt::new(
        *b"SRAT",
        48,
        3,
        crate::OEM_ID,
        crate::OEM_TABLE_ID,
        crate::OEM_REVISION,
    );
    // Reserved, must be 1 for backward compatibility.
    sdt.write_u32(36, 1);
    entries.iter().for_each(|e| sdt.append_slice(&entry(e)));
    crate::serialize(&sdt)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_and_kinds() {
        let srat = build_srat(&[
            SratEntry::Processor {
                domain: 0x0102_0304,
                apic_id: 0x10,
            },
            SratEntry::Processor {
                domain: 1,
                apic_id: 0xff,
            },
            SratEntry::Memory {
                domain: 1,
                base: 0x1_0000_0000,
                length: 0x4000_0000,
                hot_pluggable: false,
                non_volatile: false,
            },
        ]);
        assert_eq!(&srat[..4], b"SRAT");
        assert_eq!(srat.len(), 48 + 16 + 24 + 40);
        assert_eq!(u32::from_le_bytes(srat[4..8].try_into().unwrap()), 128);
        assert_eq!(srat.iter().fold(0u8, |a, &b| a.wrapping_add(b)), 0);
        assert_eq!(&srat[48..52], &[0, 16, 0x04, 0x10]);
        assert_eq!(&srat[57..60], &[0x03, 0x02, 0x01]);
        assert_eq!(&srat[64..66], &[2, 24]);
        assert_eq!(&srat[72..76], &0xffu32.to_le_bytes());
        assert_eq!(&srat[88..90], &[1, 40]);
        assert_eq!(&srat[96..104], &0x1_0000_0000u64.to_le_bytes());
        assert_eq!(&srat[116..120], &1u32.to_le_bytes());
    }
}
