//! What a root's plan must be before anything is written
//! ([`PciEcam::validate_plan`]), and the checks the window search shares
//! with it.
//!
//! The plan is where each BAR and window of a root goes, in its devices;
//! the pools it is checked against are the root's free ranges, which
//! allocation leaves as they were.

use super::{
    BarType, PCI_HEADER_TYPE_BRIDGE, PciEcam, Pools, PrefetchWindow, WINDOW_KINDS, WindowKind,
};

/// The first address a 32-bit BAR or window cannot reach. A root's pools
/// are split here ([`super::Root::of`]); no window crosses it.
pub(super) const FOUR_GIB: u64 = 1 << 32;

/// The end of the I/O space a bridge's 16-bit I/O window forwards.
const IO_END: u64 = 0x1_0000;

/// The first address window `kind` of a bridge whose prefetchable window
/// is `prefetch` cannot reach: 0 for one it does not have.
pub(super) const fn window_limit(kind: WindowKind, prefetch: PrefetchWindow) -> u64 {
    match (kind, prefetch) {
        (WindowKind::Io, _) => IO_END,
        (WindowKind::Memory, _) | (_, PrefetchWindow::Bits32) => FOUR_GIB,
        (_, PrefetchWindow::Bits64) => u64::MAX,
        (_, PrefetchWindow::None) => 0,
    }
}

/// Whether `[base, end)` is a window `kind` of such a bridge can be: not
/// empty, at its granularity, below its limit, and on one side of 4 GiB
/// (what a 32-bit BAR in it may use depends on the side).
pub(super) fn window_fits(kind: WindowKind, prefetch: PrefetchWindow, base: u64, end: u64) -> bool {
    let granule = kind.granularity();
    base < end
        && base.is_multiple_of(granule)
        && end.is_multiple_of(granule)
        && end <= window_limit(kind, prefetch)
        && (end <= FOUR_GIB || base >= FOUR_GIB)
}

/// Where something on a bus is: I/O or memory, `[base, end)`.
type Span = (bool, u64, u64);

impl PciEcam {
    /// Whether the plan of root `root` decodes as meant, within `pools`,
    /// the root's free ranges:
    ///
    /// * every movable BAR placed, at its own alignment, a 32-bit one below
    ///   4 GiB, ports below 64 KiB;
    /// * every open window one its bridge can have ([`window_fits`]);
    /// * each in a window of its parent that forwards it: I/O through I/O,
    ///   non-prefetchable memory through memory, prefetchable through
    ///   either, but a 32-bit BAR not through a prefetchable window above
    ///   4 GiB; on the root bus in a free range of a pool it may use
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
            for bar_idx in 0..self.bar_count(dev) {
                let bar = self.bar(dev, bar_idx);
                if bar.fixed {
                    continue;
                }
                let limit = match bar.bar_type {
                    BarType::None => continue,
                    BarType::Io => IO_END,
                    BarType::Memory32 => FOUR_GIB,
                    BarType::Memory64 => u64::MAX,
                };
                let Some(end) = bar.base.checked_add(bar.size) else {
                    return false;
                };
                let span = (bar.base, end);
                let routed = match bar.bar_type {
                    BarType::Io => self.routed(pools, dev, WindowKind::Io, span),
                    _ if bar.prefetchable => {
                        let wide = bar.bar_type == BarType::Memory64;
                        self.routed_prefetchable(pools, dev, wide, span)
                    }
                    BarType::Memory64 if device.parent.is_none() => {
                        self.routed_prefetchable(pools, dev, true, span)
                    }
                    _ => self.routed(pools, dev, WindowKind::Memory, span),
                };
                if !bar.allocated
                    || !bar.align().is_power_of_two()
                    || !bar.size.is_multiple_of(bar.align())
                    || !bar.base.is_multiple_of(bar.align())
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
                let Some(end) = window.base.checked_add(window.size) else {
                    return false;
                };
                let span = (window.base, end);
                let routed = match kind {
                    WindowKind::Prefetchable => {
                        let wide = device.prefetch == PrefetchWindow::Bits64;
                        self.routed_prefetchable(pools, dev, wide, span)
                    }
                    _ => self.routed(pools, dev, kind, span),
                };
                if !window_fits(kind, device.prefetch, window.base, end) || !routed {
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
                _ => end <= FOUR_GIB && pools.0.is_free(base, end),
            },
            Some(parent) => {
                let window = self.devices[parent].windows[kind as usize];
                window.placed && window.base <= base && end <= window.base + window.size
            }
        }
    }

    /// Whether prefetchable `span` of `dev` is in its parent's memory
    /// window, or its prefetchable window if `wide` (64-bit) or that is
    /// below 4 GiB; or on the root bus in a free range below 4 GiB or, if
    /// `wide`, above.
    fn routed_prefetchable(&self, pools: &Pools, dev: usize, wide: bool, span: (u64, u64)) -> bool {
        let pref = WindowKind::Prefetchable;
        match self.devices[dev].parent {
            None => {
                self.routed(pools, dev, WindowKind::Memory, span)
                    || wide && pools.1.is_free(span.0, span.1)
            }
            Some(parent) => {
                let window = self.devices[parent].windows[pref as usize];
                self.routed(pools, dev, WindowKind::Memory, span)
                    || (wide || window.base + window.size <= FOUR_GIB)
                        && self.routed(pools, dev, pref, span)
            }
        }
    }

    /// What `dev` puts on its bus: its placed movable BARs and open windows.
    fn on_its_bus(&self, dev: usize) -> impl Iterator<Item = Span> + '_ {
        let device = &self.devices[dev];
        let bars = (0..self.bar_count(dev)).filter_map(move |bar_idx| {
            let bar = self.bar(dev, bar_idx);
            (bar.bar_type != BarType::None && !bar.fixed && bar.allocated).then_some((
                bar.bar_type == BarType::Io,
                bar.base,
                bar.base + bar.size,
            ))
        });
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
}
