//! Intel MSRs shared by Westmere and Haswell ratio-based power management.

use crate::x86::cpu::msr::{Msr, MsrAccess};
use tock_registers::{interfaces::Readable, register_bitfields};

register_bitfields![u64,
    /// MSR_POWER_CTL (0x1fc).
    pub PowerCtl [
        BIDIRECTIONAL_PROCHOT OFFSET(0) NUMBITS(1) [],
        C1E OFFSET(1) NUMBITS(1) [],
        EPB_ENABLE OFFSET(18) NUMBITS(1) []
    ],
    /// MSR_MISC_PWR_MGMT (0x1aa).
    pub MiscPwrMgmt [
        EIST_HW_COORDINATION_DISABLE OFFSET(0) NUMBITS(1) [],
        THERMAL_INTERRUPT_LOCK OFFSET(22) NUMBITS(1) []
    ],
    /// IA32_MISC_ENABLE (0x1a0).
    pub MiscEnable [
        FAST_STRINGS OFFSET(0) NUMBITS(1) [],
        THERMAL_MONITOR OFFSET(3) NUMBITS(1) [],
        EIST OFFSET(16) NUMBITS(1) [],
        TURBO_DISABLE OFFSET(38) NUMBITS(1) []
    ],
    /// MSR_PLATFORM_INFO (0xce).
    pub PlatformInfo [
        MAX_NON_TURBO_RATIO OFFSET(8) NUMBITS(8) [],
        PROGRAMMABLE_TDP_LIMIT OFFSET(29) NUMBITS(1) [],
        PROGRAMMABLE_TJ_OFFSET OFFSET(30) NUMBITS(1) [],
        CONFIG_TDP_LEVELS OFFSET(33) NUMBITS(2) [],
        TIMED_MWAIT_ENABLE OFFSET(37) NUMBITS(1) [],
        MAX_EFFICIENCY_RATIO OFFSET(40) NUMBITS(8) []
    ],
    /// MSR_TEMPERATURE_TARGET (0x1a2).
    pub TemperatureTarget [
        TJ_MAX OFFSET(16) NUMBITS(8) [],
        TCC_OFFSET OFFSET(24) NUMBITS(4) []
    ],
    /// MSR_TURBO_RATIO_LIMIT (0x1ad).
    pub TurboRatioLimit [
        ONE_CORE OFFSET(0) NUMBITS(8) []
    ],
    /// MSR_CORE_THREAD_COUNT (0x35).
    pub CoreThreadCount [
        THREADS OFFSET(0) NUMBITS(16) [],
        CORES OFFSET(16) NUMBITS(16) []
    ],
    /// IA32_PERF_CTL (0x199). Westmere writes the ratio to the low byte;
    /// Sandy Bridge and newer write the ratio to bits [15:8].
    pub PerfCtl [
        RATIO OFFSET(0) NUMBITS(8) [],
        RATIO_SNB OFFSET(8) NUMBITS(8) []
    ]
];

pub const CORE_THREAD_COUNT: Msr<CoreThreadCount::Register> = Msr::new(0x35);
pub const PLATFORM_INFO: Msr<PlatformInfo::Register> = Msr::new(0xce);
pub const IA32_PERF_CTL: Msr<PerfCtl::Register> = Msr::new(0x199);
pub const IA32_THERM_INTERRUPT: Msr = Msr::new(0x19b);
pub const IA32_MISC_ENABLE: Msr<MiscEnable::Register> = Msr::new(0x1a0);
pub const TEMPERATURE_TARGET: Msr<TemperatureTarget::Register> = Msr::new(0x1a2);
pub const MISC_PWR_MGMT: Msr<MiscPwrMgmt::Register> = Msr::new(0x1aa);
pub const TURBO_RATIO_LIMIT: Msr<TurboRatioLimit::Register> = Msr::new(0x1ad);
pub const POWER_CTL: Msr<PowerCtl::Register> = Msr::new(0x1fc);

/// Turbo availability (coreboot `cpu/intel/turbo`): a package-wide fact.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TurboState {
    Unavailable,
    /// Supported, but `IA32_MISC_ENABLE` hides it.
    Disabled,
    Enabled,
}

/// coreboot `update_turbo_state()`: CPUID leaf 6 stops advertising hidden turbo.
pub fn turbo_state(cpu: &impl MsrAccess) -> TurboState {
    let capable = crate::x86::cpuid(6).0 & (1 << 1) != 0;
    let hidden = cpu.reg(IA32_MISC_ENABLE).is_set(MiscEnable::TURBO_DISABLE);
    match (capable, hidden) {
        (true, false) => TurboState::Enabled,
        (false, true) => TurboState::Disabled,
        _ => TurboState::Unavailable,
    }
}
