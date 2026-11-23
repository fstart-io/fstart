// SPDX-License-Identifier: MIT OR Apache-2.0
//! Native Skylake/Kaby Lake cold-boot CPU driver.
//!
//! Port of soc/intel/skylake/cpu.c and common/block/cpu/cpulib.c.
//! SGX, TXT and retained/warm boot are not implemented by this driver.

pub mod identification;

use super::{common_power::MISC_ENABLE, feature_control, microcode, smm::SmmCpu, smrr::SmrrPair};
use crate::x86::cpu::msr::{
    MCG_CAP, McgCap, Msr, MsrAccess, NativeMsr, PLATFORM_INFO, PlatformInfo,
};
use crate::x86::{
    cpuid, cpuid_count,
    mp::{CpuDriver, CpuIdMatch, CpuIdentity, CpuVendor},
};
use core::num::NonZeroU32;
use fstart_core::services::ServiceError;
use fstart_smm::X86SaveStateFormat;
use tock_registers::{
    interfaces::{ReadWriteable, Readable, Writeable},
    register_bitfields,
};

register_bitfields![u64,
    PicMessageControl [ TPR_UPDATES_DISABLE OFFSET(10) NUMBITS(1) [] ],
    InterruptLatency [
        LIMIT OFFSET(0) NUMBITS(10) [],
        SCALE OFFSET(10) NUMBITS(3) [],
        VALID OFFSET(15) NUMBITS(1) []
    ],
    PowerControl [
        PROCHOT_INPUT OFFSET(0) NUMBITS(1) [],
        C1E OFFSET(1) NUMBITS(1) [],
        ENERGY_BIAS OFFSET(18) NUMBITS(1) [],
        LOCK OFFSET(23) NUMBITS(1) []
    ],
    ThermalInterrupt [ CRITICAL OFFSET(4) NUMBITS(1) [] ],
    FeatureConfig [ AESNI_LOCK OFFSET(0) NUMBITS(1) [] ],
    DcaCapability [ TYPE0_ENABLE OFFSET(0) NUMBITS(1) [] ],
    EnergyBias [ POLICY OFFSET(0) NUMBITS(4) [] ],
    PmTimer [
        PORT OFFSET(0) NUMBITS(16) [],
        ENABLE OFFSET(16) NUMBITS(1) [],
        DELAY OFFSET(20) NUMBITS(12) [],
        MULTIPLIER OFFSET(32) NUMBITS(32) []
    ],
    TemperatureTarget [ OFFSET OFFSET(24) NUMBITS(4) [] ],
    TurboRatio [ ONE_CORE OFFSET(0) NUMBITS(8) [] ],
    PerformanceControl [ RATIO OFFSET(8) NUMBITS(8) [] ],
    SmmCapability [ CODE_ACCESS_CHECK OFFSET(58) NUMBITS(1) [] ],
    SmmFeatureControl [
        LOCK OFFSET(0) NUMBITS(1) [],
        SAVE_STATE_IN_MSRS OFFSET(1) NUMBITS(1) [],
        CODE_ACCESS_CHECK OFFSET(2) NUMBITS(1) []
    ],
    ChipsetUsage [ SMM_FLASH_WRITES OFFSET(0) NUMBITS(1) [] ]
];
const MISC_ENABLE: Msr<MISC_ENABLE::Register> = Msr::new(0x1a0);
const TURBO_RATIO: Msr<TurboRatio::Register> = Msr::new(0x1ad);

/// Whether `IA32_MISC_ENABLE` has Enhanced SpeedStep on (the CPU policy's
/// `eist`, as `configure_core` programmed it).
pub fn eist_enabled(cpu: &impl MsrAccess) -> bool {
    cpu.reg(MISC_ENABLE).is_set(MISC_ENABLE::EIST)
}

/// The single-core turbo ratio when turbo exists and `IA32_MISC_ENABLE`
/// does not hide it (coreboot `get_turbo_state()`, `cpu_get_max_turbo_ratio()`).
pub fn turbo_ratio(cpu: &impl MsrAccess) -> Option<u8> {
    (cpuid(6).0 & (1 << 1) != 0 && !cpu.reg(MISC_ENABLE).is_set(MISC_ENABLE::TURBO_DISABLE))
        .then(|| cpu.reg(TURBO_RATIO).read(TurboRatio::ONE_CORE) as u8)
}
const POWER_CTL: Msr<PowerControl::Register> = Msr::new(0x1fc);

#[derive(Debug, Clone, Copy)]
pub enum AcpiTimer {
    /// The PCH ACPI timer remains enabled; no CPU emulation MSR is written.
    Hardware,
    /// Explicit platform crystal, not a guessed TSC-derived crystal.
    Emulated { crystal_hz: NonZeroU32, port: u16 },
}
#[derive(Debug, Clone, Copy)]
pub struct Policy {
    pub eist: bool,
    pub aesni_lock: bool,
    /// Zero preserves the hardware's TCC offset, as in the native source.
    pub tcc_offset: u8,
    pub acpi_timer: AcpiTimer,
}
impl Default for Policy {
    fn default() -> Self {
        Self {
            eist: true,
            aesni_lock: true,
            tcc_offset: 0,
            acpi_timer: AcpiTimer::Hardware,
        }
    }
}

pub struct SkylakeCpuDriver {
    policy: Policy,
    microcode: Option<&'static [u8]>,
}
impl SkylakeCpuDriver {
    /// `microcode` is the FFS microcode blob, which stays mapped through MP
    /// init.
    #[must_use]
    pub const fn new(policy: Policy, microcode: Option<&'static [u8]>) -> Self {
        Self { policy, microcode }
    }

    fn configure_core(&self) -> Result<(), ServiceError> {
        let cpu = unsafe { NativeMsr::new() };
        let banks = cpu.reg(MCG_CAP).read(McgCap::COUNT) as u32;
        // Native cold-only ordering: clear every status, then enable controls.
        for bank in 0..banks {
            cpu.reg(Msr::<()>::new(0x401 + 4 * bank)).set(0);
        }
        for bank in 0..banks {
            cpu.reg(Msr::<()>::new(0x400 + 4 * bank)).set(u64::MAX);
        }
        cpu.reg(Msr::<PicMessageControl::Register>::new(0x2e))
            .modify(PicMessageControl::TPR_UPDATES_DISABLE::CLEAR);
        for (index, limit) in [
            (0x60a, 0x4e),
            (0x60b, 0x76),
            (0x60c, 0x94),
            (0x633, 0xfa),
            (0x634, 0x14c),
            (0x635, 0x3f2),
        ] {
            cpu.reg(Msr::<InterruptLatency::Register>::new(index))
                .write(
                    InterruptLatency::VALID::SET
                        + InterruptLatency::SCALE.val(2)
                        + InterruptLatency::LIMIT.val(limit),
                );
        }
        cpu.reg(MISC_ENABLE).modify(
            MISC_ENABLE::FAST_STRINGS::SET
                + MISC_ENABLE::TM1::SET
                + MISC_ENABLE::EIST.val(u64::from(self.policy.eist)),
        );
        cpu.reg(Msr::<()>::new(0x19b)).set(0);
        cpu.reg(Msr::<ThermalInterrupt::Register>::new(0x1b2))
            .write(ThermalInterrupt::CRITICAL::SET);
        let power = cpu.reg(POWER_CTL);
        let wanted = PowerControl::PROCHOT_INPUT::SET
            + PowerControl::ENERGY_BIAS::SET
            + PowerControl::C1E::CLEAR
            + PowerControl::LOCK::SET;
        if power.is_set(PowerControl::LOCK) {
            if !power.matches_all(wanted) {
                return Err(ServiceError::NotSupported);
            }
        } else {
            power.modify(wanted);
            if !power.matches_all(wanted) {
                return Err(ServiceError::HardwareError);
            }
        }
        if self.policy.aesni_lock && cpuid(1).2 & (1 << 25) != 0 && primary_thread() {
            let aes = cpu.reg(Msr::<FeatureConfig::Register>::new(0x13c));
            if !aes.is_set(FeatureConfig::AESNI_LOCK) {
                aes.modify(FeatureConfig::AESNI_LOCK::SET);
            }
        }
        if let AcpiTimer::Emulated { crystal_hz, port } = self.policy.acpi_timer {
            let multiplier = timer_multiplier(crystal_hz).ok_or(ServiceError::InvalidParam)?;
            cpu.reg(Msr::<PmTimer::Register>::new(0x121)).write(
                PmTimer::PORT.val(u64::from(port))
                    + PmTimer::ENABLE::SET
                    + PmTimer::DELAY.val(0x13)
                    + PmTimer::MULTIPLIER.val(u64::from(multiplier)),
            );
        }
        if cpuid(1).2 & (1 << 18) != 0 {
            cpu.reg(Msr::<DcaCapability::Register>::new(0x1f8))
                .modify(DcaCapability::TYPE0_ENABLE::SET);
        }
        if cpuid(6).2 & (1 << 3) != 0 {
            cpu.reg(Msr::<EnergyBias::Register>::new(0x1b0))
                .modify(EnergyBias::POLICY.val(6));
        }
        if cpuid(6).0 & (1 << 1) != 0 || cpu.reg(MISC_ENABLE).is_set(MISC_ENABLE::TURBO_DISABLE) {
            cpu.reg(MISC_ENABLE)
                .modify(MISC_ENABLE::TURBO_DISABLE::CLEAR);
        }
        // No SGX policy. Architectural SMRR is not gated by FEATURE_CONTROL;
        // the existing per-CPU VMX setup/lock can therefore precede relocation.
        unsafe {
            feature_control::enable_and_lock(0);
        }
        Ok(())
    }
}

/// Native shared SMI finalize security, soc/intel/common/block/smm/smihandler.c.
/// Returns false only when hardware does not implement code access checking.
/// # Safety
/// Run INSIDE the installed permanent TSEG SMM image, after every CPU's SMBASE
/// relocation, as the single rendezvous leader. FEATURE_CONTROL is package
/// scoped on these clients; concurrent per-CPU lock writes can fault. Never
/// enable this while executing the temporary outside-SMRR relocation callback.
pub unsafe fn secure_smm_entry() -> Result<bool, ServiceError> {
    let identity = CpuIdentity::current();
    if identity.vendor != CpuVendor::Intel || identification::identify(identity.signature).is_none()
    {
        return Err(ServiceError::NotSupported);
    }
    let cpu = unsafe { NativeMsr::new() };
    if !cpu
        .reg(Msr::<SmmCapability::Register>::new(0x17d))
        .is_set(SmmCapability::CODE_ACCESS_CHECK)
    {
        return Ok(false);
    }
    let control = cpu.reg(Msr::<SmmFeatureControl::Register>::new(0x4e0));
    if control.is_set(SmmFeatureControl::SAVE_STATE_IN_MSRS) {
        return Err(ServiceError::NotSupported); // The real installed image uses EM64T101 memory save state.
    }
    if !control.is_set(SmmFeatureControl::LOCK) {
        control.modify(SmmFeatureControl::CODE_ACCESS_CHECK::SET + SmmFeatureControl::LOCK::SET);
    }
    if !control
        .matches_all(SmmFeatureControl::CODE_ACCESS_CHECK::SET + SmmFeatureControl::LOCK::SET)
    {
        return Err(ServiceError::HardwareError);
    }
    Ok(true)
}

/// Native set_insmm_sts(false): this image offers no SMM flash-write service.
/// # Safety
/// Run in SMM on an identified supported Skylake/Kaby Lake CPU, with the
/// native chipset-usage mirror at fed30880h mapped UC and exclusively owned.
/// Do not call while another SMM service owns an authorized write interval.
pub unsafe fn deny_smm_flash_writes() -> Result<(), ServiceError> {
    let mirror = unsafe { fstart_core::mmio::MmioRegister::<u32>::new(0xfed3_0880) };
    let mut value = tock_registers::LocalRegisterCopy::<u64, ChipsetUsage::Register>::new(
        u64::from(mirror.get()),
    );
    value.modify(ChipsetUsage::SMM_FLASH_WRITES::CLEAR);
    let cpu = unsafe { NativeMsr::new() };
    // Native uses the MMIO mirror, not RDMSR of the special chipset MSR.
    cpu.reg(Msr::<ChipsetUsage::Register>::new(0x1fe))
        .set(value.get());
    if u64::from(mirror.get()) & ChipsetUsage::SMM_FLASH_WRITES::SET.value != 0 {
        return Err(ServiceError::HardwareError);
    }
    Ok(())
}

fn timer_multiplier(crystal_hz: NonZeroU32) -> Option<u32> {
    let crystal = u64::from(crystal_hz.get());
    u32::try_from(((3_579_545u64 << 32) + crystal / 2) / crystal)
        .ok()
        .filter(|value| *value != 0)
}
fn primary_thread() -> bool {
    let (shift, count, level, apic_id) = cpuid_count(0xb, 0);
    assert!(
        count & 0xffff != 0 && (level >> 8) & 0xff == 1,
        "missing Intel SMT topology"
    );
    apic_id & ((1u32 << (shift & 0x1f)) - 1) == 0
}

impl CpuDriver for SkylakeCpuDriver {
    fn name(&self) -> &'static str {
        "Intel Skylake/Kaby Lake"
    }
    /// Threads in the package from the topology leaf.
    fn logical_cpu_count(&self) -> Option<u16> {
        (0..8)
            .map(|level| cpuid_count(0xb, level))
            .find(|&(_, _, ecx, _)| (ecx >> 8) & 0xff == 2)
            .map(|(_, ebx, _, _)| (ebx & 0xffff) as u16)
            .filter(|threads| *threads != 0)
    }
    fn id_table(&self) -> &'static [CpuIdMatch] {
        identification::CPU_IDS
    }
    fn update_microcode(&self) {
        // The FIT loaded the BSP's before reset; this updates the APs.
        if let Some(blob) = self.microcode {
            // SAFETY: the blob is firmware-image-backed and stays mapped
            // throughout MP init.
            unsafe { microcode::update_current_cpu_logged(blob) };
        }
    }
    fn init_cpu(&self) {
        if self.configure_core().is_err() {
            fstart_log::error!("Skylake: core configuration failed");
        }
    }
    fn post_mp_init(&self) {
        let cpu = unsafe { NativeMsr::new() };
        if let Some(ratio) = turbo_ratio(&cpu).filter(|ratio| *ratio != 0) {
            cpu.reg(Msr::<PerformanceControl::Register>::new(0x199))
                .write(PerformanceControl::RATIO.val(u64::from(ratio)));
        }
        if self.policy.tcc_offset != 0
            && cpu
                .reg(PLATFORM_INFO)
                .is_set(PlatformInfo::TCC_OFFSET_WRITABLE)
        {
            cpu.reg(Msr::<TemperatureTarget::Register>::new(0x1a2))
                .modify(TemperatureTarget::OFFSET.val(u64::from(self.policy.tcc_offset)));
        }
    }
}
impl SmmCpu for SkylakeCpuDriver {
    fn smm_save_state_format(&self) -> X86SaveStateFormat {
        X86SaveStateFormat::IntelEm64t
    }
    fn smrr_pair(&self) -> Option<SmrrPair> {
        Some(SmrrPair::Architectural)
    }
}
