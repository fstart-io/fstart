//! What a root's plan must be before anything is written
//! ([`PciEcam::validate_plan`]), and what of its pools it takes
//! ([`PciEcam::commit_pools`]).
//!
//! The plan is where each BAR and window of a root goes, in its devices;
//! the pools it is checked against are the root's free ranges before
//! allocation, not what an allocator left of them.

use super::{
    BarType, PCI_HEADER_TYPE_BRIDGE, PciEcam, Pools, PrefetchWindow, ResourcePool, WINDOW_KINDS,
    WindowKind,
};

/// Where something on a bus is: I/O or memory, `[base, end)`.
type Span = (bool, u64, u64);

impl PciEcam {
    /// Whether the plan of root `root` decodes as meant, within `pools`,
    /// the root's free ranges before allocation:
    ///
    /// * every movable BAR placed, at its own alignment, a 32-bit one below
    ///   4 GiB, ports below 64 KiB;
    /// * every open window 1 MiB (4 KiB) granular, the memory window below
    ///   4 GiB, a prefetchable window only where the bridge has one and
    ///   below 4 GiB if it is 32-bit;
    /// * each in a window of its parent that forwards it: I/O through I/O,
    ///   non-prefetchable memory through memory, prefetchable through
    ///   either; on the root bus in a free range of a pool it may use
    ///   (below 4 GiB for a 32-bit BAR, a memory window or a 32-bit
    ///   prefetchable window), window padding included;
    /// * on each bus, BARs and windows apart: no window holds a sibling's
    ///   BAR or window, nor its own bridge's BAR.
    ///
    /// Fixed BARs are on the root bus and out of `pools`.
    pub(super) fn validate_plan(&self, root: usize, pools: &Pools) -> bool {
        for (dev, device) in self.devices.iter().enumerate() {
            if device.root != root {
                continue;
            }
            for bar in device.bars.iter().filter(|bar| !bar.fixed) {
                let limit = match bar.bar_type {
                    BarType::None => continue,
                    BarType::Io => 0x1_0000,
                    BarType::Memory32 => 1 << 32,
                    BarType::Memory64 => u64::MAX,
                };
                let Some(end) = bar.base.checked_add(bar.size) else {
                    return false;
                };
                let routed = match bar.bar_type {
                    BarType::Io => self.routed(pools, dev, WindowKind::Io, (bar.base, end)),
                    _ if bar.prefetchable => {
                        let wide = bar.bar_type == BarType::Memory64;
                        self.routed_prefetchable(pools, dev, wide, (bar.base, end))
                    }
                    BarType::Memory64 if device.parent.is_none() => {
                        self.routed_prefetchable(pools, dev, true, (bar.base, end))
                    }
                    _ => self.routed(pools, dev, WindowKind::Memory, (bar.base, end)),
                };
                if !bar.allocated
                    || !bar.size.is_power_of_two()
                    || !bar.base.is_multiple_of(bar.size)
                    || end > limit
                    || !routed
                {
                    return false;
                }
            }
            if device.header_type != PCI_HEADER_TYPE_BRIDGE {
                continue;
            }
            for kind in WINDOW_KINDS {
                let window = device.windows[kind as usize];
                if !window.placed {
                    continue;
                }
                let granule = kind.granularity();
                let Some(end) = window.base.checked_add(window.size) else {
                    return false;
                };
                let (limit, routed) = match kind {
                    WindowKind::Prefetchable => {
                        let wide = device.prefetch == PrefetchWindow::Bits64;
                        let limit = match device.prefetch {
                            PrefetchWindow::None => 0,
                            PrefetchWindow::Bits32 => 1 << 32,
                            PrefetchWindow::Bits64 => u64::MAX,
                        };
                        let span = (window.base, end);
                        (limit, self.routed_prefetchable(pools, dev, wide, span))
                    }
                    WindowKind::Memory => {
                        (1 << 32, self.routed(pools, dev, kind, (window.base, end)))
                    }
                    WindowKind::Io => (0x1_0000, self.routed(pools, dev, kind, (window.base, end))),
                };
                if window.size == 0
                    || !window.base.is_multiple_of(granule)
                    || !window.size.is_multiple_of(granule)
                    || end > limit
                    || !routed
                {
                    return false;
                }
            }
        }
        self.buses_apart(root)
    }

    /// Whether `span`, of `dev`, is in its parent's window `kind`, or on
    /// the root bus in a free range of the pool of that kind (below 4 GiB
    /// for memory).
    fn routed(&self, pools: &Pools, dev: usize, kind: WindowKind, (base, end): (u64, u64)) -> bool {
        match self.devices[dev].parent {
            None => match kind {
                WindowKind::Io => pools.2.is_free(base, end),
                _ => pools.0.is_free(base, end),
            },
            Some(parent) => {
                let window = self.devices[parent].windows[kind as usize];
                window.placed && window.base <= base && end <= window.base + window.size
            }
        }
    }

    /// Whether prefetchable `span` of `dev` is in its parent's memory or
    /// prefetchable window, or on the root bus in a free range below 4 GiB
    /// or, if `wide` (64-bit), above.
    fn routed_prefetchable(&self, pools: &Pools, dev: usize, wide: bool, span: (u64, u64)) -> bool {
        match self.devices[dev].parent {
            None => pools.0.is_free(span.0, span.1) || wide && pools.1.is_free(span.0, span.1),
            Some(_) => {
                self.routed(pools, dev, WindowKind::Memory, span)
                    || self.routed(pools, dev, WindowKind::Prefetchable, span)
            }
        }
    }

    /// What `dev` puts on its bus: its placed movable BARs and open windows.
    fn on_its_bus(&self, dev: usize) -> impl Iterator<Item = Span> + '_ {
        let device = &self.devices[dev];
        let bars = device
            .bars
            .iter()
            .filter(|bar| bar.bar_type != BarType::None && !bar.fixed && bar.allocated)
            .map(|bar| (bar.bar_type == BarType::Io, bar.base, bar.base + bar.size));
        let windows = WINDOW_KINDS
            .into_iter()
            .filter(move |_| device.header_type == PCI_HEADER_TYPE_BRIDGE)
            .map(move |kind| (kind, device.windows[kind as usize]))
            .filter(|(_, window)| window.placed)
            .map(|(kind, window)| {
                let end = window.base + window.size;
                (kind == WindowKind::Io, window.base, end)
            });
        bars.chain(windows)
    }

    /// Whether, on every bus of root `root`, what its functions put on it
    /// is apart.
    fn buses_apart(&self, root: usize) -> bool {
        let overlap = |a: Span, b: Span| a.0 == b.0 && a.1 < b.2 && b.1 < a.2;
        let on_root = |dev: usize| self.devices[dev].root == root;
        (0..self.devices.len())
            .filter(|&dev| on_root(dev))
            .all(|dev| {
                let spans = || self.on_its_bus(dev);
                let mine = spans()
                    .enumerate()
                    .all(|(n, a)| spans().skip(n + 1).all(|b| !overlap(a, b)));
                let theirs = (dev + 1..self.devices.len())
                    .filter(|&other| {
                        on_root(other) && self.devices[other].parent == self.devices[dev].parent
                    })
                    .all(|other| spans().all(|a| self.on_its_bus(other).all(|b| !overlap(a, b))));
                mine && theirs
            })
    }

    /// Take what is on root `root`'s bus from `pools`, its free ranges
    /// before allocation, a window whole: what is left is its pools.
    /// Bookkeeping only (nothing is allocated from them later): a pool
    /// whose free ranges would not fit [`ResourcePool`] is left empty.
    pub(super) fn commit_pools(&mut self, root: usize, pools: &Pools) {
        let mut free = *pools;
        for dev in (0..self.devices.len())
            .filter(|&dev| self.devices[dev].root == root && self.devices[dev].parent.is_none())
        {
            for (io, base, end) in self.on_its_bus(dev) {
                let reserve = |pool: &mut ResourcePool| {
                    let mut left = *pool;
                    *pool = match left.reserve_range(base, end - base) {
                        Ok(()) => left,
                        Err(_) => ResourcePool::empty(),
                    };
                };
                if io {
                    reserve(&mut free.2);
                } else {
                    reserve(&mut free.0);
                    reserve(&mut free.1);
                }
            }
        }
        let pools = &mut self.roots[root];
        (pools.mmio32, pools.mmio64, pools.io_pool) = free;
    }
}
