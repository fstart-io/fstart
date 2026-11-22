//! Layouts the subtree allocator misses ([`PciEcam::allocate_root`]).
//!
//! The subtree allocator sizes each bridge's windows bottom up, places them
//! top down and moves 64-bit prefetchable BARs below 4 GiB one at a time.
//! Some layouts no single move reaches: one bridge with a 64-bit
//! prefetchable window, 13 MiB below 4 GiB and 8 MiB above, prefetchable
//! 64-bit BARs of 8, 4, 4 and 4 MiB and two 32 KiB BARs behind it. When it
//! finds none, the root is laid out again here.
//!
//! **What may go where.** I/O only through I/O windows. Non-prefetchable
//! memory behind a bridge only through its memory window, which is 32-bit.
//! Prefetchable memory through the prefetchable window or, forwarded as
//! non-prefetchable, through the memory window; never the reverse, and a
//! BAR keeps its attributes. No window crosses 4 GiB, and a 32-bit BAR is
//! never in a prefetchable window above it. On the root bus, a 32-bit BAR,
//! a memory window and a 32-bit prefetchable window come from the pool
//! below 4 GiB; a 64-bit BAR, prefetchable or not, and a 64-bit
//! prefetchable window from either pool. Windows are 1 MiB (memory) or
//! 4 KiB (I/O) granular, and what is on one bus does not overlap. The
//! checks a layout must pass are [`PciEcam::validate_plan`]'s, and the
//! search shares them ([`super::plan::window_fits`]).
//!
//! **One bus** ([`PciEcam::pack`]). With its bridges' windows chosen, the
//! BARs on a bus take two sets of free space: A and B (below and above
//! 4 GiB on the root bus; behind a bridge its memory and prefetchable
//! windows). Some may take only A, the others either. Largest first, then
//! those for A only, each takes the first aligned slot, in B if it may and
//! there is one, else in A. Every BAR is a power of two at its own
//! alignment: one in a slot takes the same number of aligned slots of each
//! smaller size wherever it goes, and a slot in A serves every BAR one in B
//! does. So this fits a bus's BARs whenever any placement does. It does not
//! hold for windows, which are not powers of two: they are chosen first.
//! I/O is one set. BARs of one size take slots in address order, so each
//! size walks the bus once: a pack is linear in what is on the bus for
//! each BAR size there.
//!
//! **One leaf bridge** ([`PciEcam::shortcut`]). Nothing else on the root bus
//! wants memory and nothing behind the bridge is a bridge with memory
//! behind it: its windows may as well be whole free ranges of the root,
//! rounded in to 1 MiB, the memory window's below 4 GiB. Every pair of
//! ranges (or a window closed) is tried: one range for both windows is the
//! memory window alone, which may forward everything. Exact; the windows
//! then shrink to what is in them.
//!
//! **Otherwise** ([`PciEcam::search`]) the windows are chosen bridge by
//! bridge, parents first: closed, or a 1 MiB (4 KiB) aligned range in a
//! free gap of where its parent may forward it. Once every window of a
//! bridge's subtree is chosen its bus is packed. A choice is first tried
//! where the subtree allocator would have put the window, then closed,
//! then every base and end in order. Pruned only where no layout can be:
//! a window smaller than the bytes that must go through it, or whose
//! subtree's BARs do not even pack straight into it; a window whose first
//! or last 1 MiB nothing behind it can reach, or that is larger than what
//! the search put in it (the smaller one is tried too); and, once a
//! subtree is laid out, other layouts of it (outside, only its windows
//! count).
//!
//! **Budget** ([`Budget`]). The search visits at most a number of candidate
//! windows per root, rejected ones too, and spends at most a number of
//! work units: every loop of it, packing and gap walks included, charges
//! one per BAR, window, gap, device or step it goes over (a sort of `n`,
//! `n log n`). It then says so ([`PciEcamError::SearchLimit`]) rather than
//! that nothing fits. No single charge exceeds [`MAX_CHARGE`]; so a root
//! costs at most `work + MAX_CHARGE` units, plus the subtree allocator's
//! sizing and [`PciEcam::validate_plan`], each once and quadratic in its
//! functions.

use core::cmp::Reverse;

use heapless::Vec as HVec;

use super::plan::{FOUR_GIB, window_fits, window_limit};
use super::{
    BarType, Layout, MAX_PCI_DEVICES, MAX_RESOURCE_RANGES, PCI_HEADER_TYPE_BRIDGE, PciEcam,
    PciEcamError, Pools, PrefetchWindow, ResourcePool, Window, WindowKind,
};

/// What the search may spend on one root.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Budget {
    /// Candidate windows, rejected ones too.
    pub(crate) visits: usize,
    /// Work units ([`Arena::charge`]).
    pub(crate) work: usize,
}

impl Budget {
    /// Firmware's: 16384 candidates and 2^24 work units. A unit is a few
    /// nanoseconds (about 5 on a desktop core): a search that cannot
    /// finish ends in well under a second of boot. The largest of the
    /// tests' generated roots that the search lays out takes 2^21.
    pub(crate) const FIRMWARE: Self = Self {
        visits: 1 << 14,
        work: 1 << 24,
    };

    #[cfg(test)]
    pub(crate) const UNLIMITED: Self = Self {
        visits: usize::MAX,
        work: usize::MAX,
    };

    /// Firmware's work budget, `visits` candidates.
    #[cfg(test)]
    pub(crate) const fn visits(visits: usize) -> Self {
        Self {
            visits,
            work: Self::FIRMWARE.work,
        }
    }
}

/// What one bus holds at most: six BARs of each function, plus six VF BARs
/// for each SR-IOV-capable function (kept sparse to bound stack usage).
const MAX_ITEMS: usize =
    super::PF_BAR_SLOTS * MAX_PCI_DEVICES + super::PF_BAR_SLOTS * super::MAX_SRIOV_DEVICES;
/// Two windows to choose and one bus to pack per bridge, and the root bus.
const MAX_STEPS: usize = 3 * MAX_PCI_DEVICES + 1;
const _: () = assert!(
    MAX_PCI_DEVICES <= 1 << 12,
    "an Item holds a device in 12 bits"
);

/// The most one charge adds: sorting what one bus holds.
pub(super) const MAX_CHARGE: usize = sort_cost(MAX_ITEMS + MAX_STEPS);

/// Work units to sort `n` items.
const fn sort_cost(n: usize) -> usize {
    n * (usize::BITS - n.leading_zeros()) as usize + 1
}

/// An address space: I/O, or memory (non-prefetchable and prefetchable).
/// Neither constrains the other; each is laid out on its own.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Space {
    Io,
    Memory,
}

impl Space {
    const fn of(kind: WindowKind) -> Self {
        match kind {
            WindowKind::Io => Self::Io,
            WindowKind::Memory | WindowKind::Prefetchable => Self::Memory,
        }
    }

    const fn kinds(self) -> &'static [WindowKind] {
        match self {
            Self::Io => &[WindowKind::Io],
            Self::Memory => &[WindowKind::Memory, WindowKind::Prefetchable],
        }
    }

    const fn holds(self, bar_type: BarType) -> bool {
        match self {
            Self::Io => matches!(bar_type, BarType::Io),
            Self::Memory => matches!(bar_type, BarType::Memory32 | BarType::Memory64),
        }
    }
}

/// A BAR (`index` below 12) or a window (`12 + kind`) of a device: what a
/// bus holds.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Item(u16);

impl Item {
    const fn bar(dev: usize, index: usize) -> Self {
        Self(((dev as u16) << 4) | index as u16)
    }

    const fn window(dev: usize, kind: WindowKind) -> Self {
        Self(((dev as u16) << 4) | 12 | kind as u16)
    }

    const fn dev(self) -> usize {
        (self.0 >> 4) as usize
    }

    const fn index(self) -> usize {
        (self.0 & 0xf) as usize
    }
}

/// Which BARs on a bus may take set B, and A if B has no room; the others
/// take A only.
#[derive(Clone, Copy)]
enum Rule {
    /// None: I/O, or behind a bridge without a prefetchable window.
    AOnly,
    /// The root bus, A below 4 GiB, B above: 64-bit BARs.
    Root,
    /// Behind a bridge whose prefetchable window B is above 4 GiB: 64-bit
    /// prefetchable BARs.
    High,
    /// Behind a bridge whose prefetchable window B is below 4 GiB:
    /// prefetchable BARs.
    Low,
    /// Every BAR of a root at once, straight into its pools: all but
    /// 32-bit BARs on the root bus and non-prefetchable BARs behind a
    /// bridge, which only the pool below 4 GiB reaches.
    Tree,
}

/// Bytes a bridge's windows need at least, from the BARs behind it, each
/// window rounded to its granularity.
#[derive(Clone, Copy, Default)]
struct Need {
    /// Its memory and prefetchable windows together.
    memory: u64,
    /// Its memory window: non-prefetchable memory.
    np: u64,
    /// Prefetchable memory behind it, unrounded: whether it has any.
    prefetchable: u64,
    io: u64,
}

/// Where a window choice resumes.
#[derive(Clone, Copy)]
enum Cursor {
    /// The next seed: where the subtree allocator lays the window out, up
    /// from the front of the first gap of range `.0` that holds it, or
    /// down to the end of the last (`.1`).
    Seed(u8, bool),
    Closed,
    /// Every base and end in range `.0`: from its start, or after the
    /// window's current candidate (`.1` false).
    Range(u8, bool),
}

#[derive(Clone, Copy)]
enum Step {
    /// Choose window `kind` of bridge `dev`; `last` of its windows.
    Decide {
        dev: u16,
        kind: WindowKind,
        last: bool,
        cursor: Cursor,
    },
    /// Pack the bus behind `dev`, the root bus for `None`: every window on
    /// it is chosen. `last` is the step of its last window.
    Complete { dev: Option<u16>, last: u16 },
}

/// Where a walk for slots of one size in a set is: range `range` from
/// `at`, the next of what is on the bus at `next` (by base).
#[derive(Default)]
struct Walk {
    range: usize,
    at: u64,
    next: usize,
    started: bool,
    /// Work units since it was last charged.
    steps: usize,
}

/// A root's scratch: no allocation, nothing of it in `PciEcam`.
struct Arena {
    /// What is on the bus being packed, by base.
    taken: HVec<Item, MAX_ITEMS>,
    /// What is to be placed on it.
    items: HVec<Item, MAX_ITEMS>,
    /// What one size placed, to merge into `taken`.
    fresh: HVec<Item, MAX_ITEMS>,
    /// The windows already chosen on the bus a window is chosen on, by base.
    siblings: HVec<Item, MAX_ITEMS>,
    steps: HVec<Step, MAX_STEPS>,
    need: [Need; MAX_PCI_DEVICES],
    visits: usize,
    work: usize,
    budget: Budget,
}

impl Arena {
    fn new(budget: Budget) -> Self {
        Self {
            taken: HVec::new(),
            items: HVec::new(),
            fresh: HVec::new(),
            siblings: HVec::new(),
            steps: HVec::new(),
            need: [Need::default(); MAX_PCI_DEVICES],
            visits: 0,
            work: 0,
            budget,
        }
    }

    /// One more candidate window, and the unit of work it is.
    fn visit(&mut self) -> Result<(), PciEcamError> {
        if self.visits >= self.budget.visits {
            return Err(PciEcamError::SearchLimit);
        }
        self.visits += 1;
        self.charge(1)
    }

    /// `units` of work, each a constant: a BAR, window, gap, device or
    /// step gone over.
    fn charge(&mut self, units: usize) -> Result<(), PciEcamError> {
        debug_assert!(units <= MAX_CHARGE);
        self.work = self.work.saturating_add(units);
        if self.work > self.budget.work {
            return Err(PciEcamError::SearchLimit);
        }
        Ok(())
    }
}

/// Up to [`MAX_RESOURCE_RANGES`] free ranges, `[base, end)`.
type Ranges = HVec<(u64, u64), MAX_RESOURCE_RANGES>;

fn ranges(pool: &ResourcePool) -> Ranges {
    pool.ranges[..pool.count]
        .iter()
        .map(|range| (range.base, range.end))
        .collect()
}

/// `range` either side of 4 GiB: a window never crosses it.
fn halves((start, end): (u64, u64)) -> impl Iterator<Item = (u64, u64)> {
    [(start, end.min(FOUR_GIB)), (start.max(FOUR_GIB), end)]
        .into_iter()
        .filter(|(start, end)| start < end)
}

impl PciEcam {
    /// Lay out root `root` in `pools`, its free ranges, when the subtree
    /// allocator found no layout: I/O, then memory, each the one bus, the
    /// leaf bridge or the search, within `budget`.
    pub(super) fn solve(
        &mut self,
        root: usize,
        pools: &Pools,
        bridges: &[usize],
        budget: Budget,
    ) -> Result<(), PciEcamError> {
        self.unplace(root);
        // The subtree allocator's window sizes are the search's seeds.
        if self.size_windows(root, bridges).is_err() {
            for device in self.devices.iter_mut().filter(|device| device.root == root) {
                device.windows = Default::default();
            }
        }
        let mut arena = Arena::new(budget);
        let result = self.solve_in(&mut arena, root, pools, bridges);
        #[cfg(test)]
        {
            self.spent = (arena.visits, arena.work);
        }
        result
    }

    fn solve_in(
        &mut self,
        arena: &mut Arena,
        root: usize,
        pools: &Pools,
        bridges: &[usize],
    ) -> Result<(), PciEcamError> {
        self.measure(arena, root, bridges)?;
        if self.hopeless(arena, pools, root)? {
            return Err(PciEcamError::ResourceExhausted);
        }
        self.search(arena, pools, root, bridges, Space::Io)?;
        match self.shortcut(arena, pools, root, bridges)? {
            Some(true) => Ok(()),
            Some(false) => Err(PciEcamError::ResourceExhausted),
            None => self.search(arena, pools, root, bridges, Space::Memory),
        }
    }

    /// Where `item` is, `[base, end)`, placed or not.
    fn span(&self, item: Item) -> (u64, u64) {
        if item.index() < super::TOTAL_BAR_SLOTS {
            let bar = self.bar(item.dev(), item.index());
            (bar.base, bar.base.saturating_add(bar.size))
        } else {
            let device = &self.devices[item.dev()];
            let window = device.windows[item.index() - super::TOTAL_BAR_SLOTS];
            (window.base, window.base.saturating_add(window.size))
        }
    }

    fn bar_size(&self, item: Item) -> u64 {
        self.bar(item.dev(), item.index()).size
    }

    fn bar_align(&self, item: Item) -> u64 {
        self.bar(item.dev(), item.index()).align()
    }

    /// Open window `kind` of `bridge` at `[base, end)`, or close it.
    fn set_window(&mut self, bridge: usize, kind: WindowKind, span: Option<(u64, u64)>) {
        let window = &mut self.devices[bridge].windows[kind as usize];
        window.placed = span.is_some();
        (window.base, window.size) = span.map_or((0, 0), |(base, end)| (base, end - base));
        window.layout = Layout::Span;
    }

    /// Window `kind` of `bridge`, `[base, end)`, if open.
    fn open(&self, bridge: usize, kind: WindowKind) -> Option<(u64, u64)> {
        let window = self.devices[bridge].windows[kind as usize];
        window
            .placed
            .then(|| (window.base, window.base + window.size))
    }

    /// The first address window `kind` of `bridge` may not reach.
    fn limit(&self, bridge: usize, kind: WindowKind) -> u64 {
        window_limit(kind, self.devices[bridge].prefetch)
    }

    fn is_bridge(&self, dev: usize) -> bool {
        self.devices[dev].header_type == PCI_HEADER_TYPE_BRIDGE
    }

    /// The devices on the bus behind `bus`, the root bus of `root` for
    /// `None`: a walk over every device, [`Self::devices`] units.
    fn on_bus(&self, root: usize, bus: Option<usize>) -> impl Iterator<Item = usize> + '_ {
        (0..self.devices.len())
            .filter(move |&dev| self.devices[dev].root == root && self.devices[dev].parent == bus)
    }

    /// The movable BARs of `dev` in `space`.
    fn bars_in(&self, dev: usize, space: Space) -> impl Iterator<Item = Item> + '_ {
        (0..self.bar_count(dev)).filter_map(move |index| {
            let bar = self.bar(dev, index);
            (space.holds(bar.bar_type) && !bar.fixed).then_some(Item::bar(dev, index))
        })
    }

    /// The open windows of `dev` in `space`.
    fn windows_in(&self, dev: usize, space: Space) -> impl Iterator<Item = Item> + '_ {
        space
            .kinds()
            .iter()
            .filter(move |&&kind| self.is_bridge(dev) && self.open(dev, kind).is_some())
            .map(move |&kind| Item::window(dev, kind))
    }

    fn flexible(&self, rule: Rule, item: Item) -> bool {
        let device = &self.devices[item.dev()];
        let bar = self.bar(item.dev(), item.index());
        let wide = bar.bar_type == BarType::Memory64;
        match rule {
            Rule::AOnly => false,
            Rule::Root => wide,
            Rule::High => wide && bar.prefetchable,
            Rule::Low => bar.prefetchable,
            Rule::Tree if device.parent.is_none() => wide,
            Rule::Tree => bar.prefetchable,
        }
    }

    /// Sort `items` by base: [`sort_cost`] units.
    fn sort_by_base(
        &self,
        arena: &mut Arena,
        which: fn(&mut Arena) -> &mut HVec<Item, MAX_ITEMS>,
    ) -> Result<(), PciEcamError> {
        let items = which(arena);
        let cost = sort_cost(items.len());
        items.sort_unstable_by_key(|&item| self.span(item).0);
        arena.charge(cost)
    }

    /// The free gaps of `range` around `taken` (sorted, disjoint), in order:
    /// at most `taken.len() + 1` units.
    fn gaps<'a>(
        &'a self,
        taken: &'a [Item],
        (start, end): (u64, u64),
    ) -> impl Iterator<Item = (u64, u64)> + 'a {
        let first = taken.partition_point(|&item| self.span(item).1 <= start);
        let mut rest = taken[first..].iter();
        let mut cursor = start;
        core::iter::from_fn(move || {
            loop {
                if cursor >= end {
                    return None;
                }
                let Some(&item) = rest.next() else {
                    let gap = (cursor, end);
                    cursor = end;
                    return Some(gap);
                };
                let (base, item_end) = self.span(item);
                let gap = (cursor, base.min(end));
                cursor = cursor.max(item_end);
                if gap.0 < gap.1 {
                    return Some(gap);
                }
            }
        })
    }

    /// The next slot of `size` aligned to `align` in `set` around `taken`
    /// (sorted, disjoint), after those `walk` returned before: first fit,
    /// as slots of one `(align, size)` are taken in address order.
    fn next_slot(
        &self,
        taken: &[Item],
        set: &[(u64, u64)],
        size: u64,
        align: u64,
        walk: &mut Walk,
    ) -> Option<u64> {
        loop {
            let &(start, end) = set.get(walk.range)?;
            if !walk.started {
                walk.started = true;
                walk.at = start;
                walk.next = taken.partition_point(|&item| self.span(item).1 <= start);
                walk.steps += (usize::BITS - taken.len().leading_zeros()) as usize;
            }
            walk.steps += 1;
            let (item_base, item_end) = taken
                .get(walk.next)
                .map_or((u64::MAX, u64::MAX), |&item| self.span(item));
            let gap_end = item_base.min(end);
            if let Some(slot) = walk.at.checked_next_multiple_of(align)
                && slot
                    .checked_add(size)
                    .is_some_and(|slot_end| slot_end <= gap_end)
            {
                walk.at = slot + size;
                return Some(slot);
            }
            if item_base < end {
                walk.at = walk.at.max(item_end);
                walk.next += 1;
            } else {
                walk.range += 1;
                walk.started = false;
            }
        }
    }

    /// Place BAR `item` at `base`, one of `arena.fresh`.
    fn place(&mut self, arena: &mut Arena, item: Item, base: u64) {
        let bar = self.bar_mut(item.dev(), item.index());
        bar.base = base;
        bar.allocated = true;
        // As many as `items`.
        let _ = arena.fresh.push(item);
    }

    /// Merge `arena.fresh` into `arena.taken`, both by base, from the end:
    /// as many units as it moves.
    fn merge(&self, arena: &mut Arena) -> Result<(), PciEcamError> {
        self.sort_by_base(arena, |arena| &mut arena.fresh)?;
        let (old, fresh) = (arena.taken.len(), arena.fresh.len());
        // As many as `items` and the windows, which `taken` holds.
        let _ = arena.taken.extend_from_slice(&arena.fresh);
        let (mut i, mut j, mut at) = (old, fresh, old + fresh);
        while j > 0 {
            at -= 1;
            let older = arena.taken[..i].last().copied();
            arena.taken[at] = match older {
                Some(item) if self.span(item).0 > self.span(arena.fresh[j - 1]).0 => {
                    i -= 1;
                    item
                }
                _ => {
                    j -= 1;
                    arena.fresh[j]
                }
            };
        }
        arena.charge(old + fresh - at + 1)
    }

    /// Place the BARs of `arena.items` around `arena.taken` (sorted by
    /// base, and kept so): those `rule` lets take B in `b` if there is an
    /// aligned slot, else in `a`; the others in `a`. The largest first,
    /// then those for A only, then by address and register. Whether all
    /// fit; exact (the module's documentation says why). Each size walks
    /// `a` and `b` once and merges what it placed into `taken`.
    ///
    /// With `low_first`, those that may take either take `a` first, and
    /// `b` what `a` has no room for: not exact, a preference to try first.
    fn pack(
        &mut self,
        arena: &mut Arena,
        a: &[(u64, u64)],
        b: &[(u64, u64)],
        rule: Rule,
        low_first: bool,
    ) -> Result<bool, PciEcamError> {
        arena.charge(sort_cost(arena.items.len()))?;
        arena.items.sort_unstable_by_key(|&item| {
            let device = &self.devices[item.dev()];
            let bar = self.bar(item.dev(), item.index());
            let flexible = self.flexible(rule, item);
            (
                Reverse(bar.align()),
                Reverse(bar.size),
                flexible,
                device.addr,
                bar.reg,
            )
        });
        let mut start = 0;
        while start < arena.items.len() {
            let align = self.bar_align(arena.items[start]);
            let size = self.bar_size(arena.items[start]);
            let end = start
                + arena.items[start..].partition_point(|&item| {
                    self.bar_align(item) == align && self.bar_size(item) == size
                });
            let flexible =
                start + arena.items[start..end].partition_point(|&item| !self.flexible(rule, item));
            arena.fresh.clear();
            // The set taken first: as far as it goes, those that may take
            // it; then the other, the rest.
            let (first, second, mut next) = match low_first {
                true => (a, b, start),
                false => (b, a, flexible),
            };
            let mut walk = Walk::default();
            while next < end {
                let slot = self.next_slot(&arena.taken, first, size, align, &mut walk);
                arena.charge(core::mem::take(&mut walk.steps))?;
                let Some(slot) = slot else {
                    break;
                };
                self.place(arena, arena.items[next], slot);
                next += 1;
            }
            // The rest take the other set: with `b` first, A's own then the
            // flexible ones `b` had no room for; with `a` first, the flexible
            // ones it had no room for (one of its own fits nowhere).
            let rest = match low_first {
                true if next < flexible => return Ok(false),
                true => (next..end).chain(end..end),
                false => (start..flexible).chain(next..end),
            };
            let mut walk = Walk::default();
            for n in rest {
                let slot = self.next_slot(&arena.taken, second, size, align, &mut walk);
                arena.charge(core::mem::take(&mut walk.steps))?;
                let Some(slot) = slot else {
                    return Ok(false);
                };
                self.place(arena, arena.items[n], slot);
            }
            self.merge(arena)?;
            start = end;
        }
        Ok(true)
    }

    /// The sets the BARs on the bus behind `bus` (the root bus for `None`)
    /// take in `space`, and which may take B.
    fn sets(&self, pools: &Pools, bus: Option<usize>, space: Space) -> (Ranges, Ranges, Rule) {
        match (bus, space) {
            (None, Space::Io) => (ranges(&pools.2), Ranges::new(), Rule::AOnly),
            (None, Space::Memory) => (ranges(&pools.0), ranges(&pools.1), Rule::Root),
            (Some(bridge), space) => self.window_sets(bridge, space),
        }
    }

    /// The sets of the bus behind `bridge`: its windows.
    fn window_sets(&self, bridge: usize, space: Space) -> (Ranges, Ranges, Rule) {
        let window = |kind| self.open(bridge, kind).into_iter().collect::<Ranges>();
        if space == Space::Io {
            return (window(WindowKind::Io), Ranges::new(), Rule::AOnly);
        }
        let b = window(WindowKind::Prefetchable);
        // No window crosses 4 GiB: one that ends above is above.
        let rule = match b.first() {
            None => Rule::AOnly,
            Some(&(_, end)) if end > FOUR_GIB => Rule::High,
            Some(_) => Rule::Low,
        };
        (window(WindowKind::Memory), b, rule)
    }

    /// Pack the bus behind `bus` (the root bus of `root` for `None`) in
    /// `space`, around its bridges' windows. Leaves what is on it in
    /// `arena.taken`. Whether its BARs fit.
    ///
    /// The root bus's 64-bit BARs go below 4 GiB first, as the subtree
    /// allocator's do: firmware drivers map that, and some devices that
    /// declare a 64-bit BAR do not decode above it. Where that leaves a BAR
    /// without a slot, the exact pack decides.
    fn pack_bus(
        &mut self,
        arena: &mut Arena,
        pools: &Pools,
        root: usize,
        bus: Option<usize>,
        space: Space,
    ) -> Result<bool, PciEcamError> {
        let low_first: &[bool] = match (bus, space) {
            (None, Space::Memory) => &[true, false],
            _ => &[false],
        };
        for &low_first in low_first {
            arena.charge(self.devices.len())?;
            arena.items.clear();
            arena.taken.clear();
            for dev in self.on_bus(root, bus) {
                arena.items.extend(self.bars_in(dev, space));
                arena.taken.extend(self.windows_in(dev, space));
            }
            for &item in &arena.items {
                self.bar_mut(item.dev(), item.index()).allocated = false;
            }
            self.sort_by_base(arena, |arena| &mut arena.taken)?;
            let (a, b, rule) = self.sets(pools, bus, space);
            if self.pack(arena, &a, &b, rule, low_first)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// What each bridge of `root` needs at least ([`Need`]), deepest first.
    fn measure(
        &self,
        arena: &mut Arena,
        root: usize,
        bridges: &[usize],
    ) -> Result<(), PciEcamError> {
        let round = |bytes: u64, kind: WindowKind| {
            bytes
                .checked_next_multiple_of(kind.granularity())
                .unwrap_or(u64::MAX)
        };
        for &bridge in bridges.iter().rev() {
            if self.devices[bridge].root != root {
                continue;
            }
            arena.charge(self.devices.len())?;
            let mut need = Need::default();
            for dev in self.children(bridge) {
                for bar_idx in 0..self.bar_count(dev) {
                    let bar = self.bar(dev, bar_idx);
                    if bar.fixed {
                        continue;
                    }
                    match bar.bar_type {
                        BarType::Io => need.io = need.io.saturating_add(bar.size),
                        BarType::None => {}
                        _ => {
                            need.memory = need.memory.saturating_add(bar.size);
                            if bar.prefetchable {
                                need.prefetchable = need.prefetchable.saturating_add(bar.size);
                            } else {
                                need.np = need.np.saturating_add(bar.size);
                            }
                        }
                    }
                }
                if self.is_bridge(dev) {
                    let child = arena.need[dev];
                    need.memory = need.memory.saturating_add(child.memory);
                    need.np = need.np.saturating_add(child.np);
                    need.prefetchable = need.prefetchable.saturating_add(child.prefetchable);
                    need.io = need.io.saturating_add(child.io);
                }
            }
            arena.need[bridge] = Need {
                memory: round(need.memory, WindowKind::Memory),
                np: round(need.np, WindowKind::Memory),
                prefetchable: need.prefetchable,
                io: round(need.io, WindowKind::Io),
            };
        }
        Ok(())
    }

    /// Whether root `root` cannot be laid out in `pools` whatever its
    /// windows: more bytes than the pools have, or its BARs do not even
    /// pack straight into them.
    fn hopeless(
        &mut self,
        arena: &mut Arena,
        pools: &Pools,
        root: usize,
    ) -> Result<bool, PciEcamError> {
        arena.charge(self.devices.len())?;
        let (mut low, mut memory, mut io) = (0u64, 0u64, 0u64);
        for dev in self.on_bus(root, None) {
            for bar_idx in 0..self.bar_count(dev) {
                let bar = self.bar(dev, bar_idx);
                if bar.fixed {
                    continue;
                }
                match bar.bar_type {
                    BarType::Io => io = io.saturating_add(bar.size),
                    BarType::Memory32 => low = low.saturating_add(bar.size),
                    _ => {}
                }
                if Space::Memory.holds(bar.bar_type) {
                    memory = memory.saturating_add(bar.size);
                }
            }
            if self.is_bridge(dev) {
                let need = arena.need[dev];
                low = low.saturating_add(need.np);
                memory = memory.saturating_add(need.memory);
                io = io.saturating_add(need.io);
            }
        }
        if low > pools.0.free_bytes()
            || memory > pools.0.free_bytes().saturating_add(pools.1.free_bytes())
            || io > pools.2.free_bytes()
        {
            return Ok(true);
        }
        for (space, a, b, rule) in [
            (Space::Io, ranges(&pools.2), Ranges::new(), Rule::AOnly),
            (
                Space::Memory,
                ranges(&pools.0),
                ranges(&pools.1),
                Rule::Tree,
            ),
        ] {
            arena.charge(self.devices.len())?;
            arena.items.clear();
            arena.taken.clear();
            for dev in (0..self.devices.len()).filter(|&dev| self.devices[dev].root == root) {
                arena.items.extend(self.bars_in(dev, space));
            }
            // Greedy packing is only an exact impossibility test when every
            // item's size equals its power-of-two alignment (standard PF BARs);
            // skip pruning when non-power-of-two SR-IOV VF apertures are present.
            if arena
                .items
                .iter()
                .all(|&item| self.bar_size(item) == self.bar_align(item))
                && !self.pack(arena, &a, &b, rule, false)?
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Lay out the memory behind root `root`'s one bridge with memory
    /// behind it, if nothing else on the root bus wants memory and no
    /// bridge behind it has memory behind it: whether it fits; `None` if
    /// it is not such a root.
    fn shortcut(
        &mut self,
        arena: &mut Arena,
        pools: &Pools,
        root: usize,
        bridges: &[usize],
    ) -> Result<Option<bool>, PciEcamError> {
        arena.charge(bridges.len() + self.devices.len())?;
        let mut leaf = None;
        for &bridge in bridges {
            if self.devices[bridge].root != root || arena.need[bridge].memory == 0 {
                continue;
            }
            if leaf.is_some() || self.devices[bridge].parent.is_some() {
                return Ok(None);
            }
            leaf = Some(bridge);
        }
        let Some(bridge) = leaf else {
            return Ok(None);
        };
        if self
            .on_bus(root, None)
            .any(|dev| self.bars_in(dev, Space::Memory).next().is_some())
        {
            return Ok(None);
        }
        let need = arena.need[bridge];
        let granule = WindowKind::Memory.granularity();
        let inward = |(base, end): (u64, u64)| {
            let base = base.checked_next_multiple_of(granule)?;
            let end = end & !(granule - 1);
            (base < end).then_some((base, end))
        };
        let pieces = |pool: &ResourcePool| -> HVec<(u64, u64), { 2 * MAX_RESOURCE_RANGES }> {
            ranges(pool)
                .into_iter()
                .flat_map(halves)
                .filter_map(inward)
                .collect()
        };
        let (low, high) = (pieces(&pools.0), pieces(&pools.1));
        let prefetch = self.devices[bridge].prefetch;
        let fits = move |kind, (base, end)| window_fits(kind, prefetch, base, end);
        let memory = (need.np == 0).then_some(None).into_iter().chain(
            low.iter()
                .copied()
                .filter(|&range| fits(WindowKind::Memory, range))
                .map(Some),
        );
        for memory in memory {
            let prefetchable = (need.prefetchable != 0)
                .then(|| low.iter().chain(&high).copied())
                .into_iter()
                .flatten()
                .filter(|&range| fits(WindowKind::Prefetchable, range))
                .map(Some);
            for prefetchable in [None].into_iter().chain(prefetchable) {
                if prefetchable.is_some() && prefetchable == memory
                    || memory.is_none() && prefetchable.is_none()
                {
                    continue;
                }
                arena.visit()?;
                self.set_window(bridge, WindowKind::Memory, memory);
                self.set_window(bridge, WindowKind::Prefetchable, prefetchable);
                if self.pack_bus(arena, pools, root, Some(bridge), Space::Memory)? {
                    self.shrink(arena, root, bridges, Space::Memory)?;
                    return Ok(Some(true));
                }
            }
        }
        self.set_window(bridge, WindowKind::Memory, None);
        self.set_window(bridge, WindowKind::Prefetchable, None);
        Ok(Some(false))
    }

    /// Lay out `space` of root `root`: choose the windows of every bridge
    /// with something in `space` behind it, parents first, and pack each
    /// bus once every window on it is chosen ([`Step`]).
    fn search(
        &mut self,
        arena: &mut Arena,
        pools: &Pools,
        root: usize,
        bridges: &[usize],
        space: Space,
    ) -> Result<(), PciEcamError> {
        self.plan(arena, root, bridges, space)?;
        let mut i = 0;
        while i < arena.steps.len() {
            arena.charge(1)?;
            i = match arena.steps[i] {
                Step::Decide { .. } => {
                    if self.advance(arena, pools, i)? {
                        i + 1
                    } else {
                        self.back(arena, i)?
                    }
                }
                Step::Complete { dev, last } => {
                    let bus = dev.map(usize::from);
                    if !self.pack_bus(arena, pools, root, bus, space)? {
                        self.back(arena, i)?
                    } else if let Some(bridge) = bus
                        && !self.tight(arena, bridge, last.into())?
                    {
                        // Its windows' hulls are tried too: a smaller
                        // window never stands in another's way.
                        self.reset_after(arena, last.into())?;
                        last.into()
                    } else {
                        i + 1
                    }
                }
            };
        }
        self.shrink(arena, root, bridges, space)
    }

    /// The steps of laying out `space` of root `root`: for each bridge with
    /// something behind it in that space, parents first, choose its
    /// windows; once its subtree's are chosen, pack its bus. The root bus
    /// last. Every window of `space` starts closed.
    fn plan(
        &mut self,
        arena: &mut Arena,
        root: usize,
        bridges: &[usize],
        space: Space,
    ) -> Result<(), PciEcamError> {
        let full = |_| PciEcamError::ConfigError;
        arena.charge(2 * bridges.len() + 1)?;
        arena.steps.clear();
        let mut open: HVec<(usize, u16), MAX_PCI_DEVICES> = HVec::new();
        for &bridge in bridges {
            if self.devices[bridge].root != root {
                continue;
            }
            for &kind in space.kinds() {
                self.set_window(bridge, kind, None);
            }
            let need = arena.need[bridge];
            let kinds: &[WindowKind] = match space {
                Space::Io if need.io != 0 => &[WindowKind::Io],
                Space::Memory if need.memory == 0 => &[],
                Space::Memory
                    if need.prefetchable != 0
                        && self.devices[bridge].prefetch != PrefetchWindow::None =>
                {
                    &[WindowKind::Memory, WindowKind::Prefetchable]
                }
                Space::Memory => &[WindowKind::Memory],
                Space::Io => &[],
            };
            if kinds.is_empty() {
                continue;
            }
            let secondary = self.devices[bridge].secondary_bus;
            while let Some(&(top, last)) = open.last() {
                let device = &self.devices[top];
                if (device.secondary_bus..=device.subordinate_bus).contains(&secondary) {
                    break;
                }
                open.pop();
                let dev = Some(top as u16);
                arena
                    .steps
                    .push(Step::Complete { dev, last })
                    .map_err(full)?;
            }
            for (n, &kind) in kinds.iter().enumerate() {
                let step = Step::Decide {
                    dev: bridge as u16,
                    kind,
                    last: n + 1 == kinds.len(),
                    cursor: Cursor::Seed(0, false),
                };
                arena.steps.push(step).map_err(full)?;
            }
            let last = arena.steps.len() as u16 - 1;
            open.push((bridge, last))
                .map_err(|_| PciEcamError::ConfigError)?;
        }
        while let Some((top, last)) = open.pop() {
            let dev = Some(top as u16);
            arena
                .steps
                .push(Step::Complete { dev, last })
                .map_err(full)?;
        }
        let root_bus = Step::Complete { dev: None, last: 0 };
        arena.steps.push(root_bus).map_err(full)
    }

    /// The step to resume from when step `from` fails: the last window
    /// chosen before it, or, if that is inside a subtree laid out before
    /// it, that subtree's bridge's last window (outside it, only its
    /// windows count). Every later choice is undone.
    fn back(&mut self, arena: &mut Arena, from: usize) -> Result<usize, PciEcamError> {
        arena.charge(from + 1)?;
        let target = (0..from)
            .rev()
            .find_map(|step| match arena.steps[step] {
                Step::Decide { .. } => Some(step),
                Step::Complete { dev: Some(_), last } => Some(last.into()),
                Step::Complete { dev: None, .. } => None,
            })
            .ok_or(PciEcamError::ResourceExhausted)?;
        self.reset_after(arena, target)?;
        Ok(target)
    }

    /// Undo every window choice after step `target`.
    fn reset_after(&mut self, arena: &mut Arena, target: usize) -> Result<(), PciEcamError> {
        arena.charge(arena.steps.len() - target)?;
        for step in &mut arena.steps[target + 1..] {
            if let Step::Decide {
                dev, kind, cursor, ..
            } = step
            {
                *cursor = Cursor::Seed(0, false);
                let window = &mut self.devices[usize::from(*dev)].windows[*kind as usize];
                window.placed = false;
            }
        }
        Ok(())
    }

    /// The `index`th range window `kind` of `bridge` may go in: a free
    /// range of the root's pool, or a window of its parent, either side of
    /// 4 GiB.
    fn region(
        &self,
        pools: &Pools,
        bridge: usize,
        kind: WindowKind,
        index: usize,
    ) -> Option<(u64, u64)> {
        let device = &self.devices[bridge];
        let pool = |pool: &ResourcePool| ranges(pool).into_iter();
        let parent = |kinds: &'static [WindowKind]| {
            let parent = device.parent?;
            Some(
                kinds
                    .iter()
                    .filter_map(move |&kind| self.open(parent, kind)),
            )
        };
        let ranges: &mut dyn Iterator<Item = (u64, u64)> = match (device.parent, kind) {
            (None, WindowKind::Io) => &mut pool(&pools.2),
            (None, WindowKind::Prefetchable) if device.prefetch == PrefetchWindow::Bits64 => {
                &mut pool(&pools.0).chain(pool(&pools.1))
            }
            (None, _) => &mut pool(&pools.0),
            (Some(_), WindowKind::Prefetchable) => {
                &mut parent(&[WindowKind::Prefetchable, WindowKind::Memory])
                    .into_iter()
                    .flatten()
            }
            (Some(_), WindowKind::Memory) => {
                &mut parent(&[WindowKind::Memory]).into_iter().flatten()
            }
            (Some(_), WindowKind::Io) => &mut parent(&[WindowKind::Io]).into_iter().flatten(),
        };
        ranges.flat_map(halves).nth(index)
    }

    /// Fill `arena.siblings` with the windows chosen on the bus window
    /// `kind` of `bridge` is chosen on, by base.
    fn siblings(
        &self,
        arena: &mut Arena,
        bridge: usize,
        kind: WindowKind,
    ) -> Result<(), PciEcamError> {
        arena.charge(self.devices.len())?;
        let device = &self.devices[bridge];
        arena.siblings.clear();
        for dev in self.on_bus(device.root, device.parent) {
            arena.siblings.extend(
                self.windows_in(dev, Space::of(kind))
                    .filter(|&item| item != Item::window(bridge, kind)),
            );
        }
        self.sort_by_base(arena, |arena| &mut arena.siblings)
    }

    /// Choose the next candidate of step `i`, a window of a bridge. Whether
    /// there is one; if not, the window is closed and its step starts over.
    fn advance(
        &mut self,
        arena: &mut Arena,
        pools: &Pools,
        i: usize,
    ) -> Result<bool, PciEcamError> {
        let Step::Decide {
            dev,
            kind,
            last,
            mut cursor,
        } = arena.steps[i]
        else {
            return Ok(false);
        };
        let bridge = usize::from(dev);
        self.siblings(arena, bridge, kind)?;
        let found = loop {
            // A range of the region is at most 2 * 2 * 16 to find.
            arena.charge(4 * MAX_RESOURCE_RANGES)?;
            match cursor {
                Cursor::Seed(range, down) => {
                    cursor = match down {
                        false => Cursor::Seed(range, true),
                        true => Cursor::Seed(range + 1, false),
                    };
                    let Some(range) = self.region(pools, bridge, kind, range.into()) else {
                        cursor = Cursor::Closed;
                        continue;
                    };
                    if let Some(span) = self.seed(arena, bridge, kind, range, down)? {
                        arena.visit()?;
                        if self.try_window(arena, bridge, kind, last, Some(span))? {
                            break true;
                        }
                    }
                }
                Cursor::Closed => {
                    cursor = Cursor::Range(0, true);
                    arena.visit()?;
                    if self.try_window(arena, bridge, kind, last, None)? {
                        break true;
                    }
                }
                Cursor::Range(index, fresh) => {
                    let Some(range) = self.region(pools, bridge, kind, index.into()) else {
                        break false;
                    };
                    if self.next_in_range(arena, bridge, kind, last, range, fresh)? {
                        cursor = Cursor::Range(index, false);
                        break true;
                    }
                    cursor = Cursor::Range(index + 1, true);
                }
            }
        };
        if !found {
            cursor = Cursor::Seed(0, false);
            self.set_window(bridge, kind, None);
        }
        if let Step::Decide { cursor: step, .. } = &mut arena.steps[i] {
            *step = cursor;
        }
        Ok(found)
    }

    /// The subtree allocator's window `kind` of `bridge` in `range`: up
    /// from the front of its first gap that holds it, or down to the end of
    /// its last.
    fn seed(
        &self,
        arena: &mut Arena,
        bridge: usize,
        kind: WindowKind,
        range: (u64, u64),
        down: bool,
    ) -> Result<Option<(u64, u64)>, PciEcamError> {
        let window: Window = self.devices[bridge].windows[kind as usize];
        let size = if down {
            window.size_down
        } else {
            window.size_up
        };
        if size == 0 || window.align == 0 {
            return Ok(None);
        }
        arena.charge(arena.siblings.len() + 1)?;
        let fits = |(start, end): (u64, u64)| {
            if down {
                let end = end & !(window.align - 1);
                let base = end.checked_sub(size)?;
                (base >= start && base < end).then_some((base, end))
            } else {
                let base = start.checked_next_multiple_of(window.align)?;
                let end = base.checked_add(size).filter(|&span| span <= end)?;
                Some((base, end))
            }
        };
        let mut gaps = self.gaps(&arena.siblings, range);
        Ok(if down {
            gaps.filter_map(fits).last()
        } else {
            gaps.find_map(fits)
        })
    }

    /// The next base and end of window `kind` of `bridge` in `range`, in
    /// order: from its start when `fresh`, else after its current one.
    /// Only those a layout could make: base and end where something behind
    /// the bridge can start and end ([`Self::touch`]), and ends from the
    /// smallest that passes [`Self::try_window`], which passes for every
    /// larger one too: `range` does not cross 4 GiB. Whether there is one;
    /// it is chosen.
    fn next_in_range(
        &mut self,
        arena: &mut Arena,
        bridge: usize,
        kind: WindowKind,
        last: bool,
        range: (u64, u64),
        fresh: bool,
    ) -> Result<bool, PciEcamError> {
        let granule = kind.granularity();
        let step = self.touch(arena, bridge, kind)?;
        let limit = self.limit(bridge, kind);
        let gap_end = |pci: &Self, arena: &mut Arena, at: u64| {
            arena.charge(arena.siblings.len() + 1)?;
            Ok(pci
                .gaps(&arena.siblings, range)
                .find(|&(start, end)| start <= at && at < end)
                .map_or(0, |(_, end)| end.min(limit)))
        };
        let mut from = range.0;
        if !fresh && let Some((base, end)) = self.open(bridge, kind) {
            if let Some(next) = end.checked_add(step)
                && next <= gap_end(self, arena, base)?
            {
                arena.visit()?;
                self.set_window(bridge, kind, Some((base, next)));
                return Ok(true);
            }
            from = base.saturating_add(granule);
        }
        loop {
            arena.charge(arena.siblings.len() + 1)?;
            let base = self
                .gaps(&arena.siblings, range)
                .filter(|&(_, end)| end > from)
                .find_map(|(start, end)| {
                    let base = start.max(from).checked_next_multiple_of(step)?;
                    (base.checked_add(granule)? <= end).then_some(base)
                });
            let Some(base) = base else {
                return Ok(false);
            };
            arena.visit()?;
            from = base.saturating_add(granule);
            let mut high = gap_end(self, arena, base)? & !(step - 1);
            let Some(mut low) = (base + granule).checked_next_multiple_of(step) else {
                continue;
            };
            if low > high || !self.try_window(arena, bridge, kind, last, Some((base, high)))? {
                continue;
            }
            while low < high {
                let middle = low + (high - low) / step / 2 * step;
                arena.visit()?;
                if self.try_window(arena, bridge, kind, last, Some((base, middle)))? {
                    high = middle;
                } else {
                    low = middle + step;
                }
            }
            self.set_window(bridge, kind, Some((base, high)));
            return Ok(true);
        }
    }

    /// The granularity at which something behind `bridge` can start or end
    /// in window `kind`: 1 MiB (4 KiB) if a bridge with something behind
    /// it or a BAR no larger is on its bus, else its smallest BAR.
    fn touch(
        &self,
        arena: &mut Arena,
        bridge: usize,
        kind: WindowKind,
    ) -> Result<u64, PciEcamError> {
        arena.charge(self.devices.len())?;
        let granule = kind.granularity();
        let space = Space::of(kind);
        let root = self.devices[bridge].root;
        let mut step = u64::MAX;
        for dev in self.on_bus(root, Some(bridge)) {
            let need = arena.need[dev];
            let behind = match space {
                Space::Io => need.io,
                Space::Memory => need.memory,
            };
            if self.is_bridge(dev) && behind != 0 {
                return Ok(granule);
            }
            for item in self.bars_in(dev, space) {
                step = step.min(self.bar_align(item).max(granule));
            }
        }
        Ok(if step == u64::MAX { granule } else { step })
    }

    /// Open window `kind` of `bridge` at `span`, or close it, and whether
    /// what is behind the bridge could still fit: a window the bridge can
    /// have ([`window_fits`]), at least the bytes that must go through it
    /// ([`Need`]), and the BARs behind the bridge packed straight into its
    /// windows as if every bridge below forwarded anything anywhere.
    /// Before its `last` window is chosen, only the non-prefetchable
    /// memory, which must go through the memory window.
    fn try_window(
        &mut self,
        arena: &mut Arena,
        bridge: usize,
        kind: WindowKind,
        last: bool,
        span: Option<(u64, u64)>,
    ) -> Result<bool, PciEcamError> {
        self.set_window(bridge, kind, span);
        let prefetch = self.devices[bridge].prefetch;
        if span.is_some_and(|(base, end)| !window_fits(kind, prefetch, base, end)) {
            return Ok(false);
        }
        let size = |kind| self.open(bridge, kind).map_or(0, |(base, end)| end - base);
        let need = arena.need[bridge];
        let space = Space::of(kind);
        let enough = match space {
            Space::Io => size(WindowKind::Io) >= need.io,
            Space::Memory => {
                let memory = size(WindowKind::Memory);
                memory >= need.np
                    && (!last || memory + size(WindowKind::Prefetchable) >= need.memory)
            }
        };
        if !enough {
            return Ok(false);
        }
        arena.charge(self.devices.len())?;
        arena.items.clear();
        arena.taken.clear();
        for dev in self.subtree(bridge) {
            let mandatory = |item: &Item| last || !self.bar(dev, item.index()).prefetchable;
            arena
                .items
                .extend(self.bars_in(dev, space).filter(mandatory));
        }
        let (a, b, rule) = match last {
            true => self.window_sets(bridge, space),
            false => {
                let memory = self.open(bridge, WindowKind::Memory);
                (memory.into_iter().collect(), Ranges::new(), Rule::AOnly)
            }
        };
        self.pack(arena, &a, &b, rule, false)
    }

    /// Whether the windows of `bridge` the search chose range by range
    /// ([`Cursor::Range`]) are the 1 MiB (4 KiB) hulls of what is in them,
    /// `arena.taken` once its bus is packed. Its windows' steps end at
    /// `last`.
    fn tight(&self, arena: &mut Arena, bridge: usize, last: usize) -> Result<bool, PciEcamError> {
        arena.charge(2 * (arena.taken.len() + 1))?;
        let steps = arena.steps[last.saturating_sub(1)..=last].iter();
        Ok(steps.into_iter().all(|step| {
            let Step::Decide {
                dev,
                kind,
                cursor: Cursor::Range(..),
                ..
            } = *step
            else {
                return true;
            };
            if usize::from(dev) != bridge {
                return true;
            }
            let Some((base, end)) = self.open(bridge, kind) else {
                return true;
            };
            self.hull(&arena.taken, (base, end), kind.granularity()) == Some((base, end))
        }))
    }

    /// The `granule` aligned hull of what of `taken` (sorted) is in
    /// `[base, end)`: at most `taken.len()` units.
    fn hull(&self, taken: &[Item], (base, end): (u64, u64), granule: u64) -> Option<(u64, u64)> {
        let first = taken.partition_point(|&item| self.span(item).0 < base);
        let inside = taken[first..]
            .iter()
            .map(|&item| self.span(item))
            .take_while(|&(_, item_end)| item_end <= end);
        let (low, high) = inside.fold((u64::MAX, 0), |(low, high), (item_base, item_end)| {
            (low.min(item_base), high.max(item_end))
        });
        (low < high).then(|| (low & !(granule - 1), high.next_multiple_of(granule)))
    }

    /// Shrink every window of `space` of root `root` to the hull of what is
    /// in it, deepest first; close those with nothing.
    fn shrink(
        &mut self,
        arena: &mut Arena,
        root: usize,
        bridges: &[usize],
        space: Space,
    ) -> Result<(), PciEcamError> {
        for &bridge in bridges.iter().rev() {
            if self.devices[bridge].root != root {
                continue;
            }
            arena.charge(self.devices.len())?;
            arena.taken.clear();
            for dev in self.on_bus(root, Some(bridge)) {
                let placed = |item: &Item| self.bar(dev, item.index()).allocated;
                arena.taken.extend(self.bars_in(dev, space).filter(placed));
                arena.taken.extend(self.windows_in(dev, space));
            }
            self.sort_by_base(arena, |arena| &mut arena.taken)?;
            arena.charge(2 * (arena.taken.len() + 1))?;
            for &kind in space.kinds() {
                if let Some(window) = self.open(bridge, kind) {
                    let hull = self.hull(&arena.taken, window, kind.granularity());
                    self.set_window(bridge, kind, hull);
                }
            }
        }
        Ok(())
    }
}
