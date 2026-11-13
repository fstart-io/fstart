//! The SP3 bus-map rewrite and the ReadyToBoot lock-down (SOC_DF §3.4.4,
//! §3.7). The MMIO, I/O and VGA rules are [`crate::df::program`].

use fstart_pci::ConfigRegionAccess;
use tock_registers::interfaces::{ReadWriteable, Writeable};

use super::topology::Topology;
use super::{BusRange, Df, Target, Version};
use crate::df::regs::{self, CFG_ACCESS_CTRL, CFG_ADDR_CTL};
use crate::sp3::topology::Node;

fn node_ids(nodes: &[Node]) -> impl Iterator<Item = u8> + '_ {
    nodes.iter().map(|n| n.device - crate::df::NODE0_DEVICE)
}

/// The copies of CfgAddressMap/CfgAddressCntl a bus-map rewrite touches:
/// every CCM and IOMS instance on DF3 (`ffe1e526`), the node's own copy on
/// DF2.
pub fn bus_map_targets(topology: &Topology) -> heapless::Vec<Target, 16> {
    match topology.generation {
        crate::sp3::soc::Generation::Naples => {
            heapless::Vec::from_slice(&[Target::Node]).unwrap_or_default()
        }
        crate::sp3::soc::Generation::Rome | crate::sp3::soc::Generation::Milan => {
            let ioms = topology
                .root_bridges
                .iter()
                .filter(|r| r.socket == 0)
                .map(|r| Target::Instance(super::v3::IOMS[usize::from(r.index)]));
            super::v3::CCMS.map(Target::Instance).chain(ioms).collect()
        }
    }
}

/// Rewrite the bus maps for `ranges` (physical order, from
/// [`plan_buses`](crate::df::plan::plan_buses)) and update `topology`.
///
/// AGESA's sequence (`ffe1e526`, IC for the writes): clear the eight
/// CfgAddressMap registers and CfgAddressCntl on every target of every
/// node, then write the maps in ascending bus order (the primary, at bus 0,
/// is map 0) and each root bridge's CfgAddressCntl. Only run this when a
/// board asks for different bus counts than the ABL left.
pub fn bus_maps<V: Version, P: ConfigRegionAccess + Clone>(
    df: &Df<P>,
    topology: &mut Topology,
    ranges: &[BusRange],
) {
    let targets = bus_map_targets(topology);
    for node in node_ids(&topology.nodes) {
        for &t in targets.iter() {
            for i in 0..V::CFG_MAPS {
                // SAFETY: CfgAddressMap `i` (< 8) of an existing node.
                unsafe { df.reg(node, t, V::cfg_map_reg(i)) }.set(0);
            }
            // SAFETY: CfgAddressCntl of an existing node/instance.
            unsafe { df.reg(node, t, regs::CFG_ADDR_CTL) }.set(0);
        }
    }
    for (rb, bus) in topology.root_bridges.iter_mut().zip(ranges) {
        rb.bus_base = bus.base;
        rb.bus_limit = bus.limit;
    }
    let order = topology.logical();
    for (map, rb) in order.iter().enumerate() {
        for node in node_ids(&topology.nodes) {
            for &t in targets.iter() {
                // SAFETY: as above.
                unsafe { df.reg(node, t, V::cfg_map_reg(map as u8)) }
                    .write(V::cfg_map(super::topology::bus_range(rb), rb.fabric_id));
            }
        }
        let own = match topology.generation {
            crate::sp3::soc::Generation::Naples => Target::Node,
            _ => Target::Instance(0x18 + rb.index),
        };
        // SAFETY: the root bridge's own CfgAddressCntl.
        unsafe { df.reg(topology.node_of(rb), own, regs::CFG_ADDR_CTL) }
            .write(CFG_ADDR_CTL::BUS_NUM.val(u32::from(rb.bus_base)));
    }
}

/// ReadyToBoot: clear F4x44 bit 0 ("extended configuration access") on
/// every node (SOC_DF §3.7, all generations). AGESA also records it in the
/// S3 boot script; a resume path must repeat it.
pub fn ready_to_boot<P: ConfigRegionAccess + Clone>(df: &Df<P>, nodes: &[Node]) {
    for node in node_ids(nodes) {
        // SAFETY: F4x44 of an existing node.
        unsafe { df.node(node, regs::CFG_ACCESS_CTRL) }.modify(CFG_ACCESS_CTRL::EXT_CFG::CLEAR);
    }
}

#[cfg(test)]
mod tests {
    use super::super::model::DfModel;
    use super::super::topology::{discover, tests::df3_system};
    use super::super::v3::Df3;
    use super::*;
    use crate::df::plan::{IoRoute, MmioRoute, Window};
    use crate::df::program::{io, mmio, vga};
    use crate::sp3::soc::Generation;

    #[test]
    fn mmio_and_io_on_every_node() {
        let m = df3_system(2, 0b1111);
        let df = Df::new(m.clone());
        let t = discover(&df, Generation::Rome).unwrap();
        m.clear_log();
        let mmio_routes = [MmioRoute {
            pair: 6,
            window: Window::new(0xF000_0000, 0xEC0_0000),
            fabric_id: 0x1B,
        }];
        let nodes = unsafe { df.each(&t.nodes) };
        mmio::<Df3, _>(&nodes, &mmio_routes);
        io::<Df3, _>(
            &nodes,
            &[IoRoute {
                pair: 3,
                window: Window::new(0, 0x4000),
                fabric_id: 0x1B,
            }],
        );
        let log: std::vec::Vec<_> = m
            .log()
            .iter()
            .map(|((n, _, f, o), v)| (*n, *f, *o, *v))
            .collect();
        assert_eq!(
            log,
            [
                (0, 0, 0x260, 0xF000),
                (0, 0, 0x264, 0xFEBF),
                (0, 0, 0x268, 0x1B3),
                (1, 0, 0x260, 0xF000),
                (1, 0, 0x264, 0xFEBF),
                (1, 0, 0x268, 0x1B3),
                (0, 0, 0xD8, 0x3),
                (0, 0, 0xDC, 0x3000 | 0x1B),
                (1, 0, 0xD8, 0x3),
                (1, 0, 0xDC, 0x3000 | 0x1B),
            ]
        );
    }

    #[test]
    fn bus_map_rewrite_primary_first() {
        let m = df3_system(1, 0b1111);
        let df = Df::new(m.clone());
        let mut t = discover(&df, Generation::Rome).unwrap();
        let ranges = [(0x40, 0x7F), (0x80, 0xBF), (0xC0, 0xFF), (0, 0x3F)]
            .map(|(base, limit)| BusRange { base, limit });
        bus_maps::<Df3, _>(&df, &mut t, &ranges);
        // Map 0 on CCM0 and on IOMS3 routes bus 0..0x3F to the primary.
        assert_eq!(m.get(0, Some(0x10), 0, 0xA0), 0x3F00_01B3);
        assert_eq!(m.get(0, Some(0x1B), 0, 0xA0), 0x3F00_01B3);
        assert_eq!(m.get(0, Some(0x10), 0, 0xAC), 0xFFC0_01A3);
        assert_eq!(m.get(0, Some(0x1B), 0, 0x84), 0);
        assert_eq!(m.get(0, Some(0x18), 0, 0x84), 0x40);
        // Unused maps were cleared.
        assert_eq!(m.get(0, Some(0x17), 0, 0xB0), 0);
        // The rewritten map reads back as the same topology.
        assert_eq!(discover(&df, Generation::Rome).unwrap(), t);
    }

    #[test]
    fn vga_and_ready_to_boot() {
        let m = DfModel::default();
        let df = Df::new(m.clone());
        let nodes = [Node {
            socket: 0,
            die: 0,
            device: 0x18,
        }];
        m.set(0, None, 0, 0x80, 0xFFFF_FFFF);
        m.set(0, None, 4, 0x44, 0x1001);
        vga::<Df3, _>(&unsafe { df.each(&nodes) }, 0x1B);
        ready_to_boot(&df, &nodes);
        assert_eq!(m.get(0, None, 0, 0x80), 0xFFFF_C1BB);
        assert_eq!(m.get(0, None, 4, 0x44), 0x1000);
    }
}
