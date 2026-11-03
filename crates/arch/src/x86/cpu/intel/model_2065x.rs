//! Intel model 2065x (Arrandale/Clarkdale, 32 nm Westmere with on-package
//! Ironlake graphics and memory controller), following coreboot's
//! `cpu/intel/model_2065x`.
//!
//! The memory controller lives in the package, so the "northbridge" of these
//! systems is the uncore. CPU policy is therefore owned by the Ironlake
//! platform binding, not by a separately socketed CPU-package selection.

use super::feature_control;
use super::smm::{SmmCpu, SmrrPair, X86SaveStateFormat};
use crate::x86::cpu::msr::{MCG_CAP, MCG_STATUS, McgCap, Msr, MsrAccess, NativeMsr};
use crate::x86::mp::{CpuDriver, CpuIdMatch, CpuVendor};
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};
use tock_registers::{LocalRegisterCopy, register_bitfields};

/// Arrandale's bus clock is fixed at 133 MHz (coreboot `IRONLAKE_BCLK`).
pub const BCLK_MHZ: u32 = 133;

register_bitfields![u64,
    /// MSR_PKG_CST_CONFIG_CONTROL.
    pub PkgCstConfig [
        LIMIT OFFSET(0) NUMBITS(3) [],
        IO_MWAIT_REDIRECT OFFSET(10) NUMBITS(1) [],
        LOCK OFFSET(15) NUMBITS(1) [],
        C3_AUTO_DEMOTION OFFSET(25) NUMBITS(1) []
    ],
    /// MSR_POWER_CTL.
    pub PowerCtl [ C1E OFFSET(1) NUMBITS(1) [] ],
    /// MSR_MISC_PWR_MGMT.
    pub MiscPwrMgmt [
        EIST_HW_COORDINATION_DISABLE OFFSET(0) NUMBITS(1) [],
        THERMAL_INTERRUPT_LOCK OFFSET(22) NUMBITS(1) []
    ],
    /// IA32_MISC_ENABLE.
    pub MiscEnable [
        FAST_STRINGS OFFSET(0) NUMBITS(1) [],
        THERMAL_MONITOR OFFSET(3) NUMBITS(1) [],
        EIST OFFSET(16) NUMBITS(1) [],
        TURBO_DISABLE OFFSET(38) NUMBITS(1) []
    ],
    /// MSR_PLATFORM_INFO.
    pub PlatformInfo [
        MAX_NON_TURBO_RATIO OFFSET(8) NUMBITS(8) [],
        PROGRAMMABLE_TJ_OFFSET OFFSET(30) NUMBITS(1) [],
        MAX_EFFICIENCY_RATIO OFFSET(40) NUMBITS(8) []
    ],
    /// MSR_TEMPERATURE_TARGET.
    pub TemperatureTarget [
        TJ_MAX OFFSET(16) NUMBITS(8) [],
        TCC_OFFSET OFFSET(24) NUMBITS(4) []
    ],
    /// MSR_TURBO_RATIO_LIMIT.
    pub TurboRatioLimit [ ONE_CORE OFFSET(0) NUMBITS(8) [] ],
    /// MSR_CORE_THREAD_COUNT.
    pub CoreThreadCount [
        THREADS OFFSET(0) NUMBITS(16) [],
        CORES OFFSET(16) NUMBITS(16) []
    ],
    /// MSR_FEATURE_CONFIG.
    pub FeatureConfig [ AESNI_LOCK OFFSET(0) NUMBITS(1) [] ],
    /// MSR_TURBO_POWER_CURRENT_LIMIT.
    pub TurboPowerCurrentLimit [ TDP_LIMIT OFFSET(0) NUMBITS(15) [] ]
];

pub const PKG_CST_CONFIG_CONTROL: Msr<PkgCstConfig::Register> = Msr::new(0xe2);
pub const PLATFORM_INFO: Msr<PlatformInfo::Register> = Msr::new(0xce);
pub const CORE_THREAD_COUNT: Msr<CoreThreadCount::Register> = Msr::new(0x35);
pub const FEATURE_CONFIG: Msr<FeatureConfig::Register> = Msr::new(0x13c);
pub const IA32_THERM_INTERRUPT: Msr = Msr::new(0x19b);
pub const IA32_MISC_ENABLE: Msr<MiscEnable::Register> = Msr::new(0x1a0);
pub const TEMPERATURE_TARGET: Msr<TemperatureTarget::Register> = Msr::new(0x1a2);
pub const MISC_PWR_MGMT: Msr<MiscPwrMgmt::Register> = Msr::new(0x1aa);
pub const TURBO_POWER_CURRENT_LIMIT: Msr<TurboPowerCurrentLimit::Register> = Msr::new(0x1ac);
pub const TURBO_RATIO_LIMIT: Msr<TurboRatioLimit::Register> = Msr::new(0x1ad);
pub const POWER_CTL: Msr<PowerCtl::Register> = Msr::new(0x1fc);
register_bitfields![u64,
    /// IA32_PERF_CTL.
    pub PerfCtl [ RATIO OFFSET(0) NUMBITS(8) [] ]
];
/// IA32_PERF_CTL. coreboot ironlake writes the ratio to the low byte.
pub const IA32_PERF_CTL: Msr<PerfCtl::Register> = Msr::new(0x199);

/// coreboot `HIGHEST_CLEVEL`: C6 package limit.
const HIGHEST_CLEVEL: u64 = 3;

/// Turbo availability (coreboot `cpu/intel/turbo`): a package-wide fact.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TurboState {
    Unavailable,
    /// Supported, but `IA32_MISC_ENABLE` hides it.
    Disabled,
    Enabled,
}

/// coreboot `update_turbo_state()`.
pub fn turbo_state(cpu: &impl MsrAccess) -> TurboState {
    let capable = crate::x86::cpuid(6).0 & (1 << 1) != 0;
    let hidden = cpu.reg(IA32_MISC_ENABLE).is_set(MiscEnable::TURBO_DISABLE);
    match (capable, hidden) {
        (true, false) => TurboState::Enabled,
        // CPUID leaf 6 stops advertising turbo while MISC_ENABLE hides it.
        (false, true) => TurboState::Disabled,
        _ => TurboState::Unavailable,
    }
}

/// coreboot ironlake `early_cpu_init()`, MSR part: request the ratio above
/// the maximum non-turbo one and enable EIST before raminit. When CPUID
/// leaf 6 does not advertise turbo, the turbo-disable bit is cleared first.
///
/// # Safety
///
/// The current CPU must be model 2065x.
pub unsafe fn early_init() {
    // SAFETY: the caller's contract.
    let cpu = unsafe { NativeMsr::new() };
    let request = || {
        let ratio = cpu
            .reg(PLATFORM_INFO)
            .read(PlatformInfo::MAX_NON_TURBO_RATIO)
            + 1;
        cpu.reg(IA32_PERF_CTL)
            .modify(PerfCtl::RATIO.val(ratio & 0xff));
    };
    if crate::x86::cpuid_count(6, 0x8b).0 & (1 << 1) == 0 {
        request();
        cpu.reg(IA32_MISC_ENABLE)
            .modify(MiscEnable::TURBO_DISABLE::CLEAR + MiscEnable::EIST::SET);
    }
    request();
    cpu.reg(IA32_MISC_ENABLE).modify(MiscEnable::EIST::SET);
}

/// Board/chipset CPU policy (coreboot `cpu_intel_model_2065x_config`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Model2065xPolicy {
    /// TCC activation offset in degrees below TjMax; 0 keeps the default.
    pub tcc_offset: u8,
}

pub struct Model2065xCpuDriver {
    microcode: Option<&'static [u8]>,
    policy: Model2065xPolicy,
}

impl Model2065xCpuDriver {
    pub const fn new(microcode: Option<&'static [u8]>, policy: Model2065xPolicy) -> Self {
        Self { microcode, policy }
    }
}

/// Arrandale / Clarkdale CPUIDs (coreboot `cpu_table`).
static IDS: &[CpuIdMatch] = &[
    CpuIdMatch {
        vendor: CpuVendor::Intel,
        signature: 0x20650,
        mask: CpuIdMatch::EXACT_MASK,
    },
    CpuIdMatch {
        vendor: CpuVendor::Intel,
        signature: 0x20651,
        mask: CpuIdMatch::EXACT_MASK,
    },
    CpuIdMatch {
        vendor: CpuVendor::Intel,
        signature: 0x20652,
        mask: CpuIdMatch::EXACT_MASK,
    },
    CpuIdMatch {
        vendor: CpuVendor::Intel,
        signature: 0x20654,
        mask: CpuIdMatch::EXACT_MASK,
    },
    CpuIdMatch {
        vendor: CpuVendor::Intel,
        signature: 0x20655,
        mask: CpuIdMatch::EXACT_MASK,
    },
];

/// coreboot `configure_c_states()`: MWAIT-only requests, C6 package limit
/// with C3 auto-demotion.
fn c_state_config(current: u64) -> u64 {
    let mut value = LocalRegisterCopy::<u64, PkgCstConfig::Register>::new(current);
    value.modify(
        PkgCstConfig::IO_MWAIT_REDIRECT::CLEAR
            + PkgCstConfig::LIMIT.val(HIGHEST_CLEVEL)
            + PkgCstConfig::C3_AUTO_DEMOTION::SET,
    );
    value.get()
}

/// coreboot `configure_thermal_target()`.
fn thermal_target(platform_info: u64, current: u64, tcc_offset: u8) -> Option<u64> {
    let info = LocalRegisterCopy::<u64, PlatformInfo::Register>::new(platform_info);
    if !info.is_set(PlatformInfo::PROGRAMMABLE_TJ_OFFSET) || tcc_offset == 0 {
        return None;
    }
    let mut value = LocalRegisterCopy::<u64, TemperatureTarget::Register>::new(current);
    value.modify(TemperatureTarget::TCC_OFFSET.val(u64::from(tcc_offset & 0xf)));
    Some(value.get())
}

/// coreboot `intel_ht_sibling()`: a non-zero SMT-thread bit in the APIC ID.
fn is_ht_sibling() -> bool {
    let (_, ebx, _, edx) = crate::x86::cpuid(1);
    if edx & (1 << 28) == 0 {
        return false;
    }
    let apic_id = ebx >> 24;
    let (_, threads, _, _) = crate::x86::cpuid_count(0xb, 0);
    let threads_per_core = (threads & 0xffff).max(1);
    apic_id & (threads_per_core.next_power_of_two() - 1) != 0
}

impl Model2065xCpuDriver {
    /// coreboot `mca_clear_status()`.
    fn clear_machine_check_banks(cpu: &NativeMsr) {
        let banks = cpu.reg(MCG_CAP).read(McgCap::COUNT) as u32;
        cpu.reg(MCG_STATUS).set(0);
        for bank in 0..banks {
            cpu.reg(Msr::<()>::new(0x401 + 4 * bank)).set(0);
        }
    }

    /// coreboot `set_aesni_lock()`: once per core, if AES-NI exists.
    fn lock_aesni(cpu: &NativeMsr) {
        let (_, _, ecx, _) = crate::x86::cpuid(1);
        if ecx & (1 << 25) == 0 || is_ht_sibling() {
            return;
        }
        let config = cpu.reg(FEATURE_CONFIG);
        if !config.is_set(FeatureConfig::AESNI_LOCK) {
            config.modify(FeatureConfig::AESNI_LOCK::SET);
        }
    }

    /// coreboot `enable_turbo()`: only when available but hidden.
    fn enable_turbo(cpu: &NativeMsr) {
        if turbo_state(cpu) == TurboState::Disabled {
            cpu.reg(IA32_MISC_ENABLE)
                .modify(MiscEnable::TURBO_DISABLE::CLEAR);
        }
    }
}

impl SmmCpu for Model2065xCpuDriver {
    fn smm_save_state_format(&self) -> X86SaveStateFormat {
        // Revision 0x30101 (EM64T101).
        X86SaveStateFormat::IntelEm64t
    }
    fn smrr_pair(&self) -> Option<SmrrPair> {
        Some(SmrrPair::Architectural)
    }
}

impl CpuDriver for Model2065xCpuDriver {
    fn name(&self) -> &'static str {
        "Intel Arrandale/Clarkdale"
    }

    fn id_table(&self) -> &'static [CpuIdMatch] {
        IDS
    }

    fn update_microcode(&self) {
        if let Some(blob) = self.microcode {
            // SAFETY: the board supplies a firmware-backed microcode blob.
            unsafe { super::microcode::update_current_cpu_logged(blob) };
        }
    }

    fn logical_cpu_count(&self) -> Option<u16> {
        // SAFETY: model 2065x implements MSR_CORE_THREAD_COUNT.
        let cpu = unsafe { NativeMsr::new() };
        let count = cpu.reg(CORE_THREAD_COUNT);
        let threads = count.read(CoreThreadCount::THREADS) as u16;
        fstart_log::info!(
            "cpu: {} cores, {} threads enabled",
            count.read(CoreThreadCount::CORES) as u32,
            u32::from(threads)
        );
        (threads != 0).then_some(threads)
    }

    fn init_cpu(&self) {
        let bsp = crate::x86::mp::current_cpu_index() == 0;
        // SAFETY: MP matching admits only model 2065x, which implements
        // every MSR used below.
        let cpu = unsafe { NativeMsr::new() };
        // coreboot model_2065x_init(), in order. Clearing pending MCEs is
        // only valid on a cold boot; this flow never runs on a warm one.
        Self::clear_machine_check_banks(&cpu);
        // VMX enable and IA32_FEATURE_CONTROL lock. The architectural SMRR
        // pair needs no enable bit.
        // SAFETY: Intel CPU; no model-specific extra bits.
        unsafe { feature_control::enable_and_lock(0) };
        Self::lock_aesni(&cpu);

        // configure_c_states()
        let cst = cpu.reg(PKG_CST_CONFIG_CONTROL);
        cst.set(c_state_config(cst.get()));
        if bsp {
            cpu.reg(POWER_CTL).modify(PowerCtl::C1E::SET);
            // P-state HW_ALL coordination.
            cpu.reg(MISC_PWR_MGMT)
                .modify(MiscPwrMgmt::EIST_HW_COORDINATION_DISABLE::CLEAR);
        }

        // configure_misc()
        cpu.reg(IA32_MISC_ENABLE).modify(
            MiscEnable::FAST_STRINGS::SET
                + MiscEnable::THERMAL_MONITOR::SET
                + MiscEnable::EIST::SET,
        );
        cpu.reg(IA32_THERM_INTERRUPT).set(0);

        // configure_thermal_target()
        let target = cpu.reg(TEMPERATURE_TARGET);
        if let Some(value) = thermal_target(
            cpu.reg(PLATFORM_INFO).get(),
            target.get(),
            self.policy.tcc_offset,
        ) {
            target.set(value);
        }

        Self::enable_turbo(&cpu);

        // coreboot intel_model_2065x_finalize_smm() locks these from the SMM
        // finalize call. Nothing writes them after this point, so lock them
        // with the rest of the per-CPU configuration.
        cst.modify(PkgCstConfig::LOCK::SET);
        cpu.reg(MISC_PWR_MGMT)
            .modify(MiscPwrMgmt::THERMAL_INTERRUPT_LOCK::SET);
    }

    fn post_mp_init(&self) {
        // coreboot set_max_ratio(): reports the ratio BIOS_RESET_CPL selects.
        // SAFETY: model 2065x implements these MSRs.
        let cpu = unsafe { NativeMsr::new() };
        let ratio = if turbo_state(&cpu) == TurboState::Enabled {
            cpu.reg(TURBO_RATIO_LIMIT).read(TurboRatioLimit::ONE_CORE)
        } else {
            cpu.reg(PLATFORM_INFO)
                .read(PlatformInfo::MAX_NON_TURBO_RATIO)
        };
        fstart_log::info!(
            "model_2065x: frequency set to {} MHz",
            ratio as u32 * BCLK_MHZ
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::x86::mp::CpuIdentity;

    #[test]
    fn matches_arrandale_and_clarkdale_only() {
        let driver = Model2065xCpuDriver::new(None, Model2065xPolicy::default());
        for signature in [0x20650, 0x20651, 0x20652, 0x20654, 0x20655] {
            assert!(driver.matches(CpuIdentity {
                vendor: CpuVendor::Intel,
                signature
            }));
        }
        assert!(!driver.matches(CpuIdentity {
            vendor: CpuVendor::Intel,
            signature: 0x206a7
        }));
        assert_eq!(driver.smrr_pair(), Some(SmrrPair::Architectural));
    }

    #[test]
    fn c_state_and_thermal_values_match_coreboot() {
        // Clears IO_MWAIT, limit C6 (3), C3 auto demotion.
        assert_eq!(c_state_config(0x0000_0407), 0x0200_0003);
        // TCC offset requires PLATFORM_INFO[30] and a non-zero offset.
        assert_eq!(thermal_target(0, 0x0069_0000, 5), None);
        assert_eq!(thermal_target(1 << 30, 0x0069_0000, 0), None);
        assert_eq!(thermal_target(1 << 30, 0x0f69_0000, 5), Some(0x0569_0000));
    }
}
