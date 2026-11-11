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
use fstart_pci::{PciRoot, PciWindow};

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

/// The parts of `window` outside `keep`: below it and above it.
fn outside(window: Window, keep: Option<Window>) -> [Window; 2] {
    let Some(keep) = keep else {
        return [window, Window::new(0, 0)];
    };
    let below = keep.base.min(window.end()).saturating_sub(window.base);
    let above = window.end().saturating_sub(keep.end().max(window.base));
    [
        Window::new(window.base, below),
        Window::new(window.end() - above, above),
    ]
}

/// One root bridge in the segment `pci`. Its windows are what the DF routes
/// to it: its I/O (the primary's legacy range included) and its MMIO below
/// and above 4 GiB. PCI is allocated from the plan's PCI pools only; the
/// rest (legacy I/O, the non-PCI pools of the NBIO's own BARs, a second
/// low window) is reserved.
pub fn root(rb: &RootBridgePlan, pci: &PciRootInfo) -> Option<PciRoot> {
    let io = |span: Window| PciWindow::io(span.base, span.size);
    let mmio = |span: Window| PciWindow::mmio(span.base, span.size);
    let below_pci = pci_span(&rb.below_pools);
    let above_pci = rb.above_pools.map(|pools| pci_span(&pools));
    PciRoot::new(
        PciRootInfo {
            bus_start: rb.bus.base,
            bus_end: rb.bus.limit,
            ..*pci
        },
        rb.io
            .map(io)
            .into_iter()
            .chain([mmio(rb.below)])
            .chain(rb.second_below.map(mmio))
            .chain(rb.above.map(mmio)),
        rb.io
            .into_iter()
            .flat_map(|routed| outside(routed, rb.io_pool))
            .map(io)
            .chain(outside(rb.below, Some(below_pci)).map(mmio))
            .chain(rb.second_below.map(mmio))
            .chain(
                rb.above
                    .into_iter()
                    .flat_map(|routed| outside(routed, above_pci))
                    .map(mmio),
            ),
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
    fn root_decodes_the_df_routes_and_allocates_from_the_pci_pools() {
        let plan = plan();
        let root = root(&plan, &segment(Generation::Milan)).unwrap();
        assert_eq!(root.info().ecam_base, 0xe000_0000);
        assert_eq!(root.buses(), 0..=0x3f);
        // What ACPI reports: the routes.
        assert!(root.io().eq([0..=0x3fff]));
        assert!(root.mmio().eq([
            plan.below.base..=plan.below.end() - 1,
            0x100_0000_0000..=0x13f_ffff_ffff
        ]));
        // What the allocator stays out of: everything but the PCI pools.
        let reserved: Vec<_> = root
            .reserved()
            .iter()
            .map(|w| (w.kind, w.base, w.size))
            .collect();
        assert_eq!(
            reserved,
            [
                (PciWindowKind::Io, 0, 0x1000),
                (PciWindowKind::Mmio, 0xaf00_0000, 0x0100_0000),
                (PciWindowKind::Mmio, 0x100_0000_0000, 0x2020_0000),
                // The test plan's pools stop short of the routed window.
                (PciWindowKind::Mmio, 0x130_3020_0000, 0xf_cfe0_0000),
            ]
        );
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
