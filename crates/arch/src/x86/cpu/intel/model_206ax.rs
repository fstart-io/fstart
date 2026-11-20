//! Intel model 206AX/306AX (Sandy Bridge and Ivy Bridge), following
//! coreboot `cpu/intel/model_206ax`: MSR power management, VR current
//! limits, thermal and turbo configuration, the package power limits that
//! follow `BIOS_RESET_CPL`, the configurable-TDP flex ratio, and the SMM
//! facts (EM64T101 save state, architectural SMRR, 4 MiB IED).

use super::feature_control;
use super::smm::{SmmCpu, SmrrPair, X86SaveStateFormat};
use crate::x86::cpu::msr::{MCG_CAP, MCG_STATUS, McgCap, Msr, MsrAccess, NativeMsr};
use crate::x86::mp::{CpuDriver, CpuIdMatch, CpuIdentity, CpuVendor};
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};
use tock_registers::{LocalRegisterCopy, register_bitfields};

/// Sandy Bridge and Ivy Bridge use a fixed 100 MHz bus clock.
pub const BCLK_MHZ: u32 = 100;
/// IED region the CPU reserves at the top of TSEG (coreboot `IED_REGION_SIZE`).
pub const IED_SIZE: u32 = 4 << 20;
/// Minimum TSEG (coreboot `SMM_TSEG_SIZE`).
pub const TSEG_SIZE: u32 = 8 << 20;
/// First Ivy Bridge stepping with configurable TDP.
const IVB_CONFIG_TDP_MIN_CPUID: u32 = 0x306a2;

register_bitfields![u64,
    pub PLATFORM_INFO [
        MAX_NON_TURBO_RATIO OFFSET(8) NUMBITS(8) [],
        SET_TURBO_LIMIT OFFSET(28) NUMBITS(1) [],
        SET_TDP OFFSET(29) NUMBITS(1) [],
        TCC_OFFSET_PROGRAMMABLE OFFSET(30) NUMBITS(1) [],
        CONFIG_TDP_LEVELS OFFSET(33) NUMBITS(2) [],
        MAX_EFFICIENCY_RATIO OFFSET(40) NUMBITS(8) []
    ],
    pub FLEX_RATIO [
        RATIO OFFSET(8) NUMBITS(8) [],
        ENABLE OFFSET(16) NUMBITS(1) [],
        LOCK OFFSET(20) NUMBITS(1) []
    ],
    pub PKG_CST_CONFIG [
        PACKAGE_LIMIT OFFSET(0) NUMBITS(3) [],
        IO_MWAIT_REDIRECT OFFSET(10) NUMBITS(1) [],
        LOCK OFFSET(15) NUMBITS(1) [],
        C3_AUTO_DEMOTION OFFSET(25) NUMBITS(1) [],
        C1_AUTO_DEMOTION OFFSET(26) NUMBITS(1) [],
        C3_UNDEMOTION OFFSET(27) NUMBITS(1) [],
        C1_UNDEMOTION OFFSET(28) NUMBITS(1) []
    ],
    pub MISC_PWR_MGMT [
        EIST_HW_DISABLE OFFSET(0) NUMBITS(1) [],
        LOCK_THERM_INT OFFSET(22) NUMBITS(1) []
    ],
    pub POWER_CTL [
        BIDIR_PROCHOT OFFSET(0) NUMBITS(1) [],
        C1E OFFSET(1) NUMBITS(1) [],
        ENERGY_PERF_BIAS_ACCESS OFFSET(18) NUMBITS(1) []
    ],
    pub IRTL [
        RESPONSE OFFSET(0) NUMBITS(10) [],
        UNIT OFFSET(10) NUMBITS(3) [
            Ns1024 = 2
        ],
        VALID OFFSET(15) NUMBITS(1) []
    ],
    pub CURRENT_CONFIG [
        LIMIT OFFSET(0) NUMBITS(13) [],
        LOCK OFFSET(31) NUMBITS(1) [],
        PSI OFFSET(32) NUMBITS(30) []
    ],
    pub TEMPERATURE_TARGET [
        TJMAX OFFSET(16) NUMBITS(8) [],
        TCC_OFFSET OFFSET(24) NUMBITS(4) []
    ],
    pub MISC_ENABLE [
        FAST_STRINGS OFFSET(0) NUMBITS(1) [],
        TM OFFSET(3) NUMBITS(1) [],
        EIST OFFSET(16) NUMBITS(1) [],
        TURBO_DISABLE OFFSET(38) NUMBITS(1) []
    ],
    pub POWER_LIMIT [
        PL1 OFFSET(0) NUMBITS(15) [],
        PL1_ENABLE OFFSET(15) NUMBITS(1) [],
        PL1_CLAMP OFFSET(16) NUMBITS(1) [],
        PL1_TIME OFFSET(17) NUMBITS(7) [],
        PL2 OFFSET(32) NUMBITS(15) [],
        PL2_ENABLE OFFSET(47) NUMBITS(1) [],
        PL2_CLAMP OFFSET(48) NUMBITS(1) []
    ],
    pub POWER_SKU [
        TDP OFFSET(0) NUMBITS(15) [],
        MIN_POWER OFFSET(16) NUMBITS(15) [],
        MAX_POWER OFFSET(32) NUMBITS(15) [],
        MAX_TIME OFFSET(48) NUMBITS(7) []
    ],
    pub LT_LOCK [
        LOCK OFFSET(0) NUMBITS(1) []
    ]
];

const MSR_CORE_THREAD_COUNT: Msr = Msr::new(0x35);
const IA32_MC0_STATUS: u32 = 0x401;
pub const MSR_PLATFORM_INFO: Msr<PLATFORM_INFO::Register> = Msr::new(0xce);
const MSR_PKG_CST_CONFIG_CONTROL: Msr<PKG_CST_CONFIG::Register> = Msr::new(0xe2);
const IA32_PERF_CTL: Msr = Msr::new(0x199);
const IA32_THERM_INTERRUPT: Msr = Msr::new(0x19b);
pub const MSR_FLEX_RATIO: Msr<FLEX_RATIO::Register> = Msr::new(0x194);
const IA32_MISC_ENABLE: Msr<MISC_ENABLE::Register> = Msr::new(0x1a0);
pub const MSR_TEMPERATURE_TARGET: Msr<TEMPERATURE_TARGET::Register> = Msr::new(0x1a2);
pub const MSR_MISC_PWR_MGMT: Msr<MISC_PWR_MGMT::Register> = Msr::new(0x1aa);
pub const MSR_TURBO_RATIO_LIMIT: Msr = Msr::new(0x1ad);
const IA32_ENERGY_PERF_BIAS: Msr = Msr::new(0x1b0);
const IA32_PACKAGE_THERM_INTERRUPT: Msr = Msr::new(0x1b2);
const MSR_POWER_CTL: Msr<POWER_CTL::Register> = Msr::new(0x1fc);
const MSR_LT_LOCK_MEMORY: Msr<LT_LOCK::Register> = Msr::new(0x2e7);
const MSR_PP0_CURRENT_CONFIG: Msr<CURRENT_CONFIG::Register> = Msr::new(0x601);
const MSR_PP1_CURRENT_CONFIG: Msr<CURRENT_CONFIG::Register> = Msr::new(0x602);
pub const MSR_PKG_POWER_SKU_UNIT: Msr = Msr::new(0x606);
const MSR_PKGC3_IRTL: Msr<IRTL::Register> = Msr::new(0x60a);
const MSR_PKGC6_IRTL: Msr<IRTL::Register> = Msr::new(0x60b);
const MSR_PKGC7_IRTL: Msr<IRTL::Register> = Msr::new(0x60c);
pub const MSR_PKG_POWER_LIMIT: Msr<POWER_LIMIT::Register> = Msr::new(0x610);
pub const MSR_PKG_POWER_SKU: Msr<POWER_SKU::Register> = Msr::new(0x614);
pub const MSR_CONFIG_TDP_NOMINAL: Msr = Msr::new(0x648);
const MSR_TURBO_ACTIVATION_RATIO: Msr = Msr::new(0x64c);
const IA32_PLATFORM_DCA_CAP: Msr = Msr::new(0x1f8);
const MSR_FEATURE_CONFIG: Msr = Msr::new(0x13c);

/// Default primary-plane (Icc) limit: 112 A.
const PP0_CURRENT_LIMIT: u64 = 112 << 3;
/// Default secondary-plane (IAXG) limit, Sandy Bridge: 35 A.
const PP1_CURRENT_LIMIT_SNB: u64 = 35 << 3;
/// Default secondary-plane (IAXG) limit, Ivy Bridge: 50 A.
const PP1_CURRENT_LIMIT_IVB: u64 = 50 << 3;

/// VR12 phase count at a light-load power state (coreboot `vr12_phases`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Vr12Phases {
    KeepDefault = 0,
    All = 1,
    Two = 2,
    One = 3,
    LightLoad = 4,
}

/// One VR12 PSI state: phases and current threshold in amps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PsiState {
    pub phases: Vr12Phases,
    pub current: u16,
}

impl PsiState {
    pub const DEFAULT: Self = Self {
        phases: Vr12Phases::KeepDefault,
        current: 0,
    };
}

/// ACPI C-state levels advertised as C1/C2/C3 (coreboot `cpu_acpi_level`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum CpuCstate {
    Disabled = 0,
    C1 = 1,
    C1E = 2,
    C3 = 3,
    C6 = 4,
    C7 = 5,
    C7S = 6,
}

/// Board CPU policy (coreboot `struct cpu_intel_model_206ax_config`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Model206axConfig {
    /// CPU states advertised to the OS as ACPI C1, C2 and C3.
    pub acpi_cstates: [CpuCstate; 3],
    /// TCC activation offset in degrees C; zero keeps the fused value.
    pub tcc_offset: u8,
    /// Long-term (PL1) and short-term (PL2) package limits in mW; zero
    /// derives them from the SKU's TDP.
    pub pl1_mw: u32,
    pub pl1_clamp: bool,
    pub pl2_mw: u32,
    pub pl2_clamp: bool,
    /// Primary-plane (Icc) VR current limit in amps; zero uses Intel's default.
    pub pp0_current_limit: u16,
    /// Secondary-plane (IAXG) VR current limit in amps.
    pub pp1_current_limit: u16,
    pub pp0_psi: [PsiState; 3],
    pub pp1_psi: [PsiState; 3],
    /// Turbo ratio limit for 1..4 active cores; zero keeps the fused value.
    pub turbo_limits: [u8; 4],
    /// Hyper-threading enabled (coreboot CMOS option `hyper_threading`).
    pub hyper_threading: bool,
}

impl Model206axConfig {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            // sandybridge/chipset.cb defaults.
            acpi_cstates: [CpuCstate::C1, CpuCstate::C3, CpuCstate::C7],
            tcc_offset: 0,
            pl1_mw: 0,
            pl1_clamp: false,
            pl2_mw: 0,
            pl2_clamp: false,
            pp0_current_limit: 0,
            pp1_current_limit: 0,
            pp0_psi: [PsiState::DEFAULT; 3],
            pp1_psi: [PsiState::DEFAULT; 3],
            turbo_limits: [0; 4],
            hyper_threading: true,
        }
    }
}

impl Default for Model206axConfig {
    fn default() -> Self {
        Self::new()
    }
}

static IDS: &[CpuIdMatch] = &[
    CpuIdMatch {
        vendor: CpuVendor::Intel,
        signature: 0x206a0,
        mask: CpuIdMatch::ALL_STEPPINGS_MASK,
    },
    CpuIdMatch {
        vendor: CpuVendor::Intel,
        signature: 0x306a0,
        mask: CpuIdMatch::ALL_STEPPINGS_MASK,
    },
];

/// CPUID.1 signature.
#[must_use]
pub fn cpuid_signature() -> u32 {
    crate::x86::cpuid(1).0
}

#[must_use]
pub const fn is_ivy(signature: u32) -> bool {
    signature & 0xffff0 == 0x306a0
}

#[must_use]
pub const fn is_sandy(signature: u32) -> bool {
    signature & 0xffff0 == 0x206a0
}

fn cpu() -> NativeMsr {
    // SAFETY: every caller runs on a model 206AX/306AX CPU (MP matching or
    // platform pairing), which implements the MSRs used in this module.
    unsafe { NativeMsr::new() }
}

/// coreboot `cpu_config_tdp_levels()`.
#[must_use]
pub fn config_tdp_levels() -> u32 {
    if cpuid_signature() < IVB_CONFIG_TDP_MIN_CPUID {
        return 0;
    }
    cpu()
        .reg(MSR_PLATFORM_INFO)
        .read(PLATFORM_INFO::CONFIG_TDP_LEVELS) as u32
}

/// coreboot bootblock `set_flex_ratio_to_tdp_nominal()`: on configurable-TDP
/// parts select the nominal ratio. Returns the ratio, zero when unchanged.
/// The chipset must then latch it through its soft-reset data register.
#[must_use]
pub fn set_flex_ratio_to_tdp_nominal() -> u8 {
    if cpuid_signature() < IVB_CONFIG_TDP_MIN_CPUID {
        return 0;
    }
    let msr = cpu();
    let flex = msr.reg(MSR_FLEX_RATIO);
    if !flex.is_set(FLEX_RATIO::ENABLE) {
        return 0;
    }
    if msr
        .reg(MSR_PLATFORM_INFO)
        .read(PLATFORM_INFO::CONFIG_TDP_LEVELS)
        == 0
    {
        return 0;
    }
    let nominal = (msr.reg(MSR_CONFIG_TDP_NOMINAL).get() & 0xff) as u8;
    if flex.read(FLEX_RATIO::RATIO) != u64::from(nominal) {
        flex.modify(FLEX_RATIO::RATIO.val(u64::from(nominal)) + FLEX_RATIO::LOCK::SET);
    }
    nominal
}

/// Whether the CPU supports hyper-threading (coreboot `intel_ht_supported`).
#[must_use]
pub fn ht_supported() -> bool {
    let (_, _, _, edx) = crate::x86::cpuid(1);
    if edx & (1 << 28) == 0 {
        return false;
    }
    let (max, _, _, _) = crate::x86::cpuid(0);
    if max < 0xb {
        return false;
    }
    let (_, ebx, _, _) = crate::x86::cpuid_count(0xb, 0);
    ebx & 0xffff > 1
}

/// TjMax in degrees C (`MSR_TEMPERATURE_TARGET[23:16]`).
#[must_use]
pub fn tjmax() -> u8 {
    cpu()
        .reg(MSR_TEMPERATURE_TARGET)
        .read(TEMPERATURE_TARGET::TJMAX) as u8
}

const POWER_LIMIT_TIME_SEC_TO_MSR: [(u8, u8); 25] = [
    (0, 0x00),
    (1, 0x0a),
    (2, 0x0b),
    (3, 0x4b),
    (4, 0x0c),
    (5, 0x2c),
    (6, 0x4c),
    (7, 0x6c),
    (8, 0x0d),
    (10, 0x2d),
    (12, 0x4d),
    (14, 0x6d),
    (16, 0x0e),
    (20, 0x2e),
    (24, 0x4e),
    (28, 0x6e),
    (32, 0x0f),
    (40, 0x2f),
    (48, 0x4f),
    (56, 0x6f),
    (64, 0x10),
    (80, 0x30),
    (96, 0x50),
    (112, 0x70),
    (128, 0x11),
];

fn time_sec_to_msr(seconds: u8) -> Option<u8> {
    POWER_LIMIT_TIME_SEC_TO_MSR
        .iter()
        .find(|(s, _)| *s == seconds)
        .map(|(_, encoded)| *encoded)
}

fn time_msr_to_sec(encoded: u8) -> u8 {
    POWER_LIMIT_TIME_SEC_TO_MSR
        .iter()
        .find(|(_, e)| *e == encoded)
        .map_or(0, |(s, _)| *s)
}

/// Package power limit register value (coreboot `set_power_limits()`).
fn power_limit_value(config: &Model206axConfig, unit: u32, sku: u64, mut time: u8) -> Option<u64> {
    let sku = LocalRegisterCopy::<u64, POWER_SKU::Register>::new(sku);
    let mut tdp = sku.read(POWER_SKU::TDP) as u32;
    let min_power = sku.read(POWER_SKU::MIN_POWER) as u32;
    let max_power = sku.read(POWER_SKU::MAX_POWER) as u32;
    let max_time = sku.read(POWER_SKU::MAX_TIME) as u8;
    let sku_time = time_msr_to_sec(max_time);
    if sku_time > time {
        time = sku_time;
    }
    if min_power > 0 && tdp < min_power {
        tdp = min_power;
    }
    if max_power > 0 && tdp > max_power {
        tdp = max_power;
    }
    // coreboot indexes a sparse table; an unlisted time encodes as zero.
    let time_val = time_sec_to_msr(time).unwrap_or(0);
    let pl1 = if config.pl1_mw != 0 {
        (config.pl1_mw * unit) / 1000
    } else {
        tdp
    };
    let pl2 = if config.pl2_mw != 0 {
        (config.pl2_mw * unit) / 1000
    } else {
        (tdp * 125) / 100
    };
    // coreboot applies `pl2_clamp` to both limits.
    let mut value = POWER_LIMIT::PL1.val(u64::from(pl1 & 0x7fff))
        + POWER_LIMIT::PL1_ENABLE::SET
        + POWER_LIMIT::PL1_TIME.val(u64::from(time_val & 0x7f))
        + POWER_LIMIT::PL2.val(u64::from(pl2 & 0x7fff))
        + POWER_LIMIT::PL2_ENABLE::SET;
    if config.pl2_clamp {
        value += POWER_LIMIT::PL1_CLAMP::SET + POWER_LIMIT::PL2_CLAMP::SET;
    }
    Some(value.value)
}

/// coreboot `set_power_limits()`: program the package power limits, which
/// takes effect only after the northbridge set `BIOS_RESET_CPL`.
pub fn set_power_limits(config: &Model206axConfig, power_limit_1_time: u8) {
    if power_limit_1_time > 128 {
        return;
    }
    let msr = cpu();
    if !msr.reg(MSR_PLATFORM_INFO).is_set(PLATFORM_INFO::SET_TDP) {
        return;
    }
    let unit = 2u32 << ((msr.reg(MSR_PKG_POWER_SKU_UNIT).get() & 0xf) as u32).saturating_sub(1);
    let sku = msr.reg(MSR_PKG_POWER_SKU).get();
    let Some(value) = power_limit_value(config, unit, sku, power_limit_1_time) else {
        return;
    };
    msr.reg(MSR_PKG_POWER_LIMIT).set(value);
    // Configurable-TDP parts use the nominal turbo activation ratio.
    if config_tdp_levels() != 0 {
        let nominal = msr.reg(MSR_CONFIG_TDP_NOMINAL).get() & 0xff;
        msr.reg(MSR_TURBO_ACTIVATION_RATIO).set(nominal);
    }
}

/// coreboot `intel_model_206ax_finalize_smm()`: lock thermal interrupt
/// routing and the memory configuration.
pub fn finalize() {
    let msr = cpu();
    msr.reg(MSR_MISC_PWR_MGMT)
        .modify(MISC_PWR_MGMT::LOCK_THERM_INT::SET);
    msr.reg(MSR_LT_LOCK_MEMORY).modify(LT_LOCK::LOCK::SET);
}

/// Logical processors per package (`MSR_CORE_THREAD_COUNT[15:0]`).
#[must_use]
pub fn threads_per_package() -> u32 {
    (cpu().reg(MSR_CORE_THREAD_COUNT).get() & 0xffff) as u32
}

/// Whether turbo is available and enabled (coreboot `get_turbo_state`).
#[must_use]
pub fn turbo_enabled() -> bool {
    let (max, _, _, _) = crate::x86::cpuid(0);
    if max < 6 {
        return false;
    }
    let turbo_cap = crate::x86::cpuid(6).0 & (1 << 1) != 0;
    let disabled = cpu()
        .reg(IA32_MISC_ENABLE)
        .is_set(MISC_ENABLE::TURBO_DISABLE);
    turbo_cap && !disabled
}

/// The model 206AX/306AX CPU driver.
pub struct Model206axCpuDriver {
    config: Model206axConfig,
    microcode: Option<&'static [u8]>,
}

impl Model206axCpuDriver {
    #[must_use]
    pub const fn new(config: Model206axConfig, microcode: Option<&'static [u8]>) -> Self {
        Self { config, microcode }
    }

    #[must_use]
    pub const fn config(&self) -> &Model206axConfig {
        &self.config
    }
}

fn psi_bits(current: u64, psi: &[PsiState; 3]) -> u64 {
    let mut value = current;
    for (i, state) in psi.iter().enumerate() {
        if state.phases == Vr12Phases::KeepDefault {
            continue;
        }
        let shift = 32 + i * 10;
        value &= !(0x3ff << shift);
        value |= (state.phases as u64 - 1) << (shift + 7);
        value |= u64::from(state.current) << shift;
    }
    value
}

fn current_config_bits(current: u64, limit: u64, psi: &[PsiState; 3]) -> u64 {
    let mut value = LocalRegisterCopy::<u64, CURRENT_CONFIG::Register>::new(current);
    value.modify(CURRENT_CONFIG::LIMIT.val(limit) + CURRENT_CONFIG::LOCK::SET);
    psi_bits(value.get(), psi)
}

impl Model206axCpuDriver {
    /// coreboot `configure_c_states()`.
    fn configure_c_states(&self, bsp: bool, ivy: bool) {
        let msr = cpu();
        msr.reg(MSR_PKG_CST_CONFIG_CONTROL).modify(
            PKG_CST_CONFIG::C1_UNDEMOTION::SET
                + PKG_CST_CONFIG::C3_UNDEMOTION::SET
                + PKG_CST_CONFIG::C1_AUTO_DEMOTION::SET
                + PKG_CST_CONFIG::C3_AUTO_DEMOTION::SET
                + PKG_CST_CONFIG::IO_MWAIT_REDIRECT::CLEAR
                + PKG_CST_CONFIG::PACKAGE_LIMIT.val(7)
                + PKG_CST_CONFIG::LOCK::SET,
        );
        if bsp {
            // Package-scope MSRs: once per package.
            msr.reg(MSR_MISC_PWR_MGMT)
                .modify(MISC_PWR_MGMT::EIST_HW_DISABLE::CLEAR);
            msr.reg(MSR_POWER_CTL).modify(
                POWER_CTL::ENERGY_PERF_BIAS_ACCESS::SET
                    + POWER_CTL::C1E::SET
                    + POWER_CTL::BIDIR_PROCHOT::SET,
            );
            let irtl = |response: u64| {
                (IRTL::VALID::SET + IRTL::UNIT::Ns1024 + IRTL::RESPONSE.val(response)).value
            };
            msr.reg(MSR_PKGC3_IRTL)
                .set(irtl(if ivy { 0x3b } else { 0x50 }));
            msr.reg(MSR_PKGC6_IRTL)
                .set(irtl(if ivy { 0x50 } else { 0x68 }));
            msr.reg(MSR_PKGC7_IRTL)
                .set(irtl(if ivy { 0x57 } else { 0x6d }));
            let pp0 = msr.reg(MSR_PP0_CURRENT_CONFIG);
            let limit = match self.config.pp0_current_limit {
                0 => PP0_CURRENT_LIMIT,
                amps => u64::from(amps) * 8,
            };
            pp0.set(current_config_bits(pp0.get(), limit, &self.config.pp0_psi));
            let pp1 = msr.reg(MSR_PP1_CURRENT_CONFIG);
            let limit = match (self.config.pp1_current_limit, ivy) {
                (0, true) => PP1_CURRENT_LIMIT_IVB,
                (0, false) => PP1_CURRENT_LIMIT_SNB,
                (amps, _) => u64::from(amps) * 8,
            };
            pp1.set(current_config_bits(pp1.get(), limit, &self.config.pp1_psi));
        }
        if msr
            .reg(MSR_PLATFORM_INFO)
            .is_set(PLATFORM_INFO::SET_TURBO_LIMIT)
        {
            let limit = msr.reg(MSR_TURBO_RATIO_LIMIT);
            let mut value = limit.get();
            for (i, &ratio) in self.config.turbo_limits.iter().enumerate() {
                if ratio != 0 {
                    value &= !(0xff << (8 * i));
                    value |= u64::from(ratio) << (8 * i);
                }
            }
            limit.set(value);
        }
    }

    /// coreboot `configure_thermal_target()`.
    fn configure_thermal_target(&self, bsp: bool) {
        let msr = cpu();
        if bsp
            && self.config.tcc_offset != 0
            && msr
                .reg(MSR_PLATFORM_INFO)
                .is_set(PLATFORM_INFO::TCC_OFFSET_PROGRAMMABLE)
        {
            msr.reg(MSR_TEMPERATURE_TARGET).modify(
                TEMPERATURE_TARGET::TCC_OFFSET.val(u64::from(self.config.tcc_offset & 0xf)),
            );
        }
    }

    /// coreboot `configure_misc()`.
    fn configure_misc(&self, bsp: bool) {
        let msr = cpu();
        msr.reg(IA32_MISC_ENABLE)
            .modify(MISC_ENABLE::FAST_STRINGS::SET + MISC_ENABLE::TM::SET + MISC_ENABLE::EIST::SET);
        msr.reg(IA32_THERM_INTERRUPT).set(0);
        if bsp {
            // Package critical interrupt only.
            msr.reg(IA32_PACKAGE_THERM_INTERRUPT).set(1 << 4);
        }
    }
}

impl SmmCpu for Model206axCpuDriver {
    fn smm_save_state_format(&self) -> X86SaveStateFormat {
        X86SaveStateFormat::IntelEm64t
    }
    fn smrr_pair(&self) -> Option<SmrrPair> {
        Some(SmrrPair::Architectural)
    }
    fn ied_size(&self) -> u32 {
        IED_SIZE
    }
}

impl CpuDriver for Model206axCpuDriver {
    fn name(&self) -> &'static str {
        "Intel Sandy Bridge/Ivy Bridge"
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
    fn pre_mp_init(&self) {
        let (_, _, ecx, _) = crate::x86::cpuid(1);
        fstart_log::info!(
            "model_206ax: AES {} TXT {} VT {}",
            u8::from(ecx & (1 << 25) != 0),
            u8::from(ecx & (1 << 6) != 0),
            u8::from(ecx & (1 << 5) != 0)
        );
    }
    fn init_cpu(&self) {
        let identity = CpuIdentity::current();
        let bsp = crate::x86::lapic::Lapic::is_bsp();
        let ivy = is_ivy(identity.signature);
        let msr = cpu();
        // Clear pending machine-check status (coreboot `mca_clear_status`).
        let banks = msr.reg(MCG_CAP).read(McgCap::COUNT) as u32;
        msr.reg(MCG_STATUS).set(0);
        for bank in 0..banks {
            msr.reg(Msr::<()>::new(IA32_MC0_STATUS + 4 * bank)).set(0);
        }
        // VMX and the feature-control lock.
        // SAFETY: model 206AX implements IA32_FEATURE_CONTROL.
        unsafe { feature_control::enable_and_lock(0) };
        self.configure_c_states(bsp, ivy);
        self.configure_misc(bsp);
        self.configure_thermal_target(bsp);
        // coreboot `set_aesni_lock`: lock AES-NI enabled when supported.
        if crate::x86::cpuid(1).2 & (1 << 25) != 0 {
            let feature = msr.reg(MSR_FEATURE_CONFIG);
            let value = feature.get();
            if value & 1 == 0 {
                feature.set(value | 1);
            }
        }
        // coreboot `configure_dca_cap`: enable DCA when advertised.
        if crate::x86::cpuid(1).2 & (1 << 18) != 0 {
            let dca = msr.reg(IA32_PLATFORM_DCA_CAP);
            dca.set(dca.get() | 1);
        }
        // coreboot `set_energy_perf_bias(ENERGY_POLICY_NORMAL)`, gated on
        // CPUID.6:ECX[3].
        if crate::x86::cpuid(6).2 & (1 << 3) != 0 {
            let epb = msr.reg(IA32_ENERGY_PERF_BIAS);
            epb.set((epb.get() & !0xf) | 6);
        }
        // coreboot `enable_turbo`.
        if crate::x86::cpuid(6).0 & (1 << 1) != 0 {
            msr.reg(IA32_MISC_ENABLE)
                .modify(MISC_ENABLE::TURBO_DISABLE::CLEAR);
        }
    }
    fn post_mp_init(&self) {
        // coreboot `set_max_ratio()`: effective once BIOS_RESET_CPL is set.
        let msr = cpu();
        let ratio = if turbo_enabled() {
            msr.reg(MSR_TURBO_RATIO_LIMIT).get() & 0xff
        } else if config_tdp_levels() != 0 {
            msr.reg(MSR_CONFIG_TDP_NOMINAL).get() & 0xff
        } else {
            msr.reg(MSR_PLATFORM_INFO)
                .read(PLATFORM_INFO::MAX_NON_TURBO_RATIO)
        };
        msr.reg(IA32_PERF_CTL).set(ratio << 8);
        fstart_log::info!(
            "model_206ax: frequency set to {} MHz",
            ratio as u32 * BCLK_MHZ
        );
    }
}

/// One ACPI P-state (coreboot `generate_P_state_entries`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pstate {
    pub mhz: u32,
    pub mw: u32,
    pub ratio: u32,
}

/// P-state list facts read from the CPU.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PstateLimits {
    pub ratio_min: u32,
    pub ratio_max: u32,
    pub ratio_turbo: Option<u32>,
    pub power_max_mw: u32,
    /// Hardware coordinates P-states (`MISC_PWR_MGMT[0]` clear).
    pub hw_coordination: bool,
}

impl PstateLimits {
    /// Read the limits from the current CPU.
    #[must_use]
    pub fn current() -> Self {
        let msr = cpu();
        let info = msr.reg(MSR_PLATFORM_INFO).extract();
        let ratio_max = if config_tdp_levels() != 0 {
            (msr.reg(MSR_CONFIG_TDP_NOMINAL).get() & 0xff) as u32
        } else {
            info.read(PLATFORM_INFO::MAX_NON_TURBO_RATIO) as u32
        };
        let unit = 2u32 << ((msr.reg(MSR_PKG_POWER_SKU_UNIT).get() & 0xf) as u32).saturating_sub(1);
        let tdp = msr.reg(MSR_PKG_POWER_SKU).read(POWER_SKU::TDP) as u32;
        Self {
            ratio_min: info.read(PLATFORM_INFO::MAX_EFFICIENCY_RATIO) as u32,
            ratio_max,
            ratio_turbo: turbo_enabled()
                .then(|| (msr.reg(MSR_TURBO_RATIO_LIMIT).get() & 0xff) as u32),
            power_max_mw: (tdp / unit.max(1)) * 1000,
            hw_coordination: !msr
                .reg(MSR_MISC_PWR_MGMT)
                .is_set(MISC_PWR_MGMT::EIST_HW_DISABLE),
        }
    }
}

/// coreboot `calculate_power`.
const fn calculate_power(tdp: u32, p1_ratio: u32, ratio: u32) -> u32 {
    let m = (110_000 - ((p1_ratio - ratio) * 625)) / 11;
    let m = (m * m) / 1000;
    let mut power = (ratio * 100_000 / p1_ratio) / 100;
    power *= (m / 100) * (tdp / 1000);
    power / 1000
}

/// The `_PSS` entries, turbo first when enabled (coreboot
/// `generate_P_state_entries`, `PSS_MAX_ENTRIES` 8, ratio step 2).
#[must_use]
pub fn pstates(limits: &PstateLimits) -> heapless::Vec<Pstate, 9> {
    let mut states = heapless::Vec::new();
    let clock_max = limits.ratio_max * BCLK_MHZ;
    let mut step = 2;
    let mut entries = limits.ratio_max.saturating_sub(limits.ratio_min) / step;
    while entries > 8 - 1 {
        step <<= 1;
        entries >>= 1;
    }
    if let Some(turbo) = limits.ratio_turbo {
        let _ = states.push(Pstate {
            mhz: clock_max + 1,
            mw: limits.power_max_mw,
            ratio: turbo,
        });
    }
    let _ = states.push(Pstate {
        mhz: clock_max,
        mw: limits.power_max_mw,
        ratio: limits.ratio_max,
    });
    if entries > 0 {
        let mut ratio = limits.ratio_min + (entries - 1) * step;
        loop {
            let _ = states.push(Pstate {
                mhz: ratio * BCLK_MHZ,
                mw: calculate_power(limits.power_max_mw, limits.ratio_max, ratio),
                ratio,
            });
            if ratio < limits.ratio_min + step {
                break;
            }
            ratio -= step;
        }
    }
    states
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn power_limits_follow_coreboot() {
        let config = Model206axConfig::new();
        // 35 W TDP in 1/8 W units, no SKU minimum/maximum, 28 s.
        let sku = 280u64;
        let value = power_limit_value(&config, 8, sku, 28).unwrap();
        assert_eq!(value & 0x7fff, 280);
        assert_ne!(value & (1 << 15), 0);
        assert_eq!((value >> 17) & 0x7f, 0x6e);
        assert_eq!((value >> 32) & 0x7fff, 350);
        assert_ne!(value & (1 << 47), 0);
        assert_eq!(time_msr_to_sec(0x6e), 28);
    }

    #[test]
    fn current_config_programs_limit_lock_and_psi() {
        let psi = [
            PsiState {
                phases: Vr12Phases::Two,
                current: 20,
            },
            PsiState {
                phases: Vr12Phases::All,
                current: 5,
            },
            PsiState::DEFAULT,
        ];
        let value = current_config_bits(0, 98 * 8, &psi);
        assert_eq!(value & 0x1fff, 98 * 8);
        assert_ne!(value & (1 << 31), 0);
        assert_eq!((value >> 32) & 0x3ff, 1 << 7 | 20);
        assert_eq!((value >> 42) & 0x3ff, 5);
    }

    #[test]
    fn pss_table_matches_coreboot_shape() {
        // i5-2520M: ratio 8..25, turbo 32, 35 W.
        let limits = PstateLimits {
            ratio_min: 8,
            ratio_max: 25,
            ratio_turbo: Some(32),
            power_max_mw: 35_000,
            hw_coordination: true,
        };
        let states = pstates(&limits);
        assert_eq!(
            states[0],
            Pstate {
                mhz: 2501,
                mw: 35_000,
                ratio: 32
            }
        );
        assert_eq!(states[1].mhz, 2500);
        // (25 - 8) / 2 = 8 entries -> step 4, 4 entries: 20, 16, 12, 8.
        let ratios: heapless::Vec<u32, 9> = states.iter().map(|s| s.ratio).collect();
        assert_eq!(&ratios[..], &[32, 25, 20, 16, 12, 8]);
        assert!(states[2].mw < 35_000);
    }

    #[test]
    fn matches_sandy_and_ivy_bridge() {
        let driver = Model206axCpuDriver::new(Model206axConfig::new(), None);
        for signature in [0x206a7, 0x306a9] {
            assert!(driver.matches(CpuIdentity {
                vendor: CpuVendor::Intel,
                signature,
            }));
        }
        assert!(!driver.matches(CpuIdentity {
            vendor: CpuVendor::Intel,
            signature: 0x1067a,
        }));
        assert_eq!(driver.smrr_pair(), Some(SmrrPair::Architectural));
        assert_eq!(driver.ied_size(), 4 << 20);
    }
}
