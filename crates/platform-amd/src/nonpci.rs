// SPDX-License-Identifier: MIT OR Apache-2.0
//! The non-PCI MMIO of a root bridge: the part of its window the DF plan
//! keeps out of the PCI allocator (`RootBridgePlan::below_pools.non_pci`)
//! for what the silicon init places itself, the NBIO's BARs (IOMMU,
//! IOAPIC, SMU, PSP, a secondary FCH) and CXL's RCRBs. One [`NonPci`] per
//! root bridge hands it out, so nothing placed there overlaps: SP3 takes
//! its BARs from the top, SP7 its NBIO BARs from the bottom and CXL then
//! from the top of what is left.

use fstart_driver_amd::df::plan::{RootBridgePlan, Window};

/// Bump allocator over a root bridge's non-PCI MMIO pool, from both ends:
/// [`NonPci::alloc`] from the top down, naturally aligned,
/// [`NonPci::alloc_bottom`] from the bottom up.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
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

    /// `size` bytes aligned to `size` (rounded up to a power of two), from
    /// the top down: the pool base stays free for larger ones.
    pub fn alloc(&mut self, size: u64) -> Option<u64> {
        let align = size.checked_next_power_of_two()?;
        let base = self.top.checked_sub(size)? & !(align - 1);
        (base >= self.base && size != 0).then(|| {
            self.top = base;
            base
        })
    }

    /// `size` bytes aligned to `align` (a power of two), from the bottom up.
    pub fn alloc_bottom(&mut self, size: u64, align: u64) -> Option<u64> {
        if size == 0 || !align.is_power_of_two() {
            return None;
        }
        let base = self.base.checked_add(align - 1)? & !(align - 1);
        let end = base.checked_add(size)?;
        (end <= self.top).then(|| {
            self.base = end;
            base
        })
    }

    /// What is still free.
    pub const fn free(&self) -> Window {
        Window {
            base: self.base,
            size: self.top - self.base,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_pci_bars_are_aligned_and_fit() {
        let mut pool = NonPci::new(Window::new(0xaf00_0000, 0x0100_0000));
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

    /// Both ends of one pool never meet: what the bottom took, the top
    /// does not get, and the other way round.
    #[test]
    fn both_ends_share_one_pool() {
        let mut pool = NonPci::new(Window::new(0xa000_0000, 0x20_0000));
        assert_eq!(pool.alloc_bottom(0x8_0000, 0x8_0000), Some(0xa000_0000));
        assert_eq!(pool.alloc_bottom(0x1000, 0x1_0000), Some(0xa008_0000));
        assert_eq!(pool.free(), Window::new(0xa008_1000, 0x17_f000));
        assert_eq!(pool.alloc(0x10_0000), Some(0xa010_0000));
        // 0xa008_1000..0xa010_0000 left: no 1 MiB, aligned or not.
        assert_eq!(pool.alloc(0x10_0000), None);
        assert_eq!(pool.alloc_bottom(0x10_0000, 0x1000), None);
        assert_eq!(pool.alloc_bottom(0x2000, 0x1000), Some(0xa008_1000));
        assert_eq!(pool.alloc_bottom(0x1000, 3), None);
    }
}
