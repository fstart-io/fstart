//! PCI root bridges from the DF plan (SOC_DF §3.4, NBIO_SMU §3.2).
//!
//! Every root bridge (ZP die / IOMS quadrant) is one PCI segment-0 root
//! with its own bus range and the windows the DF routes to it. The plan
//! splits each window into prefetchable, non-prefetchable and non-PCI
//! pools; PCI enumeration gets the first two (contiguous, prefetchable
//! below), the non-PCI pool feeds the fixed NBIO BARs (SMU, PSP, IOAPIC,
//! IOMMU) through [`NonPci`].

use fstart_driver_zen::df::plan::{Pools, RootBridgePlan, Window};
use fstart_pci::PciEcamConfig;

/// The span of the PCI pools (prefetch then non-prefetch, contiguous by
/// construction of the planner) of a window.
fn pci_span(pools: &Pools) -> Window {
    let base = pools.prefetch.base.min(pools.non_prefetch.base);
    let end = pools.prefetch.end().max(pools.non_prefetch.end());
    Window::new(base, end - base)
}

/// The allocator configuration of one root bridge. `ecam` is the whole
/// segment's ECAM `(base, size)`: config addresses are absolute.
pub fn root_config(rb: &RootBridgePlan, ecam: (u64, u64)) -> PciEcamConfig {
    let below = pci_span(&rb.below_pools);
    let above = rb.above_pools.map(|p| pci_span(&p));
    let io = rb.io_pool.unwrap_or(Window::new(0, 0));
    PciEcamConfig {
        ecam_base: ecam.0,
        ecam_size: ecam.1,
        mmio32_base: below.base,
        mmio32_size: below.size,
        mmio64_base: above.map_or(0, |w| w.base),
        mmio64_size: above.map_or(0, |w| w.size),
        pio_base: io.base,
        pio_size: io.size,
        bus_start: rb.bus.base,
        bus_end: rb.bus.limit,
    }
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
    use fstart_driver_zen::df::BusRange;

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
    fn root_config_uses_the_pci_pools_and_absolute_ecam() {
        let c = root_config(&plan(), (0xe000_0000, 0x1000_0000));
        assert_eq!((c.ecam_base, c.ecam_size), (0xe000_0000, 0x1000_0000));
        assert_eq!((c.mmio32_base, c.mmio32_size), (0xa000_0000, 0x0f00_0000));
        assert_eq!(c.mmio64_base, 0x100_2020_0000);
        assert_eq!((c.pio_base, c.pio_size), (0x1000, 0x3000));
        assert_eq!((c.bus_start, c.bus_end), (0, 0x3f));
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
