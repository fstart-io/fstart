// SPDX-License-Identifier: MIT OR Apache-2.0
//! SRAT, MSCT and SLIT of an AMD system with more than one NUMA domain
//! (AmdAcpiDxe installs them only then): the processors' and the DRAM's
//! proximity domains and the distances between them are the platform's.
//! Shared by SP3 (Naples dies, Rome/Milan NPS groups) and the noncar SoCs
//! (Zeppelin dies).
extern crate alloc;

use alloc::vec::Vec;
use fstart_acpi::msct::{DomainCapacity, build_msct};
use fstart_acpi::slit::build_slit;
use fstart_acpi::srat::{SratEntry, build_srat};

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

/// A processor and its domain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CpuAffinity {
    pub apic_id: u32,
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

/// `[SRAT, MSCT, SLIT]` (AmdAcpiDxe order), or nothing with one domain.
/// `distance` is the SLIT distance between two different domains.
pub fn tables(
    numa: &Numa,
    cpus: impl Iterator<Item = CpuAffinity> + Clone,
    distance: impl Fn(Domain, Domain) -> u8,
) -> Vec<Vec<u8>> {
    let n = numa.domains.len();
    if n <= 1 {
        return Vec::new();
    }
    let entries: Vec<SratEntry> = cpus
        .clone()
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
            processors: cpus.clone().filter(|c| c.domain == d).count() as u32,
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
        build_slit(n, |i, j| distance(numa.domains[i], numa.domains[j])),
    ]
}
