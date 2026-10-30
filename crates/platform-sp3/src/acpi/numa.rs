//! SRAT, MSCT and SLIT (ACPI_SMM §3.4, §3.5).
//!
//! AmdAcpiDxe installs these only when the fabric reports more than one
//! domain; the domains themselves (Naples: die, Rome/Milan: NPS channel
//! groups, `df::dram::numa_domains`) and the CPU/memory → domain mapping
//! are inputs.

extern crate alloc;

use alloc::vec::Vec;

#[cfg(any(feature = "bundle-reset", test))]
use fstart_driver_amd::sp3::df::dram::{DramMap, NumaDomain, df3_channel, domain_of};
use fstart_driver_amd::sp3::soc::Generation;
pub use fstart_platform_amd::numa::{CpuAffinity, Domain, MemoryAffinity, Numa};

use super::Cpu;

/// Merge per-node DRAM views into sorted, unique proximity ranges.
#[cfg(any(feature = "bundle-reset", test))]
pub(crate) fn memory_from_maps(
    maps: impl IntoIterator<Item = DramMap>,
    domains: &[NumaDomain],
) -> Vec<(u64, u64, u32)> {
    let mut memory: Vec<_> = maps
        .into_iter()
        .flat_map(|map| map.ranges().collect::<Vec<_>>())
        .filter_map(|(base, end, dst)| {
            let (socket, channel) = df3_channel(dst)?;
            Some((base, end, domain_of(domains, socket, channel)?))
        })
        .collect();
    memory.sort_unstable();
    memory.dedup();
    memory
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
    fstart_platform_amd::numa::tables(
        numa,
        cpus.iter().map(|c| CpuAffinity {
            apic_id: c.apic_id,
            domain: c.domain,
        }),
        |a, b| distance(generation, a, b),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use fstart_driver_amd::sp3::df::DramRule;
    use fstart_driver_amd::sp3::df::dram::{Nps, numa_domains};

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
    fn memory_covers_both_sockets_without_replicated_ranges() {
        let map = |base, end, dst, hole| DramMap {
            rules: core::iter::once(DramRule {
                base,
                end,
                dst,
                hole,
                socket_interleave: false,
                die_interleave: 0,
                channel_interleave: 3,
            })
            .collect(),
            hole_base: Some(0xb000_0000),
        };
        let local = map(0, 0x20_5000_0000, 0, true);
        let remote = map(0x20_5000_0000, 0x40_5000_0000, 0x20, false);
        let memory = memory_from_maps([local.clone(), remote, local], &numa_domains(Nps::Nps1, 2));
        assert_eq!(
            memory,
            [
                (0, 0xb000_0000, 0),
                (0x1_0000_0000, 0x20_5000_0000, 0),
                (0x20_5000_0000, 0x40_5000_0000, 1),
            ]
        );
        assert_eq!(
            memory.iter().map(|&(b, e, _)| e - b).sum::<u64>(),
            256 << 30
        );
    }

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
