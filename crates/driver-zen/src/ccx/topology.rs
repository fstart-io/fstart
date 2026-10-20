//! Per-thread topology: APIC ID layout and the table scope predicates
//! (CCX_SP3 §3.1, §3.6).
//!
//! The APIC IDs are assigned by the PSP/ABL before reset release; this code
//! only decodes them. The layouts are those of the PPRs (54945 §2.1.12.2.1.3
//! for Naples; the CPUID Fn0000_000B shifts of 55803/55898 for Rome and
//! Milan) and of the donors' CRAT/SRAT helpers:
//!
//! | | SMT | no SMT |
//! |---|---|---|
//! | Naples | `socket<<6 \| die<<4 \| ccx<<3 \| core<<1 \| thread` | `socket<<6 \| die<<4 \| ccx<<3 \| core` |
//! | Rome | `socket<<7 \| ccd<<4 \| ccx<<3 \| core<<1 \| thread` | `socket<<6 \| ccd<<3 \| ccx<<2 \| core` |
//! | Milan | `socket<<7 \| ccd<<4 \| core<<1 \| thread` | `socket<<6 \| ccd<<3 \| core` |
//!
//! IDs are 32-bit (x2APIC wide, CPUID Fn8000_001E_EAX) even though SP3 never
//! exceeds 0xff: a 2P 64-core part uses 0xff, the xAPIC broadcast ID, which
//! is why it needs x2APIC.

use crate::soc::Generation;

/// The table scopes of the AGESA engine. Only `Thread`, `Core` and
/// `Complex` carry records on SP3; the dispatcher stops at the first scope
/// whose predicate fails.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Scope {
    /// 0: every thread.
    Thread,
    /// 1: the first thread of a core.
    Core,
    /// 2: the first thread of an L3 complex (CCX; on Milan the CCD).
    Complex,
    /// 3: the first thread of a die (Naples) or CCD (Rome); Milan's engine
    /// uses the socket mask here.
    Die,
    /// 4: the first thread of a socket.
    Socket,
    /// 5: the BSP.
    Bsp,
}

/// Where a thread sits. `die` is the ZP die on Naples and the CCD on Rome
/// and Milan; all fields are the logical numbers encoded in the APIC ID.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Location {
    pub socket: u8,
    pub die: u8,
    pub ccx: u8,
    pub core: u8,
    pub thread: u8,
}

/// The APIC ID layout of a generation with or without SMT.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApicLayout {
    pub generation: Generation,
    pub smt: bool,
}

/// Field widths from the least significant bit up.
struct Widths {
    thread: u32,
    core: u32,
    /// Unused bit: Naples keeps the core field 3 bits wide without SMT.
    pad: u32,
    ccx: u32,
    die: u32,
}

impl ApicLayout {
    pub const fn new(generation: Generation, smt: bool) -> Self {
        Self { generation, smt }
    }

    const fn widths(self) -> Widths {
        let thread = self.smt as u32;
        match self.generation {
            Generation::Naples => Widths {
                thread,
                core: 2,
                pad: 1 - thread,
                ccx: 1,
                die: 2,
            },
            Generation::Rome => Widths {
                thread,
                core: 2,
                pad: 0,
                ccx: 1,
                die: 3,
            },
            Generation::Milan => Widths {
                thread,
                core: 3,
                pad: 0,
                ccx: 0,
                die: 3,
            },
        }
    }

    /// Bit position of the socket field.
    pub const fn socket_shift(self) -> u32 {
        let w = self.widths();
        w.thread + w.core + w.pad + w.ccx + w.die
    }

    /// Cores per L3 complex the layout has room for.
    pub const fn cores_per_complex(self) -> u32 {
        1 << self.widths().core
    }

    /// Decode an APIC ID.
    pub const fn decode(self, apic_id: u32) -> Location {
        let w = self.widths();
        const fn field(v: u32, shift: u32, bits: u32) -> u8 {
            ((v >> shift) & ((1 << bits) - 1)) as u8
        }
        let core = w.thread;
        let ccx = core + w.core + w.pad;
        let die = ccx + w.ccx;
        Location {
            socket: field(apic_id, die + w.die, 1),
            die: field(apic_id, die, w.die),
            ccx: field(apic_id, ccx, w.ccx),
            core: field(apic_id, core, w.core),
            thread: field(apic_id, 0, w.thread),
        }
    }

    /// Encode a location (inverse of [`decode`](Self::decode)).
    pub const fn encode(self, l: Location) -> u32 {
        let w = self.widths();
        let core = w.thread;
        let ccx = core + w.core + w.pad;
        let die = ccx + w.ccx;
        (l.socket as u32) << (die + w.die)
            | (l.die as u32) << die
            | (l.ccx as u32) << ccx
            | (l.core as u32) << core
            | l.thread as u32
    }

    /// The APIC ID mask whose bits must be clear for `scope`, exactly as the
    /// engines test it (ZP `ffdc3dea`, SSP `ffdc509e`, GN `ffcff609`,
    /// instruction-checked). `None` for `Thread` and `Bsp`.
    pub const fn scope_mask(self, scope: Scope) -> Option<u32> {
        // (with SMT, without SMT)
        let (smt, single) = match (self.generation, scope) {
            (_, Scope::Thread | Scope::Bsp) => return None,
            (_, Scope::Core) => (1, 0),
            (Generation::Naples, Scope::Complex) => (0x7, 0x7),
            (Generation::Naples, Scope::Die) => (0xf, 0xf),
            (Generation::Naples, Scope::Socket) => (0x3f, 0x3f),
            (Generation::Rome, Scope::Complex) => (0x7, 0x3),
            (Generation::Rome, Scope::Die) => (0xf, 0x7),
            (Generation::Milan, Scope::Complex) => (0xf, 0x7),
            (Generation::Rome | Generation::Milan, _) => (0x7f, 0x3f),
        };
        Some(if self.smt { smt } else { single })
    }
}

/// The executing thread as the CCX code sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Thread {
    pub layout: ApicLayout,
    /// x2APIC-wide APIC ID.
    pub apic_id: u32,
    pub bsp: bool,
}

impl Thread {
    pub const fn new(generation: Generation, apic_id: u32, smt: bool, bsp: bool) -> Self {
        Self {
            layout: ApicLayout::new(generation, smt),
            apic_id,
            bsp,
        }
    }

    /// From CPUID Fn8000_001E EAX (ExtendedApicId; valid with the APIC
    /// enabled) and EBX (ThreadsPerCore in \[15:8\]) and APIC_BAR.BSP.
    pub const fn from_cpuid(generation: Generation, ext_eax: u32, ext_ebx: u32, bsp: bool) -> Self {
        Self::new(generation, ext_eax, (ext_ebx >> 8) & 0xff != 0, bsp)
    }

    pub const fn generation(&self) -> Generation {
        self.layout.generation
    }

    pub const fn smt(&self) -> bool {
        self.layout.smt
    }

    pub const fn location(&self) -> Location {
        self.layout.decode(self.apic_id)
    }

    /// The scope predicate of the table engine.
    pub const fn in_scope(&self, scope: Scope) -> bool {
        match scope {
            Scope::Bsp => self.bsp,
            _ => match self.layout.scope_mask(scope) {
                Some(mask) => self.apic_id & mask == 0,
                None => true,
            },
        }
    }

    /// The first (or only) thread of its core: owner of core-wide MSRs.
    pub const fn primary(&self) -> bool {
        self.in_scope(Scope::Core)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Generation::*;

    #[test]
    fn layouts_round_trip() {
        let l = Location {
            socket: 1,
            die: 5,
            ccx: 1,
            core: 2,
            thread: 1,
        };
        let rome = ApicLayout::new(Rome, true);
        assert_eq!(rome.encode(l), 0x80 | 5 << 4 | 1 << 3 | 2 << 1 | 1);
        assert_eq!(rome.decode(rome.encode(l)), l);
        let milan = ApicLayout::new(Milan, false);
        let m = Location {
            socket: 1,
            die: 7,
            ccx: 0,
            core: 7,
            thread: 0,
        };
        assert_eq!(milan.encode(m), 0x40 | 7 << 3 | 7);
        assert_eq!(milan.decode(0x7f), m);
        // Naples without SMT keeps the CCX at bit 3 (PPR 54945 ApicId[2:0]).
        let naples = ApicLayout::new(Naples, false);
        assert_eq!(naples.decode(0x4b).ccx, 1);
        assert_eq!(naples.decode(0x4b).core, 3);
        assert_eq!(naples.socket_shift(), 6);
        assert_eq!(ApicLayout::new(Milan, true).socket_shift(), 7);
    }

    #[test]
    fn complex_scope_is_the_first_thread_of_the_l3() {
        // The engine's literal masks agree with the layouts.
        for (g, smt) in [Naples, Rome, Milan]
            .into_iter()
            .flat_map(|g| [(g, true), (g, false)])
        {
            let layout = ApicLayout::new(g, smt);
            let mask = layout.scope_mask(Scope::Complex).unwrap();
            let complex_bits = layout.encode(Location {
                ccx: 1,
                ..Location::default()
            });
            let below = if g == Milan {
                layout.encode(Location {
                    die: 1,
                    ..Location::default()
                })
            } else {
                complex_bits
            };
            assert_eq!(mask, below - 1, "{g:?} smt={smt}");
        }
    }

    #[test]
    fn scope_predicates() {
        let t = |g, id, smt| Thread::new(g, id, smt, false);
        assert!(t(Rome, 0x08, true).in_scope(Scope::Complex));
        assert!(!t(Rome, 0x08, true).in_scope(Scope::Die), "CCX 1 of CCD 0");
        assert!(t(Milan, 0x10, true).in_scope(Scope::Complex));
        assert!(
            !t(Milan, 0x10, true).in_scope(Scope::Die),
            "GN scope 3 is the socket"
        );
        assert!(!t(Naples, 0x01, true).primary());
        assert!(t(Naples, 0x01, false).primary());
        assert!(Thread::new(Milan, 0x81, true, true).in_scope(Scope::Bsp));
        let ap = Thread::from_cpuid(Milan, 0xff, 0x0100, false);
        assert!(ap.smt() && ap.location().socket == 1 && !ap.primary());
    }
}
