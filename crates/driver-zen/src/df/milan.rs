//! Milan (GN) fabric init: the Rome sequence with the GN deltas of
//! AmdFabricSspPei 1.0.0.G (SOC_DF §3.6):
//!
//! - clock gating F0x3F0 low nibble cleared when performance tracing is off
//!   (PCD 0x196);
//! - no F4xC8 write (it is skipped for the GN logical family);
//! - PIE RAS status only logged, F0x3F8 bit 3 = `!PCD 0x41`;
//! - a multi-region scrubber (`ffd02151`, DC): the socket's DRAM rules are
//!   merged into at most four regions and programmed into F6x40/44/48/4C
//!   +16·k, with the sequential-scrub mode from the interleave;
//! - Misc: F5xF8 bit 10.
//!
//! Order: clock gating → CS → PIE RAS → scrubber → misc.

use fstart_pci::ConfigRegionAccess;
use heapless::Vec;
use tock_registers::interfaces::{ReadWriteable, Readable};

use super::topology::Topology;
use super::v3::{
    self, ChannelCtl, Df3, PIE, RasFields, SCRUB_ADDR_HI, SCRUB_ADDR_LO, SCRUB_BASE, SCRUB_LIMIT,
    SocketMemory,
};
use super::{Df, DramRule, Reg, Target, dram, or_bits};

/// Scrubber regions per socket (GN SoC table `ffd06ab4`: PIE 0x1E, CS
/// base 0, 4 regions; OB).
pub const SCRUBBERS: usize = 4;
/// F5xF8: bit 10 set on GN (Misc Init, IC).
const F5XF8: Reg = Reg::new(5, 0xF8);
/// DRAM rules the GN scrubber code reads (F0x110..F0x14C).
const SCRUB_RULES: usize = 8;

/// Milan policy; `Default` is the donor (milan-s8030-v404) default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    /// F0x3F8: PCD 0x17A (keep), 0x17B (0xFF → 6), bit 3 = `!PCD 0x41`
    /// (TRUE → clear).
    pub ras: RasFields,
    /// PCD 0x196 "performance tracing" option: clears F0x3F0\[3:0\].
    pub perf_tracing_off: bool,
    /// Scrub period in hours, 0 = off (PCD 0x16B: 0xFF → 24 h).
    pub scrub_hours: u8,
    /// F6xB8: CTL0 = PCD 0x16D, CTL1 = PCD 0x16C (TRUE), mode PCD 0x16E.
    pub scrub_channel: ChannelCtl,
    /// PCD 0x16F (TRUE): rate = SeqScrubMode (1, 2) or 0; F2x228 field 0.
    pub scrub_rate_cap: bool,
    /// PCD 0x42 (0xFF = `None`): merge regions down to this many.
    pub scrubbers: Option<u8>,
    /// F6x4C\[31:28\] NumSeqScrubsCstateExit (PCD 0x43: 0xFF → 0xF).
    pub num_seq_scrubs_cstate_exit: u8,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            ras: RasFields::default(),
            perf_tracing_off: false,
            scrub_hours: 24,
            scrub_channel: ChannelCtl {
                ctl0: true,
                ctl1: true,
                mode: ChannelCtl::mode_from_option(0xFF),
            },
            scrub_rate_cap: true,
            scrubbers: None,
            num_seq_scrubs_cstate_exit: 0xF,
        }
    }
}

/// PEI fabric init on every socket. `memory[socket]` from the APOB.
pub fn init<P: ConfigRegionAccess + Clone>(
    df: &Df<P>,
    topology: &Topology,
    policy: &Policy,
    memory: &[SocketMemory],
) {
    let sockets = topology.sockets;
    v3::clock_gating(
        df,
        sockets,
        if policy.perf_tracing_off { 1 & !0xF } else { 1 },
    );
    v3::cs_init(df, sockets);
    v3::pie_ras(df, sockets, policy.ras, false);
    for (s, mem) in (0..sockets).zip(memory) {
        scrub(df, s, policy, mem);
    }
    for s in 0..sockets {
        // SAFETY: F5xF8 of an existing node.
        let r = unsafe { df.node(s, F5XF8) };
        or_bits(&r, 0x400);
    }
}

/// The two regions touch, directly or across the MMIO hole.
fn joinable(a: &DramRule, b: &DramRule, hole: Option<u64>) -> bool {
    let (lo, hi) = if b.base < a.base { (b, a) } else { (a, b) };
    lo.end == hi.base || (hi.base == 1 << 32 && hole == Some(lo.end))
}

/// No socket/die interleave and a channel interleave of 1, 2 or COD4_2
/// (`ffd0304a`).
fn simple(r: &DramRule) -> bool {
    !r.socket_interleave && r.die_interleave == 0 && matches!(r.channel_interleave, 0 | 1 | 0xC)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Interleave {
    /// Pass 1 (`0x80000001`): COD4_2 stays, anything else becomes 2-channel.
    Keep,
    /// Forced merges: interleave field 0.
    Clear,
}

/// `a` absorbs `b` (`ffd02f18`): a's DstFabricID, the union of both
/// ranges, the hole flag of either.
fn merge(a: DramRule, b: DramRule, ilv: Interleave) -> DramRule {
    DramRule {
        base: a.base.min(b.base),
        end: a.end.max(b.end),
        hole: a.hole || b.hole,
        channel_interleave: match ilv {
            Interleave::Keep if a.channel_interleave == 0xC && b.channel_interleave == 0xC => 0xC,
            Interleave::Keep => 1,
            Interleave::Clear => 0,
        },
        ..a
    }
}

/// The socket's scrub regions from the DRAM rules (DC, `ffd02151`): rules
/// to one of the socket's CS (UMC 0–7), merged
/// 1. pairwise forward when both are simple and target the same CS pair,
/// 2. forward regardless of interleave down to `limit` (only when the
///    policy sets a count),
/// 3. backward down to [`SCRUBBERS`].
pub fn scrub_regions(
    rules: &[DramRule],
    socket: u8,
    hole: Option<u64>,
    limit: Option<u8>,
) -> Vec<DramRule, SCRUB_RULES> {
    let mut r: Vec<Option<DramRule>, SCRUB_RULES> = rules
        .iter()
        .filter(|x| (x.dst >> 5) & 1 == u16::from(socket) && x.dst & 0x1F < 8)
        .take(SCRUB_RULES)
        .map(|&x| Some(x))
        .collect();
    let n = r.len();
    let mut count = n;

    for i in 1..n {
        for j in i..n {
            if let (Some(a), Some(b)) = (r[i - 1], r[j])
                && joinable(&a, &b, hole)
                && simple(&a)
                && simple(&b)
                && (a.dst ^ b.dst) & 0xFE == 0
            {
                r[i - 1] = Some(merge(a, b, Interleave::Keep));
                r[j] = None;
                count -= 1;
            }
        }
    }

    let mut squeeze = |r: &mut Vec<Option<DramRule>, SCRUB_RULES>,
                       target: usize,
                       pairs: &mut dyn Iterator<Item = (usize, usize)>| {
        for (keep, absorb) in pairs {
            if count <= target {
                break;
            }
            if let (Some(a), Some(b)) = (r[keep], r[absorb])
                && joinable(&a, &b, hole)
            {
                r[keep] = Some(merge(a, b, Interleave::Clear));
                r[absorb] = None;
                count -= 1;
            }
        }
    };
    if let Some(l) = limit {
        let target = usize::from(l).clamp(1, SCRUBBERS);
        squeeze(
            &mut r,
            target,
            &mut (0..n).flat_map(|i| (i + 1..n).map(move |j| (i, j))),
        );
    }
    squeeze(
        &mut r,
        SCRUBBERS,
        &mut (0..n)
            .rev()
            .flat_map(|hi| (0..hi).rev().map(move |lo| (lo, hi))),
    );
    r.into_iter().flatten().collect()
}

/// System interleave mode (`ffd03077`): 0 socket interleave without COD, 1
/// all COD1_8, 2 all COD2_4, 3 all COD4_2, 4 other channel interleave, 5
/// none.
pub fn interleave_mode(regions: &[DramRule]) -> u8 {
    let n = regions.len();
    let count = |c: u8| regions.iter().filter(|r| r.channel_interleave == c).count();
    let cod = count(0xC) + count(0xD) + count(0xE);
    if regions.iter().any(|r| r.socket_interleave) {
        return if cod == 0 { 0 } else { 5 };
    }
    match () {
        _ if n == count(0xC) => 3,
        _ if n == count(0xD) => 2,
        _ if n == count(0xE) => 1,
        _ if regions
            .iter()
            .any(|r| matches!(r.channel_interleave, 1 | 3 | 5 | 6)) =>
        {
            4
        }
        _ => 5,
    }
}

/// SeqScrubMode from the interleave mode.
pub const fn seq_scrub_mode(mode: u8) -> u8 {
    match mode {
        0 | 1 => 2,
        2 => 1,
        _ => 0,
    }
}

/// Register `reg` of scrubber region `k`.
fn at<R: tock_registers::RegisterLongName>(k: usize, reg: Reg<R>) -> Reg<R> {
    reg.at(k as u8, v3::SCRUB_REGION_STRIDE)
}

fn scrub<P: ConfigRegionAccess + Clone>(
    df: &Df<P>,
    socket: u8,
    policy: &Policy,
    mem: &SocketMemory,
) {
    let map = dram::read::<Df3, _>(df, socket, Target::Instance(PIE));
    let rules: Vec<DramRule, 16> = map.rules.iter().take(SCRUB_RULES).copied().collect();
    let regions = scrub_regions(&rules, socket, map.hole_base, policy.scrubbers);
    // SAFETY: scrubber registers of an existing node.
    if unsafe { df.node(socket, v3::SCRUB_LIMIT) }.get() == 0 || mem.present == 0 {
        return;
    }
    let size = regions.iter().fold(0u64, |s, r| {
        let hole = map
            .hole_base
            .filter(|_| r.hole)
            .map_or(0, |h| (1 << 32) - h);
        s + (r.end - r.base) - hole
    });
    if size == 0 {
        return;
    }
    let ctl = policy.scrub_channel;
    if ctl.ctl0 || ctl.ctl1 {
        v3::scrub_channels(df, socket, mem.channels, mem.present, ctl);
    }
    if policy.scrub_hours == 0 {
        return;
    }
    let seq = seq_scrub_mode(interleave_mode(&regions));
    let used = regions.len().min(SCRUBBERS);
    // SAFETY (block): scrubber region registers 0..4 of an existing node.
    unsafe {
        for (k, r) in regions.iter().take(SCRUBBERS).enumerate() {
            df.node(socket, at(k, v3::SCRUB_BASE)).modify(
                SCRUB_BASE::BASE.val((r.base >> 20) as u32)
                    + SCRUB_BASE::EN::CLEAR
                    + SCRUB_BASE::REGION_EN::CLEAR,
            );
            df.node(socket, at(k, v3::SCRUB_LIMIT))
                .modify(SCRUB_LIMIT::LIMIT.val(((r.end >> 20) - 1) as u32));
            df.node(socket, at(k, SCRUB_ADDR_HI))
                .modify(v3::SCRUB_ADDR_HI::ADDR.val((r.base >> 32) as u32));
            df.node(socket, at(k, SCRUB_ADDR_LO))
                .modify(v3::SCRUB_ADDR_LO::ADDR.val((r.base as u32) >> 6));
        }
        df.node(socket, SCRUB_ADDR_HI).modify(
            v3::SCRUB_ADDR_HI::SEQ_SCRUB_MODE.val(u32::from(seq))
                + v3::SCRUB_ADDR_HI::NUM_SEQ_SCRUBS_CSTATE_EXIT
                    .val(u32::from(policy.num_seq_scrubs_cstate_exit.min(0xF))),
        );
        let rate = if policy.scrub_rate_cap {
            if matches!(seq, 1 | 2) { seq } else { 0 }
        } else {
            v3::scrub_rate(policy.scrub_hours, size, 6 + u32::from(seq))
        };
        df.node(socket, SCRUB_LIMIT)
            .modify(v3::SCRUB_LIMIT::RATE.val(u32::from(rate)));
        for k in (0..used).rev() {
            let en = if k == 0 {
                SCRUB_BASE::EN::SET
            } else {
                SCRUB_BASE::EN::CLEAR
            };
            df.node(socket, at(k, SCRUB_BASE))
                .modify(SCRUB_BASE::REGION_EN::SET + en);
        }
        // Every GN revision takes F2x228.
        v3::cs_f2x228(df, socket, mem.channels, rate, policy.scrub_rate_cap);
    }
}

#[cfg(test)]
mod tests {
    use super::super::model::DfModel;
    use super::*;
    use crate::soc::Generation;
    use crate::topology::Node;

    const GIB: u64 = 1 << 30;

    fn rule(base: u64, end: u64, dst: u16, chan: u8, hole: bool) -> DramRule {
        DramRule {
            base,
            end,
            dst,
            hole,
            socket_interleave: false,
            die_interleave: 0,
            channel_interleave: chan,
        }
    }

    #[test]
    fn nps4_regions_merge_across_the_hole() {
        // NPS4: four COD4_2 quadrants, the first split by the 2 GiB hole.
        let rules = [
            rule(0, 2 * GIB, 0, 0xC, false),
            rule(4 * GIB, 18 * GIB, 0, 0xC, true),
            rule(18 * GIB, 34 * GIB, 2, 0xC, false),
            rule(34 * GIB, 50 * GIB, 4, 0xC, false),
            rule(50 * GIB, 66 * GIB, 6, 0xC, false),
            // Socket 1 and a non-CS rule are not this socket's.
            rule(66 * GIB, 82 * GIB, 0x20, 0xC, false),
        ];
        let r = scrub_regions(&rules, 0, Some(2 * GIB), None);
        let got: std::vec::Vec<_> = r
            .iter()
            .map(|x| (x.base, x.end, x.dst, x.channel_interleave))
            .collect();
        assert_eq!(
            got,
            [
                (0, 18 * GIB, 0, 0xC),
                (18 * GIB, 34 * GIB, 2, 0xC),
                (34 * GIB, 50 * GIB, 4, 0xC),
                (50 * GIB, 66 * GIB, 6, 0xC)
            ]
        );
        assert_eq!(seq_scrub_mode(interleave_mode(&r)), 0);
    }

    #[test]
    fn forced_merges() {
        let rules: std::vec::Vec<_> = (0..6u64)
            .map(|k| rule(k * GIB, (k + 1) * GIB, k as u16, 0x5, false))
            .collect();
        // Pass 1 does not merge (8-channel interleave); pass 3 merges
        // backward down to four.
        let r = scrub_regions(&rules, 0, None, None);
        assert_eq!(r.len(), 4);
        assert_eq!((r[0].base, r[0].end, r[0].channel_interleave), (0, GIB, 5));
        assert_eq!(
            (r[3].base, r[3].end, r[3].channel_interleave),
            (3 * GIB, 6 * GIB, 0)
        );
        // A policy count forces forward merges first.
        let r = scrub_regions(&rules, 0, None, Some(1));
        assert_eq!(r.len(), 1);
        assert_eq!((r[0].base, r[0].end), (0, 6 * GIB));
    }

    #[test]
    fn interleave_modes() {
        let r = |c| [rule(0, GIB, 0, c, false)];
        assert_eq!(interleave_mode(&r(0xE)), 1);
        assert_eq!(interleave_mode(&r(0xD)), 2);
        assert_eq!(interleave_mode(&r(0x5)), 4);
        assert_eq!(interleave_mode(&r(0)), 5);
        let mut s = r(0x5);
        s[0].socket_interleave = true;
        assert_eq!(interleave_mode(&s), 0);
    }

    #[test]
    fn init_programs_regions() {
        let m = DfModel::default();
        let df = Df::new(m.clone());
        // NPS1: one COD1_8 rule [0, 66 GiB) with the 2 GiB hole.
        m.set(0, Some(PIE), 0, 0x110, 0xE << 2 | 3);
        m.set(0, Some(PIE), 0, 0x114, (66 * 4 - 1) << 12);
        m.set(0, Some(PIE), 0, 0x104, 0x8000_0001);
        m.set(0, None, 6, 0x44, 0x1);
        let t = Topology {
            generation: Generation::Milan,
            sockets: 1,
            nodes: [Node {
                socket: 0,
                die: 0,
                device: 0x18,
            }]
            .into_iter()
            .collect(),
            root_bridges: heapless::Vec::new(),
        };
        let mem = SocketMemory {
            channels: 8,
            present: 0xFF,
        };
        init(&df, &t, &Policy::default(), &[mem]);
        // Region 0: [0, 66 GiB), SeqScrubMode 2 (COD1_8), rate = 2 (capped),
        // region and global enable.
        assert_eq!(m.get(0, None, 6, 0x40), 3);
        assert_eq!(m.get(0, None, 6, 0x44), ((66 << 10) - 1) << 4 | 2);
        assert_eq!(m.get(0, None, 6, 0x4C), 0xF200_0000);
        assert_eq!(m.get(0, None, 6, 0x48), 0);
        assert_eq!(m.get(0, None, 5, 0xF8), 0x400);
        assert_eq!(m.get(0, None, 0, 0x3F0), 1);
        assert_eq!(m.get(0, None, 4, 0xC8), 0, "no F4xC8 on GN");
        assert_eq!(m.get(0, Some(5), 2, 0x228), 0x0100_0000);
    }
}
