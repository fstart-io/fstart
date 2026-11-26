// SPDX-License-Identifier: MIT OR Apache-2.0
//! The decoders of a CXL switch hierarchy below one root port (CxlV2Manager
//! `CxlMgrEnableSwitchRecursive` `0xE6C0`, `CxlMgrSwitchDecoderProgramming`
//! `0xCEA0`, `CxlMgrEnableEndpointMemoryPools` `0xB9B0`;
//! reports/research/CXL_COMPLETE.md §2.2.2, §2.2.3).
//!
//! The ABL maps a switch as one APCB region, the sum of every endpoint
//! below it (nested switches included), into one window. [`plan`] carves
//! that window into the decoders of every switch upstream port and every
//! endpoint below it, as far down as the vendor goes ([`MAX_DEPTH`]
//! switch levels):
//!
//! - at each switch, the endpoints on its downstream ports whose pools are
//!   identical in count, size and type form one interleave set
//!   (`PcdAmdCxlSwitchInterleave`, both donors); without interleave, and
//!   for a nested switch, each child is a set of its own;
//! - the sets follow each other in the window, each pool of a set taking
//!   one range: one upstream decoder per range (ways = members, the target
//!   list their downstream ports' numbers, way `i` in byte `i` as the
//!   vendor writes it for switches), and on each member endpoint the
//!   decoder of that pool over the same range;
//! - a nested switch takes a one-way range of its parent's and plans its
//!   own children inside it.
//!
//! The vendor's decoders differ where they cannot decode: its endpoints
//! below a switch keep the window's ways and are placed after the earlier
//! endpoints' sizes while their decoder still spans the whole window, and
//! a nested switch is left out of its parent's sets (only its base is
//! clamped into the parent's range). fstart decodes as the CXL spec does
//! and Linux checks for committed decoders: every decoder of one range has
//! the range's base and size; an endpoint interleaves over every way above
//! it, at the window's granularity; a switch at the window's granularity
//! times the ways above it (the address bit after the ones the levels
//! above select). Below a host-interleaved window each root port's
//! hierarchy must be the same (as the ABL requires of the interleaved
//! regions anyway): a range of `n` bytes of device memory per way takes
//! `n` times the ways above it of the window.

use heapless::Vec;

use super::apcb::{MAX_POOLS, Pool};

/// Most nodes (switch upstream ports and endpoints) below one root port.
pub const MAX_NODES: usize = 32;
/// Most decoders [`plan`] assigns below one root port.
pub const MAX_ASSIGNED: usize = 64;
/// The vendor's recursion limit: switch levels below a root port.
pub const MAX_DEPTH: usize = 4;
/// Most targets of one decoder (Target List Low/High).
pub const MAX_TARGETS: usize = 8;

/// A switch's upstream port or an endpoint below a root port.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
    /// Its upstream switch (index of the node) and the number of the
    /// downstream port it is on (PCIe Link Capabilities Port Number);
    /// `None` for the node right below the root port.
    pub parent: Option<(usize, u8)>,
    pub switch: bool,
    /// An endpoint's pools in decoder order (unused for a switch).
    pub pools: Vec<Pool, MAX_POOLS>,
}

/// The range the top switch's region was mapped at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window {
    pub base: u64,
    pub size: u64,
    /// The host bridge's ways over it.
    pub ways: u8,
    /// Its interleave granularity in bytes.
    pub granularity: u32,
}

/// One decoder of one node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Assigned {
    pub node: usize,
    /// The node's decoder index (an endpoint's: its pool).
    pub decoder: u8,
    pub base: u64,
    pub size: u64,
    pub ways: u8,
    pub granularity: u32,
    /// A switch's Target List Low/High: way `i`'s downstream port in byte
    /// `i`. `None` for an endpoint.
    pub targets: Option<[u8; MAX_TARGETS]>,
}

/// The children of switch `s`, in node order.
fn children(nodes: &[Node], s: usize) -> impl Iterator<Item = usize> + '_ {
    nodes
        .iter()
        .enumerate()
        .filter(move |(_, n)| n.parent.is_some_and(|(p, _)| p == s))
        .map(|(i, _)| i)
}

/// The device memory below node `i` at switch level `depth`: an
/// endpoint's pools, a switch's children's (down to [`MAX_DEPTH`]).
fn below(nodes: &[Node], i: usize, depth: usize) -> u64 {
    let node = &nodes[i];
    if !node.switch {
        return node.pools.iter().fold(0, |a, p| a.saturating_add(p.size));
    }
    if depth >= MAX_DEPTH {
        return 0;
    }
    children(nodes, i).fold(0, |a, c| a.saturating_add(below(nodes, c, depth + 1)))
}

/// The device memory below node `i` right below a root port: what its APCB
/// region sums.
pub fn total(nodes: &[Node], i: usize) -> u64 {
    below(nodes, i, 0)
}

/// The interleave sets of switch `s`'s children: identical endpoints
/// together with `interleave`, everything else alone, in node order.
fn sets(nodes: &[Node], s: usize, interleave: bool) -> Vec<Vec<usize, MAX_TARGETS>, MAX_NODES> {
    let mut out: Vec<Vec<usize, MAX_TARGETS>, MAX_NODES> = Vec::new();
    for c in children(nodes, s) {
        let node = &nodes[c];
        let joined = interleave
            && !node.switch
            && !node.pools.is_empty()
            && out.iter_mut().any(|set| {
                let first = &nodes[set[0]];
                !first.switch && first.pools == node.pools && set.push(c).is_ok()
            });
        if !joined {
            let mut set = Vec::new();
            let _ = set.push(c);
            if out.push(set).is_err() {
                break;
            }
        }
    }
    out
}

struct Planner<'a> {
    nodes: &'a [Node],
    interleave: bool,
    granularity: u32,
    end: u64,
    out: Vec<Assigned, MAX_ASSIGNED>,
}

impl Planner<'_> {
    fn push(&mut self, a: Assigned) {
        let _ = self.out.push(a);
    }

    /// Switch `s`'s ranges from `base` on, `above` ways above it; returns
    /// the end of the last.
    fn switch(&mut self, s: usize, mut base: u64, above: u32, depth: usize) -> u64 {
        if depth >= MAX_DEPTH {
            return base;
        }
        // A switch selects on the address bit after the ways above it.
        let switch_gran = self.granularity.saturating_mul(above);
        let mut decoder = 0u8;
        for set in sets(self.nodes, s, self.interleave) {
            let first = &self.nodes[set[0]];
            if first.switch {
                let span = below(self.nodes, set[0], depth + 1).saturating_mul(above.into());
                let Some(end) = base.checked_add(span).filter(|&e| e <= self.end) else {
                    return base;
                };
                if span != 0 {
                    self.push(Assigned {
                        node: s,
                        decoder,
                        base,
                        size: span,
                        ways: 1,
                        granularity: switch_gran,
                        targets: Some(targets(self.nodes, &set)),
                    });
                    decoder += 1;
                    self.switch(set[0], base, above, depth + 1);
                }
                base = end;
                continue;
            }
            let ways = set.len() as u32;
            for (n, pool) in first.pools.iter().enumerate() {
                let span = pool
                    .size
                    .saturating_mul(ways.into())
                    .saturating_mul(above.into());
                if span == 0 {
                    continue;
                }
                let Some(end) = base.checked_add(span).filter(|&e| e <= self.end) else {
                    return base;
                };
                self.push(Assigned {
                    node: s,
                    decoder,
                    base,
                    size: span,
                    ways: ways as u8,
                    granularity: switch_gran,
                    targets: Some(targets(self.nodes, &set)),
                });
                decoder += 1;
                for &member in &set {
                    self.push(Assigned {
                        node: member,
                        decoder: n as u8,
                        base,
                        size: span,
                        ways: (ways * above) as u8,
                        granularity: self.granularity,
                        targets: None,
                    });
                }
                base = end;
            }
        }
        base
    }
}

/// The downstream ports of `set`, way order.
fn targets(nodes: &[Node], set: &[usize]) -> [u8; MAX_TARGETS] {
    let mut out = [0u8; MAX_TARGETS];
    for (way, &i) in set.iter().enumerate().take(MAX_TARGETS) {
        out[way] = nodes[i].parent.map_or(0, |(_, port)| port);
    }
    out
}

/// The decoders of the hierarchy below switch `top` (the node right below
/// the root port) in `window` ([module documentation](self)), switches'
/// and endpoints' in planning order. Ranges that would leave the window
/// are not assigned.
pub fn plan(
    nodes: &[Node],
    top: usize,
    window: Window,
    interleave: bool,
) -> Vec<Assigned, MAX_ASSIGNED> {
    let mut planner = Planner {
        nodes,
        interleave,
        granularity: window.granularity,
        end: window.base.saturating_add(window.size),
        out: Vec::new(),
    };
    if nodes.get(top).is_some_and(|n| n.switch) {
        planner.switch(top, window.base, window.ways.max(1).into(), 0);
    }
    planner.out
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use crate::cxl::apcb::PoolType;

    const G: u64 = 1 << 30;

    fn pools(sizes: &[u64]) -> Vec<Pool, MAX_POOLS> {
        sizes
            .iter()
            .map(|&size| Pool {
                size,
                kind: PoolType::Volatile,
            })
            .collect()
    }

    fn switch(parent: Option<(usize, u8)>) -> Node {
        Node {
            parent,
            switch: true,
            pools: Vec::new(),
        }
    }

    fn ep(parent: usize, port: u8, sizes: &[u64]) -> Node {
        Node {
            parent: Some((parent, port)),
            switch: false,
            pools: pools(sizes),
        }
    }

    fn window(ways: u8, size: u64) -> Window {
        Window {
            base: 0x400 * G,
            size,
            ways,
            granularity: 256,
        }
    }

    fn of(a: &[Assigned], node: usize) -> std::vec::Vec<(u8, u64, u64, u8, u32)> {
        a.iter()
            .filter(|a| a.node == node)
            .map(|a| (a.decoder, a.base, a.size, a.ways, a.granularity))
            .collect()
    }

    #[test]
    fn identical_endpoints_interleave_others_follow() {
        let nodes = [
            switch(None),
            ep(0, 4, &[16 * G]),
            ep(0, 5, &[16 * G]),
            ep(0, 6, &[8 * G]),
        ];
        assert_eq!(total(&nodes, 0), 40 * G);
        let a = plan(&nodes, 0, window(1, 40 * G), true);
        let base = 0x400 * G;
        assert_eq!(
            of(&a, 0),
            [(0, base, 32 * G, 2, 256), (1, base + 32 * G, 8 * G, 1, 256)]
        );
        assert_eq!(a[0].targets, Some([4, 5, 0, 0, 0, 0, 0, 0]));
        // Both members decode the whole range, two ways.
        assert_eq!(of(&a, 1), [(0, base, 32 * G, 2, 256)]);
        assert_eq!(of(&a, 2), [(0, base, 32 * G, 2, 256)]);
        assert_eq!(of(&a, 3), [(0, base + 32 * G, 8 * G, 1, 256)]);

        // Without interleave each endpoint follows the previous one.
        let a = plan(&nodes, 0, window(1, 40 * G), false);
        assert_eq!(
            of(&a, 2),
            [(0, base + 16 * G, 16 * G, 1, 256)],
            "after the first"
        );
        // A window too small for the last set: it gets no decoders.
        let a = plan(&nodes, 0, window(1, 32 * G), true);
        assert!(of(&a, 3).is_empty());
    }

    #[test]
    fn nested_switches_and_host_interleave() {
        // A host-interleaved window (two root ports, 2 ways at 256 B); below
        // this root port a switch with an endpoint and a nested switch of
        // two identical two-pool endpoints.
        let nodes = [
            switch(None),
            ep(0, 1, &[8 * G]),
            switch(Some((0, 2))),
            ep(2, 0x10, &[4 * G, 2 * G]),
            ep(2, 0x11, &[4 * G, 2 * G]),
        ];
        assert_eq!(total(&nodes, 2), 12 * G);
        assert_eq!(total(&nodes, 0), 20 * G);
        let base = 0x400 * G;
        let a = plan(&nodes, 0, window(2, 40 * G), true);
        // Top switch: the endpoint, then the nested switch one way.
        assert_eq!(
            of(&a, 0),
            [
                (0, base, 16 * G, 1, 512),
                (1, base + 16 * G, 24 * G, 1, 512)
            ]
        );
        assert_eq!(
            a.iter()
                .find(|a| a.node == 0 && a.decoder == 1)
                .unwrap()
                .targets
                .unwrap()[0],
            2
        );
        assert_eq!(of(&a, 1), [(0, base, 16 * G, 2, 256)]);
        // Nested switch: pool 0 then pool 1 of its set, two ways each, at
        // the bit after the host's.
        let nested = base + 16 * G;
        assert_eq!(
            of(&a, 2),
            [
                (0, nested, 16 * G, 2, 512),
                (1, nested + 16 * G, 8 * G, 2, 512)
            ]
        );
        // Each endpoint interleaves over all four ways.
        assert_eq!(
            of(&a, 4),
            [
                (0, nested, 16 * G, 4, 256),
                (1, nested + 16 * G, 8 * G, 4, 256)
            ]
        );
        assert_eq!(
            a.iter().find(|a| a.node == 2).unwrap().targets,
            Some([0x10, 0x11, 0, 0, 0, 0, 0, 0])
        );
    }

    #[test]
    fn depth_is_bounded() {
        let mut nodes: std::vec::Vec<Node> = std::vec![switch(None)];
        for level in 1..=MAX_DEPTH {
            nodes.push(switch(Some((level - 1, 0))));
        }
        nodes.push(ep(MAX_DEPTH, 0, &[G]));
        assert_eq!(total(&nodes, 0), 0);
        assert!(plan(&nodes, 0, window(1, 8 * G), true).is_empty());
        // One level less fits.
        let last = nodes.len() - 1;
        nodes[last].parent = Some((MAX_DEPTH - 1, 0));
        assert_eq!(total(&nodes, 0), G);
        let a = plan(&nodes, 0, window(1, 8 * G), true);
        assert_eq!(of(&a, last), [(0, 0x400 * G, G, 1, 256)]);
    }
}
