// SPDX-License-Identifier: MIT OR Apache-2.0
//! The RS cores: Genoa's Zen 4 and Siena's Zen 4c CCDs on the RS I/O die.
//! Their revisions and logical IDs, the topology of their APIC IDs, their
//! release through the SMN thread-enable registers and the per-thread init
//! the CPU driver runs ([`thread_init`], the hook of
//! [`super::mp::Fam17hCpu::multi_socket`]): the vendor reset table
//! ([`super::tables::rs`], AGESA `CcxProgramTablesAtReset`), the microcode,
//! TW_CFG.CombineCr0Cd, the C-states, the CPU watchdog and the
//! write-combining speculation.
//!
//! The S8040's NFX V0.02 AmdCcxZen4Pei and AmdSocSp5RsPei are the source;
//! openSIL's Turin CCX code agrees where it overlaps.
use fstart_arch::x86::cpu::msr::{Msr, MsrAccess};
use fstart_arch::x86::mp::CpuIdentity;
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};
use tock_registers::register_bitfields;

use super::logical::{Id, LogicalId};
use super::microcode::{Load, Patch};
use super::msr::{CSTATE_BASE_ADDR, CSTATE_CONFIG, CstateBaseAddr, TW_CFG, TwCfg};
use super::pm::{self, Wdt};
use super::table::PlatformFlags;
use super::topology::Scope;
use crate::smn::{SmnAccess, SmnReg};

/// An RS core revision: RS (Genoa, Zen 4) or RSDN (Siena, Zen 4c).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Revision {
    /// RS-B0 (0x00A10F10).
    RsB0,
    /// RS-B1 (0x00A10F11).
    RsB1,
    /// RS-B2 (0x00A10F12).
    RsB2,
    /// RSDN-A0 (0x00AA0F00).
    RsdnA0,
    /// RSDN-A1 (0x00AA0F01).
    RsdnA1,
    /// RSDN-A2 (0x00AA0F02), the S8040's.
    RsdnA2,
}

impl Revision {
    /// The revision of `cpu`; `None` for another CPU, or a stepping no
    /// vendor table was recovered for.
    pub fn of(cpu: CpuIdentity) -> Option<Self> {
        Some(match (cpu.family(), cpu.model(), cpu.signature & 0xf) {
            (0x19, 0x11, 0) => Self::RsB0,
            (0x19, 0x11, 1) => Self::RsB1,
            (0x19, 0x11, 2) => Self::RsB2,
            (0x19, 0xa0, 0) => Self::RsdnA0,
            (0x19, 0xa0, 1) => Self::RsdnA1,
            (0x19, 0xa0, 2) => Self::RsdnA2,
            _ => return None,
        })
    }

    /// Zen 4c cores.
    pub const fn dense(self) -> bool {
        matches!(self, Self::RsdnA0 | Self::RsdnA1 | Self::RsdnA2)
    }

    /// The logical IDs of the RS SoC translation table (Siena
    /// AmdSocSp5RsPei `ffa6fb50`, revision rows `ffa6fb0c`/`ffa6fb20`). Siena
    /// A2 has no row: AGESA maps an unlisted stepping to the newest row of
    /// its family, A1. The core logical IDs are not recovered: the
    /// core-guarded records are skipped.
    pub const fn logical_id(self) -> LogicalId {
        let (family, revision) = match self {
            Self::RsB0 => (logical::CPU, logical::B0),
            Self::RsB1 => (logical::CPU, logical::B1),
            Self::RsB2 => (logical::CPU, logical::B2),
            Self::RsdnA0 => (logical::DENSE_CPU, logical::DENSE_A0),
            Self::RsdnA1 | Self::RsdnA2 => (logical::DENSE_CPU, logical::DENSE_A1),
        };
        LogicalId {
            cpu: Id { family, revision },
            core: None,
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::RsB0 => "RS-B0",
            Self::RsB1 => "RS-B1",
            Self::RsB2 => "RS-B2",
            Self::RsdnA0 => "RSDN-A0",
            Self::RsdnA1 => "RSDN-A1",
            Self::RsdnA2 => "RSDN-A2",
        }
    }
}

/// The CPU families and revision bits of the RS table's guards.
pub mod logical {
    /// Genoa.
    pub const CPU: u32 = 0x1000;
    pub const B0: u32 = 0x10;
    pub const B1: u32 = 0x20;
    pub const B2: u32 = 0x40;
    /// Siena.
    pub const DENSE_CPU: u32 = 0x8000;
    pub const DENSE_A0: u32 = 0x1;
    pub const DENSE_A1: u32 = 0x2;
}

/// The executing thread: `socket<<8 | complex<<4 | core<<1 | thread` with
/// SMT, the complex being Genoa's CCD or one of a Siena CCD's two CCXs (the
/// S8040's four CCDs number their 128 threads 0–127, the Onyx's ten CCDs
/// 160 threads 0–159).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Thread {
    pub apic_id: u32,
    pub smt: bool,
    pub bsp: bool,
}

impl Thread {
    /// From CPUID Fn8000_001E (ExtendedApicId), the shared topology
    /// ([`super::Thread`]) and APIC_BAR.BSP.
    pub fn current(cpu: &(impl MsrAccess + ?Sized)) -> Self {
        use fstart_arch::x86::cpu::msr::{APIC_BASE, ApicBase};
        Self {
            apic_id: fstart_arch::x86::cpuid(0x8000_001e).0,
            smt: super::Thread::current().threads_per_core() > 1,
            bsp: cpu.reg(APIC_BASE).is_set(ApicBase::BSP),
        }
    }

    /// The table engine's scope predicate. The RS table has records of the
    /// thread, core and complex scopes only.
    pub const fn in_scope(&self, scope: Scope) -> bool {
        let (smt, single) = match scope {
            Scope::Thread => return true,
            Scope::Bsp => return self.bsp,
            Scope::Core => (0x1, 0x0),
            Scope::Complex => (0xf, 0x7),
            Scope::Die | Scope::Socket => (0xff, 0x7f),
        };
        self.apic_id & if self.smt { smt } else { single } == 0
    }

    /// The first thread of its core.
    pub const fn primary(&self) -> bool {
        self.in_scope(Scope::Core)
    }
}

register_bitfields![u32,
    /// The thread-enable register of a CCD: bit `complex * 16 + core *
    /// threads per core + thread`, by the logical slots.
    pub ThreadEnable [
        THREADS OFFSET(0) NUMBITS(32) []
    ]
];

/// The thread-enable register of physical CCD `ccd` (AmdSocSp5RsPei
/// `ffd24456`): `0x3A081018 + ccd * 0x2000000`, with 32-bit wrapping and
/// `+0xF6000000` for CCDs 0–7.
pub const fn thread_enable(ccd: u8) -> SmnReg<ThreadEnable::Register> {
    let address = 0x3a08_1018u32.wrapping_add((ccd as u32).wrapping_mul(0x0200_0000));
    SmnReg::new(if ccd < 8 {
        address.wrapping_add(0xf600_0000)
    } else {
        address
    })
}

/// One AP to release: its CCD's register and its bit there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Launch {
    pub ccd: u8,
    pub bit: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchError {
    /// The BSP is not logical CCD 0, complex 0, core 0, thread 0.
    Bsp,
    /// A thread 1 without a thread 0, or SMT on some cores only.
    Threads,
    Capacity,
}

/// Every AP of `map` in release order, the logical-zero BSP excluded. The
/// bit spacing follows the BSP's core, as ApobRsPei does.
pub fn launch_order<const N: usize>(
    map: &crate::apob::rs::CoreMap,
) -> Result<heapless::Vec<Launch, N>, LaunchError> {
    let (ccd, ccx, core) = map.cores().next().ok_or(LaunchError::Bsp)?;
    if (ccd.logical, ccx.logical, core.logical) != (0, 0, 0) || !core.threads[0] {
        return Err(LaunchError::Bsp);
    }
    let per_core = 1 + u8::from(core.threads[1]);
    let mut order = heapless::Vec::new();
    for (ccd, ccx, core) in map.cores() {
        if (!core.threads[0] && core.threads[1]) || (per_core == 1 && core.threads[1]) {
            return Err(LaunchError::Threads);
        }
        let first = ccx.logical * 16 + per_core * core.logical;
        for thread in (0..per_core).filter(|&t| core.threads[usize::from(t)]) {
            if (ccd.logical, first + thread) == (0, 0) {
                continue;
            }
            order
                .push(Launch {
                    ccd: ccd.id,
                    bit: first + thread,
                })
                .map_err(|_| LaunchError::Capacity)?;
        }
    }
    Ok(order)
}

/// Release `ap`: set its bit in its CCD's thread-enable register. `false`
/// if the register does not read, or the thread already runs.
pub fn release(smn: &impl SmnAccess, ap: Launch) -> bool {
    let reg = smn.reg(thread_enable(ap.ccd));
    let before = reg.get();
    if before == u32::MAX || before & 1 << ap.bit != 0 {
        return false;
    }
    reg.write(ThreadEnable::THREADS.val(before | 1 << ap.bit));
    true
}

register_bitfields![u64,
    /// Core::X86::Msr::LS_CFG.
    pub LsCfg [
        /// PPR-reserved. Every AGESA write-combining speculation setup clears
        /// it (Zen 4 PEI, openSIL Turin CcxMiscInit), whichever way it goes.
        WC_SPEC_RSVD OFFSET(53) NUMBITS(1) []
    ],
    /// Core::X86::Msr::LS_CFG3.
    pub LsCfg3 [
        /// Write-combining speculation (AmdWcSpecConfiguration): both bits.
        WC_SPEC0 OFFSET(30) NUMBITS(1) [],
        WC_SPEC1 OFFSET(33) NUMBITS(1) []
    ]
];

/// [`super::msr::LS_CFG`] and [`super::msr::LS_CFG3`] with their fields.
const LS_CFG: Msr<LsCfg::Register> = Msr::new(super::msr::LS_CFG.index());
const LS_CFG3: Msr<LsCfg3::Register> = Msr::new(super::msr::LS_CFG3.index());

/// The board's choices for every thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    /// CC6 in CSTATE_CONFIG (AGESA, openSIL CcxInitializeC6 and the
    /// S8040's vendor firmware: `0x484848`).
    pub cc6: bool,
    /// The I/O range the C-state requests use (`_CST`): openSIL's Onyx
    /// configuration and the S8040's vendor firmware take 813h.
    pub cstate_io_base: u16,
    /// The CPU watchdog, `None` off.
    pub wdt: Option<Wdt>,
    /// Write-combining speculation (LS_CFG3\[30\], \[33\]), `None` keeps the
    /// reset value.
    pub wc_spec: Option<bool>,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            cc6: true,
            cstate_io_base: 0x813,
            wdt: None,
            wc_spec: None,
        }
    }
}

/// What every thread runs, from what the BSP found.
pub struct Threads {
    pub revision: Revision,
    pub policy: Policy,
    /// The patch for the CPUs, if the FFS has one.
    pub microcode: Option<Patch<'static>>,
}

/// The per-thread init on the executing thread: the reset table, the
/// microcode (the first thread of a core; the BSP leaves its address for
/// the PSP's reload on resume, as AGESA does), TW_CFG.CombineCr0Cd (SMT
/// siblings share CR0.CD: without it the OS disabling the cache one thread
/// at a time runs a thread uncached next to a cached sibling, losing
/// stores), the NBIO's PCIe errors masked in its machine-check banks
/// ([`super::mca::NBIO_PCIE_SIDEBAND`]), CC6 and the C-state I/O base, the
/// watchdog and the write-combining speculation. False if the microcode
/// load failed.
pub fn thread_init<M: MsrAccess + ?Sized>(threads: &Threads, cpu: &M) -> bool {
    let thread = Thread::current(cpu);
    super::table::apply_scoped(
        cpu,
        super::tables::rs::RECORDS,
        |scope| thread.in_scope(scope),
        &threads.revision.logical_id(),
        // The platform-guarded records (x2APIC enable among them) are not
        // decoded and stay skipped, as on both boards' native boots.
        PlatformFlags(0),
    );
    let mut ok = true;
    if let Some(patch) = &threads.microcode
        && thread.primary()
    {
        let load = Load {
            record_for_s3: thread.bsp,
            ..Load::default()
        };
        // SAFETY: ring 0, caching on; the patch is in the reserved, identity
        // mapped FFS copy.
        ok = unsafe { patch.apply(cpu, patch.address(), CpuIdentity::current().signature, load) }
            .is_ok();
    }
    cpu.reg(TW_CFG).modify(TwCfg::COMBINE_CR0_CD::SET);
    // The OS owns AER (the platform's `_OSC`): the NBIO's PCIe errors are
    // not machine checks as well.
    super::mca::mask_by_ip(cpu, &[super::mca::NBIO_PCIE_SIDEBAND]);
    if thread.primary() {
        cpu.reg(CSTATE_CONFIG)
            .set(pm::cstate_cfg(threads.policy.cc6));
    }
    cpu.reg(CSTATE_BASE_ADDR)
        .write(CstateBaseAddr::CSTATE_ADDR.val(threads.policy.cstate_io_base.into()));
    if let Some(wdt) = threads.policy.wdt {
        pm::init_wdt(cpu, Some(wdt), None);
    }
    if let Some(on) = threads.policy.wc_spec {
        cpu.reg(LS_CFG).modify(LsCfg::WC_SPEC_RSVD::CLEAR);
        cpu.reg(LS_CFG3).modify(if on {
            LsCfg3::WC_SPEC0::SET + LsCfg3::WC_SPEC1::SET
        } else {
            LsCfg3::WC_SPEC0::CLEAR + LsCfg3::WC_SPEC1::CLEAR
        });
    }
    ok
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apob::rs::{Ccd, Ccx, Core, CoreMap};
    use fstart_arch::x86::cpu::msr::testing::MsrSpace;

    #[test]
    fn revisions_and_logical_ids() {
        let of = |signature| {
            Revision::of(CpuIdentity {
                vendor: fstart_arch::x86::mp::CpuVendor::Amd,
                signature,
            })
        };
        assert_eq!(of(0x00a1_0f11), Some(Revision::RsB1));
        assert_eq!(of(0x00aa_0f02), Some(Revision::RsdnA2));
        for signature in [0x00a0_0f11, 0x00a1_0f01, 0x00aa_0f03, 0x00b0_0f20] {
            assert_eq!(of(signature), None);
        }
        let id = Revision::RsdnA2.logical_id();
        assert_eq!((id.cpu.family, id.cpu.revision), (0x8000, 0x2));
        assert!(Revision::RsdnA2.dense() && !Revision::RsB0.dense());
    }

    #[test]
    fn scopes_follow_the_complex() {
        let t = |apic_id| Thread {
            apic_id,
            smt: true,
            bsp: false,
        };
        assert!(t(0x10).in_scope(Scope::Complex) && t(0x10).primary());
        assert!(!t(0x11).primary() && !t(0x12).in_scope(Scope::Complex));
    }

    #[test]
    fn release_uses_logical_slots_on_the_physical_ccd() {
        let core = |logical, id| Core {
            logical,
            id,
            threads: [true, true],
        };
        let mut map = CoreMap {
            ccds: [Ccd {
                logical: 0,
                id: 8,
                ccxs: [
                    Ccx {
                        logical: 0,
                        id: 1,
                        cores: [core(0, 5), core(1, 2)].into_iter().collect(),
                    },
                    Ccx {
                        logical: 1,
                        id: 0,
                        cores: [core(0, 3)].into_iter().collect(),
                    },
                ]
                .into_iter()
                .collect(),
            }]
            .into_iter()
            .collect(),
        };
        let order = launch_order::<8>(&map).unwrap();
        assert_eq!(
            order
                .iter()
                .map(|l| (l.ccd, l.bit))
                .collect::<std::vec::Vec<_>>(),
            [(8, 1), (8, 2), (8, 3), (8, 16), (8, 17)]
        );
        assert_eq!(thread_enable(8).address(), 0x4a08_1018);
        assert_eq!(thread_enable(1).address(), 0x3208_1018);
        assert_eq!(thread_enable(0).address(), 0x3008_1018);
        map.ccds[0].ccxs[0].cores[0].threads = [false, true];
        assert_eq!(launch_order::<8>(&map), Err(LaunchError::Bsp));
        map.ccds[0].ccxs[0].cores[0].threads = [true, false];
        assert_eq!(launch_order::<8>(&map), Err(LaunchError::Threads));
    }

    #[test]
    fn siena_threads_take_the_reset_table_and_the_policy() {
        let cpu = MsrSpace::new([(0xc001_0402, 0xff), (0xc001_1022, 0), (0x1b, 0xfee0_0900)]);
        let threads = Threads {
            revision: Revision::RsdnA2,
            policy: Policy {
                wdt: Some(Wdt::DEFAULT),
                wc_spec: Some(true),
                ..Policy::default()
            },
            microcode: None,
        };
        // The host's CPUID decides the thread's scopes; run the table as a
        // primary thread of a complex.
        crate::ccx::table::apply_scoped(
            &cpu,
            crate::ccx::tables::rs::RECORDS,
            |scope| scope <= Scope::Core,
            &threads.revision.logical_id(),
            PlatformFlags(0),
        );
        let regs = cpu.into_inner();
        // The generic set, then the family-0x8000 clear, in vendor order.
        assert_eq!(regs.get(0xc001_0402), 0xf7);
        assert_eq!(regs.get(0xc001_1000) >> 24, 0x33);
        // Core scope ran, complex scope did not.
        assert_eq!(regs.get(0xc001_1022), 0x8680_0000_0000_0000);
        assert!(!regs.written(0xc001_1093));
        // The platform-guarded x2APIC enable is skipped.
        assert_eq!(regs.get(0x1b), 0xfee0_0900);
        // Genoa-only records do not apply.
        assert!(!regs.written(0xc001_10de));
    }
}
