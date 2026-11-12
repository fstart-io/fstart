//! PCI root bridges from the DF plan (SOC_DF §3.4, NBIO_SMU §3.2).
//!
//! Every root bridge (ZP die / IOMS quadrant) is one PCI segment-0 root
//! with its own bus range and the windows the DF routes to it. The plan
//! splits each window into prefetchable, non-prefetchable and non-PCI
//! pools; PCI enumeration gets the first two (contiguous, prefetchable
//! below), the non-PCI pool feeds the fixed NBIO BARs (SMU, PSP, IOAPIC,
//! IOMMU) through [`NonPci`].

use fstart_core::description::PciRootInfo;
use fstart_driver_amd::sp3::df::plan::{Pools, RootBridgePlan, Window};
use fstart_driver_amd::sp3::soc::Generation;
use fstart_pci::{PciRole, PciRoot};

/// The span of the PCI pools (prefetch then non-prefetch, contiguous by
/// construction of the planner) of a window.
fn pci_span(pools: &Pools) -> Window {
    let base = pools.prefetch.base.min(pools.non_prefetch.base);
    let end = pools.prefetch.end().max(pools.non_prefetch.end());
    Window::new(base, end - base)
}

/// The PCI segment `generation`'s ECAM window decodes: what the allocator,
/// the MCFG, the DSDT and the payload all use.
pub fn segment(generation: Generation) -> PciRootInfo {
    let (ecam_base, buses) = generation.ecam();
    PciRootInfo {
        segment: 0,
        ecam_base,
        bus_start: 0,
        bus_end: (buses - 1) as u8,
    }
}

/// One root bridge in the segment `pci`, from what the DF routes to it.
///
/// Its routes are forwarded: its I/O (the primary's legacy ports included)
/// and its MMIO below and above 4 GiB, the primary's with the legacy VGA
/// memory, the FCH's fixed decodes and a second low window. PCI devices get the plan's PCI pools.
/// The non-PCI pools hold the NBIO's own MMIO and are not forwarded, nor is
/// the ECAM window, which the primary owns.
pub fn root(rb: &RootBridgePlan, pci: &PciRootInfo) -> Option<PciRoot> {
    use PciRole::{Forwarded, Own};
    // The DF routes all of its 25-bit I/O space; the ports end at 64 Ki.
    let io = |role: PciRole, span: Window| {
        role.io(
            span.base,
            span.end().min(0x1_0000).saturating_sub(span.base),
        )
    };
    let mmio = |role: PciRole, span: Window| role.mmio(span.base, span.size);
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

/// Bump allocator over a root bridge's non-PCI MMIO pool (below 4 GiB).
/// Allocations are naturally aligned (every NBIO BAR is aligned to its
/// size) and taken from the top down, keeping the pool base free for the
/// larger ones that follow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NonPci {
    base: u64,
    top: u64,
}

impl NonPci {
    pub const fn new(pool: Window) -> Self {
        Self {
            base: pool.base,
            top: pool.base + pool.size,
        }
    }

    /// The non-PCI pool below 4 GiB of `rb`.
    pub const fn of(rb: &RootBridgePlan) -> Self {
        Self::new(rb.below_pools.non_pci)
    }

    /// `size` bytes aligned to `size` (rounded up to a power of two).
    pub fn alloc(&mut self, size: u64) -> Option<u64> {
        let align = size.checked_next_power_of_two()?;
        let base = self.top.checked_sub(size)? & !(align - 1);
        (base >= self.base && size != 0).then(|| {
            self.top = base;
            base
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fstart_driver_amd::sp3::df::BusRange;
    use fstart_pci::PciWindowKind;

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
    fn root_forwards_the_df_routes_and_allocates_from_the_pci_pools() {
        use PciWindowKind::{Io, Mmio};
        let plan = plan();
        let root = root(&plan, &segment(Generation::Milan)).unwrap();
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
        assert!(last.io().eq([0xd000..=0xffff]));
    }

    fn root_of(plan: &RootBridgePlan) -> PciRoot {
        root(plan, &segment(Generation::Milan)).unwrap()
    }

    #[test]
    fn mcfg_per_generation() {
        let m = fstart_acpi::build_mcfg(&[segment(Generation::Naples)]);
        assert_eq!(&m[..4], b"MCFG");
        assert_eq!(
            u64::from_le_bytes(m[44..52].try_into().unwrap()),
            0xF000_0000
        );
        assert_eq!(m[55], 127);
        let m = fstart_acpi::build_mcfg(&[segment(Generation::Milan)]);
        assert_eq!(
            u64::from_le_bytes(m[44..52].try_into().unwrap()),
            0xE000_0000
        );
        assert_eq!(m[55], 255);
    }

    #[test]
    fn non_pci_bars_are_aligned_and_fit() {
        let mut pool = NonPci::of(&plan());
        let smu = pool.alloc(1 << 20).unwrap();
        let psp = pool.alloc(1 << 20).unwrap();
        let ioapic = pool.alloc(0x1000).unwrap();
        let iommu = pool.alloc(512 << 10).unwrap();
        assert_eq!(smu, 0xaff0_0000);
        assert_eq!(psp, 0xafe0_0000);
        assert_eq!(ioapic, 0xafdf_f000);
        assert_eq!(iommu, 0xafd0_0000, "aligned below the IOAPIC page");
        assert_eq!(NonPci::new(Window::new(0x1000, 0x1000)).alloc(0x2000), None);
        assert_eq!(pool.alloc(0), None);
    }
}
