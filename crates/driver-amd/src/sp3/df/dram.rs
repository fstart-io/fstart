//! DRAM map read-back (the ABL programs it; the x86 only reads) and the
//! NPS → NUMA domain mapping for SRAT (SOC_DF §3.7, ACPI_SMM §3.5).

use fstart_pci::ConfigRegionAccess;
use heapless::Vec;
use tock_registers::interfaces::Readable;

use super::{DRAM_HOLE, Df, DramRule, Reg, Target, Version, regs};
pub use crate::sp3::apob::nps::Nps;

/// The DRAM rules and MMIO hole as one node sees them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DramMap {
    /// Valid rules in register order.
    pub rules: Vec<DramRule, 16>,
    /// DramHoleControl base when valid: DRAM in \[hole, 4 GiB) of a rule
    /// with `hole` set is remapped above 4 GiB.
    pub hole_base: Option<u64>,
}

impl DramMap {
    /// Memory ranges `(base, end, dst)` with the hole cut out.
    pub fn ranges(&self) -> impl Iterator<Item = (u64, u64, u16)> + '_ {
        self.rules.iter().flat_map(move |r| {
            let split = self
                .hole_base
                .filter(|&h| r.hole && r.base < h && r.end > 1 << 32);
            let first = match split {
                Some(h) => (r.base, h, r.dst),
                None => (r.base, r.end, r.dst),
            };
            let second = split.map(|_| (1 << 32, r.end, r.dst));
            core::iter::once(first).chain(second)
        })
    }
}

/// Read the DRAM map of `node` through `target` (AGESA: PIE instance 0x1E
/// on DF3, instance 5 on ZP; §3.6, DF_ZP §3.8).
pub fn read<V: Version, P: ConfigRegionAccess + Clone>(
    df: &Df<P>,
    node: u8,
    target: Target,
) -> DramMap {
    let rules = (0..V::DRAM_RULES)
        .filter_map(|i| {
            // SAFETY: DRAM rule `i` (< 16) of an existing node.
            let (b, l) = unsafe {
                (
                    df.reg(
                        node,
                        target,
                        Reg::<()>::new(0, regs::DRAM_BASE_OFFSET).at(i, regs::DRAM_STRIDE),
                    )
                    .get(),
                    df.reg(
                        node,
                        target,
                        Reg::<()>::new(0, regs::DRAM_LIMIT_OFFSET).at(i, regs::DRAM_STRIDE),
                    )
                    .get(),
                )
            };
            V::decode_dram(b, l)
        })
        .collect();
    // SAFETY: DramHoleControl of an existing node.
    let hole = unsafe { df.reg(node, target, regs::DRAM_HOLE) }.extract();
    DramMap {
        rules,
        hole_base: hole
            .is_set(DRAM_HOLE::VALID)
            .then(|| u64::from(hole.read(DRAM_HOLE::BASE)) << 24),
    }
}

/// A NUMA domain as a set of memory channels: bit `16 · socket + channel`
/// (8 UMCs per socket). The proximity domain is the index in the list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NumaDomain {
    pub channels: u32,
}

impl NumaDomain {
    pub const fn has_channel(&self, socket: u8, channel: u8) -> bool {
        self.channels & 1 << (16 * socket as u32 + channel as u32) != 0
    }
}

/// Domains of an NPS mode (AmdFabricSspDxe `0x163c`, ACPI_SMM §3.5): NPS0
/// one domain over every socket's channels, NPS1/2/4 1/2/4 domains of
/// 8/4/2 channels per socket. The APOB 0x14 extra domains and the "LLC as
/// NUMA" option are not modelled (their meaning is open).
pub fn numa_domains(nps: Nps, sockets: u8) -> Vec<NumaDomain, 8> {
    let socket_mask = |s: u8| 0xFFu32 << (16 * u32::from(s));
    match nps {
        Nps::Nps0 => core::iter::once(NumaDomain {
            channels: (0..sockets).map(socket_mask).fold(0, |a, m| a | m),
        })
        .collect(),
        Nps::Nps1 | Nps::Nps2 | Nps::Nps4 => {
            let per = match nps {
                Nps::Nps1 => 1u32,
                Nps::Nps2 => 2,
                _ => 4,
            };
            let width = 8 / per;
            (0..sockets)
                .flat_map(|s| {
                    (0..per).map(move |d| NumaDomain {
                        channels: ((1 << width) - 1) << (width * d + 16 * u32::from(s)),
                    })
                })
                .collect()
        }
    }
}

/// The domain (index into `domains`) owning `(socket, channel)`.
pub fn domain_of(domains: &[NumaDomain], socket: u8, channel: u8) -> Option<u32> {
    domains
        .iter()
        .position(|d| d.has_channel(socket, channel))
        .map(|i| i as u32)
}

/// `(socket, channel)` of a DF3 DRAM rule destination: CS instances 0–7
/// are the UMC channels (`fabric_id = socket << 5 | instance`).
pub const fn df3_channel(dst: u16) -> Option<(u8, u8)> {
    let instance = dst & 0x1F;
    if instance < 8 {
        Some(((dst >> 5) as u8, instance as u8))
    } else {
        None
    }
}

/// The channel whose domain an IOMS (root bridge `ioms`, 0–3) belongs to:
/// quadrant `i` holds channels `2i, 2i + 1` (AmdFabricSspDxe `0x2284`:
/// NPS4 IOMS `i` → domain `i`, NPS2 IOMS 0/1 → 0 and 2/3 → 1).
pub const fn ioms_channel(ioms: u8) -> u8 {
    2 * ioms
}

/// The channel whose domain a CCD (physical index) belongs to: CCD `j` sits
/// on channel `j`'s quadrant (AmdFabricSspDxe `0x163c` builds each domain's
/// CCX mask as two CCX bits per channel of the domain, from the lowest
/// channel up). Milan shares the IOD layout (inferred).
pub const fn ccd_channel(ccd: u8) -> u8 {
    ccd
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nps_masks() {
        let m = |nps, s| -> std::vec::Vec<u32> {
            numa_domains(nps, s).iter().map(|d| d.channels).collect()
        };
        assert_eq!(m(Nps::Nps0, 2), [0x00FF_00FF]);
        assert_eq!(m(Nps::Nps1, 2), [0xFF, 0xFF_0000]);
        assert_eq!(m(Nps::Nps2, 1), [0x0F, 0xF0]);
        assert_eq!(
            m(Nps::Nps4, 2),
            [
                0x3, 0xC, 0x30, 0xC0, 0x3_0000, 0xC_0000, 0x30_0000, 0xC0_0000
            ]
        );
        assert!(numa_domains(Nps::Nps4, 1)[1].has_channel(0, 3));
    }

    #[test]
    fn quadrant_domains() {
        let nps4 = numa_domains(Nps::Nps4, 2);
        // IOMS i → domain i, CCD 2i/2i+1 → domain i, socket 1 → 4 + i.
        assert_eq!(domain_of(&nps4, 0, ioms_channel(2)), Some(2));
        assert_eq!(domain_of(&nps4, 0, ccd_channel(5)), Some(2));
        assert_eq!(domain_of(&nps4, 1, ioms_channel(3)), Some(7));
        let nps2 = numa_domains(Nps::Nps2, 1);
        assert_eq!(domain_of(&nps2, 0, ioms_channel(1)), Some(0));
        assert_eq!(domain_of(&nps2, 0, ioms_channel(2)), Some(1));
        assert_eq!(df3_channel(0x20 | 6), Some((1, 6)));
        assert_eq!(df3_channel(0x0A), None);
    }

    #[cfg(any(feature = "rome", feature = "milan"))]
    #[test]
    fn read_back_splits_the_hole() {
        use super::super::model::DfModel;
        use super::super::v3::{Df3, PIE};
        let m = DfModel::default();
        // Rule 0: [0, 66 GiB) with the hole at 2 GiB, socket 0 CS 0.
        m.set(0, Some(PIE), 0, 0x110, 0x3);
        m.set(0, Some(PIE), 0, 0x114, (66 * 4 - 1) << 12);
        m.set(0, Some(PIE), 0, 0x104, 0x8000_0001);
        let map = read::<Df3, _>(&Df::new(m), 0, Target::Instance(PIE));
        assert_eq!(map.rules.len(), 1);
        let r: std::vec::Vec<_> = map.ranges().collect();
        assert_eq!(r, [(0, 0x8000_0000, 0), (1 << 32, 66 << 30, 0)]);
    }
}
