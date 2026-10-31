//! SRAT, MSCT and SLIT of a system with more than one proximity domain:
//! the platform says which domain each processor and DRAM range is in and
//! how far the domains are apart.

use alloc::vec::Vec;

use crate::msct::{DomainCapacity, build_msct};
use crate::slit::build_slit;
use crate::srat::{SratEntry, build_srat};

/// DRAM `[base, end)` of a domain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryAffinity {
    pub base: u64,
    pub end: u64,
    pub domain: u32,
}

/// A processor and its domain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CpuAffinity {
    pub apic_id: u32,
    pub domain: u32,
}

/// `[SRAT, MSCT, SLIT]` of `domains` proximity domains, or nothing with
/// one. `distance(a, b)` is the SLIT distance between two different
/// domains; `phys_addr_bits` bounds the MSCT's address space.
pub fn tables(
    domains: usize,
    memory: &[MemoryAffinity],
    cpus: impl Iterator<Item = CpuAffinity> + Clone,
    phys_addr_bits: u8,
    distance: impl Fn(usize, usize) -> u8,
) -> Vec<Vec<u8>> {
    if domains <= 1 {
        return Vec::new();
    }
    let entries: Vec<SratEntry> = cpus
        .clone()
        .map(|c| SratEntry::Processor {
            domain: c.domain,
            apic_id: c.apic_id,
        })
        .chain(memory.iter().map(|m| SratEntry::Memory {
            domain: m.domain,
            base: m.base,
            length: m.end - m.base,
            hot_pluggable: false,
            non_volatile: false,
        }))
        .collect();
    let capacity: Vec<DomainCapacity> = (0..domains as u32)
        .map(|d| DomainCapacity {
            domain: d,
            processors: cpus.clone().filter(|c| c.domain == d).count() as u32,
            memory: memory
                .iter()
                .filter(|m| m.domain == d)
                .map(|m| m.end - m.base)
                .sum(),
        })
        .collect();
    let max_address = (1u64 << phys_addr_bits.min(63)) - 1;
    alloc::vec![
        build_srat(&entries),
        build_msct(&capacity, 1, max_address),
        build_slit(domains, distance),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_domain_is_no_tables() {
        assert!(tables(1, &[], core::iter::empty(), 48, |_, _| 20).is_empty());
    }

    #[test]
    fn two_domains() {
        let memory = [
            MemoryAffinity {
                base: 0,
                end: 1 << 30,
                domain: 0,
            },
            MemoryAffinity {
                base: 1 << 32,
                end: 2 << 32,
                domain: 1,
            },
        ];
        let cpus = [0u32, 1].map(|domain| CpuAffinity {
            apic_id: domain * 16,
            domain,
        });
        let t = tables(2, &memory, cpus.into_iter(), 48, |_, _| 16);
        let signatures: Vec<&[u8]> = t.iter().map(|t| &t[..4]).collect();
        assert_eq!(signatures, [b"SRAT", b"MSCT", b"SLIT"]);
        assert_eq!(&t[2][44..], &[10, 16, 16, 10]);
        for t in &t {
            assert_eq!(t.iter().fold(0u8, |a, &b| a.wrapping_add(b)), 0);
        }
    }
}
