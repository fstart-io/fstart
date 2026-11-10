// SPDX-License-Identifier: MIT OR Apache-2.0
//! NUMA proximity domains of a single-socket Rs SoC for SRAT/SLIT/MSCT, from
//! the APOB's actual NPS, the DF4 DRAM map and the CPU topology. openSIL
//! DfXBuildDomainInfo: NPS4 domain Q owns CSs 3Q..3Q+2 and the CCDs of
//! {111h, 444h, 888h, 222h}[Q]; NPS2 half H owns CSs 6H..6H+5 and the CCDs
//! of {555h, AAAh}[H]. DF4 numbers the present CSs densely: on Siena, with
//! six of the IOD's twelve UMCs, CS fabric ID k is the k-th present UMC.
//! Checked against the S8040's NFX SRAT (NPS4).
use alloc::vec::Vec;
use fstart_acpi::numa::{CpuAffinity, MemoryAffinity};
use fstart_driver_amd::apob::{nps::Nps, sysmap::Region};

/// One socket's CS (UMC) count on the Rs IOD.
const CS_COUNT: u16 = 12;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// NPS0 (two sockets) or an unknown CCD/CS.
    Topology,
    /// The DRAM rules do not cover the PSP's RAM exactly, or a domain has
    /// no CPU.
    Coverage,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Numa {
    pub nps: Nps,
    pub domains: usize,
    pub memory: Vec<MemoryAffinity>,
    pub cpus: Vec<CpuAffinity>,
}

/// The domain of physical CCD `ccd`.
pub const fn ccd_domain(nps: Nps, ccd: u8) -> Option<u32> {
    if ccd >= 12 {
        return None;
    }
    let bit = 1u16 << ccd;
    match nps {
        Nps::Nps1 => Some(0),
        Nps::Nps2 => Some(if bit & 0x555 != 0 { 0 } else { 1 }),
        Nps::Nps4 => {
            let maps = [0x111u16, 0x444, 0x888, 0x222];
            let mut q = 0;
            while q < 4 {
                if maps[q] & bit != 0 {
                    return Some(q as u32);
                }
                q += 1;
            }
            None
        }
        Nps::Nps0 => None,
    }
}

/// The IOD quadrant of each physical IOS (its NBIO), as Siena's vendor
/// firmware gives the roots' `_PXM` in NPS4 (PCI0/IOS2 3, S0D0/IOS0 1,
/// S0D3/IOS3 2). IOS1's quadrant is unknown: it is fused off on the parts
/// seen.
const IOS_QUADRANT: [Option<u32>; 4] = [Some(1), None, Some(3), Some(2)];

/// The domain of the PCI root on physical IOS `ios`. NPS2 half H holds the
/// quadrants 2H and 2H+1 (CSs 6H..6H+5; CCDs 555h = 111h | 444h).
pub const fn ios_domain(nps: Nps, ios: u8) -> Option<u32> {
    if ios >= 4 {
        return None;
    }
    match (nps, IOS_QUADRANT[ios as usize]) {
        (Nps::Nps1, _) => Some(0),
        (Nps::Nps2, Some(q)) => Some(q / 2),
        (Nps::Nps4, Some(q)) => Some(q),
        _ => None,
    }
}

/// The domain of the CS with fabric ID `cs`, the `cs`-th set bit of the
/// present UMC mask `umcs` (twelve bits; all twelve on Genoa).
pub const fn cs_domain(nps: Nps, umcs: u16, cs: u16) -> Option<u32> {
    let mut umc = 0;
    let mut seen = 0;
    while umc < CS_COUNT {
        if umcs & (1 << umc) != 0 {
            if seen == cs {
                return match nps {
                    Nps::Nps1 => Some(0),
                    Nps::Nps2 => Some((umc / 6) as u32),
                    Nps::Nps4 => Some((umc / 3) as u32),
                    Nps::Nps0 => None,
                };
            }
            seen += 1;
        }
        umc += 1;
    }
    None
}

/// Assign every CPU and every PSP RAM byte to its domain. `umcs` are the
/// SoC's present UMCs; `cpus` the threads' `(APIC ID, physical CCD)`.
pub fn domains(
    nps: Nps,
    umcs: u16,
    dram: impl Iterator<Item = (u64, u64, u16)>,
    regions: &[Region],
    cpus: impl Iterator<Item = (u32, u8)>,
) -> Result<Numa, Error> {
    let domains = match nps {
        Nps::Nps1 => 1,
        Nps::Nps2 => 2,
        Nps::Nps4 => 4,
        Nps::Nps0 => return Err(Error::Topology),
    };
    let cpus = cpus
        .map(|(apic_id, ccd)| {
            Ok(CpuAffinity {
                apic_id,
                domain: ccd_domain(nps, ccd).ok_or(Error::Topology)?,
            })
        })
        .collect::<Result<Vec<_>, Error>>()?;
    let mut memory = Vec::new();
    for (base, end, cs) in dram {
        let domain = cs_domain(nps, umcs, cs).ok_or(Error::Topology)?;
        for region in regions.iter().filter(|r| r.kind.is_ram()) {
            let (start, stop) = (base.max(region.base), end.min(region.end()));
            if start < stop {
                memory.push(MemoryAffinity {
                    base: start,
                    end: stop,
                    domain,
                });
            }
        }
    }
    memory.sort_unstable_by_key(|m| m.base);
    let ram: u64 = regions
        .iter()
        .filter(|r| r.kind.is_ram())
        .map(|r| r.size)
        .sum();
    if memory.iter().map(|m| m.end - m.base).sum::<u64>() != ram
        || memory.windows(2).any(|w| w[0].end > w[1].base)
        || (0..domains as u32).any(|d| !cpus.iter().any(|c| c.domain == d))
    {
        return Err(Error::Coverage);
    }
    Ok(Numa {
        nps,
        domains,
        memory,
        cpus,
    })
}

/// Read the live NPS, DRAM map and CPU topology and assign the domains.
#[cfg(all(target_arch = "x86_64", target_os = "none"))]
pub fn discover(
    apob: &fstart_driver_amd::apob::Apob<'_, fstart_driver_amd::apob::rs::Rs>,
    regions: &[Region],
    cpus: &[super::rs_mp::Cpu],
    umcs: u16,
) -> Result<Numa, ()> {
    use fstart_driver_amd::df::v4;
    let nps = fstart_driver_amd::apob::rs::nps(apob).map_err(|_| {
        fstart_log::error!("rs: APOB 9/13h NPS record missing or invalid");
    })?;
    // SAFETY: BSP-exclusive DF4 transport; broadcast reads only.
    let df = unsafe { v4::Df::new(fstart_pci::ecam::GlobalEcam) };
    let map = v4::dram::read(&df);
    for rule in &map.rules {
        fstart_log::info!(
            "rs: DF DRAM {:#x}..{:#x} CS={} hole={}",
            rule.base,
            rule.end,
            rule.destination,
            rule.hole
        );
    }
    let numa = domains(
        nps,
        umcs,
        map.ranges(),
        regions,
        cpus.iter().map(|c| (c.apic_id, c.ccd)),
    )
    .map_err(|error| {
        fstart_log::error!(
            "rs: NUMA assignment of CPUs/DRAM inconsistent ({})",
            match error {
                Error::Topology => "topology",
                Error::Coverage => "coverage",
            }
        );
    })?;
    for m in &numa.memory {
        fstart_log::info!(
            "rs: NUMA memory {:#x}..{:#x} domain={}",
            m.base,
            m.end,
            m.domain
        );
    }
    fstart_log::info!(
        "rs: NUMA NPS{} domains={} CPUs={}",
        match nps {
            Nps::Nps0 => 0u8,
            Nps::Nps1 => 1,
            Nps::Nps2 => 2,
            Nps::Nps4 => 4,
        },
        numa.domains,
        numa.cpus.len()
    );
    Ok(numa)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fstart_driver_amd::apob::sysmap::RegionKind;

    #[test]
    fn root_domains_match_the_nfx_pxm() {
        // NFX NPS4 DSDT: S0D0 (IOS0, bus C0h) 1, PCI0 (IOS2, 00h) 3,
        // S0D3 (IOS3, 40h) 2. IOS1 is fused and unmeasured.
        assert_eq!(ios_domain(Nps::Nps4, 0), Some(1));
        assert_eq!(ios_domain(Nps::Nps4, 2), Some(3));
        assert_eq!(ios_domain(Nps::Nps4, 3), Some(2));
        assert_eq!(ios_domain(Nps::Nps4, 1), None);
        assert_eq!(ios_domain(Nps::Nps2, 0), Some(0));
        assert_eq!(ios_domain(Nps::Nps2, 2), Some(1));
        assert_eq!(ios_domain(Nps::Nps1, 1), Some(0));
        assert_eq!(ios_domain(Nps::Nps0, 0), None);
        assert_eq!(ios_domain(Nps::Nps4, 4), None);
    }

    #[test]
    fn siena_nps4_matches_the_nfx_srat() {
        // A 64-core Siena: four CCDs of 32 threads; UMCs 1, 3, 4, 7, 9, 10
        // are CS fabric IDs 0..5, and the ABL's rules target CS 1, 4, 0, 3.
        let cpus = || (0..128u32).map(|apic| (apic, (apic >> 5) as u8));
        let ram = |base, size| Region {
            base,
            size,
            kind: RegionKind::Ram,
        };
        let regions = [ram(0, 0xb000_0000), ram(1 << 32, 0x5f_5000_0000)];
        let dram = [
            (0u64, 0x20_5000_0000u64, 1u16),
            (0x20_5000_0000, 0x40_5000_0000, 4),
            (0x40_5000_0000, 0x50_5000_0000, 0),
            (0x50_5000_0000, 0x60_5000_0000, 3),
        ];
        let umcs = 1 << 1 | 1 << 3 | 1 << 4 | 1 << 7 | 1 << 9 | 1 << 10;
        let numa = domains(Nps::Nps4, umcs, dram.into_iter(), &regions, cpus()).unwrap();
        assert_eq!(numa.domains, 4);
        // NFX: APIC 00h -> 0, 20h -> 3, 40h -> 1, 60h -> 2.
        for (apic, domain) in [(0, 0), (0x1f, 0), (0x20, 3), (0x40, 1), (0x7f, 2)] {
            assert_eq!(numa.cpus[apic].domain, domain);
        }
        let memory: Vec<_> = numa
            .memory
            .iter()
            .map(|m| (m.base, m.end, m.domain))
            .collect();
        assert_eq!(
            memory,
            [
                (0, 0xb000_0000, 1),
                (1 << 32, 0x20_5000_0000, 1),
                (0x20_5000_0000, 0x40_5000_0000, 3),
                (0x40_5000_0000, 0x50_5000_0000, 0),
                (0x50_5000_0000, 0x60_5000_0000, 2),
            ]
        );
        // A rule naming no CS, or an NPS0 system, is not published.
        let bad = [(0u64, 1u64 << 32, 6u16)];
        assert!(domains(Nps::Nps4, umcs, bad.into_iter(), &regions, cpus()).is_err());
    }

    #[test]
    fn genoa_quadrants_and_halves_follow_opensil() {
        assert_eq!(
            (0..12)
                .map(|c| ccd_domain(Nps::Nps4, c).unwrap())
                .collect::<Vec<_>>(),
            [0, 3, 1, 2, 0, 3, 1, 2, 0, 3, 1, 2]
        );
        assert_eq!(ccd_domain(Nps::Nps2, 3), Some(1));
        assert_eq!(cs_domain(Nps::Nps2, 0xfff, 6), Some(1));
        assert_eq!(cs_domain(Nps::Nps1, 0xfff, 11), Some(0));
        assert_eq!(cs_domain(Nps::Nps4, 0xfff, 7), Some(2));
        assert_eq!(cs_domain(Nps::Nps4, 0xfff, 12), None);
    }
}
