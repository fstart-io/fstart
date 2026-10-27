//! MSCT (Maximum System Characteristics Table), ACPI 6.5 §5.2.19.

use acpi_tables::sdt::Sdt;
use alloc::vec::Vec;
use zerocopy::{
    Immutable, IntoBytes,
    little_endian::{U32, U64},
};

/// Capacities of one proximity domain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DomainCapacity {
    pub domain: u32,
    /// Logical processors the domain can hold.
    pub processors: u32,
    /// Bytes of memory the domain can hold.
    pub memory: u64,
}

#[derive(IntoBytes, Immutable)]
#[repr(C, packed)]
struct DomainInfo {
    revision: u8,
    length: u8,
    range_low: U32,
    range_high: U32,
    processors: U32,
    memory: U64,
}

const HEADER: u32 = 56;

/// Build the MSCT (revision 1). `max_clock_domains` and the domain count
/// are written as "maximum minus one" like the spec requires.
pub fn build_msct(
    domains: &[DomainCapacity],
    max_clock_domains: u32,
    max_physical_address: u64,
) -> Vec<u8> {
    let mut sdt = Sdt::new(
        *b"MSCT",
        HEADER,
        1,
        crate::OEM_ID,
        crate::OEM_TABLE_ID,
        crate::OEM_REVISION,
    );
    let max_domain = domains.iter().map(|d| d.domain).max().unwrap_or(0);
    sdt.write_u32(36, HEADER);
    sdt.write_u32(40, max_domain);
    sdt.write_u32(44, max_clock_domains.saturating_sub(1));
    sdt.write_u64(48, max_physical_address);
    domains.iter().for_each(|d| {
        sdt.append_slice(
            DomainInfo {
                revision: 1,
                length: 22,
                range_low: d.domain.into(),
                range_high: d.domain.into(),
                processors: d.processors.into(),
                memory: d.memory.into(),
            }
            .as_bytes(),
        )
    });
    crate::serialize(&sdt)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout() {
        let msct = build_msct(
            &[
                DomainCapacity {
                    domain: 0,
                    processors: 64,
                    memory: 1 << 40,
                },
                DomainCapacity {
                    domain: 1,
                    processors: 64,
                    memory: 1 << 40,
                },
            ],
            1,
            (1 << 48) - 1,
        );
        assert_eq!(&msct[..4], b"MSCT");
        assert_eq!(msct.len(), 56 + 2 * 22);
        assert_eq!(&msct[36..48], &[56, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(&msct[78..80], &[1, 22]);
        assert_eq!(&msct[80..84], &1u32.to_le_bytes());
        assert_eq!(msct.iter().fold(0u8, |a, &b| a.wrapping_add(b)), 0);
    }
}
