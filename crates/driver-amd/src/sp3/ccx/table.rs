//! The MSR table engine (CCX_SP3 §3.1, instruction-checked).
//!
//! Naples' engine has 11 opcodes, Rome's and Milan's 13; on SP3 only the MSR
//! opcodes appear and they differ only in their guards:
//!
//! | Opcode | ZP | SSP / GN |
//! |---|---|---|
//! | 3 | plain | plain |
//! | 4 | CPU logical ID | CPU logical ID |
//! | 5 | platform flags | core logical ID |
//! | 6 | – | platform flags |
//! | 7 | – | CPU logical ID + platform flags |
//!
//! so one [`Record`] with optional guards covers both. A record writes
//! `msr = (msr & !clear) | set`. The tables run on every thread (the BSP in
//! PEI, each AP from its start routine), scopes in order; the first scope
//! whose predicate fails ends the run.

use fstart_arch::x86::cpu::msr::MsrAccess;

use super::logical::LogicalId;
use super::topology::{Scope, Thread};
use super::x2apic::ApicMode;
use crate::sp3::soc::Generation;

/// A logical-ID guard: passes if the family and the revision bits overlap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Guard {
    pub family: u32,
    pub revisions: u32,
}

/// A platform-flag requirement: any of the bits, or all of them (AGESA's
/// bit 31 of the requested mask).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    Any(u32),
    All(u32),
}

impl Platform {
    pub const fn any(bits: u32) -> Self {
        Self::Any(bits)
    }

    pub const fn matches(self, flags: PlatformFlags) -> bool {
        match self {
            Self::Any(bits) => flags.0 & bits != 0,
            Self::All(bits) => flags.0 & bits == bits,
        }
    }
}

/// The platform flags of one engine (bit meanings differ per generation:
/// [`plat`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PlatformFlags(pub u32);

/// Platform-flag bits per engine (builders ZP `ffdc39e1`, SSP `ffdc5027`,
/// GN `ffcff518`). Bit 0 (AM4 package) never applies on SP3 and its records
/// are dropped by the generator; Naples uses no other bit.
pub mod plat {
    /// Rome.
    pub mod ssp {
        /// PcdAmdX2ApicMode TRUE.
        pub const X2APIC: u32 = 1 << 1;
        /// PcdAmdX2ApicMode FALSE.
        pub const XAPIC: u32 = 1 << 2;
        /// PEI PCD 0xbe: FEATURE_EXT_ID bit 31 on B0; no donor PEIM sets it
        /// and its purpose is not public.
        pub const IBS_31: u32 = 1 << 3;
    }

    /// Milan.
    pub mod gn {
        /// PkgType 4 (SP3) on a part other than the Rome-family special case.
        pub const SP3: u32 = 1 << 3;
        /// PcdAmdApicMode == x2APIC.
        pub const X2APIC: u32 = 1 << 4;
        /// PcdAmdApicMode == xAPIC.
        pub const XAPIC: u32 = 1 << 5;
        /// PEI PCD 0x48 FALSE (the donor default; no record uses it).
        pub const PCD_48_FALSE: u32 = 1 << 6;
        /// PEI PCD 0x40\[3:0\] != 0: hide RDRAND and RDSEED from CPUID.
        pub const HIDE_RNG: u32 = 1 << 7;
        /// PEI PCD 0x2d: FEATURE_EXT_ID bit 31 on B0+ (as Rome's PCD 0xbe).
        pub const IBS_31: u32 = 1 << 8;
    }
}

/// The table policy: what the PCD-driven platform flags select.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TablePolicy {
    /// Milan: clear CPUID RDRAND (C001_1004\[62\]) and RDSEED
    /// (C001_1002\[18\]). Donor default off.
    pub hide_rng: bool,
    /// Rome/Milan: set FEATURE_EXT_ID (C001_10DC) bit 31 (an IBS CPUID bit,
    /// purpose not public). Donor default off.
    pub ibs_31: bool,
}

impl PlatformFlags {
    /// The flags an SP3 platform passes to the engine of `generation` with
    /// the resolved APIC mode (CCX_SP3 §3.1: ZP 0; SSP 0x4/0x2; GN
    /// 0x68/0x58 with the donor defaults).
    pub const fn sp3(generation: Generation, apic: ApicMode, policy: TablePolicy) -> Self {
        const fn bit(on: bool, b: u32) -> u32 {
            if on { b } else { 0 }
        }
        let x2 = matches!(apic, ApicMode::X2Apic);
        let xapic = matches!(apic, ApicMode::XApic);
        Self(match generation {
            Generation::Naples => 0,
            Generation::Rome => {
                // A boolean PCD: anything but x2APIC counts as xAPIC.
                bit(x2, plat::ssp::X2APIC)
                    | bit(!x2, plat::ssp::XAPIC)
                    | bit(policy.ibs_31, plat::ssp::IBS_31)
            }
            Generation::Milan => {
                plat::gn::SP3
                    | plat::gn::PCD_48_FALSE
                    | bit(x2, plat::gn::X2APIC)
                    | bit(xapic, plat::gn::XAPIC)
                    | bit(policy.hide_rng, plat::gn::HIDE_RNG)
                    | bit(policy.ibs_31, plat::gn::IBS_31)
            }
        })
    }
}

/// One table record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Record {
    pub scope: Scope,
    pub cpu: Option<Guard>,
    pub core: Option<Guard>,
    pub platform: Option<Platform>,
    pub msr: u32,
    pub clear: u64,
    pub set: u64,
}

impl Record {
    pub const fn new(scope: Scope, msr: u32, clear: u64, set: u64) -> Self {
        Self {
            scope,
            cpu: None,
            core: None,
            platform: None,
            msr,
            clear,
            set,
        }
    }

    pub const fn cpu(self, family: u32, revisions: u32) -> Self {
        Self {
            cpu: Some(Guard { family, revisions }),
            ..self
        }
    }

    pub const fn core(self, family: u32, revisions: u32) -> Self {
        Self {
            core: Some(Guard { family, revisions }),
            ..self
        }
    }

    pub const fn platform(self, platform: Platform) -> Self {
        Self {
            platform: Some(platform),
            ..self
        }
    }

    /// The guards pass for these IDs and flags (scope aside).
    pub const fn applies(&self, ids: &LogicalId, flags: PlatformFlags) -> bool {
        let cpu = match self.cpu {
            Some(g) => ids.cpu_is(g.family, g.revisions),
            None => true,
        };
        let core = match self.core {
            Some(g) => ids.core_is(g.family, g.revisions),
            None => true,
        };
        let platform = match self.platform {
            Some(p) => p.matches(flags),
            None => true,
        };
        cpu && core && platform
    }

    /// `old` after this record.
    pub const fn update(&self, old: u64) -> u64 {
        (old & !self.clear) | self.set
    }
}

/// The phase-1 table of `generation`, if its feature is built.
pub fn records(generation: Generation) -> Option<&'static [Record]> {
    match generation {
        #[cfg(feature = "naples")]
        Generation::Naples => Some(super::zen1::tables::RECORDS),
        #[cfg(feature = "rome")]
        Generation::Rome => Some(super::zen2::tables::RECORDS),
        #[cfg(feature = "milan")]
        Generation::Milan => Some(super::zen3::tables::RECORDS),
        #[allow(unreachable_patterns)]
        _ => None,
    }
}

/// Run `table` on the executing thread.
pub fn apply<M: MsrAccess + ?Sized>(
    cpu: &M,
    table: &[Record],
    thread: &Thread,
    ids: &LogicalId,
    flags: PlatformFlags,
) {
    table
        .iter()
        .take_while(|r| thread.in_scope(r.scope))
        .filter(|r| r.applies(ids, flags))
        .for_each(|r| cpu.write(r.msr, r.update(cpu.read(r.msr))));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cpu::Revision;
    use crate::sp3::ccx::logical::{gn, ssp};
    use fstart_arch::x86::cpu::msr::testing::MsrSpace;

    const TABLE: &[Record] = &[
        Record::new(Scope::Thread, 0x10, 0xff, 0x01),
        Record::new(Scope::Thread, 0x11, 0x1, 0x1).core(gn::CORE, gn::A0),
        Record::new(Scope::Thread, 0x12, 0x1, 0x1).platform(Platform::any(plat::gn::X2APIC)),
        Record::new(Scope::Core, 0x20, 0xf0, 0x30).cpu(gn::CPU, gn::B0 | gn::B1),
        Record::new(Scope::Complex, 0x30, 0x1, 0x1),
    ];

    fn run(rev: Revision, apic_id: u32, apic: ApicMode) -> MsrSpace {
        let cpu = MsrSpace::new([(0x10, 0xffff), (0x20, 0xff)]);
        let flags = PlatformFlags::sp3(Generation::Milan, apic, TablePolicy::default());
        let thread = Thread::new(Generation::Milan, apic_id, true, false);
        apply(&cpu, TABLE, &thread, &LogicalId::of(rev), flags);
        cpu.into_inner()
    }

    #[test]
    fn guards_and_scopes() {
        let primary = run(Revision::GnB1, 0x10, ApicMode::XApic);
        assert_eq!(primary.get(0x10), 0xff01);
        assert!(!primary.written(0x11), "core A0 only");
        assert!(!primary.written(0x12), "x2APIC only");
        assert_eq!(primary.get(0x20), 0x3f);
        assert!(primary.written(0x30), "first thread of CCD 1");

        // Secondary thread: scope 1 fails, so the run stops before scope 2.
        let sibling = run(Revision::GnA0, 0x11, ApicMode::X2Apic);
        assert_eq!(
            sibling.writes.iter().map(|w| w.0).collect::<Vec<_>>(),
            [0x10, 0x11, 0x12]
        );
    }

    #[test]
    fn sp3_flags() {
        let p = TablePolicy::default();
        assert_eq!(
            PlatformFlags::sp3(Generation::Naples, ApicMode::XApic, p).0,
            0
        );
        assert_eq!(
            PlatformFlags::sp3(Generation::Rome, ApicMode::XApic, p).0,
            0x4
        );
        assert_eq!(
            PlatformFlags::sp3(Generation::Rome, ApicMode::X2Apic, p).0,
            0x2
        );
        assert_eq!(
            PlatformFlags::sp3(Generation::Milan, ApicMode::XApic, p).0,
            0x68
        );
        assert_eq!(
            PlatformFlags::sp3(Generation::Milan, ApicMode::X2Apic, p).0,
            0x58
        );
        assert!(Platform::All(0x6).matches(PlatformFlags(0x7)));
        assert!(!Platform::All(0x6).matches(PlatformFlags(0x4)));
    }

    /// Every generated table is in dispatch order, writes inside its clear
    /// mask, and only guards on its own logical families.
    #[test]
    fn generated_tables_are_well_formed() {
        let gens = [
            (Generation::Naples, crate::sp3::ccx::logical::zp::CPU, 0),
            (Generation::Rome, ssp::CPU, ssp::CORE),
            (Generation::Milan, gn::CPU, gn::CORE),
        ];
        for (g, cpu_family, core_family) in gens {
            let Some(t) = records(g) else { continue };
            assert!(t.windows(2).all(|w| w[0].scope <= w[1].scope), "{g:?}");
            assert!(t.iter().all(|r| r.set & !r.clear == 0 && r.clear != 0));
            assert!(
                t.iter()
                    .all(|r| r.cpu.is_none_or(|c| c.family == cpu_family))
            );
            assert!(
                t.iter()
                    .all(|r| r.core.is_none_or(|c| c.family == core_family))
            );
        }
    }

    /// Against the "applies (SP3)" column of CCX_SP3 §3.3 (the
    /// contracts' `applies_sp3`): records that apply per revision and mode.
    #[test]
    fn applies_like_the_report() {
        let p = TablePolicy::default();
        let cases = [
            (Revision::ZpB1, ApicMode::XApic, 20, 0xc001_102d, 1 << 34),
            (Revision::SspB0, ApicMode::X2Apic, 27, 0x1b, 1 << 10),
            (Revision::GnB2, ApicMode::XApic, 36, 0xc001_1004, 1 << 53),
        ];
        for (rev, apic, count, msr, clear) in cases {
            let g = rev.generation();
            let Some(t) = records(g) else { continue };
            let (ids, flags) = (LogicalId::of(rev), PlatformFlags::sp3(g, apic, p));
            let applied: Vec<_> = t.iter().filter(|r| r.applies(&ids, flags)).collect();
            assert_eq!(applied.len(), count, "{rev:?}");
            assert!(
                applied.iter().any(|r| r.msr == msr && r.clear == clear),
                "{rev:?}"
            );
        }
    }
}
