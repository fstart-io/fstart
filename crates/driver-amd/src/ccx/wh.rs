// SPDX-License-Identifier: MIT OR Apache-2.0
//! WH (Zen 6 / Zen 6c) CCX identification and logical IDs, dual-word
//! SMN AP thread release, CPUID leaf `0x8000_0026` extended x2APIC ID
//! calculation, the APIC mode, and per-thread MSR initialization
//! ([`thread_init`]).
//!
//! Verified against openSIL `CcxLaunchThreadWh`, `CcxCalcLocalApicId`,
//! `ConfigureApicMode`, `InitializeCcxAndLaunchAps`, `CcxApEntrySil`,
//! `Zen6SetMiscMsrs`, `Zen6CcxPrefetcher` and `Zen6CStateInit` of the Dell
//! SilVenicePei, and AmdCcxWhDxe's CPPC request (CCX §2).

use core::sync::atomic::{AtomicBool, Ordering};

use fstart_arch::x86::cpu::amd::msr::{SVM_KEY, VM_CR, VmCr};
use fstart_arch::x86::cpu::msr::{APIC_BASE, ApicBase, Msr, MsrAccess};
use heapless::Vec;
use tock_registers::fields::FieldValue;
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};
use tock_registers::register_bitfields;

use super::brand;
use super::logical::{Id, LogicalId};
use super::microcode::{Load, PATCH_ADDR_S3, PATCH_LOADER, Patch};
use super::msr::{
    C001_0298, CPPC_CAP1, CPPC_REQUEST, CPUID_FEATURES, CSTATE_BASE_ADDR, CSTATE_CONFIG, CppcCap1,
    CppcRequest, CpuidFeatures, CstateBaseAddr, CstateConfig, TW_CFG, TwCfg,
};
use super::pm::{self, Wdt};
use super::table::{self, PlatformFlags};
use super::topology::Scope;
use super::x2apic::{ApicMode, ApicModePolicy};
use crate::apob::v2::CoreMapV2;
use crate::smn::{SmnAccess, SmnReg};

/// Family 1Ah extended family + base family (`0x0F + 0x0B = 0x1A`).
pub const FAMILY_1AH: u8 = 0x1a;
/// WH classic (Zen 6) model range (`0x50..=0x5F`, CPUID EAX `0x00B5_0Fxx`).
pub const MODEL_CLASSIC_MIN: u8 = 0x50;
pub const MODEL_CLASSIC_MAX: u8 = 0x5f;
/// WH dense (Zen 6c) model range (`0xC0..=0xCF`, CPUID EAX `0x00BC_0Fxx`).
pub const MODEL_DENSE_MIN: u8 = 0xc0;
pub const MODEL_DENSE_MAX: u8 = 0xcf;
/// Backwards-compatible aliases for WH classic model bounds.
pub const MODEL_MIN: u8 = MODEL_CLASSIC_MIN;
pub const MODEL_MAX: u8 = MODEL_CLASSIC_MAX;
/// Maximum logical IODs per system (2 sockets × 2 IODs).
pub const MAX_IODS: u8 = 4;
/// Maximum AP threads per die (8 CCDs × 64 threads = 512).
pub const MAX_THREADS_PER_DIE: usize = 512;

/// SMN `ThreadEnable` low word (`0x2076_0020`, 32-bit, threads 0..=31 of a CCD).
pub const THREAD_ENABLE_LO_BASE: u32 = 0x2076_0020;
/// SMN `ThreadEnable` high word (`0x2076_0024`, 32-bit, threads 32..=63 of a CCD).
pub const THREAD_ENABLE_HI_BASE: u32 = 0x2076_0024;

register_bitfields![u32,
    /// SMN `(phys_ccd << 24) | 0x2076_0020` / `0x2076_0024` (32-bit):
    /// per-CCD AP thread release bitmap (bit `t` releases thread `t`).
    pub ThreadEnable [
        MASK OFFSET(0) NUMBITS(32) []
    ]
];

register_bitfields![u64,
    /// Core::X86::Msr::LS_CFG (`0xC001_1020`).
    pub LsCfg [
        WC_SPEC_RSVD OFFSET(53) NUMBITS(1) []
    ],
    /// Core::X86::Msr::LS_CFG3 (`0xC001_10E5`).
    pub LsCfg3 [
        WC_SPEC0 OFFSET(30) NUMBITS(1) [],
        WC_SPEC1 OFFSET(33) NUMBITS(1) []
    ],
    /// Core::X86::Msr::BU_CFG3 (`0xC001_102B`): Zen 6 L2 prefetcher controls.
    /// Enables: Zen6CcxPrefetcher (`f51f70`, instruction-checked) clears
    /// them to disable a prefetcher.
    pub L2Prefetch [
        L2_STREAM_EN OFFSET(0) NUMBITS(1) [],
        L2_UP_DOWN_EN OFFSET(2) NUMBITS(1) []
    ],
    /// Core::X86::Msr::L1_PREFETCH (`0xC001_10E7`): Zen 6 L1 prefetcher controls.
    pub L1Prefetch [
        L1_STREAM_DIS OFFSET(16) NUMBITS(1) [],
        L1_STRIDE_DIS OFFSET(17) NUMBITS(1) [],
        L1_REGION_DIS OFFSET(18) NUMBITS(1) []
    ]
];

const LS_CFG: Msr<LsCfg::Register> = Msr::new(super::msr::LS_CFG.index());
const LS_CFG3: Msr<LsCfg3::Register> = Msr::new(super::msr::LS_CFG3.index());
const L2_PREFETCH: Msr<L2Prefetch::Register> = Msr::new(super::msr::BU_CFG3.index());
const L1_PREFETCH: Msr<L1Prefetch::Register> = Msr::new(0xc001_10e7);

/// WH core revision decoded from CPUID Fn0000_0001_EAX.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Revision {
    pub signature: u32,
    pub model: u8,
    pub stepping: u8,
}

impl Revision {
    /// Decode CPUID Fn0000_0001_EAX if it belongs to WH classic (Family 1Ah
    /// models 50h–5Fh) or WH dense (models C0h–CFh).
    pub const fn from_signature(signature: u32) -> Option<Self> {
        let base_family = ((signature >> 8) & 0x0f) as u8;
        let ext_family = ((signature >> 20) & 0xff) as u8;
        let family = if base_family == 0x0f {
            base_family.saturating_add(ext_family)
        } else {
            base_family
        };
        let model = ((((signature >> 16) & 0x0f) << 4) | ((signature >> 4) & 0x0f)) as u8;
        let stepping = (signature & 0x0f) as u8;
        if family == FAMILY_1AH
            && ((model >= MODEL_CLASSIC_MIN && model <= MODEL_CLASSIC_MAX)
                || (model >= MODEL_DENSE_MIN && model <= MODEL_DENSE_MAX))
        {
            Some(Self {
                signature,
                model,
                stepping,
            })
        } else {
            None
        }
    }

    /// True for Zen 6c dense models (`0xC0..=0xCF`).
    pub const fn is_dense(self) -> bool {
        self.model >= MODEL_DENSE_MIN && self.model <= MODEL_DENSE_MAX
    }

    /// True for steppings that require an extra read-write cycle on the low
    /// `ThreadEnable` word after writing the high word (Family 1Ah model 50h
    /// stepping 0h or Bh, CPUID `0x00B5_0F00` / `0x00B5_0F0B`).
    pub const fn needs_thread_enable_low_kick(self) -> bool {
        self.model == 0x50 && matches!(self.stepping, 0x0 | 0xb)
    }

    /// The SoC logical ID of the vendor's CPUID mapping (`f24b20`,
    /// instruction-checked; [`crate::ccx::tables::wh::LOGICAL`]): a family
    /// row of this extended model matches if one of its revision rows has
    /// this model\[3:0\]; the exact (model, stepping) row wins, else the
    /// family's *first* row (an unknown model-50h stepping runs as the
    /// model-51h row, revision 0x10). `None` if no row has the model
    /// (e.g. model 52h): then no SoC-guarded record applies. No WH record
    /// has a core guard, so the core logical ID is left out.
    pub fn logical_id(self) -> Option<LogicalId> {
        let rows = crate::ccx::tables::wh::LOGICAL;
        let (ext, lo) = (self.model >> 4, self.model & 0xf);
        let mut rest = rows;
        while let Some(&(e, _, _, family, _)) = rest.first() {
            let len = rest
                .iter()
                .position(|r| (r.0, r.3) != (e, family))
                .unwrap_or(rest.len());
            let (group, next) = rest.split_at(len);
            if e == ext && group.iter().any(|r| r.1 == lo) {
                let row = group
                    .iter()
                    .find(|r| (r.1, r.2) == (lo, self.stepping))
                    .unwrap_or(&group[0]);
                return Some(LogicalId {
                    cpu: Id {
                        family,
                        revision: row.4.into(),
                    },
                    core: None,
                    model: Some(row.1),
                });
            }
            rest = next;
        }
        None
    }

    pub const fn name(self) -> &'static str {
        match (self.is_dense(), self.stepping) {
            (false, 0) => "WH-A0",
            (false, 0xb) => "WH-B0",
            (false, _) => "WH",
            (true, 0) => "WHc-A0",
            (true, _) => "WHc",
        }
    }
}

/// Low and high `ThreadEnable` SMN registers for physical CCD `phys_ccd` (`0..=15`).
pub const fn thread_enable_regs(
    phys_ccd: u8,
) -> Option<(
    SmnReg<ThreadEnable::Register>,
    SmnReg<ThreadEnable::Register>,
)> {
    if phys_ccd >= 16 {
        return None;
    }
    let ccd = (phys_ccd as u32) << 24;
    Some((
        SmnReg::new(ccd | THREAD_ENABLE_LO_BASE),
        SmnReg::new(ccd | THREAD_ENABLE_HI_BASE),
    ))
}

/// Release threads in 64-bit `mask` on `phys_ccd` through `smn`, applying the
/// extra low-word read/write erratum when `revision.needs_thread_enable_low_kick()`
/// is set and upper threads (`mask >> 32 != 0`) are enabled.
pub fn enable_threads(
    smn: &impl SmnAccess,
    revision: Revision,
    phys_ccd: u8,
    mask: u64,
) -> Option<()> {
    let (lo_reg, hi_reg) = thread_enable_regs(phys_ccd)?;
    let lo = mask as u32;
    let hi = (mask >> 32) as u32;
    if lo != 0 {
        let cur = smn.reg(lo_reg).get();
        smn.reg(lo_reg).set(cur | lo);
    }
    if hi != 0 {
        let cur = smn.reg(hi_reg).get();
        smn.reg(hi_reg).set(cur | hi);
        if revision.needs_thread_enable_low_kick() {
            let cur_lo = smn.reg(lo_reg).get();
            smn.reg(lo_reg).set(cur_lo);
        }
    }
    Some(())
}

/// One AP thread to release through its owning IOD's SMN `ThreadEnable` pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Launch {
    pub socket: u8,
    pub die: u8,
    /// Physical CCD ID (`0..16`, selects SMN `phys_ccd << 24`).
    pub phys_ccd: u8,
    /// Thread bit within the 64-bit `ThreadEnable` pair (`0..64`).
    pub bit: u8,
    /// The APIC ID the thread will report ([`apic_id`]).
    pub apic_id: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchError {
    /// Expected BSP thread 0 on logical CCD 0 / core 0 is missing.
    Bsp,
    /// Asymmetric SMT or out-of-range thread bit (> 63).
    Threads,
    /// More APs than buffer capacity `N`.
    TooMany,
    /// The topology does not fit the CPUID `0x8000_0026` APIC ID fields.
    ApicId,
}

/// Build the AP release order for `(socket, die)` from its APOB V2 `3/3`
/// [`CoreMapV2`], matching openSIL `CcxLaunchThreadWh`:
/// `bit = logical_core_in_ccd * threads_per_core + thread`.
///
/// Pass `skip_bsp = true` for `(socket = 0, die = 0)` so thread 0 of the first
/// logical core (already running as BSP) is omitted. Each thread's APIC ID
/// follows from `shifts` with `prior_dies_ccds` CCDs on the socket's earlier
/// dies ([`apic_id`]).
pub fn launch_order<const N: usize>(
    socket: u8,
    die: u8,
    map: &CoreMapV2,
    skip_bsp: bool,
    shifts: TopologyShifts,
    prior_dies_ccds: u32,
) -> Result<Vec<Launch, N>, LaunchError> {
    let mut threads_per_core: Option<u8> = None;
    for (_, _, core) in map.cores() {
        let t = match core.threads {
            [true, true] => 2u8,
            [true, false] => 1u8,
            _ => return Err(LaunchError::Threads),
        };
        if let Some(prev) = threads_per_core {
            if prev != t {
                return Err(LaunchError::Threads);
            }
        } else {
            threads_per_core = Some(t);
        }
    }
    let tpc = match threads_per_core {
        Some(t) => t,
        None => {
            return if skip_bsp {
                Err(LaunchError::Bsp)
            } else {
                Ok(Vec::new())
            };
        }
    };

    let mut out = Vec::new();
    let mut saw_bsp = !skip_bsp;
    for (l_ccd, ccd) in map.ccds.iter().enumerate() {
        if ccd.id >= 16 {
            return Err(LaunchError::Threads);
        }
        let mut l_core = 0u16;
        for (l_ccx, ccx) in ccd.ccxs.iter().enumerate() {
            for (l_core_in_ccx, core) in ccx.cores.iter().enumerate() {
                for t in 0..tpc {
                    if !core.threads[usize::from(t)] {
                        continue;
                    }
                    let bit = l_core * u16::from(tpc) + u16::from(t);
                    if bit >= 64 {
                        return Err(LaunchError::Threads);
                    }
                    if skip_bsp && l_ccd == 0 && l_core == 0 && t == 0 {
                        saw_bsp = true;
                        continue;
                    }
                    let apic_id = apic_id(
                        shifts,
                        socket.into(),
                        prior_dies_ccds,
                        l_ccd as u32,
                        l_ccx as u32,
                        l_core_in_ccx as u32,
                        t.into(),
                        tpc == 2,
                    )
                    .ok_or(LaunchError::ApicId)?;
                    out.push(Launch {
                        socket,
                        die,
                        phys_ccd: ccd.id,
                        bit: bit as u8,
                        apic_id,
                    })
                    .map_err(|_| LaunchError::TooMany)?;
                }
                l_core += 1;
            }
        }
    }
    if !saw_bsp {
        return Err(LaunchError::Bsp);
    }
    Ok(out)
}

/// Release a single AP described by `ap` via `smn` (which must target
/// `(ap.socket, ap.die)`'s root bus). Returns `false` if the target bit is
/// invalid.
pub fn release(smn: &impl SmnAccess, revision: Revision, ap: Launch) -> bool {
    if ap.bit >= 64 {
        return false;
    }
    enable_threads(smn, revision, ap.phys_ccd, 1u64 << ap.bit).is_some()
}

/// The WH SoC logical families and revisions the table's guards use
/// ([`Revision::logical_id`]).
pub mod logical {
    /// WH (Zen 6), extended model 5.
    pub const CPU: u32 = 0x0100_0000;
    /// WH dense (Zen 6c), extended model C.
    pub const DENSE_CPU: u32 = 0x0200_0000;
    /// Every family of the WH table (with two more packages' families).
    pub const TABLE: u32 = 0x0f00_0000;
    /// Model 50h stepping 0 (A0) and Bh (B0); WH dense model C0h.
    pub const A0: u32 = 0x1;
    pub const B0: u32 = 0x2;
    /// Model 51h, and any model-50h stepping without a row.
    pub const DEFAULT: u32 = 0x10;
    /// Model C8h.
    pub const DENSE_8: u32 = 0x100;
    /// Model 58h.
    pub const MODEL_8: u32 = 0x200;
}

/// The logical ID of a CPU without a row: it passes no SoC guard.
const NO_ID: LogicalId = LogicalId {
    cpu: Id {
        family: 0,
        revision: 0,
    },
    core: None,
    model: None,
};

/// Current executing thread context for per-thread MSR programming.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Thread {
    pub apic_id: u32,
    pub smt: bool,
    pub bsp: bool,
}

impl Thread {
    /// Read current thread coordinates from CPUID `0x8000_001E`, shared SMT
    /// topology, and `APIC_BASE.BSP`.
    pub fn current(cpu: &(impl MsrAccess + ?Sized)) -> Self {
        Self {
            apic_id: fstart_arch::x86::cpuid(0x8000_001e).0,
            smt: super::Thread::current().threads_per_core() > 1,
            bsp: cpu.reg(APIC_BASE).is_set(ApicBase::BSP),
        }
    }

    /// The vendor engine's scope predicates (`f4d900`..`f4da30`,
    /// instruction-checked). They take the 8-bit initial APIC ID (CPUID
    /// Fn0000_0001_EBX\[31:24\], the APIC ID's low byte, even in x2APIC
    /// mode) and the threads per core: core `id % tpc == 0`, complex
    /// `id & (tpc == 1 ? 7 : 0xf) == 0`, die and socket
    /// `id & (tpc == 1 ? 0x3f : 0x7f) == 0`, BSP `APIC_BASE[8]`.
    pub const fn in_scope(&self, scope: Scope) -> bool {
        let id = self.apic_id & 0xff;
        let (smt, single) = match scope {
            Scope::Thread => return true,
            Scope::Bsp => return self.bsp,
            Scope::Core => return !self.smt || id & 1 == 0,
            Scope::Complex => (0xf, 0x7),
            Scope::Die | Scope::Socket => (0x7f, 0x3f),
        };
        id & if self.smt { smt } else { single } == 0
    }

    /// True on the first hardware thread of its core (thread 0).
    pub const fn primary(&self) -> bool {
        self.in_scope(Scope::Core)
    }
}

/// Board/platform policy applied on every Zen 6 hardware thread. `Default`
/// is the vendor's (openSIL's CCX input-block defaults, CCX §2.6), where
/// the donors' PCD overrides are not known.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    /// AmdApicMode (input 0; vendor Auto, which is x2APIC on WH):
    /// [`apic_mode`].
    pub apic: ApicModePolicy,
    /// CStateMode (input 0x1b, vendor on): CSTATE_CONFIG, C001_0298 and the
    /// C-state I/O base.
    pub cstates: bool,
    /// Cc6Ctrl (input 0x1c, vendor on): the four CC6EN fields.
    pub cc6: bool,
    /// The C-state I/O base (input 0x1d; `_CST` and the FADT read it back
    /// from the MSR). openSIL's default is 0x413, the donors override it by
    /// PCD and their value is not recovered; 813h is the Genoa/Turin
    /// boards' value.
    pub cstate_io_base: u16,
    /// The CPU watchdog (input 0x13, vendor off). Written on every core
    /// primary either way, disabled when `None`.
    pub wdt: Option<Wdt>,
    /// CpuWdtCfg\[9:7\] when enabled (input 0x1a, vendor 0xff: keep).
    pub wdt_severity: Option<u8>,
    /// Write-combining speculation (input 0xa5, vendor on): LS_CFG\[53\]
    /// cleared, LS_CFG3\[30\], \[33\]; `None` keeps the reset value.
    pub wc_spec: Option<bool>,
    /// Core performance boost (CpbMode, input 0x1f, vendor on; off sets
    /// HWCR.CpbDis).
    pub boost: bool,
    /// SvmEnable (input 0xb1, vendor on): VM_CR.SVMDIS clear.
    pub svm: bool,
    /// SvmLock (input 0xb2, vendor on): SVM_KEY 0 and VM_CR.LOCK.
    pub svm_lock: bool,
    /// SYSCFG.SMEE (input 0x22, vendor off).
    pub smee: bool,
    /// Disable the L1 stream, stride and region and the L2 stream and
    /// up/down prefetchers (inputs 0xe..=0x12, vendor 1: all enabled).
    pub disable_l1_stream: bool,
    pub disable_l1_stride: bool,
    pub disable_l1_region: bool,
    pub disable_l2_stream: bool,
    pub disable_l2_up_down: bool,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            apic: ApicModePolicy::Auto,
            cstates: true,
            cc6: true,
            cstate_io_base: 0x813,
            wdt: None,
            wdt_severity: None,
            wc_spec: Some(true),
            boost: true,
            svm: true,
            svm_lock: true,
            smee: false,
            disable_l1_stream: false,
            disable_l1_stride: false,
            disable_l1_region: false,
            disable_l2_stream: false,
            disable_l2_up_down: false,
        }
    }
}

/// The APIC mode of a WH system (openSIL ConfigureApicMode `f26db0`):
/// xAPIC without an IOMMU (input 0xa7), else x2APIC for Auto (`f50d50`
/// returns x2APIC unconditionally) and x2APIC, and for compatibility xAPIC
/// if every APIC ID is below 0xff. `max_apic_id` is the highest APIC ID of
/// the system. `None`: xAPIC cannot address the threads.
///
/// Where the vendor leaves compatibility mode (no EXTD, HWCR\[32\]) for
/// IDs of 0xff and above, fstart's MP engine needs x2APIC to start those
/// threads, so the mode is x2APIC.
pub const fn apic_mode(policy: ApicModePolicy, iommu: bool, max_apic_id: u32) -> Option<ApicMode> {
    let fits = max_apic_id < 0xff;
    let mode = if !iommu {
        ApicMode::XApic
    } else {
        match policy {
            ApicModePolicy::Auto | ApicModePolicy::X2Apic => ApicMode::X2Apic,
            ApicModePolicy::XApic => ApicMode::XApic,
            ApicModePolicy::Compatibility if fits => ApicMode::XApic,
            ApicModePolicy::Compatibility => ApicMode::X2Apic,
        }
    };
    if matches!(mode, ApicMode::XApic) && !fits {
        None
    } else {
        Some(mode)
    }
}

/// The ASP-authenticated path selects the retained patch with a sentinel,
/// never an x86 FFS address (SilVenicePei:0xf4de7f..0xf4dea6).
pub enum Microcode {
    Asp,
    Host(Option<Patch<'static>>),
}

/// Shared BSP-discovered configuration passed to every thread's [`thread_init`].
pub struct Threads {
    pub revision: Revision,
    pub policy: Policy,
    pub microcode: Microcode,
    /// From [`apic_mode`]; the MP engine runs every LAPIC in this mode
    /// before [`thread_init`] (x2APIC exactly when it is
    /// [`ApicMode::X2Apic`]), so the table's APIC_BASE.EXTD record only
    /// confirms what the engine did.
    pub apic: ApicMode,
    /// The processor name the BSP read from the SMU
    /// ([`crate::smu::v14::READ_BRAND_STRING`]).
    pub brand: Option<brand::Name>,
}

/// Microcode loads one thread at a time: SMT siblings share the patch RAM.
static MICROCODE: AtomicBool = AtomicBool::new(false);
/// How long a thread waits for another's load ([`load_microcode`]): about
/// 2^28 pause loops, seconds where a load takes milliseconds.
const MICROCODE_SPINS: u32 = 1 << 28;

/// Per-thread Zen 6 initialization hook for [`super::mp::Fam17hCpu::multi_socket`],
/// on every thread, in the order of the vendor's AP entry (`CcxApEntrySil`
/// `f29020`; the BSP does the same steps in InitializeCcxAndLaunchAps):
///
/// 1. The CPU watchdog on core primaries, disabled unless the policy has one.
/// 2. Microcode on every thread (the BSP clears C001_100C first).
/// 3. The reset table ([`crate::ccx::tables::wh`]) with the WH engine.
/// 4. MCA_CONFIG of every bank; the NBIO's PCIe sideband errors masked.
/// 5. SVM enable and lock.
/// 6. What the vendor's BSP sets and its APs take from its sync list: the
///    name string, the xAPIC-mode CPUID x2APIC hide, write-combining
///    speculation, the P0 rewrite and boost, SMEE, the prefetchers, the
///    C-states.
/// 7. TW_CFG.CombineCr0Cd.
/// 8. The CPPC request (AmdCcxWhDxe, every thread).
///
/// Returns `false` if microcode loading failed.
pub fn thread_init<M: MsrAccess + ?Sized>(threads: &Threads, cpu: &M) -> bool {
    let thread = Thread::current(cpu);
    thread_init_with(threads, cpu, thread)
}

fn thread_init_with<M: MsrAccess + ?Sized>(threads: &Threads, cpu: &M, thread: Thread) -> bool {
    let policy = &threads.policy;
    let primary = thread.primary();
    if primary {
        pm::init_wdt(cpu, policy.wdt, policy.wdt_severity);
    }
    let ok = load_microcode(threads, cpu, thread);
    table::apply_every_scope(
        cpu,
        crate::ccx::tables::wh::RECORDS,
        |scope| thread.in_scope(scope),
        &threads.revision.logical_id().unwrap_or(NO_ID),
        PlatformFlags::wh(threads.apic),
    );
    super::mca::configure_banks(cpu);
    // The OS owns AER (the platform's `_OSC`): the NBIO's PCIe errors are
    // not machine checks as well.
    super::mca::mask_by_ip(cpu, &[super::mca::NBIO_PCIE_SIDEBAND]);
    svm(cpu, policy.svm, policy.svm_lock);

    if let Some(name) = &threads.brand {
        brand::write(cpu, name);
    }
    if threads.apic == ApicMode::XApic {
        cpu.reg(CPUID_FEATURES).modify(CpuidFeatures::X2APIC::CLEAR);
    }
    if let Some(on) = policy.wc_spec {
        cpu.reg(LS_CFG).modify(LsCfg::WC_SPEC_RSVD::CLEAR);
        cpu.reg(LS_CFG3).modify(if on {
            LsCfg3::WC_SPEC0::SET + LsCfg3::WC_SPEC1::SET
        } else {
            LsCfg3::WC_SPEC0::CLEAR + LsCfg3::WC_SPEC1::CLEAR
        });
    }
    pm::init_pstate(cpu, policy.boost);
    if policy.smee {
        super::security::set_smee(cpu, true);
    }
    prefetchers(cpu, policy);
    if policy.cstates {
        if primary {
            cpu.reg(CSTATE_CONFIG).modify(cstate_fields(policy.cc6));
            cpu.reg(C001_0298).modify(cstate_fields(policy.cc6));
        }
        cpu.reg(CSTATE_BASE_ADDR)
            .write(CstateBaseAddr::CSTATE_ADDR.val(policy.cstate_io_base.into()));
    }

    cpu.reg(TW_CFG).modify(TwCfg::COMBINE_CR0_CD::SET);
    cppc_request(cpu);
    ok
}

/// openSIL LoadMicrocodePatch (`f4dd70`) and the AP's patch write: the BSP
/// clears C001_100C; the ASP-authenticated path writes the sentinel on
/// every thread, the host path loads the patch on every thread that does
/// not run it yet (a sibling after its primary finds it installed).
///
/// The wait for another thread's load is bounded ([`MICROCODE_SPINS`]): a
/// thread that never finishes its load (a fault in the patch write) costs
/// the others their patch (`false`, which the caller reports), not the
/// boot.
fn load_microcode<M: MsrAccess + ?Sized>(threads: &Threads, cpu: &M, thread: Thread) -> bool {
    let mut spins = 0u32;
    while MICROCODE
        .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
        .is_err()
    {
        spins += 1;
        if spins == MICROCODE_SPINS {
            return false;
        }
        core::hint::spin_loop();
    }
    if thread.bsp {
        cpu.reg(PATCH_ADDR_S3).set(0);
    }
    let ok = match &threads.microcode {
        Microcode::Asp => {
            cpu.reg(PATCH_LOADER).set(u64::from(u32::MAX));
            true
        }
        Microcode::Host(Some(patch)) => {
            let load = Load {
                record_for_s3: thread.bsp,
                ..Load::default()
            };
            // SAFETY: ring 0, caching on, one thread at a time; the patch
            // lives in the identity-mapped reserved FFS copy.
            unsafe { patch.apply(cpu, patch.address(), threads.revision.signature, load) }.is_ok()
        }
        Microcode::Host(None) => true,
    };
    MICROCODE.store(false, Ordering::Release);
    ok
}

/// CcxSvmInit (`f4d3c0`, instruction-checked).
fn svm<M: MsrAccess + ?Sized>(cpu: &M, enable: bool, lock: bool) {
    if lock {
        cpu.reg(SVM_KEY).set(0);
    }
    cpu.reg(VM_CR)
        .modify(VmCr::SVMDIS.val((!enable).into()) + VmCr::LOCK.val(lock.into()));
}

/// Zen6CcxPrefetcher (`f51f70`, instruction-checked): L1 bits set and L2
/// bits cleared disable; nothing is written for an enabled prefetcher.
fn prefetchers<M: MsrAccess + ?Sized>(cpu: &M, policy: &Policy) {
    let l1 = [
        (policy.disable_l1_stream, L1Prefetch::L1_STREAM_DIS::SET),
        (policy.disable_l1_stride, L1Prefetch::L1_STRIDE_DIS::SET),
        (policy.disable_l1_region, L1Prefetch::L1_REGION_DIS::SET),
    ];
    for (_, field) in l1.into_iter().filter(|(off, _)| *off) {
        cpu.reg(L1_PREFETCH).modify(field);
    }
    let l2 = [
        (policy.disable_l2_stream, L2Prefetch::L2_STREAM_EN::CLEAR),
        (policy.disable_l2_up_down, L2Prefetch::L2_UP_DOWN_EN::CLEAR),
    ];
    for (_, field) in l2.into_iter().filter(|(off, _)| *off) {
        cpu.reg(L2_PREFETCH).modify(field);
    }
}

/// Zen6CStateInit's fields (`f526e0`, instruction-checked): CC1 at DFS ID 8
/// and CC6EN = Cc6Ctrl on all four CCRs; bit 7 of each byte and \[63:32\]
/// keep their value.
fn cstate_fields(cc6: bool) -> FieldValue<u64, CstateConfig::Register> {
    let cc6 = u64::from(cc6);
    CstateConfig::CCR0_CC1DFSID.val(8)
        + CstateConfig::CCR1_CC1DFSID.val(8)
        + CstateConfig::CCR2_CC1DFSID.val(8)
        + CstateConfig::CCR3_CC1DFSID.val(8)
        + CstateConfig::CCR0_CC6EN.val(cc6)
        + CstateConfig::CCR1_CC6EN.val(cc6)
        + CstateConfig::CCR2_CC6EN.val(cc6)
        + CstateConfig::CCR3_CC6EN.val(cc6)
}

/// AmdCcxWhDxe's CPPC request on every thread (Dell `0xbc0`, instruction-
/// checked): the highest performance as the maximum, the lowest nonlinear
/// one as the minimum unless a minimum is already set, no desired
/// performance, EPP 0. Linux's amd-pstate keeps the firmware's EPP on
/// server platforms.
fn cppc_request<M: MsrAccess + ?Sized>(cpu: &M) {
    let cap = cpu.reg(CPPC_CAP1).extract();
    let current = cpu.reg(CPPC_REQUEST).read(CppcRequest::MIN_PERF);
    let min = if current != 0 {
        current
    } else {
        cap.read(CppcCap1::LOWEST_NONLINEAR_PERF)
    };
    cpu.reg(CPPC_REQUEST).write(
        CppcRequest::MAX_PERF.val(cap.read(CppcCap1::HIGHEST_PERF))
            + CppcRequest::MIN_PERF.val(min),
    );
}

/// Shift widths from CPUID leaf `0x8000_0026` subleaves 0..=3 (`EAX[4:0]`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TopologyShifts {
    /// Core shift (level type 1 / thread): bit position of logical core ID.
    pub core_shift: u8,
    /// Complex shift (level type 2 / core): bit position of logical complex ID.
    pub complex_shift: u8,
    /// CCD shift (level type 3 / complex): bit position of socket-wide CCD ID.
    pub ccd_shift: u8,
    /// Socket shift (level type 4 / die/CCD): bit position of socket ID.
    pub socket_shift: u8,
}

impl TopologyShifts {
    /// Decode CPUID leaf `0x8000_0026` `(eax, ecx)` pairs by level type
    /// (`(ecx >> 8) & 0xff`: `1`=thread, `2`=core, `3`=complex, `4`=socket).
    pub const fn from_cpuid_subleaves(subleaves: &[(u32, u32)]) -> Option<Self> {
        let mut core_shift = None;
        let mut complex_shift = None;
        let mut ccd_shift = None;
        let mut socket_shift = None;
        let mut i = 0;
        while i < subleaves.len() {
            let (eax, ecx) = subleaves[i];
            let level_type = (ecx >> 8) & 0xff;
            let shift = (eax & 0x1f) as u8;
            match level_type {
                0 => break,
                1 => core_shift = Some(shift),
                2 => complex_shift = Some(shift),
                3 => ccd_shift = Some(shift),
                4 => socket_shift = Some(shift),
                _ => {}
            }
            i += 1;
        }
        let (Some(core_shift), Some(complex_shift), Some(ccd_shift), Some(socket_shift)) =
            (core_shift, complex_shift, ccd_shift, socket_shift)
        else {
            return None;
        };
        if core_shift > complex_shift
            || complex_shift > ccd_shift
            || ccd_shift > socket_shift
            || socket_shift >= 32
        {
            return None;
        }
        Some(Self {
            core_shift,
            complex_shift,
            ccd_shift,
            socket_shift,
        })
    }
}

impl TopologyShifts {
    /// The executing CPU's shifts from CPUID `0x8000_0026` (subleaves
    /// until level type 0, at most 8).
    pub fn current() -> Option<Self> {
        let mut subleaves = [(0u32, 0u32); 8];
        let mut n = 0;
        while n < subleaves.len() {
            let (eax, _, ecx, _) = fstart_arch::x86::cpuid_count(0x8000_0026, n as u32);
            subleaves[n] = (eax, ecx);
            n += 1;
            if (ecx >> 8) & 0xff == 0 {
                break;
            }
        }
        Self::from_cpuid_subleaves(&subleaves[..n])
    }
}

const fn fits_bits(value: u32, bits: u8) -> bool {
    if bits >= 32 {
        true
    } else {
        (value >> bits) == 0
    }
}

/// Compute the x2APIC ID for a thread on a dual-IOD socket, accumulating
/// `prior_dies_ccds` (the CCD count of earlier dies on the same socket) and
/// validating field widths against `shifts`.
#[allow(clippy::too_many_arguments)]
pub const fn apic_id(
    shifts: TopologyShifts,
    socket: u32,
    prior_dies_ccds: u32,
    logical_ccd: u32,
    logical_complex: u32,
    logical_core: u32,
    thread: u32,
    smt: bool,
) -> Option<u32> {
    if shifts.core_shift > shifts.complex_shift
        || shifts.complex_shift > shifts.ccd_shift
        || shifts.ccd_shift > shifts.socket_shift
        || shifts.socket_shift >= 32
    {
        return None;
    }
    if (!smt && thread != 0) || !fits_bits(thread, shifts.core_shift) {
        return None;
    }
    if !fits_bits(logical_core, shifts.complex_shift - shifts.core_shift)
        || !fits_bits(logical_complex, shifts.ccd_shift - shifts.complex_shift)
    {
        return None;
    }
    let Some(socket_ccd) = prior_dies_ccds.checked_add(logical_ccd) else {
        return None;
    };
    if !fits_bits(socket_ccd, shifts.socket_shift - shifts.ccd_shift)
        || !fits_bits(socket, 32 - shifts.socket_shift)
    {
        return None;
    }
    Some(
        (socket << shifts.socket_shift)
            | (socket_ccd << shifts.ccd_shift)
            | (logical_complex << shifts.complex_shift)
            | (logical_core << shifts.core_shift)
            | thread,
    )
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use crate::apob::ccx::Core;
    use crate::apob::v2::{CcdV2, CcxV2};
    use crate::smn::testing::{SmnModel, SmnTest};
    use fstart_arch::x86::cpu::msr::testing::MsrSpace;
    use std::vec::Vec as StdVec;

    #[derive(Default)]
    struct TraceSmn {
        regs: std::collections::BTreeMap<u32, u32>,
        ops: StdVec<(char, u32, u32)>,
    }

    impl SmnModel for TraceSmn {
        fn read(&mut self, addr: u32) -> u32 {
            let val = *self.regs.get(&addr).unwrap_or(&0);
            self.ops.push(('R', addr, val));
            val
        }
        fn write(&mut self, addr: u32, value: u32) {
            self.regs.insert(addr, value);
            self.ops.push(('W', addr, value));
        }
    }

    #[test]
    fn revision_detects_wh_classic_dense_and_erratum() {
        let a0 = Revision::from_signature(0x00b5_0f00).unwrap();
        assert_eq!((a0.model, a0.stepping, a0.is_dense()), (0x50, 0, false));
        assert!(a0.needs_thread_enable_low_kick());

        let b0 = Revision::from_signature(0x00b5_0f0b).unwrap();
        assert!(b0.needs_thread_enable_low_kick());

        let c0 = Revision::from_signature(0x00b5_0f21).unwrap();
        assert!(!c0.needs_thread_enable_low_kick());

        let dense = Revision::from_signature(0x00bc_0f00).unwrap();
        assert_eq!((dense.model, dense.is_dense()), (0xc0, true));
        assert!(!dense.needs_thread_enable_low_kick());

        assert!(Revision::from_signature(0x00a1_0f11).is_none());
    }

    #[test]
    fn thread_enable_enforces_low_high_low_erratum_write_order() {
        let lo = 0x0300_0000 | THREAD_ENABLE_LO_BASE;
        let hi = 0x0300_0000 | THREAD_ENABLE_HI_BASE;
        let mask = 0x0000_000f_ffff_ffffu64;

        let erratum = SmnTest::new(TraceSmn::default());
        let a0 = Revision::from_signature(0x00b5_0f00).unwrap();
        enable_threads(&erratum.smn(), a0, 3, mask).unwrap();
        let writes: StdVec<(u32, u32)> = erratum
            .model()
            .ops
            .iter()
            .filter(|(op, _, _)| *op == 'W')
            .map(|(_, a, v)| (*a, *v))
            .collect();
        assert_eq!(
            writes,
            std::vec![(lo, 0xffff_ffff), (hi, 0x0000_000f), (lo, 0xffff_ffff)]
        );

        let clean = SmnTest::new(TraceSmn::default());
        let c0 = Revision::from_signature(0x00b5_0f21).unwrap();
        enable_threads(&clean.smn(), c0, 3, mask).unwrap();
        let writes: StdVec<(u32, u32)> = clean
            .model()
            .ops
            .iter()
            .filter(|(op, _, _)| *op == 'W')
            .map(|(_, a, v)| (*a, *v))
            .collect();
        assert_eq!(writes, std::vec![(lo, 0xffff_ffff), (hi, 0x0000_000f)]);
        assert!(thread_enable_regs(16).is_none());
    }

    #[test]
    fn launch_order_handles_32_core_64_thread_ccd_and_bsp_skip() {
        let mut cores = heapless::Vec::new();
        for c in 0..32u8 {
            let _ = cores.push(Core {
                id: c,
                threads: [true, true],
            });
        }
        let mut ccxs = heapless::Vec::new();
        let _ = ccxs.push(CcxV2 { id: 0, cores });
        let mut ccds = heapless::Vec::new();
        let _ = ccds.push(CcdV2 { id: 4, ccxs });
        let map = CoreMapV2 {
            complexes_per_ccd: 1,
            cores_per_complex: 32,
            ccds,
        };

        // 32 SMT cores in one complex per CCD, up to 16 CCDs a socket.
        let shifts = TopologyShifts::from_cpuid_subleaves(&[
            (1, 1 << 8),
            (6, 2 << 8),
            (6, 3 << 8),
            (10, 4 << 8),
        ])
        .unwrap();
        // Die 0 skips BSP (bit 0) -> 63 APs, bits 1..=63.
        let die0 = launch_order::<64>(0, 0, &map, true, shifts, 0).unwrap();
        assert_eq!(die0.len(), 63);
        assert_eq!(
            die0[0],
            Launch {
                socket: 0,
                die: 0,
                phys_ccd: 4,
                bit: 1,
                apic_id: 1,
            }
        );
        assert_eq!(
            die0[62],
            Launch {
                socket: 0,
                die: 0,
                phys_ccd: 4,
                bit: 63,
                apic_id: 63,
            }
        );

        // Die 1 includes bit 0 -> all 64 APs, after die 0's one CCD.
        let die1 = launch_order::<64>(0, 1, &map, false, shifts, 1).unwrap();
        assert_eq!(die1.len(), 64);
        assert_eq!(
            die1[0],
            Launch {
                socket: 0,
                die: 1,
                phys_ccd: 4,
                bit: 0,
                apic_id: 1 << 6,
            }
        );

        // Release bit 32 triggers high-word + low-word kick on WH-A0.
        let smn = SmnTest::new(TraceSmn::default());
        let a0 = Revision::from_signature(0x00b5_0f00).unwrap();
        assert!(release(&smn.smn(), a0, die1[32]));
        let writes: StdVec<(u32, u32)> = smn
            .model()
            .ops
            .iter()
            .filter(|(op, _, _)| *op == 'W')
            .map(|(_, a, v)| (*a, *v))
            .collect();
        assert_eq!(
            writes,
            std::vec![
                (0x0400_0000 | THREAD_ENABLE_HI_BASE, 1),
                (0x0400_0000 | THREAD_ENABLE_LO_BASE, 0),
            ]
        );
    }

    #[test]
    fn logical_ids_follow_the_vendor_rows() {
        let id = |sig| Revision::from_signature(sig).unwrap().logical_id();
        let cpu = |sig| id(sig).map(|i| (i.cpu.family, i.cpu.revision, i.model));
        use logical::*;
        assert_eq!(cpu(0x00b5_0f00), Some((CPU, A0, Some(0))));
        assert_eq!(cpu(0x00b5_0f0b), Some((CPU, B0, Some(0))));
        // An unknown model-50h stepping takes the family's first row.
        assert_eq!(cpu(0x00b5_0f05), Some((CPU, DEFAULT, Some(1))));
        assert_eq!(cpu(0x00b5_0f10), Some((CPU, DEFAULT, Some(1))));
        assert_eq!(cpu(0x00b5_0f80), Some((CPU, MODEL_8, Some(8))));
        assert_eq!(cpu(0x00bc_0f00), Some((DENSE_CPU, A0, Some(0))));
        assert_eq!(cpu(0x00bc_0f03), Some((DENSE_CPU, A0, Some(0))));
        assert_eq!(cpu(0x00bc_0f80), Some((DENSE_CPU, DENSE_8, Some(8))));
        // No revision row has model 2.
        assert_eq!(id(0x00b5_0f20), None);
        assert!(id(0x00b5_0f00).unwrap().core.is_none());
    }

    #[test]
    fn apic_mode_follows_the_vendor() {
        use ApicModePolicy::*;
        assert_eq!(apic_mode(Auto, true, 0x3f), Some(ApicMode::X2Apic));
        assert_eq!(apic_mode(Auto, true, 0x3ff), Some(ApicMode::X2Apic));
        assert_eq!(apic_mode(X2Apic, false, 0x3f), Some(ApicMode::XApic));
        assert_eq!(apic_mode(Auto, false, 0xff), None);
        assert_eq!(apic_mode(XApic, true, 0xfe), Some(ApicMode::XApic));
        assert_eq!(apic_mode(XApic, true, 0x100), None);
        assert_eq!(apic_mode(Compatibility, true, 0xfe), Some(ApicMode::XApic));
        assert_eq!(apic_mode(Compatibility, true, 0xff), Some(ApicMode::X2Apic));
        assert_eq!(PlatformFlags::wh(ApicMode::X2Apic).0, 0x50);
        assert_eq!(PlatformFlags::wh(ApicMode::XApic).0, 0x60);
        assert_eq!(PlatformFlags::wh(ApicMode::Compatibility).0, 0x60);
    }

    #[test]
    fn scopes_use_the_initial_apic_id() {
        let t = |apic_id, smt| Thread {
            apic_id,
            smt,
            bsp: false,
        };
        assert!(t(0x10, true).in_scope(Scope::Complex) && t(0x10, true).primary());
        assert!(!t(0x11, true).primary() && !t(0x18, true).in_scope(Scope::Complex));
        assert!(t(0x8, false).in_scope(Scope::Complex) && t(0x9, false).primary());
        assert!(t(0x80, true).in_scope(Scope::Die) && !t(0x40, true).in_scope(Scope::Die));
        assert!(t(0x40, false).in_scope(Scope::Die));
        // x2APIC IDs above 255: the low byte, as CPUID 1 EBX[31:24].
        assert!(t(0x110, true).in_scope(Scope::Complex));
    }

    /// The generated table: well formed, WH-guarded, and the CCX report's
    /// counts (66 records for WH A0/B0, 19 for models 58h/C8h, 46 picking
    /// MCA banks by MCA_IPID).
    #[test]
    fn generated_table_applies_like_the_report() {
        let t = crate::ccx::tables::wh::RECORDS;
        assert_eq!(t.len(), 67);
        assert!(t.windows(2).all(|w| w[0].scope <= w[1].scope));
        assert!(t.iter().all(|r| r.set & !r.clear == 0 && r.clear != 0));
        assert!(
            t.iter()
                .all(|r| r.cpu.is_none_or(|c| c.family & logical::TABLE != 0))
        );
        assert!(t.iter().all(|r| r.core.is_none()));
        assert_eq!(t.iter().filter(|r| r.msr_test.is_some()).count(), 46);
        let applying = |sig, apic| {
            let ids = Revision::from_signature(sig).unwrap().logical_id().unwrap();
            let flags = PlatformFlags::wh(apic);
            t.iter().filter(|r| r.applies(&ids, flags)).count()
        };
        for sig in [0x00b5_0f00, 0x00b5_0f0b] {
            assert_eq!(applying(sig, ApicMode::X2Apic), 66);
            assert_eq!(applying(sig, ApicMode::XApic), 66);
        }
        assert_eq!(applying(0x00b5_0f80, ApicMode::X2Apic), 19);
        assert_eq!(applying(0x00bc_0f80, ApicMode::X2Apic), 19);
    }

    fn threads(apic: ApicMode, microcode: Microcode) -> Threads {
        let mut name = [0u8; 48];
        name[..8].copy_from_slice(b"AMD EPYC");
        Threads {
            revision: Revision::from_signature(0x00b5_0f00).unwrap(),
            policy: Policy {
                disable_l1_stream: true,
                disable_l2_stream: true,
                ..Policy::default()
            },
            microcode,
            apic,
            brand: Some(name),
        }
    }

    const BSP: Thread = Thread {
        apic_id: 0,
        smt: true,
        bsp: true,
    };

    #[test]
    fn thread_init_runs_the_vendor_steps_on_a_bsp() {
        let threads = threads(ApicMode::X2Apic, Microcode::Asp);
        let cpu = MsrSpace::new([
            // The MP engine already switched to x2APIC.
            (0x1b, 0xfee0_0d00),
            (0x179, 32),
            // Bank 7 is an L3 (HWID B0h, type 7) supporting TEL and deferred
            // logging, with TEL on.
            (0xc000_2074, 0x2_0000_0206),
            (0xc000_2075, 0x7_00b0_0000_0000),
            (0xc001_0296, 0xffff_ffff_0000_0080),
            (0xc001_0064, 0x8000_0000_0000_1234),
            (0xc001_02b0, 0xc8_80_40_10),
            (0xc001_102b, 0x5),
            (0xc001_1020, 1 << 53),
            (0xc001_1004, 1 << 53),
        ]);
        assert!(thread_init_with(&threads, &cpu, BSP));
        let regs = cpu.into_inner();
        // Watchdog first, disabled.
        assert_eq!(regs.writes[0], (0xc001_0074, 0));
        assert_eq!(regs.get(PATCH_ADDR_S3.index()), 0);
        assert_eq!(regs.get(PATCH_LOADER.index()), u64::from(u32::MAX));
        // The table's EXTD record agrees with the engine's mode.
        assert_eq!(regs.get(0x1b), 0xfee0_0d00);
        assert_eq!(regs.get(0xc001_0015), 0x800_6000, "no HWCR[32] in x2APIC");
        // Table, then McaConfigInit: MCAX, no TEL, deferred in MCA_STATUS.
        assert_eq!(regs.get(0xc000_2074), 0x5_0000_0006);
        assert_eq!(regs.get(0xc000_2084), 1 << 32, "bank 8 is not an L3");
        assert_eq!(regs.get(0xc001_0010), 0x14_0000, "core scope ran");
        assert_eq!(regs.get(0xc000_2004), 0x5_0000_0000, "complex scope ran");
        assert_eq!(regs.get(0xc001_0114), 0x8, "SVM on and locked");
        assert!(regs.written(0xc001_0118));
        assert_eq!(regs.get(0xc001_0030), u64::from_le_bytes(*b"AMD EPYC"));
        assert_eq!(regs.get(0xc001_1004), 1 << 53, "x2APIC stays in CPUID");
        assert_eq!(regs.get(0xc001_1020), 0x4_4000_0000_0000);
        assert_eq!(regs.get(0xc001_10e5), (1 << 30) | (1 << 33));
        assert!(regs.written(0xc001_0064), "P0 rewrite");
        assert_eq!(regs.get(0xc001_0064), 0x8000_0000_0000_1234);
        assert_eq!(regs.get(0xc001_10e7), (1 << 16) | (1 << 6));
        assert_eq!(regs.get(0xc001_102b), 0x4, "L2 stream off = bit 0 clear");
        assert_eq!(regs.get(0xc001_0296), 0xffff_ffff_4848_48c8);
        assert_eq!(regs.get(0xc001_0298), 0x4848_4848);
        assert_eq!(regs.get(0xc001_0073), 0x813);
        assert_eq!(regs.get(TW_CFG.index()), 1 << 49);
        assert_eq!(regs.get(0xc001_02b3), 0x40c8);
        assert_eq!(regs.writes.last().unwrap().0, 0xc001_02b3, "CPPC last");
    }

    #[test]
    fn thread_init_on_an_xapic_sibling() {
        let threads = threads(ApicMode::XApic, Microcode::Asp);
        let cpu = MsrSpace::new([
            (0x1b, 0xfee0_0800),
            (0xc001_1004, 1 << 53),
            (0xc001_02b3, 0x2000),
        ]);
        let sibling = Thread {
            apic_id: 0x11,
            smt: true,
            bsp: false,
        };
        assert!(thread_init_with(&threads, &cpu, sibling));
        let regs = cpu.into_inner();
        assert!(!regs.written(0xc001_0074), "watchdog: core primaries");
        assert!(!regs.written(PATCH_ADDR_S3.index()), "BSP only");
        assert_eq!(regs.get(PATCH_LOADER.index()), u64::from(u32::MAX));
        assert_eq!(regs.get(0x1b), 0xfee0_0800, "no EXTD in xAPIC");
        assert_eq!(regs.get(0xc001_0015) >> 32, 1, "HWCR[32] in xAPIC");
        assert_eq!(regs.get(0xc001_1004), 0, "x2APIC hidden from CPUID");
        assert!(!regs.written(0xc001_0296) && !regs.written(0xc001_0298));
        assert!(!regs.written(0xc001_0010), "core scope skipped");
        assert_eq!(regs.get(0xc001_0073), 0x813);
        assert_eq!(regs.get(TW_CFG.index()), 1 << 49);
        assert_eq!(regs.get(0xc001_02b3), 0x2000, "the minimum kept");
    }

    #[test]
    fn host_microcode_without_a_patch_only_clears_the_s3_address() {
        let threads = threads(ApicMode::X2Apic, Microcode::Host(None));
        let cpu = MsrSpace::new([(0xc001_100c, 0x1234)]);
        assert!(thread_init_with(&threads, &cpu, BSP));
        let regs = cpu.into_inner();
        assert_eq!(regs.get(0xc001_100c), 0);
        assert!(!regs.written(PATCH_LOADER.index()));
    }

    #[test]
    fn apic_id_decodes_cpuid_subleaves_complex_and_smt() {
        let shifts = TopologyShifts::from_cpuid_subleaves(&[
            (1, 1 << 8),
            (5, 2 << 8),
            (6, 3 << 8),
            (10, 4 << 8),
        ])
        .unwrap();
        assert_eq!(
            apic_id(shifts, 0, 4, 1, 1, 2, 1, true),
            Some((5 << 6) | (1 << 5) | (2 << 1) | 1)
        );
        assert_eq!(apic_id(shifts, 0, 4, 1, 1, 2, 1, false), None);
        assert_eq!(apic_id(shifts, 0, 4, 1, 2, 2, 0, false), None);
        assert_eq!(
            apic_id(shifts, 1, 4, 0, 0, 0, 0, false),
            Some((1 << 10) | (4 << 6))
        );
    }
}
