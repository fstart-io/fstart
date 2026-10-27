//! SRAT, MSCT and SLIT (ACPI_SMM §3.4, §3.5).
//!
//! AmdAcpiDxe installs these only when the fabric reports more than one
//! domain; the domains themselves (Naples: die, Rome/Milan: NPS channel
//! groups, `df::dram::numa_domains`) and the CPU/memory → domain mapping
//! are inputs.

extern crate alloc;

use alloc::vec::Vec;

use fstart_acpi::msct::{DomainCapacity, build_msct};
use fstart_acpi::slit::build_slit;
use fstart_acpi::srat::{SratEntry, build_srat};
use fstart_driver_zen::soc::Generation;

use super::Cpu;

/// A proximity domain; its proximity number is its index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Domain {
    pub socket: u8,
}

/// DRAM `[base, end)` of a domain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryAffinity {
    pub base: u64,
    pub end: u64,
    pub domain: u32,
}

/// NUMA description.
#[derive(Debug, Clone, Copy)]
pub struct Numa<'a> {
    pub domains: &'a [Domain],
    pub memory: &'a [MemoryAffinity],
    /// CPUID Fn8000_0008_EAX\[7:0\], for the MSCT.
    pub phys_addr_bits: u8,
}

/// SLIT distance between two different domains at the donor defaults:
/// Naples PCD 0x112 = 0xFF → other die 16, other socket 32; Rome/Milan
/// "auto" (PCD 0x179 ≠ 0) with PCD 0x17A TRUE → same socket 12, other
/// socket 32. The APOB type-0x14 extra domains (50/60/255) are not
/// modelled (ACPI_SMM §7.2).
pub const fn distance(generation: Generation, a: Domain, b: Domain) -> u8 {
    match (generation, a.socket == b.socket) {
        (_, false) => 32,
        (Generation::Naples, true) => 16,
        (Generation::Rome | Generation::Milan, true) => 12,
    }
}

/// `[SRAT, MSCT, SLIT]` (AmdAcpiDxe order), or nothing with one domain.
pub fn tables(generation: Generation, numa: &Numa, cpus: &[Cpu]) -> Vec<Vec<u8>> {
    let n = numa.domains.len();
    if n <= 1 {
        return Vec::new();
    }
    let entries: Vec<SratEntry> = cpus
        .iter()
        .map(|c| SratEntry::Processor {
            domain: c.domain,
            apic_id: c.apic_id,
        })
        .chain(numa.memory.iter().map(|m| SratEntry::Memory {
            domain: m.domain,
            base: m.base,
            length: m.end - m.base,
            hot_pluggable: false,
            non_volatile: false,
        }))
        .collect();
    let capacity: Vec<DomainCapacity> = (0..n as u32)
        .map(|d| DomainCapacity {
            domain: d,
            processors: cpus.iter().filter(|c| c.domain == d).count() as u32,
            memory: numa
                .memory
                .iter()
                .filter(|m| m.domain == d)
                .map(|m| m.end - m.base)
                .sum(),
        })
        .collect();
    let max_address = (1u64 << numa.phys_addr_bits.min(63)) - 1;
    alloc::vec![
        build_srat(&entries),
        build_msct(&capacity, 1, max_address),
        build_slit(n, |i, j| distance(
            generation,
            numa.domains[i],
            numa.domains[j]
        )),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    const CPUS: [Cpu; 2] = [
        Cpu {
            apic_id: 0,
            domain: 0,
        },
        Cpu {
            apic_id: 0x40,
            domain: 1,
        },
    ];

    #[test]
    fn one_domain_emits_nothing() {
        let numa = Numa {
            domains: &[Domain { socket: 0 }],
            memory: &[],
            phys_addr_bits: 48,
        };
        assert!(tables(Generation::Rome, &numa, &CPUS).is_empty());
    }

    #[test]
    fn nps4_on_two_sockets() {
        let domains: Vec<Domain> = (0..8).map(|d| Domain { socket: d / 4 }).collect();
        let memory = [
            MemoryAffinity {
                base: 0,
                end: 0x8000_0000,
                domain: 0,
            },
            MemoryAffinity {
                base: 0x1_0000_0000,
                end: 0x2_0000_0000,
                domain: 5,
            },
        ];
        let numa = Numa {
            domains: &domains,
            memory: &memory,
            phys_addr_bits: 48,
        };
        let t = tables(Generation::Milan, &numa, &CPUS);
        let sigs: Vec<&[u8]> = t.iter().map(|t| &t[..4]).collect();
        assert_eq!(sigs, [b"SRAT", b"MSCT", b"SLIT"]);
        let slit = &t[2][44..];
        assert_eq!(slit.len(), 64);
        assert_eq!(&slit[..8], &[10, 12, 12, 12, 32, 32, 32, 32]);
        for t in &t {
            assert_eq!(t.iter().fold(0u8, |a, &b| a.wrapping_add(b)), 0);
        }
    }

    #[test]
    fn naples_die_distances() {
        let (a, b, c) = (
            Domain { socket: 0 },
            Domain { socket: 0 },
            Domain { socket: 1 },
        );
        assert_eq!(distance(Generation::Naples, a, b), 16);
        assert_eq!(distance(Generation::Naples, a, c), 32);
        assert_eq!(distance(Generation::Rome, a, b), 12);
    }
}
