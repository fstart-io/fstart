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
}

impl Numa {
    /// The socket of domain `domain`.
    pub fn socket(&self, domain: usize) -> u8 {
        match self.nps.domains_per_socket() {
            Some(per) => (domain / usize::from(per)) as u8,
            None => 0,
        }
    }
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
    let domains = domain_count(nps, sockets).ok_or(Error::Topology)?;
    let cpus = cpus_iter
        .map(|(socket, ccd)| {
            if socket >= sockets {
                return Err(Error::Topology);
            }
            ccd_domain(nps, socket, ccd).ok_or(Error::Topology)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let ram = || regions.iter().filter(|r| r.kind.is_ram());
    let mut memory = Vec::new();
    for (base, end, channels) in dram {
        if sockets < 2 && channels >> CHANNELS_PER_SOCKET != 0 {
            return Err(Error::Topology);
        }
        let domain = channels_domain(nps, sockets, channels).ok_or(Error::Topology)?;
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
}
