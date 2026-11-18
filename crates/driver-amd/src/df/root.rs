// SPDX-License-Identifier: MIT OR Apache-2.0
//! A PCI root bridge of the fabric (a die's root complex, an IOMS
//! quadrant) as a PCI root: what the DF routes to it and what that is for.
//! The allocator and the ACPI host bridge both take it from here
//! ([`planned`]), from the DF plan the silicon init programmed (SOC_DF
//! §3.4, NBIO_SMU §3.2), or from the plan of the routes another agent
//! programmed ([`routed`]).
use super::map::RootComplex;
use super::plan::{Pools, RootBridgePlan, Window as PlanWindow};
use fstart_pci::PciRole::{self, Forwarded, Own};
use fstart_pci::{PciRoot, PciRootInfo};

/// Where the legacy LPC/FCH I/O ends: forwarded like the rest, not for PCI
/// devices.
const LEGACY_IO: u64 = 0x1000;
/// The IOAPIC, HPET and FCH fixed MMIO and everything above.
const FIXED_MMIO: u64 = 0xfec0_0000;
/// The top 12 GiB of the 48-bit address space are the fabric's own and
/// cannot hold MMIO (coreboot `DF_RESERVED_TOP_12GB_MMIO_SIZE`).
const FABRIC_RESERVED: u64 = (1 << 48) - (12 << 30);

/// A root bridge of the fabric as a PCI root.
pub type Root = PciRoot;

/// The plan of a root complex whose routes another agent programmed
/// (Picasso's ABL), `routes` as the DF has them: one I/O window, up to two
/// MMIO windows below 4 GiB (abutting routes count as one; the one ending
/// highest is the main window) and one above, all for PCI devices but
/// `non_pci`, the NBIO's own MMIO. The windows below 4 GiB end at the fixed
/// MMIO, the one above below the fabric's reserved top.
///
/// `None` if the buses are outside the segment `pci` or the DF routes more
/// than that.
pub fn routed(
    routes: &RootComplex,
    index: u8,
    pci: &PciRootInfo,
    non_pci: PlanWindow,
) -> Option<RootBridgePlan> {
    let below_4g = |&(base, size): &(u64, u64)| base + size <= 1 << 32;
    // The routes below 4 GiB, abutting ones joined: the main window and a
    // second one at most.
    let mut low = heapless::Vec::<(u64, u64), 2>::new();
    let mut routes_below: heapless::Vec<(u64, u64), { super::map::MMIO_PAIRS }> =
        routes.mmio.iter().copied().filter(below_4g).collect();
    routes_below.sort_unstable();
    for (base, size) in routes_below {
        match low.last_mut() {
            Some(last) if last.0 + last.1 == base => last.1 += size,
            _ => low.push((base, size)).ok()?,
        }
    }
    let mut above_4g = routes.mmio.iter().filter(|route| !below_4g(route));
    let above = above_4g.next();
    if above_4g.next().is_some() {
        return None;
    }
    // The window ending highest is the main one, as on the planner's primary.
    low.sort_unstable_by_key(|&(base, size)| core::cmp::Reverse(base + size));
    let (below, second) = (low.first(), low.get(1));
    let window = |&(base, size): &(u64, u64), limit: u64| {
        PlanWindow::new(base, (base + size).min(limit).saturating_sub(base))
    };
    let pools = |window: PlanWindow, non_pci: PlanWindow| Pools {
        prefetch: PlanWindow::new(window.base, 0),
        non_prefetch: window,
        non_pci,
    };
    let below = window(below?, FIXED_MMIO);
    let second_below = second.map(|second| window(second, FIXED_MMIO));
    let above = above.map(|route| window(route, FABRIC_RESERVED));
    let io = match routes.io.as_slice() {
        [] => None,
        [(base, size)] => Some(PlanWindow::new(*base, *size)),
        _ => return None,
    };
    let bus = super::BusRange {
        base: *routes.buses.start(),
        limit: (*routes.buses.end()).min(pci.bus_end),
    };
    (bus.base <= bus.limit).then_some(RootBridgePlan {
        socket: 0,
        index,
        fabric_id: 0,
        primary: bus.base == 0,
        bus,
        below,
        below_pools: pools(below, non_pci),
        second_below,
        above,
        above_pools: above.map(|above| pools(above, PlanWindow::new(above.base, 0))),
        io,
        io_pool: io.map(|io| {
            let base = io.base.max(LEGACY_IO);
            PlanWindow::new(base, io.end().min(0x1_0000).saturating_sub(base))
        }),
    })
}

/// The span of the PCI pools (prefetch then non-prefetch, contiguous by
/// construction of the planner) of a window.
fn pci_span(pools: &Pools) -> PlanWindow {
    let base = pools.prefetch.base.min(pools.non_prefetch.base);
    let end = pools.prefetch.end().max(pools.non_prefetch.end());
    PlanWindow::new(base, end - base)
}

/// The root bridge `rb` of a DF plan in the segment `pci`: what the plan
/// routes to it and what of that is for PCI.
///
/// Its routes are forwarded: its I/O (the primary's legacy ports included)
/// and its MMIO below and above 4 GiB, the primary's with the FCH's fixed
/// decodes and a second low window. PCI devices get the plan's PCI pools
/// and the primary's second low window. The legacy VGA memory goes to the
/// root bridge of the legacy VGA display, once enumeration found it
/// ([`fstart_pci::PciEcam::forward_legacy_vga_memory`]).
/// The non-PCI pools hold the NBIO's own MMIO and are not forwarded, nor is
/// the ECAM window, which the primary owns.
pub fn planned(rb: &RootBridgePlan, pci: &PciRootInfo) -> Option<Root> {
    // The DF routes all of its 25-bit I/O space; the ports end at 64 Ki.
    let io = |role: PciRole, span: PlanWindow| {
        role.io(
            span.base,
            span.end().min(0x1_0000).saturating_sub(span.base),
        )
    };
    // Nothing above the fabric's reserved top is MMIO, whatever the plan
    // routes (the ZP split's last window ends at 2^48).
    let mmio = |role: PciRole, span: PlanWindow| {
        role.mmio(
            span.base,
            span.end().min(FABRIC_RESERVED).saturating_sub(span.base),
        )
    };
    let pools = |pools: &Pools| {
        [
            mmio(PciRole::Window, pci_span(pools)),
            mmio(Own, pools.non_pci),
        ]
    };
    // What only the primary has: the ECAM window.
    let primary = |size: u64| if rb.primary { size } else { 0 };
    PciRoot::new(
        PciRootInfo {
            bus_start: rb.bus.base,
            bus_end: rb.bus.limit,
            ..*pci
        },
        (rb.io.map(|route| io(Forwarded, route)).into_iter())
            .chain(rb.io_pool.map(|pool| io(PciRole::Window, pool)))
            .chain([mmio(Forwarded, rb.below)])
            .chain(pools(&rb.below_pools))
            // The primary's second window below 4 GiB, after its pools: PCI
            // devices get it whole.
            .chain(rb.second_below.map(|window| mmio(PciRole::Window, window)))
            .chain(rb.above.map(|route| mmio(Forwarded, route)))
            .chain(rb.above_pools.iter().flat_map(pools))
            .chain([Own.mmio(pci.ecam_base, primary(pci.ecam_size()))]),
    )
    .ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::df::BusRange;
    use crate::df::plan::{Pools, Window};
    use fstart_pci::PciWindowKind;

    /// Milan's segment: ECAM at 0xE000_0000, 256 buses.
    fn segment() -> PciRootInfo {
        PciRootInfo {
            segment: 0,
            ecam_base: 0xe000_0000,
            bus_start: 0,
            bus_end: 0xff,
        }
    }

    fn plan() -> RootBridgePlan {
        let below = Window::new(0xa000_0000, 0x1000_0000);
        RootBridgePlan {
            socket: 0,
            index: 3,
            fabric_id: 0x23,
            primary: true,
            bus: BusRange {
                base: 0,
                limit: 0x3f,
            },
            below,
            below_pools: Pools {
                prefetch: Window::new(0xa000_0000, 0x0c00_0000),
                non_prefetch: Window::new(0xac00_0000, 0x0300_0000),
                non_pci: Window::new(0xaf00_0000, 0x0100_0000),
            },
            second_below: None,
            above: Some(Window::new(0x100_0000_0000, 0x40_0000_0000)),
            above_pools: Some(Pools {
                non_pci: Window::new(0x100_0000_0000, 0x2020_0000),
                prefetch: Window::new(0x100_2020_0000, 0x3000_0000_0000 >> 8),
                non_prefetch: Window::new(0x100_2020_0000 + (0x3000_0000_0000 >> 8), 0x1000_0000),
            }),
            io: Some(Window::new(0, 0x4000)),
            io_pool: Some(Window::new(0x1000, 0x3000)),
        }
    }

    #[test]
    fn planned_root_forwards_the_df_routes_and_allocates_from_the_pci_pools() {
        use PciWindowKind::{Io, Mmio};
        let plan = plan();
        let root = planned(&plan, &segment()).unwrap();
        assert_eq!(root.info().ecam_base, 0xe000_0000);
        assert_eq!(root.buses(), 0..=0x3f);
        let spans = |ranges: &[fstart_pci::PciWindow]| -> Vec<_> {
            ranges.iter().map(|r| (r.kind, r.base, r.end())).collect()
        };
        // `_CRS`: the routes without the non-PCI pools.
        assert_eq!(
            spans(root.forwarded()),
            [
                (Io, 0, 0x4000),
                (Mmio, 0xa000_0000, 0xaf00_0000),
                (Mmio, 0x100_2020_0000, 0x140_0000_0000),
            ]
        );
        // The allocator: the PCI pools.
        assert_eq!(
            spans(root.allocatable()),
            [
                (Io, 0x1000, 0x4000),
                (Mmio, 0xa000_0000, 0xaf00_0000),
                // The test plan's pools stop short of the routed window.
                (Mmio, 0x100_2020_0000, 0x130_3020_0000),
            ]
        );
        // The motherboard device: the non-PCI pools and, on the primary,
        // the ECAM window of the segment.
        assert_eq!(
            spans(root.motherboard()),
            [
                (Mmio, 0xaf00_0000, 0xb000_0000),
                (Mmio, 0x100_0000_0000, 0x100_2020_0000),
                (Mmio, 0xe000_0000, 0xf000_0000),
            ]
        );
        // Another root bridge does not own the ECAM window. The last one
        // has the rest of the DF's I/O space routed to it: the ports end at
        // 64 Ki.
        let last = RootBridgePlan {
            primary: false,
            io: Some(Window::new(0xd000, 0x200_0000 - 0xd000)),
            io_pool: Some(Window::new(0xd000, 0x3000)),
            ..plan
        };
        let last = root_of(&last);
        assert_eq!(last.motherboard().len(), 2);
        assert!(last.io().eq(core::iter::once(0xd000..=0xffff)));
    }

    fn root_of(plan: &RootBridgePlan) -> PciRoot {
        planned(plan, &segment()).unwrap()
    }

    /// A single die (one root bridge, TOM at 2 GiB): the leftover below the
    /// ECAM is the primary's second low window, and BARs may go there once
    /// the pools above the ECAM are full.
    #[test]
    fn the_second_low_window_is_for_pci_devices() {
        use PciWindowKind::Mmio;
        let mut plan = plan();
        plan.second_below = Some(Window::new(0x8000_0000, 0x2000_0000));
        let root = planned(&plan, &segment()).unwrap();
        let allocatable: Vec<_> = (root.allocatable().iter())
            .filter(|r| r.kind == Mmio && r.base < 1 << 32)
            .map(|r| (r.base, r.end()))
            .collect();
        // It abuts the pools above it: one range up to the non-PCI pool.
        assert_eq!(allocatable, [(0x8000_0000, 0xaf00_0000)]);
    }

    /// Picasso: the ABL routes one window below the fixed MMIO, the ECAM
    /// and the SMU's MMIO inside it; the root is that window without them.
    #[test]
    fn routed_window_without_ecam_and_non_pci() {
        use crate::df::map::RootComplex;
        let routes = RootComplex {
            buses: 0..=255,
            mmio: [(0xd000_0000, 0x2ec0_0000)].into_iter().collect(),
            io: [(0, 0x1_0000)].into_iter().collect(),
        };
        let pci = PciRootInfo {
            segment: 0,
            ecam_base: 0xf800_0000,
            bus_start: 0,
            bus_end: 63,
        };
        let smu = Window::new(0xfd00_0000, 0x28_0000);
        let plan = routed(&routes, 0, &pci, smu).unwrap();
        assert!(plan.primary);
        let root = planned(&plan, &pci).unwrap();
        assert_eq!(root.buses(), 0..=63);
        let spans = |ranges: &[fstart_pci::PciWindow]| -> std::vec::Vec<_> {
            ranges.iter().map(|r| (r.kind, r.base, r.end())).collect()
        };
        use PciWindowKind::{Io, Mmio};
        assert!(root.io().eq(core::iter::once(0..=0xffff)));
        assert_eq!(
            root.mmio().collect::<std::vec::Vec<_>>(),
            [
                0xd000_0000..=0xf7ff_ffff,
                0xfc00_0000..=0xfcff_ffff,
                0xfd28_0000..=0xfebf_ffff
            ]
        );
        assert_eq!(
            spans(root.allocatable()),
            [
                (Io, 0x1000, 0x1_0000),
                (Mmio, 0xd000_0000, 0xf800_0000),
                (Mmio, 0xfc00_0000, 0xfd00_0000),
                (Mmio, 0xfd28_0000, 0xfec0_0000),
            ]
        );
        assert_eq!(
            spans(root.motherboard()),
            [
                (Mmio, 0xfd00_0000, 0xfd28_0000),
                (Mmio, 0xf800_0000, 0xfc00_0000)
            ]
        );
        // The same window as two abutting routes is the same root.
        let mut split = routes.clone();
        split.mmio = [(0xd000_0000, 0x2800_0000), (0xf800_0000, 0x06c0_0000)]
            .into_iter()
            .collect();
        assert_eq!(routed(&split, 0, &pci, smu), Some(plan));
        // A second, disjoint window below 4 GiB is the plan's second one.
        let mut two = routes.clone();
        two.mmio.push((0x8000_0000, 0x1000_0000)).unwrap();
        let second = routed(&two, 0, &pci, smu).unwrap();
        assert_eq!(second.below, plan.below);
        assert_eq!(
            second.second_below,
            Some(Window::new(0x8000_0000, 0x1000_0000))
        );
        // Three are more than a plan holds.
        two.mmio.push((0x6000_0000, 0x1000_0000)).unwrap();
        assert_eq!(routed(&two, 0, &pci, smu), None);
    }
}
