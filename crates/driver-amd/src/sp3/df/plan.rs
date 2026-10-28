//! The DF resource planner: MMIO below/above 4 GiB, I/O and bus ranges per
//! root bridge, as a pure function of the memory map and the topology.
//!
//! The algorithm is AGESA's *equal split* (SOC_DF §3.4.1 for DF3 and the
//! AGESA ZP split of SP4r2 SOC_DISPATCH §4), with two board overrides the
//! vendor firmware applies through its request table: PCI bus counts and I/O
//! sizes per root bridge (S8030: 4 × 64 buses, I/O 0x4000/0x4000/0x4000/
//! 0x3000, SOC_DF §3.4). AMI's request-path combination search (§3.4.2) is
//! deliberately not reproduced.
//!
//! The DF3 numbers are pinned against `df3_plan()` in
//! `sp3/tools/build_soc_df.py` (tests below).
//!
//! # Naples
//!
//! [`Policy::naples`] is the AGESA ZP split run over all dies of both
//! sockets (die 0 of socket 0 primary, 8 MiB Non-PCI per die below 4 GiB,
//! above-4G limit 0x7FD_0000_0000, last above window to 2^48). The Naples
//! donor does not run that code: AMI's CrbPei writes an early map
//! (0xED00_0000–0xFFFF_FFFF to die 0, 16 MiB per other die below it) and
//! AMI's PciRootBridge rewrites one below-4G pair per die after its GCD
//! allocation, with the primary limit fixed at 0xFED0 (0xFEC0 on A0/B0/B1)
//! and an allocator whose die-0 pool is `[0xFE00_0000, +0xB0_0000)` and
//! whose other dies split `[TOM, 0xF000_0000)` (SOC_DF §3.2). Above 4 GiB
//! AMI splits top-down from 2 TiB in equal shares skipping the
//! 0xFD_0000_0000 hole, into F0x280–2F8, and I/O into F0xC0/C4 + 8k; DF2
//! has 16 MMIO rules (ROOTBRIDGE_2P §1). The planner instead gives every
//! die a proportional share up front, like AGESA ZP; the windows differ from
//! the donor's but route every root bridge.

use heapless::Vec;

use super::BusRange;
use crate::sp3::topology::{MAX_ROOT_BRIDGES, RootBridge};

const MIB: u64 = 1 << 20;
const GIB: u64 = 1 << 30;
const TIB: u64 = 1 << 40;
const ALIGN_16M: u64 = !(16 * MIB - 1);
/// Top of the MMIO region the DF routes above the ECAM (§3.4.1).
const MMIO_TOP: u64 = 0xFE00_0000;
/// The primary's window above the ECAM extends to 0xFEBF_FFFF.
const PRIMARY_EXTENSION: u64 = 0xC0_0000;
/// The HyperTransport hole \[0xFD_0000_0000, 1 TiB) the above-4G split skips.
const HT_HOLE: u64 = 0xFD_0000_0000;
/// I/O space the DF routes (25 bits).
const IO_SPACE: u64 = 0x200_0000;
/// Usable x86 port space.
const IO_PORTS: u64 = 0x1_0000;
/// Legacy ports kept on the primary root bridge.
const IO_LEGACY: u64 = 0x1000;

/// A physical address or port range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window {
    pub base: u64,
    pub size: u64,
}

impl Window {
    pub const fn new(base: u64, size: u64) -> Self {
        Self { base, size }
    }
    pub const fn end(&self) -> u64 {
        self.base + self.size
    }
}

/// How the above-4G MMIO limit is derived.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AboveLimit {
    /// `2^PA − reserve` (DF3: 12 GiB below the top of the address space).
    BelowTop(u64),
    /// A fixed limit (AGESA ZP: 0x7FD_0000_0000).
    Fixed(u64),
}

/// Planner policy. [`Policy::df3`] and [`Policy::naples`] are the donor
/// algorithms; boards add [`with_bus_counts`](Self::with_bus_counts) /
/// [`with_io_sizes`](Self::with_io_sizes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    pub above_limit: AboveLimit,
    /// Limit cap: the limit is lowered to `(cap + 1) & !0xFFFF` when that is
    /// lower (Rome PCD 0x2, Milan 0x186: 0x7FB_FFFF_FFFF).
    pub above_cap: Option<u64>,
    /// Non-PCI MMIO at the top of each below-4G window (NBIO BARs etc.).
    pub nonpci_below: u64,
    /// Non-PCI MMIO at the (256 MiB aligned) base of each above-4G window.
    pub nonpci_above: u64,
    /// Above-4G windows smaller than this are not routed.
    pub above_min: u64,
    /// End of the last above-4G window (ZP: 2^48) instead of the limit.
    pub above_last_end: Option<u64>,
    /// DF3 recomputes the share of the root bridges above and below the
    /// ECAM per region; ZP reuses the global share.
    pub per_region_share: bool,
    /// Fixed window routed to the primary root bridge (0xFED0_0000, 64 KiB).
    pub spare: Option<Window>,
    /// PCI buses per root bridge, logical order (0 = keep).
    pub bus_counts: Option<[u16; MAX_ROOT_BRIDGES]>,
    /// I/O bytes per root bridge, logical order; root bridges with 0 get no
    /// I/O window. `None`: equal split.
    pub io_sizes: Option<[u32; MAX_ROOT_BRIDGES]>,
}

impl Policy {
    /// Rome/Milan equal split (AmdSocSp3RmPei `ffe1d299`, Milan `ffd3ae47`).
    pub const fn df3() -> Self {
        Self {
            above_limit: AboveLimit::BelowTop(12 * GIB),
            above_cap: Some(0x7FB_FFFF_FFFF),
            nonpci_below: 16 * MIB,
            nonpci_above: 0x2020_0000,
            above_min: 0x2020_0000,
            above_last_end: None,
            per_region_share: true,
            spare: Some(Window::new(0xFED0_0000, 0x1_0000)),
            bus_counts: None,
            io_sizes: None,
        }
    }

    /// AGESA ZP split over all Naples dies (see the module docs).
    pub const fn naples() -> Self {
        Self {
            above_limit: AboveLimit::Fixed(0x7FD_0000_0000),
            above_cap: None,
            nonpci_below: 8 * MIB,
            nonpci_above: 0,
            above_min: 1,
            above_last_end: Some(1 << 48),
            per_region_share: false,
            spare: Some(Window::new(0xFED0_0000, 0x1_0000)),
            bus_counts: None,
            io_sizes: None,
        }
    }

    pub const fn with_bus_counts(mut self, counts: [u16; MAX_ROOT_BRIDGES]) -> Self {
        self.bus_counts = Some(counts);
        self
    }

    pub const fn with_io_sizes(mut self, sizes: [u32; MAX_ROOT_BRIDGES]) -> Self {
        self.io_sizes = Some(sizes);
        self
    }
}

/// Planner inputs.
#[derive(Debug, Clone, Copy)]
pub struct Input<'a> {
    /// Top of DRAM below 4 GiB (MSR C001_001A).
    pub tom: u64,
    /// Top of DRAM above 4 GiB (MSR C001_001D), 0 if none.
    pub tom2: u64,
    pub ecam_base: u64,
    pub ecam_size: u64,
    /// Physical address bits, already reduced by the SME reduction where the
    /// generation applies it (Rome always, Milan per policy; §3.4).
    pub phys_addr_bits: u8,
    /// Root bridges with their final bus ranges, in physical order
    /// ([`Topology::root_bridges`](super::topology::Topology)): the above-4G
    /// split walks this order.
    pub root_bridges: &'a [RootBridge],
    /// MmioAddress rule pairs of the DF version.
    pub mmio_pairs: u8,
}

/// The pools a consumer allocates from inside one MMIO window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pools {
    pub prefetch: Window,
    pub non_prefetch: Window,
    /// Non-PCI MMIO (size 0 if the policy reserves none).
    pub non_pci: Window,
}

/// What one root bridge gets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RootBridgePlan {
    pub socket: u8,
    pub index: u8,
    pub fabric_id: u16,
    pub primary: bool,
    pub bus: BusRange,
    /// The routed below-4G window (the primary's includes the 0xC0_0000
    /// extension when it sits above the ECAM).
    pub below: Window,
    /// Pools of `below` without the primary's extension (fixed FCH
    /// decodes live in 0xFE00_0000–0xFEBF_FFFF; the pool split itself is DC).
    pub below_pools: Pools,
    /// Primary only: the leftover of the other region.
    pub second_below: Option<Window>,
    pub above: Option<Window>,
    pub above_pools: Option<Pools>,
    pub io: Option<Window>,
    /// Ports the root bridge's devices may use (legacy ports excluded on the
    /// primary, capped at 64 Ki).
    pub io_pool: Option<Window>,
}

/// One MmioAddress rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MmioRoute {
    pub pair: u8,
    pub window: Window,
    pub fabric_id: u16,
}

/// One X86IO rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IoRoute {
    pub pair: u8,
    pub window: Window,
    pub fabric_id: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// Sorted by pair.
    pub mmio: Vec<MmioRoute, 16>,
    pub io: Vec<IoRoute, 8>,
    /// Physical order (as the input).
    pub root_bridges: Vec<RootBridgePlan, MAX_ROOT_BRIDGES>,
    pub above_ecam_too_small: bool,
    pub below_ecam_too_small: bool,
    /// Root bridges placed above the ECAM.
    pub above_ecam: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanError {
    NoRootBridges,
    NoPrimary,
    /// Both regions are too small for a fair share (AGESA asserts).
    NoMmioBelow4G,
    /// More MMIO rules than the DF has.
    OutOfPairs,
    /// I/O requests exceed the port space.
    IoOverflow,
    /// Bus requests exceed 256 buses.
    BusOverflow,
}

/// Physical slot of each root bridge: `socket · per_socket + rank by index`.
fn slots(rbs: &[RootBridge]) -> (u8, Vec<u8, MAX_ROOT_BRIDGES>) {
    let per_socket = (0..=1u8)
        .map(|s| rbs.iter().filter(|r| r.socket == s).count())
        .max()
        .unwrap_or(0) as u8;
    let slot = rbs
        .iter()
        .map(|rb| {
            let rank = rbs
                .iter()
                .filter(|r| r.socket == rb.socket && r.index < rb.index)
                .count() as u8;
            rb.socket * per_socket + rank
        })
        .collect();
    (per_socket, slot)
}

/// Logical order: indices of `rbs` by ascending bus base.
fn logical(rbs: &[RootBridge]) -> Vec<usize, MAX_ROOT_BRIDGES> {
    let mut v: Vec<usize, MAX_ROOT_BRIDGES> = (0..rbs.len()).collect();
    v.sort_unstable_by_key(|&i| (rbs[i].bus_base, rbs[i].socket, rbs[i].index));
    v
}

fn below_pools(w: Window, nonpci: u64) -> Pools {
    let non_pci = Window::new(w.end() - nonpci, nonpci);
    let prefetch = ((w.size - nonpci) * 4 / 5) & ALIGN_16M;
    Pools {
        prefetch: Window::new(w.base, prefetch),
        non_prefetch: Window::new(w.base + prefetch, w.size - nonpci - prefetch),
        non_pci,
    }
}

fn above_pools(w: Window, nonpci: u64) -> Pools {
    let np_base = if nonpci == 0 {
        w.base
    } else {
        w.base.next_multiple_of(256 * MIB)
    };
    let non_pci = Window::new(np_base, nonpci);
    let rest = Window::new(non_pci.end(), w.end().saturating_sub(non_pci.end()));
    let prefetch = (rest.size * 4 / 5) & ALIGN_16M;
    Pools {
        prefetch: Window::new(rest.base, prefetch),
        non_prefetch: Window::new(rest.base + prefetch, rest.size - prefetch),
        non_pci,
    }
}

/// Bus ranges for `policy.bus_counts`, in the input order, or `None` when
/// every root bridge already has its requested count (AGESA only rewrites
/// the maps on a mismatch, §3.4.2). The primary gets bus 0, the others
/// follow in logical order.
pub fn plan_buses(
    rbs: &[RootBridge],
    policy: &Policy,
) -> Result<Option<Vec<BusRange, MAX_ROOT_BRIDGES>>, PlanError> {
    let Some(counts) = policy.bus_counts else {
        return Ok(None);
    };
    let order = logical(rbs);
    let want = |k: usize| match counts[k] {
        0 => rbs[order[k]].bus_limit as u16 - rbs[order[k]].bus_base as u16 + 1,
        c => c,
    };
    if (0..rbs.len())
        .all(|k| want(k) == rbs[order[k]].bus_limit as u16 - rbs[order[k]].bus_base as u16 + 1)
    {
        return Ok(None);
    }
    let p = rbs
        .iter()
        .position(|r| r.primary)
        .ok_or(PlanError::NoPrimary)?;
    let pk = order.iter().position(|&i| i == p).unwrap_or(0);
    let mut ranges: Vec<BusRange, MAX_ROOT_BRIDGES> = (0..rbs.len())
        .map(|_| BusRange { base: 0, limit: 0 })
        .collect();
    let mut next = 0u16;
    for k in core::iter::once(pk).chain((0..rbs.len()).filter(|&k| k != pk)) {
        let end = next + want(k);
        if end > 256 {
            return Err(PlanError::BusOverflow);
        }
        ranges[order[k]] = BusRange {
            base: next as u8,
            limit: (end - 1) as u8,
        };
        next = end;
    }
    Ok(Some(ranges))
}

/// Plan MMIO and I/O for `input.root_bridges` (with their final buses).
pub fn plan(input: &Input, policy: &Policy) -> Result<Plan, PlanError> {
    let rbs = input.root_bridges;
    let n = rbs.len() as u64;
    if n == 0 {
        return Err(PlanError::NoRootBridges);
    }
    let p = rbs
        .iter()
        .position(|r| r.primary)
        .ok_or(PlanError::NoPrimary)?;
    let (_, slot) = slots(rbs);
    let max_slots = input.mmio_pairs / 2;
    if slot.iter().any(|&s| s >= max_slots) {
        return Err(PlanError::OutOfPairs);
    }

    // Below 4 GiB (§3.4.1).
    let cfg_base = input.ecam_base;
    let cfg_end = input.ecam_base + input.ecam_size;
    let above = MMIO_TOP.saturating_sub(cfg_end);
    let below = cfg_base.saturating_sub(input.tom);
    let mut total = above + below;
    let (mut above_small, mut below_small) = (false, false);
    if n > 1 {
        above_small = above < (below / n) * 8 / 10;
        below_small = below < (above / n) * 8 / 10;
        if above_small && below_small {
            return Err(PlanError::NoMmioBelow4G);
        }
        if above_small {
            total -= above;
        }
        if below_small {
            total -= below;
        }
    }
    let per = total / n;
    let per16 = per & ALIGN_16M;
    if per16 == 0 {
        return Err(PlanError::NoMmioBelow4G);
    }
    let (mut size, mut cursor, n_above, per_above) = if above_small {
        (below - per16 * (n - 1), cfg_base, 0, per16)
    } else {
        let mut q = above / per;
        if above - per * q > per / 2 {
            q += 1;
        }
        let q = q.max(1);
        let pa = if policy.per_region_share {
            (above / q) & ALIGN_16M
        } else {
            per16
        };
        (above - pa * (q - 1), MMIO_TOP, q, pa)
    };

    let order = core::iter::once(p).chain((0..rbs.len()).filter(|&i| i != p));
    let mut below_w: [Window; MAX_ROOT_BRIDGES] = [Window::new(0, 0); MAX_ROOT_BRIDGES];
    let mut left = n_above;
    let mut per_below = None;
    for (k, i) in order.enumerate() {
        let k = k as u64;
        if left > 0 {
            if k == 1 {
                size = per_above;
            }
            left -= 1;
        } else if !below_small {
            if k == n_above && k > 0 {
                let m = n - k;
                let pb = if policy.per_region_share {
                    (below / m) & ALIGN_16M
                } else {
                    per16
                };
                per_below = Some(pb);
                size = below - pb * (m - 1);
                cursor = cfg_base;
            } else if k == n_above + 1 {
                size = per_below.unwrap_or(per16);
            }
        }
        cursor -= size;
        below_w[i] = Window::new(cursor, size);
    }

    let mut mmio: Vec<MmioRoute, 16> = Vec::new();
    let mut push = |pair: u8, window: Window, fabric_id: u16| {
        mmio.push(MmioRoute {
            pair,
            window,
            fabric_id,
        })
        .map_err(|_| PlanError::OutOfPairs)
    };
    let routed = |i: usize| {
        let w = below_w[i];
        if i == p && !above_small {
            Window::new(w.base, w.size + PRIMARY_EXTENSION)
        } else {
            w
        }
    };
    for i in 0..rbs.len() {
        push(2 * slot[i], routed(i), rbs[i].fabric_id)?;
    }

    let free = |s: &u8| !slot.contains(s);
    let second = (n < 8 && ((above_small && above > 0) || (below > 0 && (below_small || n == 1))))
        .then(|| (0..max_slots).find(free))
        .flatten()
        .map(|s| {
            let w = if above_small {
                Window::new(cfg_end, above + PRIMARY_EXTENSION)
            } else {
                Window::new(input.tom, below)
            };
            (s, w)
        });
    if let Some((s, w)) = second {
        push(2 * s, w, rbs[p].fabric_id)?;
    }

    // Above 4 GiB.
    let mut limit = match policy.above_limit {
        AboveLimit::BelowTop(r) => (1u64 << input.phys_addr_bits).saturating_sub(r),
        AboveLimit::Fixed(l) => l,
    };
    if let Some(cap) = policy.above_cap.filter(|&c| c < limit) {
        limit = (cap + 1) & !0xFFFF;
    }
    let mut base4 = input.tom2.max(4 * GIB);
    let span1 = if base4 < HT_HOLE {
        limit.min(HT_HOLE).saturating_sub(base4)
    } else {
        0
    };
    let span2 = if limit > TIB {
        limit.saturating_sub(base4.max(TIB))
    } else {
        0
    };
    if span1 == 0 && span2 == 0 {
        limit = 0;
    } else if span2 <= span1 {
        limit = limit.min(HT_HOLE);
    } else {
        base4 = input.tom2.max(TIB);
    }
    let mut above_w: [Option<Window>; MAX_ROOT_BRIDGES] = [None; MAX_ROOT_BRIDGES];
    if limit > base4 {
        let per_rb = ((limit - base4) / n) & ALIGN_16M;
        let mut cur = base4;
        // Physical order: the input order.
        for i in 0..rbs.len() {
            let sz = if i == rbs.len() - 1 {
                policy.above_last_end.unwrap_or(limit) - cur
            } else {
                per_rb
            };
            if sz >= policy.above_min {
                above_w[i] = Some(Window::new(cur, sz));
                push(2 * slot[i] + 1, Window::new(cur, sz), rbs[i].fabric_id)?;
            }
            cur += per_rb;
        }
    }

    // Spare window: the first free below pair (not the second window's),
    // else the first above pair a root bridge left unused.
    if let Some(spare) = policy.spare {
        let hit = (0..max_slots).find_map(|s| match slot.iter().position(|&x| x == s) {
            None if second.map(|(ss, _)| ss) != Some(s) => Some(2 * s),
            Some(i) if above_w[i].is_none() => Some(2 * s + 1),
            _ => None,
        });
        if let Some(pair) = hit {
            push(pair, spare, rbs[p].fabric_id)?;
        }
    }
    mmio.sort_unstable_by_key(|r| r.pair);

    // I/O, logical order.
    let order = logical(rbs);
    let mut io_w: [Option<Window>; MAX_ROOT_BRIDGES] = [None; MAX_ROOT_BRIDGES];
    match policy.io_sizes {
        None if n == 1 => io_w[p] = Some(Window::new(0, IO_SPACE)),
        None => {
            let q = (0xF000 / n) & !0xFFF;
            let mut cursor = q + IO_LEGACY;
            let mut k = 0;
            for &i in order.iter() {
                if i == p {
                    io_w[i] = Some(Window::new(0, q + IO_LEGACY));
                    continue;
                }
                let sz = if k == n - 2 { IO_SPACE - cursor } else { q };
                io_w[i] = Some(Window::new(cursor, sz));
                cursor += sz;
                k += 1;
            }
        }
        Some(sizes) => {
            let req = |i: usize| {
                let k = order.iter().position(|&x| x == i).unwrap_or(0);
                u64::from(sizes[k]) & !0xFFF
            };
            io_w[p] = Some(Window::new(0, req(p) + IO_LEGACY));
            let mut cursor = req(p) + IO_LEGACY;
            let mut last = p;
            for &i in order.iter().filter(|&&i| i != p && req(i) != 0) {
                io_w[i] = Some(Window::new(cursor, req(i)));
                cursor += req(i);
                last = i;
            }
            if cursor > IO_PORTS {
                return Err(PlanError::IoOverflow);
            }
            if let Some(w) = io_w[last].as_mut() {
                w.size = IO_SPACE - w.base;
            }
        }
    }
    let io: Vec<IoRoute, 8> = order
        .iter()
        .enumerate()
        .filter_map(|(k, &i)| {
            io_w[i].map(|window| IoRoute {
                pair: k as u8,
                window,
                fabric_id: rbs[i].fabric_id,
            })
        })
        .collect();

    let root_bridges = rbs
        .iter()
        .enumerate()
        .map(|(i, rb)| {
            let io_pool = io_w[i].and_then(|w| {
                let base = if i == p && w.base == 0 {
                    IO_LEGACY
                } else {
                    w.base
                };
                let end = w.end().min(IO_PORTS);
                (end > base).then(|| Window::new(base, end - base))
            });
            RootBridgePlan {
                socket: rb.socket,
                index: rb.index,
                fabric_id: rb.fabric_id,
                primary: rb.primary,
                bus: BusRange {
                    base: rb.bus_base,
                    limit: rb.bus_limit,
                },
                below: routed(i),
                below_pools: below_pools(below_w[i], policy.nonpci_below),
                second_below: (i == p).then_some(second.map(|(_, w)| w)).flatten(),
                above: above_w[i],
                above_pools: above_w[i].map(|w| above_pools(w, policy.nonpci_above)),
                io: io_w[i],
                io_pool,
            }
        })
        .collect();

    Ok(Plan {
        mmio,
        io,
        root_bridges,
        above_ecam_too_small: above_small,
        below_ecam_too_small: below_small,
        above_ecam: n_above as u8,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const ECAM: (u64, u64) = (0xE000_0000, 256 * MIB);

    /// `sockets` × `per` root bridges, IOMS order = bus order, primary at
    /// physical index `primary`.
    fn rbs(sockets: u8, per: u8, primary: u8) -> std::vec::Vec<RootBridge> {
        let n = u16::from(sockets * per);
        (0..sockets)
            .flat_map(|s| (0..per).map(move |i| (s, i)))
            .enumerate()
            .map(|(k, (s, i))| {
                let base = (k as u16 * (256 / n)) as u8;
                RootBridge {
                    socket: s,
                    index: i,
                    fabric_id: (u16::from(s) << 5) + 0x18 + u16::from(i),
                    bus_base: base,
                    bus_limit: base + (256 / n - 1) as u8,
                    primary: s == 0 && i == primary,
                }
            })
            .collect()
    }

    fn input(tom: u64, tom2: u64, rbs: &[RootBridge]) -> Input<'_> {
        Input {
            tom,
            tom2,
            ecam_base: ECAM.0,
            ecam_size: ECAM.1,
            phys_addr_bits: 43,
            root_bridges: rbs,
            mmio_pairs: 16,
        }
    }

    /// `(pair, base, end)` of every MMIO route.
    fn mmio(plan: &Plan) -> std::vec::Vec<(u8, u64, u64, u16)> {
        plan.mmio
            .iter()
            .map(|r| (r.pair, r.window.base, r.window.end(), r.fabric_id))
            .collect()
    }

    fn io(plan: &Plan) -> std::vec::Vec<(u8, u64, u64)> {
        plan.io
            .iter()
            .map(|r| (r.pair, r.window.base, r.window.end()))
            .collect()
    }

    #[test]
    fn rome_1p_low_tom() {
        // df3_plan(0x8000_0000, 0x10_8000_0000, 256 MiB, 1, 4): the window
        // above the ECAM is too small, so the primary takes the top of
        // [TOM, ECAM) and a second window [ECAM end, 0xFEC0_0000).
        let r = rbs(1, 4, 3);
        let plan = plan(&input(0x8000_0000, 0x10_8000_0000, &r), &Policy::df3()).unwrap();
        assert!(plan.above_ecam_too_small);
        assert_eq!(
            mmio(&plan),
            [
                (0, 0xB000_0000, 0xC800_0000, 0x18),
                (1, 0x100_0000_0000, 0x2BF_0000_0000, 0x18),
                (2, 0x9800_0000, 0xB000_0000, 0x19),
                (3, 0x2BF_0000_0000, 0x47E_0000_0000, 0x19),
                (4, 0x8000_0000, 0x9800_0000, 0x1A),
                (5, 0x47E_0000_0000, 0x63D_0000_0000, 0x1A),
                (6, 0xC800_0000, 0xE000_0000, 0x1B),
                (7, 0x63D_0000_0000, 0x7FC_0000_0000, 0x1B),
                (8, 0xF000_0000, 0xFEC0_0000, 0x1B),
                (10, 0xFED0_0000, 0xFED1_0000, 0x1B),
            ]
        );
        assert_eq!(
            io(&plan),
            [
                (0, 0x4000, 0x7000),
                (1, 0x7000, 0xA000),
                (2, 0xA000, 0x200_0000),
                (3, 0, 0x4000)
            ]
        );
        let primary = plan.root_bridges[3];
        assert_eq!(
            primary.second_below,
            Some(Window::new(0xF000_0000, 0xEC0_0000))
        );
        // 16 MiB Non-PCI at the top, 4/5 of the rest prefetchable.
        assert_eq!(
            primary.below_pools.non_pci,
            Window::new(0xDF00_0000, 16 * MIB)
        );
        assert_eq!(
            primary.below_pools.prefetch,
            Window::new(0xC800_0000, 0x1200_0000)
        );
        assert_eq!(primary.io_pool, Some(Window::new(0x1000, 0x3000)));
        // Above: Non-PCI at the 256 MiB aligned base.
        let above = primary.above_pools.unwrap();
        assert_eq!(above.non_pci, Window::new(0x63D_0000_0000, 0x2020_0000));
    }

    #[test]
    fn rome_2p_primary_above_ecam() {
        let r = rbs(2, 4, 3);
        let plan = plan(&input(0x8000_0000, 0x10_8000_0000, &r), &Policy::df3()).unwrap();
        assert_eq!(plan.above_ecam, 1);
        let m = mmio(&plan);
        assert_eq!(
            m.len(),
            16,
            "no second or spare window on 2P: every pair used"
        );
        assert_eq!(m[6], (6, 0xF000_0000, 0xFEC0_0000, 0x1B));
        assert_eq!(m[0], (0, 0xCE00_0000, 0xE000_0000, 0x18));
        assert_eq!(m[14], (14, 0x8000_0000, 0x8D00_0000, 0x3B));
        assert_eq!(m[15], (15, 0x71C_8000_0000, 0x7FC_0000_0000, 0x3B));
        assert_eq!(io(&plan)[3], (3, 0, 0x2000));
        assert_eq!(io(&plan)[7], (7, 0x8000, 0x200_0000));
    }

    #[test]
    fn rome_1p_tom_3g() {
        let r = rbs(1, 4, 3);
        let mut i = input(0xC000_0000, 0x40_0000_0000, &r);
        i.phys_addr_bits = 48;
        let plan = plan(&i, &Policy::df3()).unwrap();
        assert_eq!(
            mmio(&plan)
                .iter()
                .filter(|m| m.0 % 2 == 0)
                .copied()
                .collect::<std::vec::Vec<_>>(),
            [
                (0, 0xD400_0000, 0xE000_0000, 0x18),
                (2, 0xCA00_0000, 0xD400_0000, 0x19),
                (4, 0xC000_0000, 0xCA00_0000, 0x1A),
                (6, 0xF000_0000, 0xFEC0_0000, 0x1B),
                (8, 0xFED0_0000, 0xFED1_0000, 0x1B),
            ]
        );
    }

    #[test]
    fn below_ecam_too_small() {
        // df3_plan(0xE000_0000, 0, 256 MiB, 1, 4): everything above the ECAM.
        let r = rbs(1, 4, 3);
        let plan = plan(&input(0xE000_0000, 0, &r), &Policy::df3()).unwrap();
        assert!(plan.below_ecam_too_small);
        let below: std::vec::Vec<_> = mmio(&plan).into_iter().filter(|m| m.0 % 2 == 0).collect();
        assert_eq!(
            below,
            [
                (0, 0xF600_0000, 0xF900_0000, 0x18),
                (2, 0xF300_0000, 0xF600_0000, 0x19),
                (4, 0xF000_0000, 0xF300_0000, 0x1A),
                (6, 0xF900_0000, 0xFEC0_0000, 0x1B),
                (8, 0xFED0_0000, 0xFED1_0000, 0x1B),
            ]
        );
    }

    #[test]
    fn single_root_bridge() {
        let r = rbs(1, 1, 0);
        let plan = plan(&input(0x8000_0000, 0x2_0000_0000, &r), &Policy::df3()).unwrap();
        assert_eq!(
            mmio(&plan),
            [
                (0, 0xF000_0000, 0xFEC0_0000, 0x18),
                (1, 0x100_0000_0000, 0x7FC_0000_0000, 0x18),
                (2, 0x8000_0000, 0xE000_0000, 0x18),
                (4, 0xFED0_0000, 0xFED1_0000, 0x18),
            ]
        );
        assert_eq!(io(&plan), [(0, 0, 0x200_0000)]);
    }

    #[test]
    fn milan_fused_ioms_slots_stay_unique() {
        // Three IOMS: the second window and the spare take distinct free pairs.
        let r = rbs(1, 3, 2);
        let plan = plan(&input(0x8000_0000, 0x10_8000_0000, &r), &Policy::df3()).unwrap();
        let pairs: std::vec::Vec<u8> = plan.mmio.iter().map(|m| m.pair).collect();
        assert_eq!(pairs, [0, 1, 2, 3, 4, 5, 6, 8]);
    }

    #[test]
    fn naples_sp4_worked_example() {
        // SP4r2 SOC_DISPATCH §4: 2 dies, TOM 0xE000_0000, TOM2 4.5 GiB,
        // 64 MiB ECAM at 0xF000_0000.
        let r: std::vec::Vec<RootBridge> = (0..2u8)
            .map(|d| RootBridge {
                socket: 0,
                index: d,
                fabric_id: u16::from(d) << 5 | 4,
                bus_base: d * 0x40,
                bus_limit: d * 0x40 + 0x3F,
                primary: d == 0,
            })
            .collect();
        let i = Input {
            tom: 0xE000_0000,
            tom2: 0x1_2000_0000,
            ecam_base: 0xF000_0000,
            ecam_size: 64 * MIB,
            phys_addr_bits: 48,
            root_bridges: &r,
            mmio_pairs: 16,
        };
        let plan = plan(&i, &Policy::naples()).unwrap();
        assert_eq!(
            mmio(&plan),
            [
                (0, 0xF400_0000, 0xFEC0_0000, 0x04),
                (1, 0x100_0000_0000, 0x47E_8000_0000, 0x04),
                (2, 0xE000_0000, 0xF000_0000, 0x24),
                (3, 0x47E_8000_0000, 1 << 48, 0x24),
                (4, 0xFED0_0000, 0xFED1_0000, 0x04),
            ]
        );
        assert_eq!(io(&plan), [(0, 0, 0x8000), (1, 0x8000, 0x200_0000)]);
        // 8 MiB Non-PCI, no Non-PCI above 4 GiB.
        assert_eq!(plan.root_bridges[1].below_pools.non_pci.size, 8 * MIB);
        assert_eq!(plan.root_bridges[1].above_pools.unwrap().non_pci.size, 0);
    }

    #[test]
    fn naples_2p_uses_all_sixteen_pairs() {
        let r: std::vec::Vec<RootBridge> = (0..8u8)
            .map(|k| RootBridge {
                socket: k / 4,
                index: k % 4,
                fabric_id: u16::from(k / 4) << 7 | u16::from(k % 4) << 5 | 4,
                bus_base: k * 0x10,
                bus_limit: k * 0x10 + 0xF,
                primary: k == 0,
            })
            .collect();
        let i = Input {
            tom: 0x8000_0000,
            tom2: 0x20_0000_0000,
            ecam_base: 0xF000_0000,
            ecam_size: 128 * MIB,
            phys_addr_bits: 48,
            root_bridges: &r,
            mmio_pairs: 16,
        };
        let plan = plan(&i, &Policy::naples()).unwrap();
        assert_eq!(plan.mmio.len(), 16);
        assert_eq!(plan.mmio[15].fabric_id, 0xE4);
        assert_eq!(plan.mmio[15].window.end(), 1 << 48);
    }

    #[test]
    fn tyan_io_and_buses() {
        // S8030 (TyanInitFabricResource): 4 × 64 buses, I/O 0x4000,
        // 0x4000, 0x4000, 0x3000 in logical order.
        let mut r = rbs(1, 4, 3);
        let mut io_sizes = [0; MAX_ROOT_BRIDGES];
        io_sizes[..4].copy_from_slice(&[0x4000, 0x4000, 0x4000, 0x3000]);
        let mut buses = [0; MAX_ROOT_BRIDGES];
        buses[..4].copy_from_slice(&[0x40; 4]);
        let policy = Policy::df3().with_io_sizes(io_sizes).with_bus_counts(buses);
        assert_eq!(plan_buses(&r, &policy), Ok(None), "ABL map already matches");

        // A 2P-style ABL map (32 buses each) is reprogrammed: primary first.
        for (k, rb) in r.iter_mut().enumerate() {
            rb.bus_base = k as u8 * 0x20;
            rb.bus_limit = rb.bus_base + 0x1F;
        }
        let ranges = plan_buses(&r, &policy).unwrap().unwrap();
        let got: std::vec::Vec<(u8, u8)> = ranges.iter().map(|b| (b.base, b.limit)).collect();
        assert_eq!(got, [(0x40, 0x7F), (0x80, 0xBF), (0xC0, 0xFF), (0, 0x3F)]);
        for (rb, b) in r.iter_mut().zip(ranges.iter()) {
            rb.bus_base = b.base;
            rb.bus_limit = b.limit;
        }
        // Primary is now logical 0 and gets its 0x4000 + 0x1000 legacy.
        let plan = plan(&input(0x8000_0000, 0x10_8000_0000, &r), &policy).unwrap();
        assert_eq!(
            io(&plan),
            [
                (0, 0, 0x5000),
                (1, 0x5000, 0x9000),
                (2, 0x9000, 0xD000),
                (3, 0xD000, 0x200_0000)
            ]
        );
        assert_eq!(
            plan.root_bridges[2].io_pool,
            Some(Window::new(0xD000, 0x3000))
        );
    }

    #[test]
    fn io_overflow_fails() {
        let r = rbs(1, 4, 3);
        let policy = Policy::df3().with_io_sizes([0x8000; MAX_ROOT_BRIDGES]);
        assert_eq!(
            plan(&input(0x8000_0000, 0, &r), &policy),
            Err(PlanError::IoOverflow)
        );
    }
}
