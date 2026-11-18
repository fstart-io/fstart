// SPDX-License-Identifier: MIT OR Apache-2.0
//! The NUMA domains of one RS socket for SRAT, SLIT and the roots' `_PXM`:
//! the APOB's NPS, the DF4 DRAM rules and the CCD of every thread. openSIL
//! `DfXBuildDomainInfo`: NPS4 domain Q owns CSs 3Q..3Q+2 and the CCDs of
//! {111h, 444h, 888h, 222h}\[Q\]; NPS2 half H owns CSs 6H..6H+5 and the CCDs
//! of {555h, AAAh}\[H\]. DF4 numbers the present CSs densely: on Siena, with
//! six of the IOD's twelve UMCs, CS fabric ID k is the k-th present UMC.
//! Checked against the S8040's vendor SRAT (NPS4).
extern crate alloc;

use alloc::vec::Vec;
use fstart_driver_amd::apob::nps::Nps;
use fstart_driver_amd::apob::sysmap::Region;

/// The CSs (UMCs) of the RS IOD.
const CS_COUNT: u16 = 12;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// NPS0 (two sockets) or an unknown CCD or CS.
    Topology,
    /// The DRAM rules do not cover the RAM exactly, or a domain has no CPU.
    Coverage,
}

/// The domain of every CPU (in the order given) and of every RAM range.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Numa {
    pub nps: Nps,
    pub domains: usize,
    /// `(base, end, domain)`, sorted.
    pub memory: Vec<(u64, u64, u32)>,
    pub cpus: Vec<u32>,
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

/// The IOD quadrant of each physical IOS, as Siena's vendor firmware gives
/// the roots' `_PXM` in NPS4 (IOS2 3, IOS0 1, IOS3 2). IOS1's is unknown:
/// it is fused off on the parts seen.
const IOS_QUADRANT: [Option<u32>; 4] = [Some(1), None, Some(3), Some(2)];

/// The domain of the root on physical IOS `ios`. NPS2 half H holds the
/// quadrants 2H and 2H+1.
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

/// The domain of the CS with fabric ID `cs`, the `cs`-th of the present
/// UMCs `umcs`.
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

/// Assign the CPUs (`ccds`: each one's physical CCD) and every RAM byte of
/// `regions` to their domains; `dram` are the DF's `(base, end, CS)`.
pub fn domains(
    nps: Nps,
    umcs: u16,
    dram: impl Iterator<Item = (u64, u64, u16)>,
    regions: &[Region],
    ccds: impl Iterator<Item = u8>,
) -> Result<Numa, Error> {
    let domains = nps.domains_per_socket().ok_or(Error::Topology)?.into();
    let cpus = ccds
        .map(|ccd| ccd_domain(nps, ccd).ok_or(Error::Topology))
        .collect::<Result<Vec<_>, _>>()?;
    let ram = || regions.iter().filter(|r| r.kind.is_ram());
    let mut memory = Vec::new();
    for (base, end, cs) in dram {
        let domain = cs_domain(nps, umcs, cs).ok_or(Error::Topology)?;
        for region in ram() {
            let (start, stop) = (base.max(region.base), end.min(region.end()));
            if start < stop {
                memory.push((start, stop, domain));
            }
        }
    }
    memory.sort_unstable();
    if memory.iter().map(|m| m.1 - m.0).sum::<u64>() != ram().map(|r| r.size).sum::<u64>()
        || memory.windows(2).any(|w| w[0].1 > w[1].0)
        || (0..domains as u32).any(|d| !cpus.contains(&d))
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

#[cfg(test)]
mod tests {
    use super::*;
    use fstart_driver_amd::apob::sysmap::RegionKind;

    #[test]
    fn root_domains_match_the_vendor_pxm() {
        // S8040 NPS4 DSDT: IOS0 (bus C0h) 1, IOS2 (00h) 3, IOS3 (40h) 2.
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
    fn siena_nps4_matches_the_vendor_srat() {
        // A 64-core Siena: four CCDs of 32 threads; UMCs 1, 3, 4, 7, 9, 10
        // are CS fabric IDs 0..5, and the rules target CS 1, 4, 0, 3.
        let ccds = || (0..128u32).map(|apic| (apic >> 5) as u8);
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
        let numa = domains(Nps::Nps4, umcs, dram.into_iter(), &regions, ccds()).unwrap();
        assert_eq!(numa.domains, 4);
        // Vendor: APIC 00h -> 0, 20h -> 3, 40h -> 1, 60h -> 2.
        for (apic, domain) in [(0, 0), (0x1f, 0), (0x20, 3), (0x40, 1), (0x7f, 2)] {
            assert_eq!(numa.cpus[apic], domain);
        }
        assert_eq!(
            numa.memory,
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
        assert!(domains(Nps::Nps4, umcs, bad.into_iter(), &regions, ccds()).is_err());
        assert!(domains(Nps::Nps0, umcs, dram.into_iter(), &regions, ccds()).is_err());
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
