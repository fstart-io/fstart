// SPDX-License-Identifier: MIT OR Apache-2.0
//! SRAT, MSCT and SLIT (ACPI_SMM §3.4, §3.5).
//!
//! AmdAcpiDxe installs these only when the fabric reports more than one
//! domain; the domains themselves (Zeppelin, Naples: die, Rome/Milan: NPS
//! channel groups) and the CPU/memory → domain mapping are inputs.

extern crate alloc;

use alloc::vec::Vec;

pub use fstart_acpi::numa::{CpuAffinity, MemoryAffinity};

use super::{Cpu, SocAcpi};

/// A proximity domain; its proximity number is its index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Domain {
    pub socket: u8,
}

/// NUMA description.
#[derive(Debug, Clone, Copy)]
pub struct Numa<'a> {
    pub domains: &'a [Domain],
    pub memory: &'a [MemoryAffinity],
    /// CPUID Fn8000_0008_EAX\[7:0\], for the MSCT.
    pub phys_addr_bits: u8,
}

/// SLIT distance between two different domains: the SoC's within a socket
/// ([`SocAcpi::socket_distance`]), 32 between sockets, the vendors'
/// defaults. The APOB type-0x14 extra domains (50/60/255) are not modelled
/// (ACPI_SMM §7.2).
pub const fn distance(soc: &SocAcpi, a: Domain, b: Domain) -> u8 {
    if a.socket == b.socket {
        soc.socket_distance
    } else {
        32
    }
}

/// `[SRAT, MSCT, SLIT]` (AmdAcpiDxe order), or nothing with one domain.
pub fn tables(soc: &SocAcpi, numa: &Numa, cpus: &[Cpu]) -> Vec<Vec<u8>> {
    fstart_acpi::numa::tables(
        numa.domains.len(),
        numa.memory,
        cpus.iter().map(|c| CpuAffinity {
            apic_id: c.apic_id,
            domain: c.domain,
        }),
        numa.phys_addr_bits,
        |a, b| distance(soc, numa.domains[a], numa.domains[b]),
    )
}
