// SPDX-License-Identifier: MIT OR Apache-2.0
//! A PCI root bridge of the fabric (a die's root complex, an IOMS
//! quadrant) as a PCI root: what the DF routes to it and what that is for.
//! The allocator and the ACPI host bridge both take it from here: from the
//! DF plan the silicon init programmed ([`planned`], SOC_DF §3.4, NBIO_SMU
//! §3.2), or from the routes as the DF has them ([`root`]).
use super::map::RootComplex;
use super::plan::{Pools, RootBridgePlan, Window as PlanWindow};
use fstart_pci::PciRole::{self, Forwarded, Own, Window};
use fstart_pci::{PciRoot, PciRootInfo};

/// Where the legacy LPC/FCH I/O ends: forwarded like the rest, not for PCI
/// devices.
const LEGACY_IO: u64 = 0x1000;
/// The IOAPIC, HPET and FCH fixed MMIO and everything above.
const FIXED_MMIO: u64 = 0xfec0_0000;
/// The top 12 GiB of the 48-bit address space are the fabric's own and
/// cannot hold MMIO (coreboot `DF_RESERVED_TOP_12GB_MMIO_SIZE`).
const FABRIC_RESERVED: u64 = (1 << 48) - (12 << 30);

/// A die's root complex as a PCI root bridge.
pub type Root = PciRoot;

/// The root bridge of the root complex `routes`, with its buses in the ECAM
/// `ecam`.
///
/// What the DF routes to the die is forwarded: its I/O, the legacy ports
/// to the FCH's fixed devices and the rest to PCI, and its MMIO, below the
/// fixed MMIO and below the fabric's reserved top; on the die with bus 0
/// the legacy VGA memory too. Two ranges of that are the die's own and not
/// forwarded: `non_pci`, the pool of the NBIO's own MMIO, and, on the die
/// with bus 0, the ECAM window.
///
/// `None` if the buses are outside the ECAM or the DF routes more than a
/// root bridge holds.
pub fn root(
    routes: &RootComplex,
    (ecam_base, ecam_size): (u64, u64),
    (non_pci_base, non_pci_size): (u64, u64),
) -> Option<Root> {
    let last_bus = u8::try_from((ecam_size >> 20).checked_sub(1)?).ok()?;
    let buses = *routes.buses.start()..=(*routes.buses.end()).min(last_bus);
    if buses.is_empty() {
        return None;
    }
    let io = routes.io.iter().flat_map(|&(base, size)| {
        let end = (base + size).min(0x1_0000);
        let pci = base.max(LEGACY_IO);
        [
            Forwarded.io(base, end.saturating_sub(base)),
            Window.io(pci, end.saturating_sub(pci)),
        ]
    });
    // The windows below 4 GiB first: the allocator fills them in this order.
    let below = |&&(base, size): &&(u64, u64)| base + size <= 1 << 32;
    let mmio = |limit: u64| {
        move |&(base, size): &(u64, u64)| {
            Window.mmio(base, (base + size).min(limit).saturating_sub(base))
        }
    };
    let low = routes.mmio.iter().filter(below).map(mmio(FIXED_MMIO));
    let high = routes
        .mmio
        .iter()
        .filter(|route| !below(route))
        .map(mmio(FABRIC_RESERVED));
    // The die with bus 0 has the legacy VGA memory routed to it and owns
    // the ECAM window.
    let primary = |size: u64| if *buses.start() == 0 { size } else { 0 };
    let vga = [Forwarded.mmio(0xa_0000, primary(0x2_0000))];
    let own = [
        Own.mmio(non_pci_base, non_pci_size),
        Own.mmio(ecam_base, primary(ecam_size)),
    ];
    PciRoot::new(
        PciRootInfo {
            segment: 0,
            ecam_base,
            bus_start: *buses.start(),
            bus_end: *buses.end(),
        },
        io.chain(vga).chain(low).chain(high).chain(own),
    )
    .ok()
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
/// and its MMIO below and above 4 GiB, the primary's with the legacy VGA
/// memory, the FCH's fixed decodes and a second low window. PCI devices get
/// the plan's PCI pools.
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
    // What only the primary has: the legacy VGA memory routed to it, and
    // the ECAM window.
    let primary = |size: u64| if rb.primary { size } else { 0 };
    PciRoot::new(
        PciRootInfo {
            bus_start: rb.bus.base,
            bus_end: rb.bus.limit,
            ..*pci
        },
        (rb.io.map(|route| io(Forwarded, route)).into_iter())
            .chain(rb.io_pool.map(|pool| io(PciRole::Window, pool)))
            .chain([Forwarded.mmio(0xa_0000, primary(0x2_0000))])
            .chain([mmio(Forwarded, rb.below)])
            .chain(pools(&rb.below_pools))
            .chain(rb.second_below.map(|window| mmio(Forwarded, window)))
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
                (Mmio, 0xa_0000, 0xc_0000),
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
}
