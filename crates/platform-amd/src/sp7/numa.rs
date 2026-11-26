// SPDX-License-Identifier: MIT OR Apache-2.0
//! NUMA domain mapping for SP7 (Venice / WH dual-IOD) SRAT, SLIT, MSCT, and
//! root bridge `_PXM` attributes, as AmdFabricWhDxe builds them (FABRIC_2P
//! §2.13; Dell PR9XJ and GIGABYTE MD25-CG0 alike):
//! - Domains (`Df4BuildDomainInfo`): NPS0 (2P only) one domain; NPS1 one
//!   per socket; NPS2 one per IOD; NPS4 one per quadrant. Domain `d` of
//!   socket `s` is number `s * per_socket + d`. Its channels, bit
//!   `socket * 16 + channel`, are NPS1 `0xFFFF << 16s`, NPS2
//!   `0xFF << 8h << 16s`, NPS4 `0xF << 4q << 16s`.
//! - CPUs (`WhGetPhysDomainOfCcd`): socket CCD `die * 4 + ccd`, quadrant
//!   `[0, 0, 1, 1, 2, 2, 3, 3]`, after the "CCD remapping for NUMA" of an IOD
//!   with exactly two CCDs in one quadrant pair ([`numa_ccd`]).
//! - Memory (`FUN_70e0`): a DRAM rule's domain is the one whose channels
//!   hold every channel the rule's copies on the CS name
//!   ([`fstart_driver_amd::df::wh::rule_channels`]).
//! - Root bridges: the vendor's per-socket tables ([`rb_domain`]).
//! - CCX as NUMA (`PcdAmdFabricCcxAsNumaDomain`, `Df4BuildDomainInfo`
//!   `0xf2ab30`; 2P_COMPLETE §2.11): one reported domain per present CCD
//!   of each socket, each in its NPS (physical) domain ([`ccx_domains`]);
//!   each DRAM rule split evenly over its physical domain's reported ones;
//!   a root bridge's `_PXM` from the vendor's root bridge to CCD masks
//!   ([`rb_ccx_domains`]).
extern crate alloc;

use alloc::vec::Vec;
use fstart_driver_amd::apob::nps::Nps;
use fstart_driver_amd::apob::sysmap::Region;

/// UMC channels per SP7 socket (8 per IOD × 2 IODs).
pub const CHANNELS_PER_SOCKET: u32 = 16;
/// Physical CCDs per SP7 IOD.
pub const CCDS_PER_DIE: u8 = 4;
/// Physical CCDs per SP7 socket.
pub const CCDS_PER_SOCKET: u8 = 2 * CCDS_PER_DIE;
/// Most sockets.
pub const MAX_SOCKETS: u8 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// Unsupported NPS/socket count or out-of-range CCD/channel/socket coordinate.
    Topology,
    /// DRAM rules do not cover system RAM cleanly or a domain has no CPU.
    Coverage,
}

/// NUMA assignment for every CPU thread (in input order) and every RAM range.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Numa {
    pub nps: Nps,
    pub domains: usize,
    /// `(base, end, domain)`, sorted by `base`.
    pub memory: Vec<(u64, u64, u32)>,
    pub cpus: Vec<u32>,
    /// With CCX as NUMA, the reported domains (domain `d` is `ccx[d]`);
    /// empty otherwise.
    pub ccx: Vec<CcxDomain>,
}

impl Numa {
    /// The socket of domain `domain`.
    pub fn socket(&self, domain: usize) -> u8 {
        if let Some(ccx) = self.ccx.get(domain) {
            return ccx.socket;
        }
        match self.nps.domains_per_socket() {
            Some(per) => (domain / usize::from(per)) as u8,
            None => 0,
        }
    }

    /// The physical (NPS) domain of reported domain `domain` when several
    /// share one (CCX as NUMA).
    pub fn physical(&self, domain: usize) -> Option<u32> {
        self.ccx.get(domain).map(|c| c.physical)
    }

    /// The channels (bit `socket * 16 + channel`) whose DRAM domain
    /// `domain` holds: its NPS domain's.
    pub fn channels(&self, domain: usize) -> u32 {
        let physical = self.physical(domain).unwrap_or(domain as u32);
        domain_channels(self.nps, physical)
    }
}

/// A reported domain of CCX as NUMA: a present CCD of a socket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CcxDomain {
    pub socket: u8,
    /// Socket CCD as the NUMA code places it ([`numa_ccd`]).
    pub ccd: u8,
    /// Its NPS domain ([`ccd_domain`]).
    pub physical: u32,
}

/// CCX as NUMA's reported domains (`Df4BuildDomainInfo`): for each socket,
/// each present CCD of `ccds` (`(socket, socket CCD)`, [`numa_ccd`]) in
/// CCD order, one domain (Zen 6 has one CCX per CCD) numbered in that order,
/// with the NPS domain of the CCD as its physical domain. At most 48 (the
/// vendor's limit).
pub fn ccx_domains(
    nps: Nps,
    sockets: u8,
    ccds: impl Iterator<Item = (u8, u8)>,
) -> Result<Vec<CcxDomain>, Error> {
    let mut present = [0u8; MAX_SOCKETS as usize];
    for (socket, ccd) in ccds {
        if socket >= sockets || ccd >= CCDS_PER_SOCKET {
            return Err(Error::Topology);
        }
        present[usize::from(socket)] |= 1 << ccd;
    }
    let mut out = Vec::new();
    for socket in 0..sockets {
        for ccd in (0..CCDS_PER_SOCKET).filter(|c| present[usize::from(socket)] & 1 << c != 0) {
            let physical = ccd_domain(nps, socket, ccd).ok_or(Error::Topology)?;
            out.push(CcxDomain {
                socket,
                ccd,
                physical,
            });
        }
    }
    if out.len() > MAX_CCX_DOMAINS {
        return Err(Error::Topology);
    }
    Ok(out)
}

/// Most reported domains of CCX as NUMA.
pub const MAX_CCX_DOMAINS: usize = 48;

/// The quadrant of a socket CCD (AmdFabricWhDxe `0x3f5b0`, GIGABYTE
/// `0x18df0`; `2P_COMPLETE/ccx_numa_rb_ccd.json`).
const CCD_QUADRANT: [u8; 8] = [0, 0, 1, 1, 2, 2, 3, 3];

/// The CCDs (bit = socket CCD) each socket-wide physical root bridge's
/// `_PXM` names with CCX as NUMA, by how many quadrants every socket
/// populates (4, both halves, else): AmdFabricWhDxe `FUN_8d00`/`FUN_30a0`,
/// Dell `0x3f3c0`/`0x3f530`/`0x3f560` (B0) and `0x3f590`/`0x3f3f0`/
/// `0x3f410` (A0); GIGABYTE `0x173b0`/`0x17420`/`0x17450`, `0x17480`/
/// `0x173e0`/`0x17400`. Generated from `2P_COMPLETE/ccx_numa_rb_ccd.json`.
const RB_CCD_B0: [[u8; 6]; 3] = [
    [0x03, 0x0c, 0x0c, 0x30, 0xc0, 0xc0],
    [0x0f, 0x0f, 0x0f, 0xf0, 0xf0, 0xf0],
    [0xff; 6],
];
const RB_CCD_A0: [[u8; 4]; 3] = [
    [0x03, 0x0c, 0x30, 0xc0],
    [0x0f, 0x0f, 0xf0, 0xf0],
    [0xff; 4],
];

/// Which mask table: every socket's CCDs in all four quadrants (0), in both
/// halves (quadrants 0-1 and 2-3, 1), else 2 (`FUN_9310`: 4, 2, 1).
fn quadrant_table(domains: &[CcxDomain]) -> usize {
    let sockets = domains.iter().map(|d| d.socket).max().map_or(0, |s| s + 1);
    let quadrants = |socket| {
        domains
            .iter()
            .filter(|d| d.socket == socket)
            .fold(0u8, |m, d| m | 1 << CCD_QUADRANT[usize::from(d.ccd)])
    };
    if (0..sockets).all(|s| quadrants(s) == 0xf) {
        0
    } else if (0..sockets).all(|s| quadrants(s) & 0x3 != 0 && quadrants(s) & 0xc != 0) {
        1
    } else {
        2
    }
}

/// The reported domains (`_PXM` candidates) of socket-wide physical root
/// bridge `socket_rb` on `socket` with CCX as NUMA (`FabricRbPxmList`
/// `0x8d00`): every domain of the socket whose CCD the root bridge's mask
/// names, `per_iod` root bridges per IOD (3 on B0, 2 on A0; other counts
/// name every CCD of the socket).
pub fn rb_ccx_domains(domains: &[CcxDomain], socket: u8, socket_rb: u8, per_iod: u8) -> Vec<u32> {
    let table = quadrant_table(domains);
    let rb = usize::from(socket_rb);
    let mask = match per_iod {
        3 => RB_CCD_B0[table].get(rb).copied(),
        2 => RB_CCD_A0[table].get(rb).copied(),
        _ => Some(0xff),
    }
    .unwrap_or(0);
    (domains.iter().enumerate())
        .filter(|(_, d)| d.socket == socket && mask & 1 << d.ccd != 0)
        .map(|(i, _)| i as u32)
        .collect()
}

/// Number of NUMA domains for `sockets` (1 or 2) at `nps`.
pub const fn domain_count(nps: Nps, sockets: u8) -> Option<usize> {
    if sockets == 0 || sockets > MAX_SOCKETS {
        return None;
    }
    match nps.domains_per_socket() {
        Some(per) => Some(sockets as usize * per as usize),
        None if sockets == 2 => Some(1),
        None => None,
    }
}

/// The channels of domain `domain` (bit `socket * 16 + channel`).
pub const fn domain_channels(nps: Nps, domain: u32) -> u32 {
    match nps {
        Nps::Nps0 => u32::MAX,
        Nps::Nps1 => 0xffff << (16 * domain),
        Nps::Nps2 => 0xff << (8 * domain),
        Nps::Nps4 => 0xf << (4 * domain),
    }
}

/// The socket-wide physical CCD (`die * 4 + ccd`) the vendor's NUMA code
/// places a CCD at: an IOD with exactly two CCDs in one quadrant pair
/// (`{0, 1}` or `{2, 3}`) has them treated as `{0, 3}`, one per quadrant
/// ("CCD remapping for NUMA", `WhBuildCcdInfo`). `ccd` is the APOB's
/// physical CCD (die-local `0..4` or socket-wide `4..8` on die 1);
/// `die_ccds` the IOD's die-local CCD mask.
pub const fn numa_ccd(die: u8, ccd: u8, die_ccds: u8) -> u8 {
    let local = ccd % CCDS_PER_DIE;
    let local = match die_ccds & 0xf {
        0b0011 | 0b1100 if local.is_multiple_of(2) => 0,
        0b0011 | 0b1100 => 3,
        _ => local,
    };
    die * CCDS_PER_DIE + local
}

/// NUMA domain of socket CCD `ccd` (`0..8`, [`numa_ccd`]) on `socket`.
pub const fn ccd_domain(nps: Nps, socket: u8, ccd: u8) -> Option<u32> {
    if socket >= MAX_SOCKETS || ccd >= CCDS_PER_SOCKET {
        return None;
    }
    let s = socket as u32;
    let quad = (ccd / 2) as u32;
    match nps {
        Nps::Nps0 => Some(0),
        Nps::Nps1 => Some(s),
        Nps::Nps2 => Some(s * 2 + quad / 2),
        Nps::Nps4 => Some(s * 4 + quad),
    }
}

/// The vendor's per-socket root bridge → domain tables (AmdFabricWhDxe,
/// `0x3f210..0x3f4f0`; FABRIC_2P `rb_pxm.json`), indexed by the socket-wide
/// physical root bridge: SP7 B0 (3 per IOD) and A0 (2 per IOD).
const RB_NPS2_B0: [u8; 6] = [0, 0, 0, 1, 1, 1];
const RB_NPS4_B0: [u8; 6] = [0, 1, 1, 2, 3, 3];
const RB_NPS2_A0: [u8; 4] = [0, 0, 1, 1];
const RB_NPS4_A0: [u8; 4] = [0, 1, 2, 3];

/// NUMA domain of socket-wide physical root bridge `socket_rb`
/// (`die * per_iod + rb`) on `socket`, with `per_iod` root bridges per IOD.
/// The vendor has tables for 3 (B0) and 2 (A0); 4 per IOD splits each IOD's
/// in halves.
pub fn rb_domain(nps: Nps, socket: u8, socket_rb: u8, per_iod: u8) -> Option<u32> {
    if socket >= MAX_SOCKETS || per_iod == 0 || socket_rb >= 2 * per_iod {
        return None;
    }
    let s = u32::from(socket);
    let rb = usize::from(socket_rb);
    let local = match (nps, per_iod) {
        (Nps::Nps0, _) => return Some(0),
        (Nps::Nps1, _) => 0,
        (Nps::Nps2, 3) => RB_NPS2_B0[rb],
        (Nps::Nps4, 3) => RB_NPS4_B0[rb],
        (Nps::Nps2, 2) => RB_NPS2_A0[rb],
        (Nps::Nps4, 2) => RB_NPS4_A0[rb],
        (Nps::Nps2, _) => socket_rb / per_iod,
        (Nps::Nps4, _) => socket_rb / per_iod * 2 + socket_rb % per_iod * 2 / per_iod,
    };
    Some(s * u32::from(nps.domains_per_socket()?) + u32::from(local))
}

/// NUMA domain of a DRAM rule that spans `channels` (bit
/// `socket * 16 + channel`): the domain holding all of them.
pub fn channels_domain(nps: Nps, sockets: u8, channels: u32) -> Option<u32> {
    if channels == 0 {
        return None;
    }
    (0..domain_count(nps, sockets)? as u32).find(|&d| channels & !domain_channels(nps, d) == 0)
}

/// Map `(base, end, channels)` DRAM rules, system RAM `regions`, and
/// per-thread `(socket, socket CCD)` coordinates into validated NUMA domains.
pub fn domains(
    nps: Nps,
    sockets: u8,
    dram: impl Iterator<Item = (u64, u64, u32)>,
    regions: &[Region],
    cpus_iter: impl Iterator<Item = (u8, u8)>,
) -> Result<Numa, Error> {
    domains_with(nps, sockets, dram, regions, cpus_iter, false)
}

/// DRAM is split at 2 MiB between the reported domains of a physical one.
const CCX_CHUNK_ALIGN: u64 = 2 << 20;

/// [`domains`], with CCX as NUMA when `ccx_as_numa`: the reported domains
/// are [`ccx_domains`] of the threads' CCDs, each thread in its CCD's, and
/// each DRAM rule of a physical domain split into as many chunks of equal
/// size (rounded up to 2 MiB) as it has reported domains, assigned in
/// order (AmdFabricWhDxe `0x70e0`), then cut to the RAM.
pub fn domains_with(
    nps: Nps,
    sockets: u8,
    dram: impl Iterator<Item = (u64, u64, u32)>,
    regions: &[Region],
    cpus_iter: impl Iterator<Item = (u8, u8)>,
    ccx_as_numa: bool,
) -> Result<Numa, Error> {
    let threads: Vec<(u8, u8)> = cpus_iter.collect();
    if threads.iter().any(|&(socket, _)| socket >= sockets) {
        return Err(Error::Topology);
    }
    let ccx = if ccx_as_numa {
        ccx_domains(nps, sockets, threads.iter().copied())?
    } else {
        Vec::new()
    };
    let domains = if ccx_as_numa {
        domain_count(nps, sockets).ok_or(Error::Topology)?;
        ccx.len()
    } else {
        domain_count(nps, sockets).ok_or(Error::Topology)?
    };
    let cpus = threads
        .iter()
        .map(|&(socket, ccd)| {
            if ccx_as_numa {
                ccx.iter()
                    .position(|d| (d.socket, d.ccd) == (socket, ccd))
                    .map(|d| d as u32)
                    .ok_or(Error::Topology)
            } else {
                ccd_domain(nps, socket, ccd).ok_or(Error::Topology)
            }
        })
        .collect::<Result<Vec<_>, _>>()?;
    let ram = || regions.iter().filter(|r| r.kind.is_ram());
    let mut memory = Vec::new();
    for (base, end, channels) in dram {
        if sockets < 2 && channels >> CHANNELS_PER_SOCKET != 0 {
            return Err(Error::Topology);
        }
        let physical = channels_domain(nps, sockets, channels).ok_or(Error::Topology)?;
        let mut parts: Vec<(u64, u64, u32)> = Vec::new();
        if ccx_as_numa {
            let reported: Vec<u32> = (ccx.iter().enumerate())
                .filter(|(_, d)| d.physical == physical)
                .map(|(i, _)| i as u32)
                .collect();
            if reported.is_empty() {
                return Err(Error::Coverage);
            }
            let chunk = (end - base)
                .div_ceil(reported.len() as u64)
                .next_multiple_of(CCX_CHUNK_ALIGN);
            let mut at = base;
            for &domain in &reported {
                let stop = at.saturating_add(chunk).min(end);
                if at < stop {
                    parts.push((at, stop, domain));
                }
                at = stop;
            }
        } else {
            parts.push((base, end, physical));
        }
        for (base, end, domain) in parts {
            for region in ram() {
                let (start, stop) = (base.max(region.base), end.min(region.end()));
                if start < stop {
                    memory.push((start, stop, domain));
                }
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
        ccx,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use fstart_driver_amd::apob::sysmap::RegionKind;

    #[test]
    fn wh_ccds_and_channels_align_across_nps() {
        assert_eq!(
            (0..8)
                .map(|c| ccd_domain(Nps::Nps4, 0, c).unwrap())
                .collect::<Vec<_>>(),
            [0, 0, 1, 1, 2, 2, 3, 3]
        );
        assert_eq!(ccd_domain(Nps::Nps2, 0, 5), Some(1));
        assert_eq!(ccd_domain(Nps::Nps4, 1, 6), Some(7));
        assert_eq!(ccd_domain(Nps::Nps4, 0, 8), None);

        // Channel masks per domain (Df4BuildDomainInfo).
        assert_eq!(domain_channels(Nps::Nps4, 5), 0xf0 << 16);
        assert_eq!(domain_channels(Nps::Nps2, 3), 0xff00_0000);
        assert_eq!(channels_domain(Nps::Nps4, 2, 0b1111 << 20), Some(5));
        assert_eq!(channels_domain(Nps::Nps4, 1, 0b1_1000), None);
        assert_eq!(channels_domain(Nps::Nps1, 2, 1 << 31), Some(1));
        assert_eq!(channels_domain(Nps::Nps0, 2, 0x0001_0001), Some(0));
        assert_eq!(channels_domain(Nps::Nps0, 1, 1), None);
        assert_eq!(channels_domain(Nps::Nps1, 1, 0), None);
    }

    #[test]
    fn two_ccd_iods_are_remapped_for_numa() {
        assert_eq!([numa_ccd(0, 0, 0b0011), numa_ccd(0, 1, 0b0011)], [0, 3]);
        assert_eq!([numa_ccd(1, 6, 0b1100), numa_ccd(1, 7, 0b1100)], [4, 7]);
        // Split pairs, four CCDs, and one CCD are left alone.
        assert_eq!([numa_ccd(0, 0, 0b0101), numa_ccd(0, 2, 0b0101)], [0, 2]);
        assert_eq!(numa_ccd(1, 1, 0b1111), 5);
        assert_eq!(numa_ccd(0, 2, 0b0100), 2);
    }

    #[test]
    fn root_bridges_follow_the_vendor_tables() {
        let b0 = |nps, s| {
            (0..6)
                .map(|rb| rb_domain(nps, s, rb, 3).unwrap())
                .collect::<Vec<_>>()
        };
        assert_eq!(b0(Nps::Nps4, 0), [0, 1, 1, 2, 3, 3]);
        assert_eq!(b0(Nps::Nps4, 1), [4, 5, 5, 6, 7, 7]);
        assert_eq!(b0(Nps::Nps2, 1), [2, 2, 2, 3, 3, 3]);
        assert_eq!(b0(Nps::Nps1, 1), [1; 6]);
        assert_eq!(b0(Nps::Nps0, 1), [0; 6]);
        let a0 = |nps| {
            (0..4)
                .map(|rb| rb_domain(nps, 1, rb, 2).unwrap())
                .collect::<Vec<_>>()
        };
        assert_eq!(a0(Nps::Nps4), [4, 5, 6, 7]);
        assert_eq!(a0(Nps::Nps2), [2, 2, 3, 3]);
        let four = (0..8)
            .map(|rb| rb_domain(Nps::Nps4, 0, rb, 4).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(four, [0, 0, 1, 1, 2, 2, 3, 3]);
        assert_eq!(rb_domain(Nps::Nps4, 0, 6, 3), None);
    }

    #[test]
    fn builds_1p_nps4_and_2p_nps2_numa_maps() {
        let ram = |base, size| Region {
            base,
            size,
            kind: RegionKind::Ram,
        };
        let regions = [ram(0, 0x8000_0000), ram(1 << 32, 0x3f_8000_0000)];
        let dram = [
            (0u64, 0x10_8000_0000u64, 0xfu32),
            (0x10_8000_0000, 0x20_8000_0000, 0xf0),
            (0x20_8000_0000, 0x30_8000_0000, 0xf00),
            (0x30_8000_0000, 0x40_8000_0000, 0xf000),
        ];
        let cpus = (0..8u8).map(|ccd| (0u8, ccd));
        let numa = domains(Nps::Nps4, 1, dram.into_iter(), &regions, cpus).unwrap();
        assert_eq!(numa.domains, 4);
        assert_eq!(numa.cpus, [0, 0, 1, 1, 2, 2, 3, 3]);
        assert_eq!(
            numa.memory,
            [
                (0, 0x8000_0000, 0),
                (1 << 32, 0x10_8000_0000, 0),
                (0x10_8000_0000, 0x20_8000_0000, 1),
                (0x20_8000_0000, 0x30_8000_0000, 2),
                (0x30_8000_0000, 0x40_8000_0000, 3),
            ]
        );
        assert_eq!(domain_count(Nps::Nps0, 1), None);
        assert_eq!(domain_count(Nps::Nps0, 2), Some(1));

        // 2P NPS2: S0D0=0, S0D1=1, S1D0=2, S1D1=3.
        let dram_2p = [
            (0u64, 0x10_8000_0000u64, 0xffu32),
            (0x10_8000_0000, 0x20_8000_0000, 0xff00),
            (0x20_8000_0000, 0x30_8000_0000, 0x00ff_0000),
            (0x30_8000_0000, 0x40_8000_0000, 0xff00_0000),
        ];
        let cpus_2p = [(0, 0), (0, 4), (1, 0), (1, 4)];
        let numa_2p = domains(
            Nps::Nps2,
            2,
            dram_2p.into_iter(),
            &regions,
            cpus_2p.into_iter(),
        )
        .unwrap();
        assert_eq!(numa_2p.domains, 4);
        assert_eq!(numa_2p.cpus, [0, 1, 2, 3]);
        assert_eq!(
            (0..4).map(|d| numa_2p.socket(d)).collect::<Vec<_>>(),
            [0, 0, 1, 1]
        );
        // A rule across both IODs of one socket fits no NPS2 domain.
        let spanning = [(0u64, 0x40_8000_0000u64, 0x0101u32)];
        assert_eq!(
            domains(
                Nps::Nps2,
                2,
                spanning.into_iter(),
                &regions,
                cpus_2p.into_iter()
            ),
            Err(Error::Topology)
        );
        // A socket-1 channel on 1P.
        let remote = [(0u64, 0x40_8000_0000u64, 1u32 << 16)];
        assert_eq!(
            domains(
                Nps::Nps1,
                1,
                remote.into_iter(),
                &regions,
                [(0, 0)].into_iter()
            ),
            Err(Error::Topology)
        );
    }

    #[test]
    fn sparse_channels_map_by_their_rule() {
        let ram = |base, size| Region {
            base,
            size,
            kind: RegionKind::Ram,
        };
        let regions = [ram(0x10_0000, 0x6def_0000), ram(1 << 32, 0x10_0000_0000)];
        // Only channel 0 (IOD0) and channel 8 (IOD1) populated.
        let dram = [
            (0u64, 0x9_0000_0000u64, 1u32),
            (0x9_0000_0000, 0x11_0000_0000, 1 << 8),
        ];
        let numa = domains(
            Nps::Nps2,
            1,
            dram.into_iter(),
            &regions,
            [(0, 0), (0, 4)].into_iter(),
        )
        .unwrap();
        assert_eq!(numa.domains, 2);
        assert_eq!(
            numa.memory,
            [
                (0x10_0000, 0x6dff_0000, 0),
                (1 << 32, 0x9_0000_0000, 0),
                (0x9_0000_0000, 0x11_0000_0000, 1),
            ]
        );
    }

    /// CCX as NUMA on 2P NPS2 with all eight CCDs of each socket: 16
    /// reported domains, two per quadrant... each in its IOD's NPS2
    /// domain; a DRAM rule split over its physical domain's four.
    #[test]
    fn ccx_as_numa_reports_a_domain_per_ccd() {
        let ccds: Vec<(u8, u8)> = (0..2).flat_map(|s| (0..8).map(move |c| (s, c))).collect();
        let domains = ccx_domains(Nps::Nps2, 2, ccds.iter().copied()).unwrap();
        assert_eq!(domains.len(), 16);
        assert_eq!(
            domains.iter().map(|d| d.physical).collect::<Vec<_>>(),
            [0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3]
        );
        assert_eq!((domains[9].socket, domains[9].ccd), (1, 1));
        // Root bridges: all four quadrants populated, B0's first table.
        assert_eq!(rb_ccx_domains(&domains, 0, 0, 3), [0, 1]);
        assert_eq!(rb_ccx_domains(&domains, 0, 2, 3), [2, 3]);
        assert_eq!(rb_ccx_domains(&domains, 1, 5, 3), [14, 15]);
        // Two CCDs per IOD (remapped to 0 and 3): both halves only.
        let sparse: Vec<(u8, u8)> = [0u8, 3, 4, 7].iter().map(|&c| (0, c)).collect();
        let sparse = ccx_domains(Nps::Nps4, 1, sparse.into_iter()).unwrap();
        assert_eq!(quadrant_table(&sparse), 0);
        let halves = ccx_domains(Nps::Nps4, 1, [(0u8, 0u8), (0, 4)].into_iter()).unwrap();
        assert_eq!(quadrant_table(&halves), 1);
        assert_eq!(rb_ccx_domains(&halves, 0, 1, 3), [0]);
        let one = ccx_domains(Nps::Nps1, 1, [(0u8, 1u8)].into_iter()).unwrap();
        assert_eq!(quadrant_table(&one), 2);
        assert_eq!(rb_ccx_domains(&one, 0, 4, 3), [0]);

        // Memory: S0D0's rule (NPS2 domain 0) over CCD domains 0..4 in
        // 2 MiB-rounded quarters, cut to the RAM.
        let ram = |base, size| Region {
            base,
            size,
            kind: fstart_driver_amd::apob::sysmap::RegionKind::Ram,
        };
        let g = 1u64 << 30;
        let regions = [ram(0, 2 * g), ram(4 * g, 60 * g)];
        let dram = [
            (0u64, 16 * g, 0xffu32),
            (16 * g, 32 * g, 0xff00),
            (32 * g, 48 * g, 0xff << 16),
            (48 * g, 64 * g, 0xff00 << 16),
        ];
        let numa = domains_with(
            Nps::Nps2,
            2,
            dram.into_iter(),
            &regions,
            ccds.iter().copied(),
            true,
        )
        .unwrap();
        assert_eq!(numa.domains, 16);
        assert_eq!(numa.cpus[9], 9);
        assert_eq!(
            &numa.memory[..3],
            [(0, 2 * g, 0), (4 * g, 8 * g, 1), (8 * g, 12 * g, 2)]
        );
        assert_eq!((numa.socket(9), numa.physical(9)), (1, Some(2)));
        assert_eq!(numa.channels(9), 0xff << 16);
        // An odd size rounds each chunk up to 2 MiB; the last takes less.
        let odd = [(0u64, 3 * (2 << 20) + 4096, 0xffffu32)];
        let numa = domains_with(
            Nps::Nps1,
            1,
            odd.into_iter(),
            &[ram(0, 3 * (2 << 20) + 4096)],
            [(0u8, 0u8), (0, 1)].into_iter(),
            true,
        )
        .unwrap();
        assert_eq!(
            numa.memory,
            [(0, 4 << 20, 0), (4 << 20, 3 * (2 << 20) + 4096, 1)]
        );
    }
}
