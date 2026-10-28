//! Topology discovery: sockets, DF nodes and PCI root bridges with their
//! fabric IDs and bus ranges (SOC_DF §3.1, IC).
//!
//! | | Naples (DF2) | Rome / Milan (DF3) |
//! |---|---|---|
//! | sockets | F1x200\[5\] + 1 | F1x200\[27\] + 1 |
//! | nodes | 4 per socket at most, F1x200\[3:0\] present dies | 1 per socket |
//! | root bridges | 1 per die | IOMS 0x18–0x1B per socket (Milan: fuse filtered) |
//! | bus base | F0x84 of the die | F0x84 of the IOMS instance |
//! | bus limit | CfgAddressMap of node 0 (own copy) | CfgAddressMap of CCM0 on node 0 |
//! | primary | socket 0 die 0 | socket 0 IOMS 0x1B |
//!
//! The ABL/PSP leaves the bus maps; this only reads them. Missing or
//! inconsistent data is an error (AGESA defaults a missing bus limit to
//! 0xFF; here it fails closed).

use fstart_pci::ConfigRegionAccess;
use heapless::Vec;
#[cfg(any(feature = "naples", feature = "rome", feature = "milan"))]
use tock_registers::interfaces::Readable;

#[cfg(any(feature = "naples", feature = "rome", feature = "milan"))]
use super::Version;
use super::{BusRange, Df};
use crate::df::MAX_NODES;
use crate::df::regs;
use crate::sp3::soc::Generation;
use crate::sp3::topology::{MAX_ROOT_BRIDGES, Node, RootBridge};

/// Milan IOMS fuse: SMN 0x5D148 bit `11 + i` set = IOMS `i` absent
/// (SOC_DF §3.9, AmdSocSp3GnPei entry, IC).
pub const IOMS_FUSE_SMN: u32 = 0x5D148;

tock_registers::register_bitfields![u32,
    pub IOMS_FUSE [
        IOMS_DISABLE OFFSET(11) NUMBITS(4) []
    ]
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TopologyError {
    /// This build has no support for the generation.
    Unsupported(Generation),
    /// A socket reports no present die (DF2).
    NoDies { socket: u8 },
    /// The node or root-bridge total in F1x204 disagrees with the per-socket
    /// view.
    Count { expected: u8, found: u8 },
    /// No enabled CfgAddressMap routes to this root bridge.
    NoBusMap { fabric_id: u16 },
    /// The FCH root bridge is missing (e.g. fused off).
    NoPrimary,
}

/// What [`discover`] found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Topology {
    pub generation: Generation,
    pub sockets: u8,
    /// DF nodes, socket-major.
    pub nodes: Vec<Node, { MAX_NODES as usize }>,
    /// Root bridges in physical order (socket, then die/IOMS index).
    pub root_bridges: Vec<RootBridge, MAX_ROOT_BRIDGES>,
}

impl Topology {
    /// The root bridge with the FCH.
    pub fn primary(&self) -> &RootBridge {
        // `discover` guarantees one.
        self.root_bridges
            .iter()
            .find(|r| r.primary)
            .expect("primary root bridge")
    }

    /// Root bridges on one socket (equal on every socket).
    pub fn root_bridges_per_socket(&self) -> u8 {
        (self.root_bridges.len() / usize::from(self.sockets)) as u8
    }

    /// AGESA's logical order: ascending bus base (`ffe20886`).
    pub fn logical(&self) -> Vec<RootBridge, MAX_ROOT_BRIDGES> {
        let mut v = self.root_bridges.clone();
        v.sort_unstable_by_key(|r| (r.bus_base, r.socket, r.index));
        v
    }

    /// DF node that owns `rb`'s registers (the die on Naples, the socket's
    /// IOD on Rome/Milan).
    pub fn node_of(&self, rb: &RootBridge) -> u8 {
        match self.generation {
            Generation::Naples => rb.socket * 4 + rb.index,
            Generation::Rome | Generation::Milan => rb.socket,
        }
    }

    /// Number of DF nodes.
    pub fn node_count(&self) -> u8 {
        self.nodes.len() as u8
    }
}

/// Read the topology. The DF config space on bus 0 must be reachable (ECAM
/// set up); Milan also reads its IOMS fuse through SMN on bus 0.
pub fn discover<P: ConfigRegionAccess + Clone>(
    df: &Df<P>,
    generation: Generation,
) -> Result<Topology, TopologyError> {
    match generation {
        #[cfg(feature = "naples")]
        Generation::Naples => discover_df2(df),
        #[cfg(feature = "rome")]
        Generation::Rome => discover_df3(df, generation, 0b1111),
        #[cfg(feature = "milan")]
        Generation::Milan => {
            // SAFETY: the BSP owns this bus's SMN index/data pair during init.
            let smn = unsafe { crate::smn::Smn::on_bus(df.pci().clone(), 0) };
            // A read-only fuse register.
            let fused = smn
                .reg(crate::smn::SmnReg::<IOMS_FUSE::Register>::new(
                    IOMS_FUSE_SMN,
                ))
                .read(IOMS_FUSE::IOMS_DISABLE);
            discover_df3(df, generation, !fused as u8 & 0b1111)
        }
        #[allow(unreachable_patterns)]
        g => {
            let _ = df;
            Err(TopologyError::Unsupported(g))
        }
    }
}

#[cfg(any(feature = "naples", feature = "rome", feature = "milan"))]
/// The bus limit of the CfgAddressMap that routes `bus_base` to `dst`.
fn bus_limit<V: Version>(maps: &[u32], fabric_id: u16) -> Result<u8, TopologyError> {
    maps.iter()
        .filter_map(|&m| V::decode_cfg_map(m))
        .find(|(_, dst)| *dst == fabric_id)
        .map(|(bus, _)| bus.limit)
        .ok_or(TopologyError::NoBusMap { fabric_id })
}

#[cfg(any(feature = "naples", feature = "rome", feature = "milan"))]
fn read_maps<P: ConfigRegionAccess + Clone, V: Version>(
    df: &Df<P>,
    target: super::Target,
) -> Vec<u32, 8> {
    (0..V::CFG_MAPS)
        // SAFETY: CfgAddressMap of node 0 (always present).
        .map(|i| unsafe { df.reg(0, target, V::cfg_map_reg(i)) }.get())
        .collect()
}

#[cfg(feature = "naples")]
fn discover_df2<P: ConfigRegionAccess + Clone>(df: &Df<P>) -> Result<Topology, TopologyError> {
    use super::v2::Df2;
    use crate::df::v2::{self, SYSTEM_CFG};

    // SAFETY (all reads below): F1x200/F1x204/F0x84/F0xA0 of present nodes.
    let sockets = unsafe { df.node(0, v2::SYSTEM_CFG) }.read(SYSTEM_CFG::OTHER_SOCK) as u8 + 1;
    let maps = read_maps::<P, Df2>(df, super::Target::Node);
    let mut nodes = Vec::new();
    let mut rbs = Vec::new();
    for s in 0..sockets {
        let present =
            unsafe { df.node(v2::node(s, 0), v2::SYSTEM_CFG) }.read(SYSTEM_CFG::DIE_PRESENT);
        // AGESA: ctz(present + 1), the contiguous present dies from die 0.
        let dies = (present + 1).trailing_zeros().min(4) as u8;
        if dies == 0 {
            return Err(TopologyError::NoDies { socket: s });
        }
        for d in 0..dies {
            let node = v2::node(s, d);
            let fabric_id = v2::fabric_id(s, d, v2::IOMS_COMPONENT);
            let base = unsafe { df.node(node, regs::CFG_ADDR_CTL) }
                .read(regs::CFG_ADDR_CTL::BUS_NUM) as u8;
            let _ = nodes.push(Node {
                socket: s,
                die: d,
                device: crate::df::NODE0_DEVICE + node,
            });
            let _ = rbs.push(RootBridge {
                socket: s,
                index: d,
                fabric_id,
                bus_base: base,
                bus_limit: bus_limit::<Df2>(&maps, fabric_id)?,
                primary: s == 0 && d == 0,
            });
        }
    }
    let total = unsafe { df.node(0, regs::COMPONENT_CNT) }.read(regs::COMPONENT_CNT::PIE) as u8;
    if total != nodes.len() as u8 {
        return Err(TopologyError::Count {
            expected: total,
            found: nodes.len() as u8,
        });
    }
    Ok(Topology {
        generation: crate::sp3::soc::Generation::Naples,
        sockets,
        nodes,
        root_bridges: rbs,
    })
}

/// `present`: bit `i` = IOMS `i` present (Milan fuse, all four on Rome).
#[cfg(any(feature = "rome", feature = "milan"))]
fn discover_df3<P: ConfigRegionAccess + Clone>(
    df: &Df<P>,
    generation: Generation,
    present: u8,
) -> Result<Topology, TopologyError> {
    use super::v3::{self, Df3, SYSTEM_CFG};

    // SAFETY (all reads below): F1x200/F1x204, F0x84 of IOMS instances and
    // CfgAddressMap of CCM0 on present nodes.
    let sockets = unsafe { df.node(0, v3::SYSTEM_CFG) }.read(SYSTEM_CFG::OTHER_SOCK) as u8 + 1;
    let maps = read_maps::<P, Df3>(df, super::Target::Instance(v3::CCM0));
    let ioms = || {
        v3::IOMS
            .iter()
            .enumerate()
            .filter(move |(i, _)| present & (1 << i) != 0)
    };
    let mut rbs = Vec::new();
    for s in 0..sockets {
        for (i, &id) in ioms() {
            let fabric_id = v3::fabric_id(s, id);
            let base = unsafe { df.inst(s, id, regs::CFG_ADDR_CTL) }
                .read(regs::CFG_ADDR_CTL::BUS_NUM) as u8;
            let _ = rbs.push(RootBridge {
                socket: s,
                index: i as u8,
                fabric_id,
                bus_base: base,
                bus_limit: bus_limit::<Df3>(&maps, fabric_id)?,
                primary: s == 0 && id == v3::PRIMARY_IOMS,
            });
        }
    }
    let total = unsafe { df.node(0, regs::COMPONENT_CNT) }.read(regs::COMPONENT_CNT::IOMS) as u8;
    if total != rbs.len() as u8 {
        return Err(TopologyError::Count {
            expected: total,
            found: rbs.len() as u8,
        });
    }
    if !rbs.iter().any(|r| r.primary) {
        return Err(TopologyError::NoPrimary);
    }
    let nodes = (0..sockets)
        .map(|s| Node {
            socket: s,
            die: 0,
            device: crate::df::NODE0_DEVICE + s,
        })
        .collect();
    Ok(Topology {
        generation,
        sockets,
        nodes,
        root_bridges: rbs,
    })
}

/// Bus range of a root bridge.
pub fn bus_range(rb: &RootBridge) -> BusRange {
    BusRange {
        base: rb.bus_base,
        limit: rb.bus_limit,
    }
}

#[cfg(test)]
pub(crate) mod tests {
    #[allow(unused_imports)]
    use super::super::model::DfModel;
    use super::*;

    #[cfg(any(feature = "rome", feature = "milan"))]
    /// A DF3 system with `sockets` × the `present` IOMS and 64 buses each in
    /// IOMS order (or 32 on 2P).
    pub fn df3_system(sockets: u8, present: u8) -> DfModel {
        let m = DfModel::default();
        let ioms: std::vec::Vec<u8> = (0..4u8).filter(|i| present & (1 << i) != 0).collect();
        let n = ioms.len() as u8 * sockets;
        let per = (256 / u16::from(n)) as u8;
        m.set(0, None, 1, 0x200, u32::from(sockets == 2) << 27);
        m.set(0, None, 1, 0x204, u32::from(n) << 16);
        let mut k = 0u8;
        for s in 0..sockets {
            for &i in &ioms {
                let base = k * per;
                let fid = u32::from(super::super::v3::fabric_id(s, 0x18 + i));
                m.set(s, Some(0x18 + i), 0, 0x84, u32::from(base));
                let map = (u32::from(base) + u32::from(per) - 1) << 24
                    | u32::from(base) << 16
                    | fid << 4
                    | 3;
                m.set(0, Some(0x10), 0, 0xA0 + 4 * u16::from(k), map);
                k += 1;
            }
        }
        m
    }

    #[cfg(feature = "rome")]
    #[test]
    fn rome_2p() {
        let df = Df::new(df3_system(2, 0b1111));
        let t = discover(&df, Generation::Rome).unwrap();
        assert_eq!(
            (t.sockets, t.root_bridges.len(), t.root_bridges_per_socket()),
            (2, 8, 4)
        );
        let p = t.primary();
        assert_eq!(
            (p.socket, p.index, p.fabric_id, p.bus_base, p.bus_limit),
            (0, 3, 0x1B, 0x60, 0x7F)
        );
        let s1 = t.root_bridges[4];
        assert_eq!(
            (s1.fabric_id, s1.bus_base, s1.bus_limit),
            (0x38, 0x80, 0x9F)
        );
        assert_eq!(t.node_of(&s1), 1);
    }

    #[cfg(feature = "milan")]
    #[test]
    fn milan_fused_ioms() {
        // IOMS 1 fused off: SMN 0x5D148 bit 12.
        let m = df3_system(1, 0b1101);
        m.smn.0.borrow_mut().smn.insert((0, IOMS_FUSE_SMN), 1 << 12);
        let t = discover(&Df::new(m.clone()), Generation::Milan).unwrap();
        let ids: std::vec::Vec<u16> = t.root_bridges.iter().map(|r| r.fabric_id).collect();
        assert_eq!(ids, [0x18, 0x1A, 0x1B]);
        // A count mismatch with F1x204 fails closed.
        m.smn.0.borrow_mut().smn.insert((0, IOMS_FUSE_SMN), 0);
        assert!(discover(&Df::new(m), Generation::Milan).is_err());
    }

    #[cfg(feature = "naples")]
    #[test]
    fn naples_2p() {
        let m = DfModel::default();
        for s in 0..2u8 {
            m.set(s * 4, None, 1, 0x200, 0xF | 1 << 5);
            for d in 0..4u8 {
                let k = s * 4 + d;
                m.set(k, None, 0, 0x84, u32::from(k) * 0x20);
                let fid = u32::from(crate::df::v2::fabric_id(s, d, 4));
                let base = u32::from(k) * 0x20;
                m.set(
                    0,
                    None,
                    0,
                    0xA0 + 4 * u16::from(k),
                    (base + 0x1F) << 24 | base << 16 | fid << 4 | 3,
                );
            }
        }
        m.set(0, None, 1, 0x204, 8);
        let t = discover(&Df::new(m), Generation::Naples).unwrap();
        assert_eq!(t.nodes.len(), 8);
        let rb = t.root_bridges[6];
        assert_eq!(
            (rb.socket, rb.index, rb.fabric_id, rb.bus_base, rb.bus_limit),
            (1, 2, 0xC4, 0xC0, 0xDF)
        );
        assert_eq!(t.node_of(&rb), 6);
        assert!(t.root_bridges[0].primary);
    }

    #[test]
    fn logical_order() {
        let t = Topology {
            generation: Generation::Rome,
            sockets: 1,
            nodes: Vec::new(),
            root_bridges: [(0u8, 0x40u8), (1, 0x80), (2, 0xC0), (3, 0)]
                .iter()
                .map(|&(i, b)| RootBridge {
                    socket: 0,
                    index: i,
                    fabric_id: 0x18 + u16::from(i),
                    bus_base: b,
                    bus_limit: b + 0x3F,
                    primary: i == 3,
                })
                .collect(),
        };
        let order: std::vec::Vec<u8> = t.logical().iter().map(|r| r.index).collect();
        assert_eq!(order, [3, 0, 1, 2]);
    }
}
