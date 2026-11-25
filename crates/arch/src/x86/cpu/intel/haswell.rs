//! Intel Haswell CPU model driver (traditional, ULT, and Crystal Well).
//!
//! Follows coreboot's `cpu/intel/haswell`: early nominal ratio / flex-ratio
//! setup, SMM IED + PRMRR relocation and fetch-lock verification, per-CPU
//! thermal/C-state/DCA/EPB configuration, and package RAPL power limits.

pub use super::haswell_vr::VrConfig;

use super::{
    ratio as common,
    smm::{SmmCpu, SmrrPair, X86SaveStateFormat},
};
use crate::x86::{
    cpu::msr::{MCG_CAP, MTRR_CAP, McgCap, Msr, MsrAccess, NativeMsr},
    mp::{CpuDriver, CpuIdMatch, CpuVendor},
};
use tock_registers::{
    interfaces::{ReadWriteable, Readable, Writeable},
    register_bitfields,
};

register_bitfields![u64,
    /// MSR_PIC_MSG_CONTROL (0x2e).
    PicMsg [
        TPR_DISABLE OFFSET(10) NUMBITS(1) []
    ],
    /// MSR_PKG_CST_CONFIG_CONTROL (0xe2); preserves fused package C-state limit.
    PkgCstConfig [
        IO_REDIRECT OFFSET(10) NUMBITS(1) [],
        LOCK OFFSET(15) NUMBITS(1) [],
        DEMOTION OFFSET(25) NUMBITS(6) [],
        TIMED_MWAIT OFFSET(31) NUMBITS(1) []
    ],
    /// MSR_FLEX_RATIO (0x194).
    FlexRatio [
        RATIO OFFSET(8) NUMBITS(8) [],
        ENABLE OFFSET(16) NUMBITS(1) [],
        LOCK OFFSET(20) NUMBITS(1) []
    ],
    /// IA32_ENERGY_PERF_BIAS (0x1b0).
    EnergyPerfBias [
        POLICY OFFSET(0) NUMBITS(4) []
    ],
    /// IA32_PLATFORM_DCA_CAP (0x1f8).
    PlatformDcaCap [
        ENABLE OFFSET(0) NUMBITS(1) []
    ]
];

const MSR_PIC_MSG_CONTROL: Msr<PicMsg::Register> = Msr::new(0x2e);
const MSR_PKG_CST_CONFIG_CONTROL: Msr<PkgCstConfig::Register> = Msr::new(0xe2);
const MSR_SMM_MCA_CAP: Msr = Msr::new(0x17d);
const MSR_FLEX_RATIO: Msr<FlexRatio::Register> = Msr::new(0x194);
const IA32_ENERGY_PERF_BIAS: Msr<EnergyPerfBias::Register> = Msr::new(0x1b0);
const IA32_PACKAGE_THERM_INTERRUPT: Msr = Msr::new(0x1b2);
const MSR_PRMRR_PHYS_BASE: Msr = Msr::new(0x1f4);
const MSR_PRMRR_PHYS_MASK: Msr = Msr::new(0x1f5);
const IA32_PLATFORM_DCA_CAP: Msr<PlatformDcaCap::Register> = Msr::new(0x1f8);
const MSR_LT_LOCK_MEMORY: Msr = Msr::new(0x2e7);
const MSR_UNCORE_PRMRR_PHYS_BASE: Msr = Msr::new(0x2f4);
const MSR_UNCORE_PRMRR_PHYS_MASK: Msr = Msr::new(0x2f5);
const MSR_SMM_FEATURE_CONTROL: Msr = Msr::new(0x4e0);
const MSR_PKGC3_IRTL: u32 = 0x60a;
const MSR_PKGC6_IRTL: u32 = 0x60b;
const MSR_PKGC7_IRTL: u32 = 0x60c;
const MSR_PKG_POWER_LIMIT: Msr = Msr::new(0x610);
const MSR_PKG_POWER_SKU: Msr = Msr::new(0x614);
const MSR_DDR_RAPL_LIMIT: Msr = Msr::new(0x618);
const MSR_PKGC8_IRTL: u32 = 0x633;
const MSR_PKGC9_IRTL: u32 = 0x634;
const MSR_PKGC10_IRTL: u32 = 0x635;
const MSR_CONFIG_TDP_NOMINAL: Msr = Msr::new(0x648);
const MSR_TURBO_ACTIVATION_RATIO: Msr = Msr::new(0x64c);

const IA32_MC0_CTL: u32 = 0x400;
const IA32_MC0_STATUS: u32 = 0x401;

const SMM_MCA_CAP_CODE_ACCESS_CHK: u64 = 1 << 58;
const SMM_FEATURE_LOCK: u64 = 1 << 0;
const SMM_CODE_CHK_EN: u64 = 1 << 2;
const MTRR_CAP_PRMRR: u64 = 1 << 12;
const IRTL_VALID_1024NS: u64 = 0x8800;
const PKG_THERM_ENABLE_POWER_LIMIT: u64 = 1 << 4;
const CPUID1_ECX_DCA: u32 = 1 << 18;
const CPUID6_ECX_EPB: u32 = 1 << 3;
const EPB_NORMAL_POLICY: u64 = 6;
const CPUID_MODEL_HASWELL_ULT: u32 = 0x40650;

/// Set the CPU to its maximum non-turbo (or nominal cTDP) frequency early in
/// the bootblock, before memory initialization (coreboot
/// `cpu/intel/haswell/romstage.c:set_max_freq`).
///
/// # Safety
///
/// Must run on a Haswell CPU.
pub unsafe fn set_max_freq() {
    // SAFETY: caller guarantees Haswell CPU.
    let cpu = unsafe { NativeMsr::new() };
    program_max_non_turbo_freq(&cpu);
}

fn program_max_non_turbo_freq(cpu: &impl MsrAccess) {
    let platform = cpu.reg(common::PLATFORM_INFO);
    let ratio = if platform.read(common::PlatformInfo::CONFIG_TDP_LEVELS) != 0 {
        cpu.reg(MSR_CONFIG_TDP_NOMINAL).get() & 0xff
    } else {
        platform.read(common::PlatformInfo::MAX_NON_TURBO_RATIO)
    };
    cpu.reg(common::IA32_PERF_CTL)
        .write(common::PerfCtl::RATIO_SNB.val(ratio));
}

/// Program the CPU half of the bootblock flex-ratio handshake. The PCH must
/// stage the returned ratio in `SOFT_RESET_DATA` and issue a CF9 CPU reset.
///
/// # Safety
///
/// The BSP must be Haswell and the platform must be able to issue a CPU reset.
pub unsafe fn nominal_flex_ratio() -> Option<u8> {
    // SAFETY: caller guarantees Haswell BSP.
    let cpu = unsafe { NativeMsr::new() };
    let flex = cpu.reg(MSR_FLEX_RATIO);
    if !flex.is_set(FlexRatio::ENABLE)
        || cpu
            .reg(common::PLATFORM_INFO)
            .read(common::PlatformInfo::CONFIG_TDP_LEVELS)
            == 0
    {
        return None;
    }
    let nominal = cpu.reg(MSR_CONFIG_TDP_NOMINAL).get() & 0xff;
    if flex.read(FlexRatio::RATIO) == nominal {
        return None;
    }
    flex.modify(FlexRatio::RATIO.val(nominal) + FlexRatio::LOCK::SET);
    Some(nominal as u8)
}

/// Lock the memory configuration MSR (`MSR_LT_LOCK_MEMORY`).
///
/// # Safety
///
/// Must run on a Haswell BSP after the system agent's memory map is final.
pub unsafe fn lock_memory_config() {
    // SAFETY: caller guarantees Haswell BSP.
    let cpu = unsafe { NativeMsr::new() };
    let reg = cpu.reg(MSR_LT_LOCK_MEMORY);
    reg.set(reg.get() | 1);
}

/// Enable and lock `SMM_CODE_CHK_EN` inside the permanent SMRR-protected SMM
/// handler. Halts with POST `0xe7` if the advertised lock is ineffective.
///
/// # Safety
///
/// Must execute inside the permanent SMRR-protected Haswell SMM handler, never
/// from the temporary relocation trampoline.
#[inline(always)]
pub unsafe fn lock_smm_fetches() {
    // Lives in this permanent image's SMRAM BSS, not in ramstage. One
    // rendezvous owner dispatches at a time; S3 installs a fresh image.
    use core::sync::atomic::{AtomicBool, Ordering};
    static CHECKED: AtomicBool = AtomicBool::new(false);
    if CHECKED.load(Ordering::Relaxed) {
        return;
    }
    // SAFETY: permanent Haswell SMM context.
    let cpu = unsafe { NativeMsr::new() };
    if !fetch_lock_effective(&cpu) {
        // SMM must not dereference a ramstage logger/vtable. POST 0xe7 means
        // the advertised package lock was ineffective (including pre-lock).
        unsafe { fstart_core::pio::outb(0x80, 0xe7) };
        crate::halt();
    }
    CHECKED.store(true, Ordering::Relaxed);
}

#[inline(always)]
fn fetch_lock_effective(cpu: &impl MsrAccess) -> bool {
    if cpu.reg(MSR_SMM_MCA_CAP).get() & SMM_MCA_CAP_CODE_ACCESS_CHK != 0 {
        let ctl = cpu.reg(MSR_SMM_FEATURE_CONTROL);
        let target = SMM_FEATURE_LOCK | SMM_CODE_CHK_EN;
        if ctl.get() & SMM_FEATURE_LOCK == 0 {
            ctl.set(ctl.get() | target);
        }
        return ctl.get() & target == target;
    }
    true
}

const fn id(signature: u32) -> CpuIdMatch {
    CpuIdMatch {
        vendor: CpuVendor::Intel,
        signature,
        mask: CpuIdMatch::EXACT_MASK,
    }
}

/// CPU driver for Intel Haswell (traditional, ULT, and Crystal Well).
pub struct HaswellCpu {
    pub microcode: Option<&'static [u8]>,
    pub tcc_offset: u8,
}

impl SmmCpu for HaswellCpu {
    fn requires_smrr(&self) -> bool {
        true
    }

    fn ied_bytes() -> u32 {
        4 << 20
    }

    unsafe fn initialize_ied(base: u32) {
        let p = base as usize as *mut u8;
        let mut header = [0u8; 48];
        header[..10].copy_from_slice(b"INTEL RSVD");
        header[10..14].copy_from_slice(&Self::ied_bytes().to_le_bytes());
        // SAFETY: caller guarantees `base` points to `ied_bytes()` writable bytes in open SMRAM.
        unsafe {
            core::ptr::copy_nonoverlapping(header.as_ptr(), p, header.len());
            core::ptr::write_bytes(p.add(1 << 20), 0, 32 << 10);
        }
    }

    unsafe fn relocate_ied(state: fstart_smm::X86SaveState, ied: u32) -> bool {
        if state.write_iedbase(ied).is_err() {
            return false;
        }
        // SAFETY: relocation SMI runs on Haswell CPU with architectural MSRs.
        let cpu = unsafe { NativeMsr::new() };
        if cpu.reg(MTRR_CAP).get() & MTRR_CAP_PRMRR != 0 {
            let base = u64::from(ied) + (2 << 20);
            let bits = crate::x86::cpuid(0x8000_0008).0 & 0xff;
            if !(32..=52).contains(&bits) {
                return false;
            }
            let mask = |bits: u32| (((1u64 << bits) - 1) & !((2 << 20) - 1)) | 0x800;
            cpu.reg(MSR_PRMRR_PHYS_BASE).set(base | 6);
            cpu.reg(MSR_PRMRR_PHYS_MASK).set(mask(bits));
            if crate::x86::lapic::Lapic::is_bsp() {
                cpu.reg(MSR_UNCORE_PRMRR_PHYS_BASE).set(base);
                cpu.reg(MSR_UNCORE_PRMRR_PHYS_MASK).set(mask(39));
            }
        }
        true
    }

    fn after_relocation(&self) {
        // SAFETY: architectural MTRR_CAP MSR on Haswell.
        if crate::x86::lapic::Lapic::is_bsp()
            && unsafe { NativeMsr::new() }.reg(MTRR_CAP).get() & MTRR_CAP_PRMRR == 0
        {
            fstart_log::info!("SMM: PRMRR not advertised; IED remains covered by SMRR");
        }
        let old = super::microcode::current_revision();
        // SAFETY: firmware microcode blob on Haswell CPU after SMM relocation.
        match unsafe { super::microcode::reload_current_cpu(self.microcode.unwrap_or_default()) } {
            Ok(()) => fstart_log::info!(
                "microcode: relocation reload {:#x} -> {:#x}",
                old,
                super::microcode::current_revision()
            ),
            Err(e) => fstart_log::warn!(
                "microcode: relocation reload skipped/failed at revision {:#x}: {:?}",
                old,
                e
            ),
        }
    }

    fn smm_save_state_format(&self) -> X86SaveStateFormat {
        X86SaveStateFormat::IntelEm64t
    }

    fn smrr_pair(&self) -> Option<SmrrPair> {
        Some(SmrrPair::Architectural)
    }
}

impl CpuDriver for HaswellCpu {
    fn name(&self) -> &'static str {
        "Intel Haswell"
    }

    fn id_table(&self) -> &'static [CpuIdMatch] {
        // Traditional (B0/C0), ULT (B0/C0/D0), and Crystal Well (B0/C0).
        const IDS: &[CpuIdMatch] = &[
            id(0x306c2),
            id(0x306c3),
            id(0x40650),
            id(0x40651),
            id(0x40660),
            id(0x40661),
        ];
        IDS
    }

    fn update_microcode(&self) {
        // SAFETY: firmware-owned microcode blob on matched Haswell CPU.
        unsafe { super::microcode::update_current_cpu_logged(self.microcode.unwrap_or_default()) };
    }

    fn logical_cpu_count(&self) -> Option<u16> {
        // SAFETY: MSR_CORE_THREAD_COUNT exists on every Haswell SKU.
        let cpu = unsafe { NativeMsr::new() };
        let n = cpu
            .reg(common::CORE_THREAD_COUNT)
            .read(common::CoreThreadCount::THREADS) as u16;
        (n != 0).then_some(n)
    }

    fn init_cpu(&self) {
        // SAFETY: matched Haswell CPU.
        let cpu = unsafe { NativeMsr::new() };
        self.configure(&cpu, crate::x86::cpuid);
    }

    fn post_mp_init(&self) {
        set_max_ratio();
    }
}

impl HaswellCpu {
    fn configure(&self, cpu: &impl MsrAccess, cpuid: impl Fn(u32) -> (u32, u32, u32, u32)) {
        let platform = cpu.reg(common::PLATFORM_INFO);

        // Enable all MCA banks and clear stale error status.
        let banks = cpu.reg(MCG_CAP).read(McgCap::COUNT) as u32;
        for i in 0..banks {
            cpu.reg(Msr::<()>::new(IA32_MC0_CTL + 4 * i)).set(u64::MAX);
        }
        for i in 0..banks {
            cpu.reg(Msr::<()>::new(IA32_MC0_STATUS + 4 * i)).set(0);
        }

        cpu.reg(MSR_PIC_MSG_CONTROL)
            .modify(PicMsg::TPR_DISABLE::CLEAR);

        let (signature, _, features, _) = cpuid(1);
        super::feature_control::configure(cpu, features, 0);

        let mut cst = PkgCstConfig::IO_REDIRECT::CLEAR
            + PkgCstConfig::LOCK::SET
            + PkgCstConfig::DEMOTION.val(0x3f);
        if platform.is_set(common::PlatformInfo::TIMED_MWAIT_ENABLE) {
            cst += PkgCstConfig::TIMED_MWAIT::SET;
        }
        cpu.reg(MSR_PKG_CST_CONFIG_CONTROL).modify(cst);

        cpu.reg(common::MISC_PWR_MGMT)
            .modify(common::MiscPwrMgmt::EIST_HW_COORDINATION_DISABLE::CLEAR);
        cpu.reg(common::POWER_CTL).modify(
            common::PowerCtl::EPB_ENABLE::SET
                + common::PowerCtl::C1E::SET
                + common::PowerCtl::BIDIRECTIONAL_PROCHOT::SET,
        );

        // Package C-state interrupt response time limits (1024 ns units).
        for (msr, latency) in [
            (MSR_PKGC3_IRTL, 0x42),
            (MSR_PKGC6_IRTL, 0x73),
            (MSR_PKGC7_IRTL, 0x91),
            (MSR_PKGC8_IRTL, 0xe4),
            (MSR_PKGC9_IRTL, 0x145),
            (MSR_PKGC10_IRTL, 0x1ef),
        ] {
            if msr >= MSR_PKGC8_IRTL && (signature & !0xf) != CPUID_MODEL_HASWELL_ULT {
                break;
            }
            cpu.reg(Msr::<()>::new(msr))
                .set(IRTL_VALID_1024NS | latency);
        }

        cpu.reg(common::IA32_MISC_ENABLE).modify(
            common::MiscEnable::FAST_STRINGS::SET
                + common::MiscEnable::THERMAL_MONITOR::SET
                + common::MiscEnable::EIST::SET,
        );
        cpu.reg(common::IA32_THERM_INTERRUPT).set(0);
        cpu.reg(IA32_PACKAGE_THERM_INTERRUPT)
            .set(PKG_THERM_ENABLE_POWER_LIMIT);

        if self.tcc_offset != 0 && platform.is_set(common::PlatformInfo::PROGRAMMABLE_TJ_OFFSET) {
            cpu.reg(common::TEMPERATURE_TARGET)
                .modify(common::TemperatureTarget::TCC_OFFSET.val(self.tcc_offset.into()));
        }
        if features & CPUID1_ECX_DCA != 0 {
            cpu.reg(IA32_PLATFORM_DCA_CAP)
                .modify(PlatformDcaCap::ENABLE::SET);
        }
        if cpuid(6).2 & CPUID6_ECX_EPB != 0 {
            cpu.reg(IA32_ENERGY_PERF_BIAS)
                .modify(EnergyPerfBias::POLICY.val(EPB_NORMAL_POLICY));
        }
        if common::turbo_state(cpu) == common::TurboState::Disabled {
            cpu.reg(common::IA32_MISC_ENABLE)
                .modify(common::MiscEnable::TURBO_DISABLE::CLEAR);
        }
    }
}

fn set_max_ratio() {
    // SAFETY: called from HaswellCpu::post_mp_init on Haswell BSP.
    let cpu = unsafe { NativeMsr::new() };
    let p = cpu.reg(common::PLATFORM_INFO);
    let ratio = if common::turbo_state(&cpu) == common::TurboState::Enabled {
        cpu.reg(common::TURBO_RATIO_LIMIT)
            .read(common::TurboRatioLimit::ONE_CORE)
    } else if p.read(common::PlatformInfo::CONFIG_TDP_LEVELS) != 0 {
        cpu.reg(MSR_CONFIG_TDP_NOMINAL).get() & 0xff
    } else {
        p.read(common::PlatformInfo::MAX_NON_TURBO_RATIO)
    };
    cpu.reg(common::IA32_PERF_CTL)
        .write(common::PerfCtl::RATIO_SNB.val(ratio));
}

/// Encode PL1 (28 seconds or SKU maximum time, whichever is longer) and
/// PL2 (125% TDP). Times use `Y[4:0]`, `Z[6:5]`: `2^Y * (1 + Z/4)`.
fn encode_power_limits(sku: u64) -> u64 {
    const TWENTY_EIGHT_SECONDS: u64 = 0x6e;
    const LIMIT_ENABLE: u64 = 1 << 15;

    let mut tdp = sku & 0x7fff;
    let min = (sku >> 16) & 0x7fff;
    let max = (sku >> 32) & 0x7fff;
    tdp = tdp.max(min);
    if max != 0 {
        tdp = tdp.min(max);
    }
    let sku_time = (sku >> 48) & 0x7f;
    let ticks = |v: u64| (4 + (v >> 5)) << (v & 31);
    let time = if ticks(sku_time) > ticks(TWENTY_EIGHT_SECONDS) {
        sku_time
    } else {
        TWENTY_EIGHT_SECONDS
    };
    let pl1 = tdp | LIMIT_ENABLE | (time << 17);
    let pl2 = (tdp * 5 / 4).min(0x7fff) | LIMIT_ENABLE;
    pl1 | (pl2 << 32)
}

/// Program package RAPL limits, mirror them into MCHBAR via the callback, and
/// copy the returned DDR RAPL limit back to `MSR_DDR_RAPL_LIMIT`.
///
/// # Safety
///
/// Must run on a Haswell BSP at least 1 ms after `BIOS_RESET_CPL`.
pub unsafe fn set_power_limits(mirror_and_read_ddr: impl FnOnce(u64) -> u64) {
    // SAFETY: caller guarantees Haswell BSP after BIOS_RESET_CPL.
    program_limits(&unsafe { NativeMsr::new() }, mirror_and_read_ddr);
}

fn program_limits(cpu: &impl MsrAccess, mirror_and_read_ddr: impl FnOnce(u64) -> u64) {
    let platform = cpu.reg(common::PLATFORM_INFO);
    if !platform.is_set(common::PlatformInfo::PROGRAMMABLE_TDP_LIMIT) {
        return;
    }
    let limit = encode_power_limits(cpu.reg(MSR_PKG_POWER_SKU).get());
    cpu.reg(MSR_PKG_POWER_LIMIT).set(limit);
    cpu.reg(MSR_DDR_RAPL_LIMIT).set(mirror_and_read_ddr(limit));
    if platform.read(common::PlatformInfo::CONFIG_TDP_LEVELS) != 0 {
        let nominal = cpu.reg(MSR_CONFIG_TDP_NOMINAL).get() & 0xff;
        cpu.reg(MSR_TURBO_ACTIVATION_RATIO).set(nominal);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::x86::cpu::msr::testing::MsrSpace;

    #[test]
    fn cpu_register_order_and_late_epb_probe() {
        let cpu = MsrSpace::new([(0x179, 2), (0xe2, 1 << 31), (0x2e, 0x401)]);
        HaswellCpu {
            microcode: None,
            tcc_offset: 0,
        }
        .configure(&cpu, |leaf| match leaf {
            1 => (0x40651, 0, (1 << 18) | 0x60, 0),
            6 => {
                assert_ne!(cpu.borrow().get(0x1fc) & (1 << 18), 0);
                (0, 0, 8, 0)
            }
            _ => unreachable!(),
        });
        let model = cpu.borrow();
        extern crate std;
        let indices: std::vec::Vec<_> = model.writes.iter().map(|w| w.0).collect();
        assert_eq!(
            &indices[..7],
            &[0x400, 0x404, 0x401, 0x405, 0x2e, 0x3a, 0xe2]
        );
        assert!(!model.written(0x17a));
        assert_eq!(model.get(0x2e), 1);
        assert_ne!(model.get(0xe2) & (1 << 31), 0);
        assert_eq!(model.get(0x1b0), 6);
        assert!(indices.ends_with(&[0x1b2, 0x1f8, 0x1b0]));
    }

    #[test]
    fn package_fetch_control_is_set_once_and_prelocked_off_is_fatal() {
        let cpu = MsrSpace::new([(0x17d, 1 << 58)]);
        assert!(fetch_lock_effective(&cpu));
        assert!(fetch_lock_effective(&cpu));
        assert_eq!(cpu.borrow().writes.as_slice(), &[(0x4e0, 5)]);
        let locked = MsrSpace::new([(0x17d, 1 << 58), (0x4e0, 1)]);
        assert!(!fetch_lock_effective(&locked));
        assert!(locked.borrow().writes.is_empty());
    }

    #[test]
    fn package_limit_precedes_mmio_mirror_and_ddr_and_tar_follow() {
        let cpu = MsrSpace::new([(0xce, (1 << 29) | (1 << 33)), (0x614, 20), (0x648, 14)]);
        program_limits(&cpu, |limit| {
            assert_eq!(cpu.borrow().writes.as_slice(), &[(0x610, limit)]);
            42
        });
        assert_eq!(&cpu.borrow().writes[1..], &[(0x618, 42), (0x64c, 14)]);
    }

    #[test]
    fn sku_tdp_is_clamped_before_short_limit() {
        let v = encode_power_limits(24 | (32 << 16) | (40 << 32));
        assert_eq!(v as u32, 32 | (1 << 15) | (0x6e << 17));
        assert_eq!(v >> 32, 40 | (1 << 15));
    }

    #[test]
    fn set_max_freq_prefers_nominal_ctdp_when_present() {
        let with_ctdp = MsrSpace::new([(0xce, (0x14 << 8) | (1 << 33)), (0x648, 0x11)]);
        program_max_non_turbo_freq(&with_ctdp);
        assert_eq!(with_ctdp.borrow().writes.as_slice(), &[(0x199, 0x11 << 8)]);

        let without_ctdp = MsrSpace::new([(0xce, 0x14 << 8)]);
        program_max_non_turbo_freq(&without_ctdp);
        assert_eq!(
            without_ctdp.borrow().writes.as_slice(),
            &[(0x199, 0x14 << 8)]
        );
    }
}
