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

/// What a proximity domain is (AmdFabricWhDxe's domain record type).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DomainKind {
    /// CPUs and their DRAM (type 0).
    Processor,
    /// CPU-less CXL memory (type 2).
    Cxl,
    /// An accelerator's memory (type 3).
    Accelerator,
}

/// A proximity domain; its proximity number is its index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Domain {
    pub socket: u8,
    /// The physical (NPS) domain it is part of when several reported
    /// domains share one (CCX-as-NUMA); `None`: its own.
    pub physical: Option<u32>,
    pub kind: DomainKind,
}

impl Domain {
    /// A processor domain of its own on `socket`.
    pub const fn cpu(socket: u8) -> Self {
        Self {
            socket,
            physical: None,
            kind: DomainKind::Processor,
        }
    }

    /// A CPU-less CXL memory domain of `socket`'s host bridges.
    pub const fn cxl(socket: u8) -> Self {
        Self {
            socket,
            physical: None,
            kind: DomainKind::Cxl,
        }
    }
}

/// SLIT distances the SoC does not choose (AmdFabricWhDxe `FabricBuildSlit`
/// auto values, Dell `0xdae0`; 2P_COMPLETE §2.12): two reported domains of
/// one physical domain (`PcdAmdFabricSlitVirtualDistance`).
pub const VIRTUAL_DISTANCE: u8 = 11;
/// A CPU to a CXL domain of its socket and of the other one
/// (`PcdAmdFabricSlitCxl{Local,Remote}Distance`).
pub const CXL_DISTANCE: [u8; 2] = [18, 28];
/// A CPU to an accelerator (`PcdAmdFabricSlitAcceleratorLocalDistance`).
pub const ACCELERATOR_DISTANCE: u8 = 17;
/// Accelerator to accelerator, same socket and not
/// (`...AcceleratorToAcceleratorDistance`).
pub const ACCELERATOR_PEER_DISTANCE: [u8; 2] = [12, 25];
/// A row of a CXL domain: it initiates nothing.
pub const UNREACHABLE: u8 = 0xff;

/// NUMA description.
#[derive(Debug, Clone, Copy)]
pub struct Numa<'a> {
    pub domains: &'a [Domain],
    pub memory: &'a [MemoryAffinity],
    /// CPUID Fn8000_0008_EAX\[7:0\], for the MSCT.
    pub phys_addr_bits: u8,
}

/// SLIT distance between two different domains: two processor domains of
/// one physical domain [`VIRTUAL_DISTANCE`], else the SoC's within a socket
/// ([`SocAcpi::socket_distance`]) and between sockets
/// ([`SocAcpi::remote_socket_distance`]), the vendors' defaults; CXL and
/// accelerator domains as AmdFabricWhDxe gives them (a CXL domain's row
/// [`UNREACHABLE`]), values below 10 raised to 11. The APOB type-0x14
/// extra domains (50/60/255) of the older SoCs are not modelled (ACPI_SMM
/// §7.2).
pub const fn distance(soc: &SocAcpi, a: Domain, b: Domain) -> u8 {
    use DomainKind::{Accelerator, Cxl, Processor};
    let local = (a.socket == b.socket) as usize;
    let d = match (a.kind, b.kind) {
        (Cxl, _) => return UNREACHABLE,
        (_, Cxl) => CXL_DISTANCE[1 - local],
        (Accelerator, Accelerator) => ACCELERATOR_PEER_DISTANCE[1 - local],
        (Accelerator, _) | (_, Accelerator) => ACCELERATOR_DISTANCE,
        (Processor, Processor) => match (a.physical, b.physical) {
            (Some(x), Some(y)) if x == y => VIRTUAL_DISTANCE,
            _ if local == 1 => soc.socket_distance,
            _ => soc.remote_socket_distance,
        },
    };
    if d < 10 { 11 } else { d }
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
