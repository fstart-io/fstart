// SPDX-License-Identifier: MIT OR Apache-2.0
//! NUMA domain mapping for SP7 (Venice / WH dual-IOD) SRAT, SLIT, MSCT, and
//! root bridge `_PXM` attributes.
//!
//! Verified against openSIL `Df4BuildDomainInfo`, `WhGetNpsMapData`, and
//! `WhGetPhysDomainOfCcd`:
//! - Each SP7 socket has 2 IODs, 16 UMC channels (8 per IOD: IOD 0 owns
//!   channels `0..8`, IOD 1 owns `8..16`), 8 physical CCDs (`0..8`, 2 per
//!   quadrant), and 8 IOHCs (4 per IOD).
//! - NPS0 (2P interleaved): 1 domain across all sockets.
//! - NPS1: 1 domain per socket (`socket`).
//! - NPS2: 2 domains per socket (`socket * 2 + iod`).
//! - NPS4: 4 domains per socket (`socket * 4 + quad`), where quadrant `q` owns
//!   UMCs `4q..4q+4`, CCDs `2q..2q+2`, and IOHCs `(iod = q / 2, rb = (q % 2) * 2 .. +2)`.
extern crate alloc;

use alloc::vec::Vec;
use fstart_driver_amd::apob::nps::Nps;
use fstart_driver_amd::apob::sysmap::Region;

/// UMC channels per SP7 socket (8 per IOD × 2 IODs).
pub const CS_PER_SOCKET: u16 = 16;
/// Physical CCDs per SP7 socket.
pub const CCDS_PER_SOCKET: u8 = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// Unsupported NPS/socket count or out-of-range CCD/CS/socket coordinate.
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

/// Number of NUMA domains for `sockets` (1 or 2) at `nps`.
pub const fn domain_count(nps: Nps, sockets: u8) -> Option<usize> {
    if sockets == 0 || sockets > 2 {
        return None;
    }
    match nps {
        Nps::Nps0 => {
            if sockets == 2 {
                Some(1)
            } else {
                None
            }
        }
        Nps::Nps1 => Some(sockets as usize),
        Nps::Nps2 => Some((sockets as usize) * 2),
        Nps::Nps4 => Some((sockets as usize) * 4),
    }
}

/// NUMA domain of physical CCD `phys_ccd` (`0..8`) on `socket` (`0..2`).
///
/// Matches `WhGetPhysDomainOfCcd` quadrant table `[0, 0, 1, 1, 2, 2, 3, 3]`.
pub const fn ccd_domain(nps: Nps, socket: u8, phys_ccd: u8) -> Option<u32> {
    if socket >= 2 || phys_ccd >= CCDS_PER_SOCKET {
        return None;
    }
    let s = socket as u32;
    let quad = (phys_ccd / 2) as u32;
    match nps {
        Nps::Nps0 => Some(0),
        Nps::Nps1 => Some(s),
        Nps::Nps2 => Some(s * 2 + quad / 2),
        Nps::Nps4 => Some(s * 4 + quad),
    }
}

/// NUMA domain of root bridge `rb_on_iod` (`0..4`) on `(socket, die)` (`die` in `0..2`).
pub const fn rb_domain(nps: Nps, socket: u8, die: u8, rb_on_iod: u8) -> Option<u32> {
    if socket >= 2 || die >= 2 || rb_on_iod >= 4 {
        return None;
    }
    let s = socket as u32;
    let d = die as u32;
    let half = (rb_on_iod / 2) as u32;
    match nps {
        Nps::Nps0 => Some(0),
        Nps::Nps1 => Some(s),
        Nps::Nps2 => Some(s * 2 + d),
        Nps::Nps4 => Some(s * 4 + d * 2 + half),
    }
}

/// NUMA domain of physical UMC channel `cs` (`0..16`, `die * 8 + local_cs`
/// from [`super::decode_wh_dram_dst`]) on `socket`, verified against the
/// 16-bit populated UMC channel bitmask `umcs` of that socket.
pub const fn cs_domain(nps: Nps, umcs: u16, socket: u8, cs: u16) -> Option<u32> {
    if socket >= 2 || cs >= CS_PER_SOCKET || (umcs & (1u16 << cs)) == 0 {
        return None;
    }
    let s = socket as u32;
    match nps {
        Nps::Nps0 => Some(0),
        Nps::Nps1 => Some(s),
        Nps::Nps2 => Some(s * 2 + (cs / 8) as u32),
        Nps::Nps4 => Some(s * 4 + (cs / 4) as u32),
    }
}

/// Map `(base, end, socket, cs_on_socket)` DRAM rules, system RAM `regions`,
/// and per-thread `(socket, phys_ccd)` coordinates into validated NUMA domains.
pub fn domains(
    nps: Nps,
    sockets: u8,
    umcs_per_socket: u16,
    dram: impl Iterator<Item = (u64, u64, u8, u16)>,
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
    for (base, end, socket, cs) in dram {
        if socket >= sockets {
            return Err(Error::Topology);
        }
        let domain = cs_domain(nps, umcs_per_socket, socket, cs).ok_or(Error::Topology)?;
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
    fn wh_ccds_rbs_and_umcs_align_across_nps1_nps2_nps4() {
        // 8 physical CCDs on socket 0: 0,1->Q0, 2,3->Q1, 4,5->Q2, 6,7->Q3.
        assert_eq!(
            (0..8)
                .map(|c| ccd_domain(Nps::Nps4, 0, c).unwrap())
                .collect::<Vec<_>>(),
            [0, 0, 1, 1, 2, 2, 3, 3]
        );
        assert_eq!(
            (0..8)
                .map(|c| ccd_domain(Nps::Nps2, 0, c).unwrap())
                .collect::<Vec<_>>(),
            [0, 0, 0, 0, 1, 1, 1, 1]
        );
        assert_eq!(ccd_domain(Nps::Nps4, 1, 6), Some(7));
        assert_eq!(ccd_domain(Nps::Nps4, 0, 8), None);

        // 8 IOHCs (2 IODs × 4 IOHCs/IOD) match the same 4 quadrants.
        assert_eq!(rb_domain(Nps::Nps4, 0, 0, 0), Some(0));
        assert_eq!(rb_domain(Nps::Nps4, 0, 0, 2), Some(1));
        assert_eq!(rb_domain(Nps::Nps4, 0, 1, 0), Some(2));
        assert_eq!(rb_domain(Nps::Nps4, 0, 1, 3), Some(3));
        assert_eq!(rb_domain(Nps::Nps2, 0, 1, 0), Some(1));
        assert_eq!(rb_domain(Nps::Nps1, 1, 1, 3), Some(1));

        // 16 full UMCs per socket: 4 per quadrant, 8 per IOD.
        assert_eq!(cs_domain(Nps::Nps4, 0xffff, 0, 0), Some(0));
        assert_eq!(cs_domain(Nps::Nps4, 0xffff, 0, 4), Some(1));
        assert_eq!(cs_domain(Nps::Nps4, 0xffff, 0, 8), Some(2));
        assert_eq!(cs_domain(Nps::Nps4, 0xffff, 0, 15), Some(3));
        assert_eq!(cs_domain(Nps::Nps2, 0xffff, 1, 8), Some(3));
        assert_eq!(cs_domain(Nps::Nps4, 0xffff, 0, 16), None);
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
            (0u64, 0x10_8000_0000u64, 0u8, 0u16),
            (0x10_8000_0000, 0x20_8000_0000, 0, 4),
            (0x20_8000_0000, 0x30_8000_0000, 0, 8),
            (0x30_8000_0000, 0x40_8000_0000, 0, 12),
        ];
        let cpus = (0..8u8).map(|ccd| (0u8, ccd));
        let numa = domains(Nps::Nps4, 1, 0xffff, dram.into_iter(), &regions, cpus).unwrap();
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

        // 1P NPS0 is invalid.
        assert_eq!(domain_count(Nps::Nps0, 1), None);
        assert_eq!(domain_count(Nps::Nps0, 2), Some(1));

        // 2P NPS2 (4 domains: S0D0=0, S0D1=1, S1D0=2, S1D1=3).
        let dram_2p = [
            (0u64, 0x10_8000_0000u64, 0u8, 0u16),
            (0x10_8000_0000, 0x20_8000_0000, 0, 8),
            (0x20_8000_0000, 0x30_8000_0000, 1, 0),
            (0x30_8000_0000, 0x40_8000_0000, 1, 8),
        ];
        let cpus_2p = [(0, 0), (0, 4), (1, 0), (1, 4)];
        let numa_2p = domains(
            Nps::Nps2,
            2,
            0xffff,
            dram_2p.into_iter(),
            &regions,
            cpus_2p.into_iter(),
        )
        .unwrap();
        assert_eq!(numa_2p.domains, 4);
        assert_eq!(numa_2p.cpus, [0, 1, 2, 3]);
    }

    #[test]
    fn cs_domain_maps_sparse_dual_iod_channels_by_physical_umc_index() {
        // Sparse 2-DIMM config: only UMC 0 (IOD0) and UMC 8 (IOD1) populated.
        let sparse_nps2 = (1u16 << 0) | (1u16 << 8);
        assert_eq!(cs_domain(Nps::Nps2, sparse_nps2, 0, 0), Some(0));
        assert_eq!(cs_domain(Nps::Nps2, sparse_nps2, 0, 8), Some(1));
        assert_eq!(cs_domain(Nps::Nps2, sparse_nps2, 0, 1), None);

        // Sparse 4-DIMM NPS4 config: one UMC per quadrant (0, 4, 8, 12).
        let sparse_nps4 = (1u16 << 0) | (1u16 << 4) | (1u16 << 8) | (1u16 << 12);
        assert_eq!(cs_domain(Nps::Nps4, sparse_nps4, 0, 0), Some(0));
        assert_eq!(cs_domain(Nps::Nps4, sparse_nps4, 0, 4), Some(1));
        assert_eq!(cs_domain(Nps::Nps4, sparse_nps4, 0, 8), Some(2));
        assert_eq!(cs_domain(Nps::Nps4, sparse_nps4, 0, 12), Some(3));
        assert_eq!(cs_domain(Nps::Nps4, sparse_nps4, 0, 2), None);

        let ram = |base, size| Region {
            base,
            size,
            kind: RegionKind::Ram,
        };
        let regions = [ram(0x10_0000, 0x6def_0000), ram(1 << 32, 0x10_0000_0000)];
        let dram = [
            (0u64, 0x9_0000_0000u64, 0u8, 0u16),
            (0x9_0000_0000, 0x11_0000_0000, 0, 8),
        ];
        let numa = domains(
            Nps::Nps2,
            1,
            sparse_nps2,
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
