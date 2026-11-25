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

/// Why the ABL's bus map does not resolve ([`resolve_roots`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteError {
    /// None, or more than [`MAX_ROOT_BRIDGES`].
    Count,
    /// A route on another PCI segment than 0.
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
            Self::Segment => "DFv4.5 PCI segment other than 0",
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
/// overlapping, all on PCI segment 0 (fstart's flow has one PCI segment and
/// one ECAM: a map the ABL spread over more segments is refused, not
/// half-used), and the FCH IOS of `(0, 0)` must have one: the primary.
pub fn resolve_roots(
    routes: &[df_wh::BusRoute],
    ios: &[df_wh::Ios],
    nodes: &[(u8, u8)],
) -> Result<Vec<RootBridge, MAX_ROOT_BRIDGES>, RouteError> {
    if routes.is_empty() || routes.len() > MAX_ROOT_BRIDGES {
        return Err(RouteError::Count);
    }
    let mut roots: Vec<RootBridge, MAX_ROOT_BRIDGES> = Vec::new();
    for route in routes {
        if route.segment != 0 {
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
                || (r.bus_base <= route.buses.limit && route.buses.base <= r.bus_limit)
        }) {
            return Err(RouteError::Overlap);
        }
        // At most `MAX_ROOT_BRIDGES` routes, checked above.
        let _ = roots.push(RootBridge {
            socket,
            index,
            fabric_id: route.destination,
            bus_base: route.buses.base,
            bus_limit: route.buses.limit,
            primary: (socket, index) == (0, rb_index(0, FCH_RB)),
        });
    }
    if !roots.iter().any(|rb| rb.primary) {
        return Err(RouteError::NoPrimary);
    }
    roots.sort_unstable_by_key(|rb| (rb.socket, rb.index));
    Ok(roots)
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
        let roots = resolve_roots(&routes_2p(), &b0_ios(), &nodes).unwrap();
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
            resolve_roots(&routes_2p(), &b0_ios(), &nodes[..2]),
            Err(RouteError::AbsentIod)
        );
        // A segment-1 route is refused.
        let mut seg = routes_2p();
        seg[0].segment = 1;
        assert_eq!(
            resolve_roots(&seg, &b0_ios(), &nodes),
            Err(RouteError::Segment)
        );
    }

    #[test]
    fn rejects_invalid_routes() {
        let ios = b0_ios();
        let base = route(0, 0x1f, 0x20);
        // Unknown IOS (IOS 3 on a 3-IOS IOD).
        assert_eq!(
            resolve_roots(&[base, route(0x20, 0x3f, 0x23)], &ios, &[(0, 0)]),
            Err(RouteError::UnknownIos)
        );
        // Absent die 1.
        assert_eq!(
            resolve_roots(&[base, route(0x20, 0x3f, 0x60)], &ios, &[(0, 0)]),
            Err(RouteError::AbsentIod)
        );
        // Overlapping buses.
        assert_eq!(
            resolve_roots(&[base, route(0x10, 0x2f, 0x21)], &ios, &[(0, 0)]),
            Err(RouteError::Overlap)
        );
        // No route to the FCH IOS of (0, 0), even at bus 0.
        assert_eq!(
            resolve_roots(&[route(0, 0x1f, 0x21)], &ios, &[(0, 0)]),
            Err(RouteError::NoPrimary)
        );
        // The FCH IOS is primary wherever its buses are.
        let roots = resolve_roots(
            &[route(0, 0x7f, 0x21), route(0x80, 0xff, 0x20)],
            &ios,
            &[(0, 0)],
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
        let roots = resolve_roots(&routes_2p(), &b0_ios(), &nodes).unwrap();
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
}
