// SPDX-License-Identifier: MIT OR Apache-2.0
//! WH (Venice; Zen 6 / Zen 6c) CCX identification, dual-word SMN AP thread
//! release, CPUID leaf `0x8000_0026` extended x2APIC ID calculation, and
//! per-thread MSR initialization ([`thread_init`]).
//!
//! Verified against openSIL `CcxLaunchThreadWh`, `CcxCalcLocalApicId`,
//! `InitializeCcxAndLaunchAps`, `Zen6SetMiscMsrs`, and `Zen6CcxPrefetcher`.

use fstart_arch::x86::cpu::msr::{APIC_BASE, ApicBase, Msr, MsrAccess};
use heapless::Vec;
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};
use tock_registers::register_bitfields;

use super::microcode::{Load, Patch};
use super::msr::{CSTATE_BASE_ADDR, CSTATE_CONFIG, CstateBaseAddr, TW_CFG, TwCfg};
use super::pm::{self, Wdt};
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
    pub L2Prefetch [
        L2_STREAM_DIS OFFSET(0) NUMBITS(1) [],
        L2_UP_DOWN_DIS OFFSET(2) NUMBITS(1) []
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

/// Linear IOD index `(socket?2:0) + (die?1:0)` for dual-IOD sockets.
pub const fn iod_index(socket: u8, die: u8) -> Option<u8> {
    if socket >= 2 || die >= 2 {
        return None;
    }
    Some((socket << 1) | die)
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchError {
    /// Expected BSP thread 0 on logical CCD 0 / core 0 is missing.
    Bsp,
    /// Asymmetric SMT or out-of-range thread bit (> 63).
    Threads,
    /// More APs than buffer capacity `N`.
    TooMany,
}

/// Build the AP release order for `(socket, die)` from its APOB V2 `3/3`
/// [`CoreMapV2`], matching openSIL `CcxLaunchThreadWh`:
/// `bit = logical_core_in_ccd * threads_per_core + thread`.
///
/// Pass `skip_bsp = true` for `(socket = 0, die = 0)` so thread 0 of the first
/// logical core (already running as BSP) is omitted.
pub fn launch_order<const N: usize>(
    socket: u8,
    die: u8,
    map: &CoreMapV2,
    skip_bsp: bool,
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
        for ccx in &ccd.ccxs {
            for core in &ccx.cores {
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
                    out.push(Launch {
                        socket,
                        die,
                        phys_ccd: ccd.id,
                        bit: bit as u8,
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

    /// True on the first hardware thread of its core (thread 0).
    pub const fn primary(&self) -> bool {
        !self.smt || (self.apic_id & 1) == 0
    }
}

/// Board/platform policy applied on every Zen 6 hardware thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    /// Enable CC6 in `CSTATE_CONFIG` (`0x484848`).
    pub cc6: bool,
    /// ACPI C-state I/O base address (`CSTATE_BASE_ADDR`, vendor default `0x813`).
    pub cstate_io_base: u16,
    /// Optional CPU watchdog configuration (`CPU_WDT_CFG`).
    pub wdt: Option<Wdt>,
    /// Optional write-combining speculation override (`LS_CFG` + `LS_CFG3`).
    pub wc_spec: Option<bool>,
    /// Optional L1/L2 hardware prefetcher disable overrides (`None` keeps reset
    /// defaults — all hardware prefetchers enabled).
    pub disable_l1_stream: bool,
    pub disable_l1_stride: bool,
    pub disable_l1_region: bool,
    pub disable_l2_stream: bool,
    pub disable_l2_up_down: bool,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            cc6: true,
            cstate_io_base: 0x813,
            wdt: None,
            wc_spec: None,
            disable_l1_stream: false,
            disable_l1_stride: false,
            disable_l1_region: false,
            disable_l2_stream: false,
            disable_l2_up_down: false,
        }
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
}

/// Per-thread Zen 6 initialization hook for [`super::mp::Fam17hCpu::multi_socket`].
///
/// Matches openSIL `InitializeCcxAndLaunchAps` / `Zen6SetMiscMsrs` /
/// `Zen6CcxPrefetcher`:
/// 1. Ensure APIC global enable (`APIC_BASE[11]`).
/// 2. Load microcode patch on primary thread of each core (recording S3 address
///    on the BSP).
/// 3. Set `TW_CFG.COMBINE_CR0_CD` (bit 49) so SMT siblings share cache-disable.
/// 4. Mask `NBIO_PCIE_SIDEBAND` in MCA so OS AER owns PCIe errors.
/// 5. Program `CSTATE_CONFIG` (on core primary) and `CSTATE_BASE_ADDR`.
/// 6. Apply optional CPU watchdog, write-combining speculation, and L1/L2
///    prefetcher overrides.
///
/// Returns `false` if microcode loading failed.
pub fn thread_init<M: MsrAccess + ?Sized>(threads: &Threads, cpu: &M) -> bool {
    let thread = Thread::current(cpu);
    thread_init_with(threads, cpu, thread)
}

fn thread_init_with<M: MsrAccess + ?Sized>(threads: &Threads, cpu: &M, thread: Thread) -> bool {
    cpu.reg(APIC_BASE).modify(ApicBase::ENABLE::SET);

    let mut ok = true;
    if thread.primary() {
        match &threads.microcode {
            Microcode::Asp => cpu
                .reg(super::microcode::PATCH_LOADER)
                .set(u64::from(u32::MAX)),
            Microcode::Host(Some(patch)) => {
                let load = Load {
                    record_for_s3: thread.bsp,
                    ..Load::default()
                };
                // SAFETY: ring 0, caching on; patch lives in identity-mapped reserved FFS.
                ok = unsafe { patch.apply(cpu, patch.address(), threads.revision.signature, load) }
                    .is_ok();
            }
            Microcode::Host(None) => {}
        }
    }

    cpu.reg(TW_CFG).modify(TwCfg::COMBINE_CR0_CD::SET);
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

    if threads.policy.disable_l1_stream
        || threads.policy.disable_l1_stride
        || threads.policy.disable_l1_region
    {
        let mut r = cpu.reg(L1_PREFETCH).extract();
        if threads.policy.disable_l1_stream {
            r.modify(L1Prefetch::L1_STREAM_DIS::SET);
        }
        if threads.policy.disable_l1_stride {
            r.modify(L1Prefetch::L1_STRIDE_DIS::SET);
        }
        if threads.policy.disable_l1_region {
            r.modify(L1Prefetch::L1_REGION_DIS::SET);
        }
        cpu.reg(L1_PREFETCH).set(r.get());
    }
    if threads.policy.disable_l2_stream || threads.policy.disable_l2_up_down {
        let mut r = cpu.reg(L2_PREFETCH).extract();
        if threads.policy.disable_l2_stream {
            r.modify(L2Prefetch::L2_STREAM_DIS::SET);
        }
        if threads.policy.disable_l2_up_down {
            r.modify(L2Prefetch::L2_UP_DOWN_DIS::SET);
        }
        cpu.reg(L2_PREFETCH).set(r.get());
    }
    ok
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

        // Die 0 skips BSP (bit 0) -> 63 APs, bits 1..=63.
        let die0 = launch_order::<64>(0, 0, &map, true).unwrap();
        assert_eq!(die0.len(), 63);
        assert_eq!(
            die0[0],
            Launch {
                socket: 0,
                die: 0,
                phys_ccd: 4,
                bit: 1
            }
        );
        assert_eq!(
            die0[62],
            Launch {
                socket: 0,
                die: 0,
                phys_ccd: 4,
                bit: 63
            }
        );

        // Die 1 includes bit 0 -> all 64 APs.
        let die1 = launch_order::<64>(0, 1, &map, false).unwrap();
        assert_eq!(die1.len(), 64);
        assert_eq!(
            die1[0],
            Launch {
                socket: 0,
                die: 1,
                phys_ccd: 4,
                bit: 0
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
    fn thread_init_programs_primary_vs_sibling_msrs() {
        let rev = Revision::from_signature(0x00b5_0f00).unwrap();
        let threads = Threads {
            revision: rev,
            policy: Policy {
                cc6: true,
                cstate_io_base: 0x813,
                wdt: Some(Wdt::DEFAULT),
                wc_spec: Some(true),
                disable_l1_stream: true,
                disable_l2_stream: true,
                ..Policy::default()
            },
            microcode: Microcode::Asp,
        };
        let primary = MsrSpace::new([(0x1b, 0xfee0_0100), (0xc001_1020, 1 << 53)]);
        assert!(thread_init_with(
            &threads,
            &primary,
            Thread {
                apic_id: 0,
                smt: true,
                bsp: true
            }
        ));
        let regs = primary.into_inner();
        assert_eq!(
            regs.get(super::super::microcode::PATCH_LOADER.index()),
            u64::from(u32::MAX)
        );
        assert_eq!(regs.get(0x1b), 0xfee0_0900);
        assert_eq!(regs.get(TW_CFG.index()), 1 << 49);
        assert_eq!(regs.get(CSTATE_CONFIG.index()), 0x48_4848);
        assert_eq!(regs.get(CSTATE_BASE_ADDR.index()), 0x813);
        assert_eq!(regs.get(0xc001_1020), 0);
        assert_eq!(regs.get(0xc001_10e5), (1 << 30) | (1 << 33));
        assert_eq!(regs.get(0xc001_10e7), 1 << 16);
        assert_eq!(regs.get(0xc001_102b), 1);

        let sibling = MsrSpace::new([(0x1b, 0xfee0_0000)]);
        assert!(thread_init_with(
            &threads,
            &sibling,
            Thread {
                apic_id: 1,
                smt: true,
                bsp: false
            }
        ));
        let sregs = sibling.into_inner();
        assert!(!sregs.written(super::super::microcode::PATCH_LOADER.index()));
        assert!(!sregs.written(CSTATE_CONFIG.index()));
        assert_eq!(sregs.get(CSTATE_BASE_ADDR.index()), 0x813);
        assert_eq!(sregs.get(TW_CFG.index()), 1 << 49);
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
