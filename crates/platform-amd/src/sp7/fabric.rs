// SPDX-License-Identifier: MIT OR Apache-2.0
//! The SP7 fabric as the silicon init sees it: which DF nodes there are,
//! which root bridge each CfgAddressMap pair routes to, and what each root
//! bridge carries (FABRIC_2P §2.4, §2.12, §2.14).
//!
//! A root bridge is an IOS of an IOD. Its [`RootBridge::index`] is
//! `die * MAX_ROOT_BRIDGES_PER_IOD + rb`, `rb` its physical index on the IOD
//! (the IOS instance minus `Ios0`), which its NBIO's SMN addresses use; the
//! IOD has as many as its IOS ([`df_wh::Devices::ios`]: 3 on B0, 2 on A0).
//! Root bridges in system order are sorted by `(socket, index)`, the
//! vendor's GnbHandle order: the IOAPIC IDs, the VDM tables and the MMIO
//! pairs follow it.
//!
//! The primary root bridge is the FCH IOS (`Ios0`, physical root bridge 0)
//! of socket 0's die 0. Each other IOD's FCH IOS carries its secondary FCH.
//! The PSP BAR is on the primary only.

use fstart_driver_amd::{
    df::{RootBridge, plan, wh as df_wh},
    fch::qilian,
    nbio::wh as nbio_wh,
};
use heapless::Vec;

use crate::nonpci::NonPci;

/// Most root bridges: 2 sockets × 2 IODs × 4 IOS (SP7 B0 has 3 per IOD).
pub const MAX_ROOT_BRIDGES: usize = 16;
// The flow's PCI allocator and the DSDT take every one of them.
const _: () = assert!(MAX_ROOT_BRIDGES <= fstart_pci::MAX_PCI_ROOTS);
const _: () = assert!(MAX_ROOT_BRIDGES <= crate::acpi::dsdt::MAX_ROOT_BRIDGES);
/// Most DF nodes.
pub const MAX_NODES: usize = df_wh::MAX_NODES as usize;
/// The plan of the DFv4.5 fabric: 16 root bridges, 32 MMIO and 16 I/O pairs.
pub type Plan =
    plan::Plan<MAX_ROOT_BRIDGES, { df_wh::MMIO_PAIRS as usize }, { df_wh::IO_PAIRS as usize }>;
/// The physical root bridge of an IOD that carries its FCH and SMU (`Ios0`).
pub const FCH_RB: u8 = 0;

/// Alignment of an NBIO IOAPIC MMIO aperture within `below_pools.non_pci` (64 KiB).
pub const IOAPIC_ALIGN: u64 = 0x1_0000;
/// Size of an NBIO IOAPIC MMIO aperture (4 KiB).
pub const IOAPIC_SIZE: u64 = 0x1000;
/// Size and alignment of a WH IOMMU MMIO BAR (physical root bridges 0 and 1).
pub const IOMMU_BAR_SIZE: u64 = nbio_wh::IOMMU_BAR_SIZE;
/// Size and alignment of the PSP MMIO BAR within `below_pools.non_pci` (1 MiB).
pub const PSP_BAR_SIZE: u64 = 0x10_0000;

/// The host bridge of a root bridge's root bus, `segment:bus:00.0`: what
/// its SMN, NBIO and devices are reached through.
pub fn root_bus(segment: u16, bus: u8) -> fstart_pci::PciAddress {
    fstart_pci::PciAddress::new(segment, bus, 0, 0)
}

/// `RootBridge::index` of physical root bridge `rb` on `die`.
pub const fn rb_index(die: u8, rb: u8) -> u8 {
    die * nbio_wh::MAX_ROOT_BRIDGES_PER_IOD + rb
}

/// `(die, rb)` of a `RootBridge::index`.
pub const fn rb_location(index: u8) -> (u8, u8) {
    (
        index / nbio_wh::MAX_ROOT_BRIDGES_PER_IOD,
        index % nbio_wh::MAX_ROOT_BRIDGES_PER_IOD,
    )
}

/// The socket-wide physical root bridge (`die * per_iod + rb`), which the
/// vendor's per-socket RB tables (`_PXM`) index.
pub const fn socket_rb(index: u8, per_iod: u8) -> u8 {
    let (die, rb) = rb_location(index);
    die * per_iod + rb
}

/// The IOS of socket 0's IOD `die` when the DF does not name them
/// (`F0x44`/`F0x50` read back nothing): the fabric IDs fstart assumed
/// before the vendor's read was known, `die << 6 | 0x20 + rb`
/// (FABRIC_2P §4 open question 1).
pub fn assumed_ios(devices: &df_wh::Devices, die: u8) -> Vec<df_wh::Ios, 4> {
    (0..devices.ios.count.min(4))
        .map(|rb| df_wh::Ios {
            die,
            rb,
            fabric_id: df_wh::fabric_id_offset(0, die) | (0x20 + rb),
        })
        .collect()
}

/// Leave out of an IOD's `ios` the root bridges the ABL harvested: bit `i`
/// of `mask` (APOB `0xA/0x78`, [`fstart_driver_amd::apob::v2::ApobV2::iohc_harvest`])
/// set drops the `i`-th IOS (`WhIsInstanceHarvested` `0xf2c410`,
/// 2P_COMPLETE §2.16). The bus maps of the ABL already route none to it.
pub fn drop_harvested(ios: &mut Vec<df_wh::Ios, 4>, mask: u32) {
    ios.retain(|i| mask.checked_shr(u32::from(i.rb)).unwrap_or(0) & 1 == 0);
}

/// Why the ABL's bus map does not resolve ([`resolve_roots`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteError {
    /// None, or more than [`MAX_ROOT_BRIDGES`].
    Count,
    /// A route on a PCI segment the ECAM window does not decode.
    Segment,
    /// A route whose base is above its limit.
    BusRange,
    /// A route to a fabric ID no IOS has.
    UnknownIos,
    /// A route to an IOD that is not there.
    AbsentIod,
    /// Two routes to one root bridge or over the same buses.
    Overlap,
    /// No route to the FCH IOS of socket 0's die 0.
    NoPrimary,
}

impl RouteError {
    /// For the log (inline: SoC code keeps no strings of its own).
    #[inline]
    pub const fn why(self) -> &'static str {
        match self {
            Self::Count => "DFv4.5 root bridge count",
            Self::Segment => "DFv4.5 PCI segment outside the ECAM",
            Self::BusRange => "DFv4.5 invalid bus range",
            Self::UnknownIos => "DFv4.5 unknown fabric ID",
            Self::AbsentIod => "DFv4.5 route targets absent IOD",
            Self::Overlap => "DFv4.5 duplicate or overlapping bus route",
            Self::NoPrimary => "DFv4.5 no route to the FCH root bridge",
        }
    }
}

/// The root bridges the CfgAddressMap `routes` name, in system order.
///
/// Each route must land on a known IOS (`ios`, socket 0's, replicated for
/// every socket) of a live node (`nodes`), no two on one root bridge or
/// overlapping, on one of the `segments` PCI segments the ECAM window
/// decodes, and the FCH IOS of `(0, 0)` must have one: the primary (on
/// segment 0, where the DF and the FCH are).
pub fn resolve_roots(
    routes: &[df_wh::BusRoute],
    ios: &[df_wh::Ios],
    nodes: &[(u8, u8)],
    segments: u16,
) -> Result<Vec<RootBridge, MAX_ROOT_BRIDGES>, RouteError> {
    if routes.is_empty() || routes.len() > MAX_ROOT_BRIDGES {
        return Err(RouteError::Count);
    }
    let mut roots: Vec<RootBridge, MAX_ROOT_BRIDGES> = Vec::new();
    for route in routes {
        if u16::from(route.segment) >= segments {
            return Err(RouteError::Segment);
        }
        if route.buses.base > route.buses.limit {
            return Err(RouteError::BusRange);
        }
        let Some((socket, die, rb)) = df_wh::locate(ios, route.destination) else {
            return Err(RouteError::UnknownIos);
        };
        if !nodes.contains(&(socket, die)) {
            return Err(RouteError::AbsentIod);
        }
        let index = rb_index(die, rb);
        if roots.iter().any(|r| {
            (r.socket, r.index) == (socket, index)
                || r.fabric_id == route.destination
                || (r.segment == u16::from(route.segment)
                    && r.bus_base <= route.buses.limit
                    && route.buses.base <= r.bus_limit)
        }) {
            return Err(RouteError::Overlap);
        }
        // At most `MAX_ROOT_BRIDGES` routes, checked above.
        let _ = roots.push(RootBridge {
            socket,
            segment: route.segment.into(),
            index,
            fabric_id: route.destination,
            bus_base: route.buses.base,
            bus_limit: route.buses.limit,
            primary: (socket, index) == (0, rb_index(0, FCH_RB)),
        });
    }
    if !roots.iter().any(|rb| rb.primary && rb.segment == 0) {
        return Err(RouteError::NoPrimary);
    }
    roots.sort_unstable_by_key(|rb| (rb.socket, rb.index));
    Ok(roots)
}

/// The bus planner's view ([`df_wh::buses::plan`]) of every root bridge of
/// `nodes`: each IOS of `ios` (socket 0's, the same on every socket) on
/// every live node, in the vendor's order (socket, die, then the APOB's
/// `9/0x2C` IOS order, `order`, else physical), with its live buses from
/// `routes` and its request from `request`. Returns them and the index of
/// the FCH root bridge of `(0, 0)`.
pub fn bus_requests(
    routes: &[df_wh::BusRoute],
    ios: &[df_wh::Ios],
    nodes: &[(u8, u8)],
    per_iod: u8,
    order: Option<&[u8]>,
    request: Option<&super::BusRequest>,
) -> Option<(Vec<df_wh::buses::Request, MAX_ROOT_BRIDGES>, usize)> {
    let mut out: Vec<df_wh::buses::Request, MAX_ROOT_BRIDGES> = Vec::new();
    let rank = |rb: u8| {
        order
            .and_then(|order| order.iter().position(|&o| o == rb))
            .unwrap_or(usize::from(rb) + 0x100)
    };
    for &(socket, die) in nodes {
        let mut on_die: Vec<&df_wh::Ios, 4> = ios.iter().filter(|i| i.die == die).collect();
        on_die.sort_unstable_by_key(|i| rank(i.rb));
        for i in on_die {
            let fabric_id = u16::from(i.fabric_id) | u16::from(socket) << df_wh::SOCKET_SHIFT;
            let live = routes.iter().find(|r| r.destination == fabric_id).map(|r| {
                let segment = u16::from(r.segment) << 8;
                (
                    segment | u16::from(r.buses.base),
                    segment | u16::from(r.buses.limit),
                )
            });
            let target = request
                .and_then(|r| {
                    r.get(usize::from(socket))?
                        .get(usize::from(die * per_iod + i.rb))
                })
                .copied()
                .unwrap_or(0);
            if live.is_none() && target == 0 {
                continue;
            }
            out.push(df_wh::buses::Request {
                socket,
                die,
                rb: i.rb,
                fabric_id,
                live,
                target,
            })
            .ok()?;
        }
    }
    let fch = out
        .iter()
        .position(|r| (r.socket, r.die, r.rb) == (0, 0, FCH_RB))?;
    Some((out, fch))
}

/// For each root bridge `(socket, index)` of `roots`, the one (its position
/// in `roots`) its IOMMU also translates: on B0 and later the IOMMU of
/// physical root bridge `n` covers `n + 2` of the same IOD, its buses and
/// its IOAPIC (`NbioGetBusDecodeRange`, index 1 not on A0; Dell
/// AmdNbioDxe `0x60f0`, GIGABYTE `0xcb38`; 2P_COMPLETE §2.6). Only for
/// root bridges with an IOMMU ([`nbio_wh::IOMMU_ROOT_BRIDGES_PER_IOD`]).
pub fn iommu_also(roots: &[(u8, u8)], a0: bool) -> Vec<Option<usize>, MAX_ROOT_BRIDGES> {
    roots
        .iter()
        .map(|&(socket, index)| {
            let (die, rb) = rb_location(index);
            if a0 || rb >= nbio_wh::IOMMU_ROOT_BRIDGES_PER_IOD {
                return None;
            }
            let other = rb_index(die, rb + 2);
            roots.iter().position(|&key| key == (socket, other))
        })
        .collect()
}

/// Per-root-bridge non-PCI MMIO allocations carved strictly inside
/// `rb.below_pools.non_pci`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Bars {
    pub ioapic: Option<u64>,
    pub iommu: Option<u64>,
    pub secondary_fch: Option<u32>,
}

/// Carve the non-PCI MMIO of root bridge `index` on `socket` from the
/// bottom of its non-PCI pool `pool` ([`NonPci::of`] its plan), in this
/// order:
/// - IOMMU (physical root bridges 0 and 1 of each IOD): 512 KiB aligned
///   below 4 GiB.
/// - IOAPIC: 64 KiB aligned, 4 KiB window.
/// - PSP BAR (the primary, `(0, 0)` root bridge 0): 1 MiB aligned below 4 GiB.
/// - Secondary FCH ACPI MMIO (the FCH root bridge of every other IOD):
///   64 KiB aligned below 4 GiB.
///
/// What is left of `pool` is the CXL RCRBs' ([`super::cxl`]), from its top.
pub fn place_bars(socket: u8, index: u8, pool: &mut NonPci) -> Option<(Bars, Option<u32>)> {
    let (die, rb) = rb_location(index);
    let mut below_4g = |size: u64, align: u64| {
        let base = pool.alloc_bottom(size, align)?;
        (base + size <= 1 << 32).then_some(base)
    };

    let iommu = if rb < nbio_wh::IOMMU_ROOT_BRIDGES_PER_IOD {
        Some(below_4g(IOMMU_BAR_SIZE, IOMMU_BAR_SIZE)?)
    } else {
        None
    };

    let ioapic_base = below_4g(IOAPIC_SIZE, IOAPIC_ALIGN)?;

    let fch_rb = rb == FCH_RB;
    let psp_bar = if fch_rb && (socket, die) == (0, 0) {
        Some(below_4g(PSP_BAR_SIZE, PSP_BAR_SIZE)? as u32)
    } else {
        None
    };

    let secondary_fch = if fch_rb && (socket, die) != (0, 0) {
        let size = qilian::SECONDARY_ACPI_MMIO_SIZE;
        Some(below_4g(size, qilian::SECONDARY_ACPI_MMIO_ALIGN)? as u32)
    } else {
        None
    };

    Some((
        Bars {
            ioapic: Some(ioapic_base),
            iommu,
            secondary_fch,
        },
        psp_bar,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use fstart_driver_amd::df::BusRange;

    fn overlaps(a_base: u64, a_size: u64, w: plan::Window) -> bool {
        a_size > 0 && w.size > 0 && a_base < w.end() && w.base < a_base + a_size
    }

    /// Socket 0's IOS of a B0 part, fabric IDs as the static assumption.
    fn b0_ios() -> std::vec::Vec<df_wh::Ios> {
        (0..2)
            .flat_map(|die| assumed_ios(&df_wh::devices::B0, die))
            .collect()
    }

    fn route(base: u8, limit: u8, destination: u16) -> df_wh::BusRoute {
        df_wh::BusRoute {
            segment: 0,
            buses: BusRange { base, limit },
            destination,
        }
    }

    /// A 2P B0 bus map: 12 root bridges of 21 buses, FCH IOS of (0, 0) at 0.
    fn routes_2p() -> std::vec::Vec<df_wh::BusRoute> {
        let fids = [
            0x20u16, 0x21, 0x22, 0x60, 0x61, 0x62, 0xa0, 0xa1, 0xa2, 0xe0, 0xe1, 0xe2,
        ];
        // Deliberately not in fabric ID order.
        let mut routes: std::vec::Vec<_> = fids
            .iter()
            .enumerate()
            .map(|(i, &f)| route(i as u8 * 21, i as u8 * 21 + 20, f))
            .collect();
        routes.reverse();
        routes
    }

    #[test]
    fn resolves_2p_b0_roots_in_system_order() {
        let nodes = [(0, 0), (0, 1), (1, 0), (1, 1)];
        let roots = resolve_roots(&routes_2p(), &b0_ios(), &nodes, 1).unwrap();
        assert_eq!(roots.len(), 12);
        let keys: std::vec::Vec<_> = roots.iter().map(|r| (r.socket, r.index)).collect();
        assert_eq!(
            keys,
            [
                (0, 0),
                (0, 1),
                (0, 2),
                (0, 4),
                (0, 5),
                (0, 6),
                (1, 0),
                (1, 1),
                (1, 2),
                (1, 4),
                (1, 5),
                (1, 6)
            ]
        );
        assert!(roots[0].primary && roots.iter().filter(|r| r.primary).count() == 1);
        assert_eq!(socket_rb(6, 3), 5);
        assert_eq!(socket_rb(4, 2), 2);
        // Socket 1 absent: its routes have no node.
        assert_eq!(
            resolve_roots(&routes_2p(), &b0_ios(), &nodes[..2], 1),
            Err(RouteError::AbsentIod)
        );
        // A segment-1 route is refused in a one-segment ECAM, taken in a
        // two-segment one, where its buses may repeat segment 0's.
        let mut seg = routes_2p();
        seg[0].segment = 1;
        seg[0].buses = seg[11].buses;
        assert_eq!(
            resolve_roots(&seg, &b0_ios(), &nodes, 1),
            Err(RouteError::Segment)
        );
        let two = resolve_roots(&seg, &b0_ios(), &nodes, 2).unwrap();
        let last = two
            .iter()
            .find(|r| r.fabric_id == seg[0].destination)
            .unwrap();
        assert_eq!((last.segment, last.bus_base), (1, 0));
    }

    #[test]
    fn rejects_invalid_routes() {
        let ios = b0_ios();
        let base = route(0, 0x1f, 0x20);
        // Unknown IOS (IOS 3 on a 3-IOS IOD).
        assert_eq!(
            resolve_roots(&[base, route(0x20, 0x3f, 0x23)], &ios, &[(0, 0)], 1),
            Err(RouteError::UnknownIos)
        );
        // Absent die 1.
        assert_eq!(
            resolve_roots(&[base, route(0x20, 0x3f, 0x60)], &ios, &[(0, 0)], 1),
            Err(RouteError::AbsentIod)
        );
        // Overlapping buses.
        assert_eq!(
            resolve_roots(&[base, route(0x10, 0x2f, 0x21)], &ios, &[(0, 0)], 1),
            Err(RouteError::Overlap)
        );
        // No route to the FCH IOS of (0, 0), even at bus 0.
        assert_eq!(
            resolve_roots(&[route(0, 0x1f, 0x21)], &ios, &[(0, 0)], 1),
            Err(RouteError::NoPrimary)
        );
        // The FCH IOS is primary wherever its buses are.
        let roots = resolve_roots(
            &[route(0, 0x7f, 0x21), route(0x80, 0xff, 0x20)],
            &ios,
            &[(0, 0)],
            1,
        )
        .unwrap();
        assert_eq!(
            (roots[0].index, roots[0].primary, roots[0].bus_base),
            (0, true, 0x80)
        );
    }

    #[test]
    fn places_bars_per_iod_with_psp_and_secondary_fchs() {
        let nodes = [(0, 0), (0, 1), (1, 0), (1, 1)];
        let roots = resolve_roots(&routes_2p(), &b0_ios(), &nodes, 1).unwrap();
        let plan: Plan = plan::plan_for(
            &plan::Input {
                tom: 0x8000_0000,
                tom2: 0x100_0000_0000,
                ecam_base: 0xe000_0000,
                ecam_size: 256 << 20,
                phys_addr_bits: 52,
                root_bridges: &roots,
                mmio_pairs: df_wh::MMIO_PAIRS,
            },
            &plan::Policy::df3(),
        )
        .unwrap();
        assert_eq!(plan.root_bridges.len(), 12);
        assert!(plan.mmio.iter().all(|r| r.pair < df_wh::MMIO_PAIRS));
        assert_eq!(plan.io.len(), 12);

        let (mut psp, mut fchs, mut iommus) = (0, 0, 0);
        for rb in &plan.root_bridges {
            let pools = rb.below_pools;
            let mut pool = NonPci::of(rb);
            let (bars, psp_bar) = place_bars(rb.socket, rb.index, &mut pool).unwrap();
            // What is left starts above every BAR.
            let left = pool.free();
            assert!(left.base >= bars.ioapic.unwrap() + IOAPIC_SIZE);
            assert_eq!(left.end(), pools.non_pci.end());
            let ioapic = bars.ioapic.unwrap();
            assert_eq!(ioapic % IOAPIC_ALIGN, 0);
            assert!(ioapic >= pools.non_pci.base && ioapic + IOAPIC_SIZE <= pools.non_pci.end());
            assert!(!overlaps(ioapic, IOAPIC_SIZE, pools.prefetch));
            assert!(!overlaps(ioapic, IOAPIC_SIZE, pools.non_prefetch));
            let (die, phys) = rb_location(rb.index);
            assert_eq!(bars.iommu.is_some(), phys < 2);
            iommus += usize::from(bars.iommu.is_some());
            if let Some(bar) = psp_bar {
                assert_eq!((rb.socket, die, phys), (0, 0, 0));
                assert_eq!(u64::from(bar) % PSP_BAR_SIZE, 0);
                assert!(!overlaps(bar.into(), PSP_BAR_SIZE, pools.prefetch));
                psp += 1;
            }
            if let Some(fch) = bars.secondary_fch {
                assert_eq!(phys, 0);
                assert_ne!((rb.socket, die), (0, 0));
                assert!(u64::from(fch) + qilian::SECONDARY_ACPI_MMIO_SIZE <= pools.non_pci.end());
                fchs += 1;
            }
        }
        assert_eq!((psp, fchs, iommus), (1, 3, 8));
    }

    /// The planner's input: every IOS of every node in vendor order, live
    /// buses from the routes, requests by socket-wide root bridge; an
    /// APOB order puts IOS 2 before IOS 1.
    #[test]
    fn bus_requests_follow_the_nodes_and_the_apob_order() {
        let nodes = [(0, 0), (0, 1), (1, 0), (1, 1)];
        let mut request = [[0u16; 8]; 2];
        request[1][1] = 30;
        let (rbs, fch) = bus_requests(
            &routes_2p(),
            &b0_ios(),
            &nodes,
            3,
            Some(&[0, 2, 1]),
            Some(&request),
        )
        .unwrap();
        assert_eq!((rbs.len(), fch), (12, 0));
        let keys: std::vec::Vec<_> = rbs.iter().map(|r| (r.socket, r.die, r.rb)).collect();
        assert_eq!(&keys[..3], [(0, 0, 0), (0, 0, 2), (0, 0, 1)]);
        assert_eq!(rbs[1].fabric_id, 0x22);
        assert_eq!(rbs[1].live, Some((42, 62)));
        // Socket 1 die 0 rb 1 (socket-wide 1) asks for 30 buses.
        let s1 = rbs
            .iter()
            .find(|r| (r.socket, r.die, r.rb) == (1, 0, 1))
            .unwrap();
        assert_eq!((s1.fabric_id, s1.target), (0xa1, 30));
        // The plan gives it 30, everything stays in segment 0: the last
        // root bridge in the APOB order (S1D1 rb 1) gives way.
        let plan = df_wh::buses::plan(&rbs, fch, 256).unwrap().unwrap();
        let count = |key| {
            let a = plan
                .iter()
                .find(|a| (a.socket, a.die, a.rb) == key)
                .unwrap();
            a.buses.limit - a.buses.base + 1
        };
        assert_eq!((count((1, 0, 1)), count((1, 1, 1))), (30, 16));
        assert!(plan.iter().all(|a| a.segment == 0));
        // Its routes resolve to the same 12 root bridges.
        let routes: std::vec::Vec<_> = plan
            .iter()
            .map(|a| df_wh::BusRoute {
                segment: a.segment,
                buses: a.buses,
                destination: a.fabric_id,
            })
            .collect();
        assert_eq!(
            resolve_roots(&routes, &b0_ios(), &nodes, 1).unwrap().len(),
            12
        );
    }

    /// B0: root bridge 0's IOMMU also covers root bridge 2 of each IOD,
    /// root bridge 1's nothing more (B0 has no fourth); none on A0.
    #[test]
    fn iommus_cover_the_third_root_bridge_of_their_iod() {
        let nodes = [(0, 0), (0, 1), (1, 0), (1, 1)];
        let roots = resolve_roots(&routes_2p(), &b0_ios(), &nodes, 1).unwrap();
        let keys: std::vec::Vec<_> = roots.iter().map(|r| (r.socket, r.index)).collect();
        let also = iommu_also(&keys, false);
        for (i, &(socket, index)) in keys.iter().enumerate() {
            let (die, rb) = rb_location(index);
            let want =
                (rb == 0).then(|| keys.iter().position(|&k| k == (socket, rb_index(die, 2))));
            assert_eq!(also[i], want.flatten(), "{socket} {index}");
        }
        assert_eq!(also.iter().flatten().count(), 4);
        assert!(iommu_also(&keys, true).iter().all(Option::is_none));
        // 1P: the same per IOD.
        let one = iommu_also(&keys[..6], false);
        assert_eq!(one.as_slice(), [Some(2), None, None, Some(5), None, None]);
    }

    #[test]
    fn harvested_ios_are_dropped() {
        let mut ios = assumed_ios(&df_wh::devices::B0, 1);
        drop_harvested(&mut ios, 0b10);
        assert_eq!(
            ios.iter().map(|i| i.rb).collect::<std::vec::Vec<_>>(),
            [0, 2]
        );
        drop_harvested(&mut ios, 0);
        assert_eq!(ios.len(), 2);
    }
}
