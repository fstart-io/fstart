//! The allocator against the one it replaced, on generated topologies.
//!
//! Master's allocator placed BAR by BAR, first fit, and derived each
//! bridge's windows from the spans of the BARs behind it. It did not check
//! what it derived: siblings' windows could interleave. Every layout it
//! produced that is valid (each window holds its subtree and nothing else,
//! siblings apart, forwardable by the bridge's windows as they are) must
//! still be one the allocator finds; and whatever the allocator produces
//! must be valid.

use super::solver::Budget;
use super::tests::endpoint;
use super::*;
use alloc::{format, string::String, vec, vec::Vec};

/// Bus (lowest first), needs the low window, alignment.
type MasterRank = (core::cmp::Reverse<u8>, u8, u64);

/// Master's allocator (e7cca8122), command writes left out.
impl PciEcam {
    fn master_behind_bridge(&self, dev_idx: usize) -> bool {
        let device = &self.devices[dev_idx];
        device.addr.bus() != self.roots[device.root].bus_start
    }

    fn master_alignment(&self, dev_idx: usize, bar_idx: usize) -> u64 {
        let bar = self.devices[dev_idx].bars[bar_idx];
        let behind_bridge = self.master_behind_bridge(dev_idx);
        match bar.bar_type {
            BarType::Memory32 | BarType::Memory64 if behind_bridge => bar.size.max(0x100000),
            BarType::Io if behind_bridge => bar.size.max(0x1000),
            BarType::None => 0,
            _ => bar.size,
        }
    }

    fn master_next(&self, pass: usize) -> Option<(usize, usize)> {
        let mut best: Option<(usize, usize, MasterRank)> = None;
        for dev_idx in 0..self.devices.len() {
            let behind_bridge = self.master_behind_bridge(dev_idx);
            if (pass == 0) == behind_bridge {
                continue;
            }
            for bar_idx in 0..6 {
                let bar = self.devices[dev_idx].bars[bar_idx];
                if bar.bar_type == BarType::None || bar.fixed || bar.allocated {
                    continue;
                }
                let rank = (
                    core::cmp::Reverse(self.devices[dev_idx].addr.bus()),
                    u8::from(bar.bar_type == BarType::Memory32),
                    self.master_alignment(dev_idx, bar_idx),
                );
                if best.is_none_or(|(_, _, best_rank)| rank > best_rank) {
                    best = Some((dev_idx, bar_idx, rank));
                }
            }
        }
        best.map(|(dev_idx, bar_idx, _)| (dev_idx, bar_idx))
    }

    fn master_pending_low(&self, root: usize) -> u64 {
        (0..self.devices.len())
            .filter(|&dev_idx| self.devices[dev_idx].root == root)
            .flat_map(|dev_idx| (0..6).map(move |bar_idx| (dev_idx, bar_idx)))
            .filter(|&(dev_idx, bar_idx)| {
                let bar = self.devices[dev_idx].bars[bar_idx];
                bar.bar_type == BarType::Memory32 && !bar.fixed && !bar.allocated
            })
            .map(|(dev_idx, bar_idx)| self.master_alignment(dev_idx, bar_idx))
            .sum()
    }

    fn master_allocate_one(&mut self, dev_idx: usize, bar_idx: usize) -> Result<(), PciEcamError> {
        let addr = self.devices[dev_idx].addr;
        let root = self.devices[dev_idx].root;
        let bar = self.devices[dev_idx].bars[bar_idx];
        let align = self.master_alignment(dev_idx, bar_idx);
        let behind_bridge = self.master_behind_bridge(dev_idx);
        let pending = self.master_pending_low(root);
        let can_use_high = bar.prefetchable || !behind_bridge;
        let pools = &mut self.roots[root];
        let base = match bar.bar_type {
            BarType::Memory32 => pools.mmio32.allocate_aligned(bar.size, align),
            BarType::Memory64 if !bar.prefetchable && behind_bridge => {
                pools.mmio32.allocate_aligned(bar.size, align)
            }
            BarType::Memory64 => {
                let mut low = pools.mmio32;
                low.allocate_aligned(bar.size, align)
                    .filter(|_| low.free_bytes() >= pending)
                    .inspect(|_| pools.mmio32 = low)
                    .or_else(|| {
                        can_use_high
                            .then(|| pools.mmio64.allocate_aligned(bar.size, align))
                            .flatten()
                    })
            }
            BarType::Io => pools.io_pool.allocate_aligned(bar.size, align),
            BarType::None => None,
        }
        .ok_or(PciEcamError::ResourceExhausted)?;
        match bar.bar_type {
            BarType::Memory32 => {
                let val = (base as u32 & 0xFFFF_FFF0) | if bar.prefetchable { 0x8 } else { 0 };
                self.write32(addr, bar.reg, val);
            }
            BarType::Memory64 => {
                let lo = (base as u32 & 0xFFFF_FFF0) | 0x4 | if bar.prefetchable { 0x8 } else { 0 };
                self.write32(addr, bar.reg, lo);
                self.write32(addr, bar.reg + 4, (base >> 32) as u32);
            }
            BarType::Io => self.write32(addr, bar.reg, (base as u32) | 0x1),
            BarType::None => {}
        }
        self.devices[dev_idx].bars[bar_idx].allocated = true;
        Ok(())
    }

    fn master_allocate(&mut self) -> Result<(), PciEcamError> {
        for pass in 0..2 {
            while let Some((dev_idx, bar_idx)) = self.master_next(pass) {
                self.master_allocate_one(dev_idx, bar_idx)?;
            }
        }
        for i in 0..self.devices.len() {
            if self.devices[i].header_type != PCI_HEADER_TYPE_BRIDGE {
                continue;
            }
            let baddr = self.devices[i].addr;
            let root = self.devices[i].root;
            let sec = self.devices[i].secondary_bus;
            let sub = self.devices[i].subordinate_bus;
            let (mut mem_lo, mut mem_hi, mut pref_lo, mut pref_hi, mut io_lo, mut io_hi) =
                (u64::MAX, 0, u64::MAX, 0, u64::MAX, 0);
            let high_pref = self.devices.iter().any(|child| {
                child.root == root
                    && (sec..=sub).contains(&child.addr.bus())
                    && child.bars.iter().any(|bar| {
                        bar.prefetchable
                            && bar.bar_type == BarType::Memory64
                            && self.read32(child.addr, bar.reg + 4) != 0
                    })
            });
            for child in &self.devices {
                if child.root != root || child.addr.bus() < sec || child.addr.bus() > sub {
                    continue;
                }
                for bar in &child.bars {
                    if bar.bar_type == BarType::None {
                        continue;
                    }
                    if bar.bar_type == BarType::Io {
                        let base = u64::from(self.read32(child.addr, bar.reg) & 0x0000_FFFC);
                        if base != 0 {
                            io_lo = io_lo.min(base);
                            io_hi = io_hi.max(base + bar.size);
                        }
                        continue;
                    }
                    let base_lo = self.read32(child.addr, bar.reg) & 0xFFFF_FFF0;
                    let base = if bar.bar_type == BarType::Memory64 {
                        (u64::from(self.read32(child.addr, bar.reg + 4)) << 32) | u64::from(base_lo)
                    } else {
                        u64::from(base_lo)
                    };
                    if base == 0 {
                        continue;
                    }
                    let end = base + bar.size;
                    if bar.prefetchable && (!high_pref || base >= 1 << 32) {
                        pref_lo = pref_lo.min(base);
                        pref_hi = pref_hi.max(end);
                    } else {
                        mem_lo = mem_lo.min(base);
                        mem_hi = mem_hi.max(end);
                    }
                }
            }
            if mem_lo < mem_hi {
                let base_reg = ((mem_lo >> 16) & 0xFFF0) as u32;
                let limit_reg = (((mem_hi - 1) >> 16) & 0xFFF0) as u32;
                self.write32(baddr, PCI_MEMORY_BASE, base_reg | (limit_reg << 16));
            } else {
                self.write32(baddr, PCI_MEMORY_BASE, 0x0000_FFFF);
            }
            if pref_lo < pref_hi {
                let base_reg = ((pref_lo >> 16) & 0xFFF0) as u32;
                let limit_reg = (((pref_hi - 1) >> 16) & 0xFFF0) as u32;
                self.write32(baddr, PCI_PREF_MEMORY_BASE, base_reg | (limit_reg << 16));
                self.write32(baddr, PCI_PREF_BASE_UPPER32, (pref_lo >> 32) as u32);
                self.write32(baddr, PCI_PREF_LIMIT_UPPER32, ((pref_hi - 1) >> 32) as u32);
            } else {
                self.write32(baddr, PCI_PREF_MEMORY_BASE, 0x0000_FFFF);
                self.write32(baddr, PCI_PREF_BASE_UPPER32, 0);
                self.write32(baddr, PCI_PREF_LIMIT_UPPER32, 0);
            }
            if io_lo < io_hi {
                let base = ((io_lo >> 8) & 0xF0) as u32;
                let limit = (((io_hi - 1) >> 8) & 0xF0) as u32;
                self.write32(baddr, PCI_IO_BASE, base | (limit << 8));
            } else {
                self.write32(baddr, PCI_IO_BASE, 0x00FF);
            }
        }
        Ok(())
    }
}

/// An inclusive span; `None` is a closed window.
pub(super) type Span = Option<(u64, u64)>;

fn overlap(a: Span, b: Span) -> bool {
    matches!((a, b), (Some(a), Some(b)) if a.0 <= b.1 && b.0 <= a.1)
}

fn within(inner: (u64, u64), outer: Span) -> bool {
    outer.is_some_and(|outer| outer.0 <= inner.0 && inner.1 <= outer.1)
}

/// What bridge `bridge` forwards, read back as the hardware would decode
/// it: I/O, memory, prefetchable memory.
pub(super) fn windows(pci: &PciEcam, bridge: &PciDev) -> [Span; 3] {
    let open = |base: u64, limit: u64| (base <= limit).then_some((base, limit));
    let io = pci.read32(bridge.addr, PCI_IO_BASE);
    let io = open(u64::from(io & 0xf0) << 8, (u64::from(io & 0xf000)) | 0xfff);
    let memory = pci.read32(bridge.addr, PCI_MEMORY_BASE);
    let memory = open(
        u64::from(memory & 0xfff0) << 16,
        u64::from(memory & 0xfff0_0000) | 0xf_ffff,
    );
    let pref = pci.read32(bridge.addr, PCI_PREF_MEMORY_BASE);
    let (upper_base, upper_limit) = match bridge.prefetch {
        PrefetchWindow::Bits64 => (
            u64::from(pci.read32(bridge.addr, PCI_PREF_BASE_UPPER32)) << 32,
            u64::from(pci.read32(bridge.addr, PCI_PREF_LIMIT_UPPER32)) << 32,
        ),
        _ => (0, 0),
    };
    let pref = match bridge.prefetch {
        PrefetchWindow::None => None,
        _ => open(
            upper_base | u64::from(pref & 0xfff0) << 16,
            upper_limit | u64::from(pref & 0xfff0_0000) | 0xf_ffff,
        ),
    };
    [io, memory, pref]
}

/// Every BAR as programmed: its device, whether it is I/O, whether it is
/// prefetchable, its span.
pub(super) fn bars(pci: &PciEcam) -> Vec<(usize, bool, bool, (u64, u64))> {
    let mut bars = Vec::new();
    for (dev_idx, device) in pci.devices.iter().enumerate() {
        for bar in device
            .bars
            .iter()
            .filter(|bar| bar.bar_type != BarType::None)
        {
            let low = pci.read32(device.addr, bar.reg);
            let base = match bar.bar_type {
                BarType::Io => u64::from(low & !3),
                BarType::Memory32 => u64::from(low & !15),
                _ => u64::from(low & !15) | u64::from(pci.read32(device.addr, bar.reg + 4)) << 32,
            };
            let io = bar.bar_type == BarType::Io;
            bars.push((dev_idx, io, bar.prefetchable, (base, base + bar.size - 1)));
        }
    }
    bars
}

/// Whether the programmed layout routes every BAR, and nothing else, to
/// where it is: the error says what does not.
pub(super) fn valid(pci: &PciEcam, pools: &[(u64, u64)]) -> Result<(), String> {
    let bars = bars(pci);
    for (i, &(dev, io, _, span)) in bars.iter().enumerate() {
        if span.0 % (span.1 - span.0 + 1) != 0 {
            return Err(format!("BAR of {dev} at {span:x?} misaligned"));
        }
        if !pools.iter().any(|&pool| within(span, Some(pool))) {
            return Err(format!("BAR of {dev} at {span:x?} outside the root"));
        }
        for &(other, other_io, _, other_span) in &bars[i + 1..] {
            if io == other_io && overlap(Some(span), Some(other_span)) {
                return Err(format!("BARs of {dev} and {other} overlap"));
            }
        }
    }
    for (b, bridge) in pci.devices.iter().enumerate() {
        if bridge.header_type != PCI_HEADER_TYPE_BRIDGE {
            continue;
        }
        let w = windows(pci, bridge);
        if overlap(w[1], w[2]) {
            return Err(format!("bridge {b}'s windows overlap"));
        }
        for window in w.iter().flatten() {
            if !pools.iter().any(|&pool| within(*window, Some(pool))) {
                return Err(format!("bridge {b}'s window {window:x?} outside the root"));
            }
        }
        let subtree = bridge.secondary_bus..=bridge.subordinate_bus;
        for &(dev, io, prefetchable, span) in &bars {
            let device = &pci.devices[dev];
            let behind = device.root == bridge.root && subtree.contains(&device.addr.bus());
            let routed = if io {
                within(span, w[0])
            } else {
                within(span, w[1]) || (prefetchable && within(span, w[2]))
            };
            let touches = if io {
                overlap(Some(span), w[0])
            } else {
                overlap(Some(span), w[1]) || overlap(Some(span), w[2])
            };
            if behind && !routed {
                return Err(format!("bridge {b} does not forward {dev}'s BAR {span:x?}"));
            }
            if !behind && touches {
                return Err(format!("bridge {b} forwards {dev}'s BAR {span:x?}"));
            }
        }
        for (c, sibling) in pci.devices.iter().enumerate() {
            if c == b
                || sibling.header_type != PCI_HEADER_TYPE_BRIDGE
                || sibling.root != bridge.root
            {
                continue;
            }
            let v = windows(pci, sibling);
            if sibling.addr.bus() == bridge.addr.bus()
                && (overlap(w[0], v[0])
                    || [w[1], w[2]]
                        .iter()
                        .any(|&w| overlap(w, v[1]) || overlap(w, v[2])))
            {
                return Err(format!("bridges {b} and {c} overlap"));
            }
            // The parent's windows hold the child's: memory in memory,
            // prefetchable memory in either.
            if sibling.secondary_bus == bridge.addr.bus() {
                let parent = v;
                let held = w[0].is_none_or(|io| within(io, parent[0]))
                    && w[1].is_none_or(|memory| within(memory, parent[1]))
                    && w[2].is_none_or(|pref| within(pref, parent[1]) || within(pref, parent[2]));
                if !held {
                    return Err(format!("bridge {c} does not hold bridge {b}'s windows"));
                }
            }
        }
    }
    Ok(())
}

/// A root's free ranges as its pools hold them, inclusive: below 4 GiB,
/// above, ports. Allocation leaves them as they were.
pub(super) fn pools_of(pci: &PciEcam) -> [Vec<(u64, u64)>; 3] {
    let root = &pci.roots[0];
    [&root.mmio32, &root.mmio64, &root.io_pool].map(|pool| {
        pool.ranges[..pool.count]
            .iter()
            .map(|range| (range.base, range.end - 1))
            .collect()
    })
}

/// Whether what the allocator wrote, read back, is typed as it must be:
/// each BAR's attribute bits as it sized; each BAR and window in a range
/// of its type (on the root bus: a pool of `pools` it may use, a 32-bit
/// BAR, memory window or 32-bit prefetchable window below 4 GiB; behind a
/// bridge: the window of its parent that forwards its type, a 32-bit BAR
/// not in a prefetchable window above 4 GiB); no window across 4 GiB; and
/// each function's command register decoding what it was given.
pub(super) fn typed(pci: &PciEcam, pools: &[Vec<(u64, u64)>; 3]) -> Result<(), String> {
    const FOUR_GIB: u64 = 1 << 32;
    let [low, high, io] = pools;
    let pooled = |span: (u64, u64), ranges: &[(u64, u64)]| {
        ranges.iter().any(|&range| within(span, Some(range)))
    };
    let crosses = |span: Span| span.is_some_and(|(base, last)| base < FOUR_GIB && last >= FOUR_GIB);
    for device in &pci.devices {
        let at = device.addr;
        let parent = device
            .parent
            .map(|parent| windows(pci, &pci.devices[parent]));
        let (mut has_io, mut has_memory) = (false, false);
        for bar in device
            .bars
            .iter()
            .filter(|bar| bar.bar_type != BarType::None && !bar.fixed)
        {
            let value = pci.read32(at, bar.reg);
            let prefetchable = if bar.prefetchable { 0x8 } else { 0 };
            let (attributes, mask, base) = match bar.bar_type {
                BarType::Io => (0x1, 0x3, u64::from(value & !0x3)),
                BarType::Memory32 => (prefetchable, 0xf, u64::from(value & !0xf)),
                _ => (
                    0x4 | prefetchable,
                    0xf,
                    u64::from(value & !0xf) | u64::from(pci.read32(at, bar.reg + 4)) << 32,
                ),
            };
            if value & mask != attributes {
                return Err(format!("{at:?} BAR {:#x} reads {value:#x}", bar.reg));
            }
            let span = (base, base + bar.size - 1);
            let wide = bar.bar_type == BarType::Memory64;
            has_io |= bar.bar_type == BarType::Io;
            has_memory |= bar.bar_type != BarType::Io;
            let typed = match (bar.bar_type, parent) {
                (BarType::Io, None) => pooled(span, io),
                (_, None) => pooled(span, low) || wide && pooled(span, high),
                (BarType::Io, Some(w)) => within(span, w[0]),
                (_, Some(w)) if !bar.prefetchable => within(span, w[1]),
                (_, Some(w)) => {
                    within(span, w[1])
                        || within(span, w[2]) && (wide || w[2].is_some_and(|w| w.1 < FOUR_GIB))
                }
            };
            if !typed {
                return Err(format!(
                    "{at:?} BAR {:#x} at {span:x?} is not where its type goes",
                    bar.reg
                ));
            }
        }
        let bridge = device.header_type == PCI_HEADER_TYPE_BRIDGE;
        let own = bridge.then(|| windows(pci, device));
        if let Some(w) = own {
            let wide = device.prefetch == PrefetchWindow::Bits64;
            let held = |window: Span, outer: &[Span]| {
                window.is_none_or(|window| outer.iter().any(|&outer| within(window, outer)))
            };
            let pool = |window: Span, ranges: &[&[(u64, u64)]]| {
                window.is_none_or(|window| ranges.iter().any(|ranges| pooled(window, ranges)))
            };
            let typed = match parent {
                None => {
                    let pref: &[&[(u64, u64)]] = if wide { &[low, high] } else { &[low] };
                    pool(w[0], &[io]) && pool(w[1], &[low]) && pool(w[2], pref)
                }
                Some(p) => held(w[0], &[p[0]]) && held(w[1], &[p[1]]) && held(w[2], &[p[1], p[2]]),
            };
            if !typed || crosses(w[1]) || crosses(w[2]) {
                return Err(format!(
                    "{at:?}'s windows {w:x?} are not where their types go"
                ));
            }
        }
        let command = pci.read16(at, PCI_COMMAND);
        let original = device.original_command;
        let io_window = own.is_some_and(|w| w[0].is_some());
        let io = has_io || io_window || !bridge && original & PCI_CMD_IO != 0;
        let memory = bridge || has_memory || original & PCI_CMD_MEMORY != 0;
        let expected = (u16::from(io) * PCI_CMD_IO)
            | (u16::from(memory) * PCI_CMD_MEMORY)
            | PCI_CMD_BUS_MASTER;
        let decode = PCI_CMD_IO | PCI_CMD_MEMORY | PCI_CMD_BUS_MASTER;
        if command & decode != expected {
            return Err(format!("{at:?} decodes {command:#x}, not {expected:#x}"));
        }
    }
    Ok(())
}

/// The topology and what master made of it, for a failure.
pub(super) fn describe(pci: &PciEcam, pools: &[(u64, u64)]) -> String {
    let mut text = format!("pools {pools:x?}\n");
    for (index, device) in pci.devices.iter().enumerate() {
        text += &format!(
            "{index} {:?} header {} buses {}-{} prefetchable {:?}\n",
            device.addr,
            device.header_type,
            device.secondary_bus,
            device.subordinate_bus,
            device.prefetch
        );
        for bar in device
            .bars
            .iter()
            .filter(|bar| bar.bar_type != BarType::None)
        {
            text += &format!(
                "  {:?} {:#x} prefetchable {}\n",
                bar.bar_type, bar.size, bar.prefetchable
            );
        }
        if device.header_type == PCI_HEADER_TYPE_BRIDGE {
            text += &format!("  windows {:x?}\n", windows(pci, device));
        }
    }
    for bar in bars(pci) {
        text += &format!("BAR {bar:x?}\n");
    }
    text
}

/// A deterministic generator.
pub(super) struct Rng(pub(super) u64);

impl Rng {
    pub(super) fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 33
    }

    pub(super) fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

const BUSES: usize = 12;

/// A root with `BUSES` buses of RAM config space and the given pools.
fn fixture(config_space: &mut [u32], pools: [(u64, u64); 3]) -> PciEcam {
    let root = Root {
        segment: 0,
        ecam_base: config_space.as_mut_ptr() as usize,
        ecam_size: BUSES << 20,
        bus_start: 0,
        bus_end: BUSES as u8 - 1,
        mmio32: ResourcePool::new(pools[0].0, pools[0].1).unwrap(),
        mmio64: ResourcePool::new(pools[1].0, pools[1].1).unwrap(),
        io_pool: ResourcePool::new(pools[2].0, pools[2].1).unwrap(),
        reserved_through: 0,
        next_bus: 1,
    };
    PciEcam::with_pools(HVec::from_iter([root]))
}

/// The devices of a generated topology: a root bus with endpoints and
/// bridges, bridges with endpoints and, two deep, bridges again.
fn topology(rng: &mut Rng) -> Vec<PciDev> {
    fn endpoint_at(rng: &mut Rng, addr: PciAddress) -> PciDev {
        let mut definitions = Vec::new();
        let mut prefetchable = Vec::new();
        let mut index = 0;
        for _ in 0..1 + rng.below(3) {
            let (kind, size, pref) = match rng.below(9) {
                0 => (BarType::Io, 16 << rng.below(5), false),
                1 | 2 => (BarType::Memory32, 0x1000 << rng.below(13), false),
                3 => (BarType::Memory32, 0x1000 << rng.below(13), true),
                4 => (BarType::Memory64, 0x1000 << rng.below(13), false),
                _ => (BarType::Memory64, 0x1000 << rng.below(13), true),
            };
            definitions.push((index, kind, size));
            prefetchable.push(pref);
            index += if kind == BarType::Memory64 { 2 } else { 1 };
            if index >= 5 {
                break;
            }
        }
        let mut device = endpoint(addr, &definitions);
        for (&(index, ..), pref) in definitions.iter().zip(prefetchable) {
            device.bars[index].prefetchable = pref;
        }
        device
    }
    fn bus(rng: &mut Rng, devices: &mut Vec<PciDev>, number: u8, next: &mut u8, depth: u32) {
        let endpoints = if depth == 0 {
            rng.below(3)
        } else {
            1 + rng.below(2)
        };
        let bridges = match depth {
            0 => 1 + rng.below(3),
            1 => rng.below(2),
            _ => u64::from(rng.below(3) == 0),
        };
        let mut slot = 0;
        for _ in 0..endpoints {
            devices.push(endpoint_at(rng, PciAddress::new(0, number, slot, 0)));
            slot += 1;
        }
        for _ in 0..bridges {
            if usize::from(*next) >= BUSES {
                break;
            }
            let secondary = *next;
            *next += 1;
            let index = devices.len();
            // A bridge has two BARs; the registers after them are its own.
            let address = PciAddress::new(0, number, slot, 0);
            let mut bridge = match rng.below(8) {
                0 => endpoint(address, &[(0, BarType::Memory32, 0x1000 << rng.below(8))]),
                1 => endpoint(address, &[(1, BarType::Io, 16 << rng.below(4))]),
                _ => endpoint(address, &[]),
            };
            slot += 1;
            bridge.header_type = PCI_HEADER_TYPE_BRIDGE;
            bridge.secondary_bus = secondary;
            bridge.prefetch = match rng.below(10) {
                0 => PrefetchWindow::None,
                1 | 2 => PrefetchWindow::Bits32,
                _ => PrefetchWindow::Bits64,
            };
            devices.push(bridge);
            bus(rng, devices, secondary, next, depth + 1);
            devices[index].subordinate_bus = *next - 1;
        }
    }
    let mut devices = Vec::new();
    let mut next = 1;
    bus(rng, &mut devices, 0, &mut next, 0);
    devices
}

fn copy(device: &PciDev) -> PciDev {
    PciDev { ..*device }
}

/// Master, the fast paths alone (#372's subtree and BAR-by-BAR layouts,
/// which firmware still runs first: as they are now, each checked by
/// [`PciEcam::validate_plan`], not a frozen #372), the window search alone
/// and both, on the same topologies. What master or the fast paths lay out
/// validly is laid out; what any of them writes is valid, and what ours
/// write is typed ([`typed`]); the search says there is no layout only
/// where neither found one.
#[test]
fn every_valid_layout_of_master_is_found_and_every_layout_is_valid() {
    let (mut found, mut master_valid, mut subtrees_valid, mut rescued) = (0, 0, 0, 0);
    for seed in 0..4000u64 {
        let mut rng = Rng(seed);
        let devices = topology(&mut rng);
        let low = (
            0x8000_0000 + (rng.below(16) << 20),
            (2 + rng.below(30)) << 20,
        );
        let high = match rng.below(3) {
            0 => (0, 0),
            _ => (
                0x1_0000_0000 + (rng.below(4) << 20),
                (2 + rng.below(30)) << 20,
            ),
        };
        let io = (0x1000, (1 + rng.below(4)) << 12);
        let pools = [low, high, io];
        let apertures: Vec<_> = pools
            .iter()
            .filter(|pool| pool.1 != 0)
            .map(|&(base, size)| (base, base + size - 1))
            .collect();

        let run = |allocators: Option<Allocators>| {
            let mut space = vec![0u32; BUSES << 18];
            let mut pci = fixture(&mut space, pools);
            for device in &devices {
                assert!(pci.devices.push(copy(device)).is_ok());
            }
            let result = match allocators {
                None => pci.master_allocate(),
                Some(allocators) => {
                    pci.allocators = allocators;
                    pci.allocate_resources()
                }
            };
            let valid = result.and_then(|()| {
                let checked = valid(&pci, &apertures).and_then(|()| match allocators {
                    None => Ok(()),
                    Some(_) => typed(&pci, &pools_of(&pci)),
                });
                checked.map_err(|error| {
                    assert!(
                        allocators.is_none(),
                        "seed {seed} {allocators:?}: {error}\n{}",
                        describe(&pci, &pools)
                    );
                    PciEcamError::ConfigError
                })
            });
            (valid, describe(&pci, &pools))
        };
        let (master, master_layout) = run(None);
        let (subtrees, _) = run(Some(Allocators::FastPaths));
        let (search, _) = run(Some(Allocators::Search(Budget::FIRMWARE)));
        let (result, _) = run(Some(Allocators::Both(Budget::FIRMWARE)));
        found += usize::from(result.is_ok());
        master_valid += usize::from(master.is_ok());
        subtrees_valid += usize::from(subtrees.is_ok());
        rescued += usize::from(result.is_ok() && subtrees.is_err());
        if master.is_ok() {
            assert_eq!(
                result,
                Ok(()),
                "seed {seed}: master's layout was valid:\n{master_layout}"
            );
        }
        if subtrees.is_ok() {
            assert_eq!(result, Ok(()), "seed {seed}");
        }
        if search == Err(PciEcamError::ResourceExhausted) {
            assert!(master.is_err() && subtrees.is_err(), "seed {seed}");
        }
        assert!(matches!(
            search,
            Ok(()) | Err(PciEcamError::ResourceExhausted | PciEcamError::SearchLimit)
        ));
    }
    // The generator exercises both outcomes, and the search finds layouts
    // the subtree allocator misses.
    assert!(
        master_valid > 400 && subtrees_valid > master_valid && rescued > 0,
        "{master_valid} {subtrees_valid} {found} {rescued}"
    );
}
