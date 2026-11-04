//! Intel Silvermont (Bay Trail, family 6 model 37h) CPU support.
//!
//! Mirrors coreboot's `soc/intel/baytrail/cpu.c` and `tsc_freq.c`: package
//! power limits and turbo on the BSP before the APs start, then per core
//! VMX, the core power MSRs and the guaranteed frequency ratio. The SoC's
//! B-unit SMM write-back policy is chipset state, programmed by the north
//! cluster driver before MP init.

use crate::x86::cpu::intel::feature_control;
use crate::x86::cpu::intel::smm::{SmmCpu, SmrrPair, X86SaveStateFormat};
use crate::x86::cpu::msr::{Msr, MsrAccess, NativeMsr};
use crate::x86::mp::{CpuDriver, CpuIdMatch, CpuVendor};
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};
use tock_registers::register_bitfields;

register_bitfields![u64,
    /// `MSR_BSEL_CR_OVERCLOCK_CONTROL` (0xcd): the bus clock select.
    pub BSEL [
        FREQ OFFSET(0) NUMBITS(3) [Mhz83 = 0, Mhz100 = 1, Mhz133 = 2, Mhz116 = 3, Mhz80 = 4]
    ],
    /// `MSR_PLATFORM_INFO` (0xce).
    pub PLATFORM_INFO [ MAX_NON_TURBO_RATIO OFFSET(8) NUMBITS(8) [] ],
    /// `MSR_POWER_MISC` (0x120).
    POWER_MISC [
        ENABLE_ULFM_AUTOCM OFFSET(2) NUMBITS(1) [],
        ENABLE_INDP_AUTOCM OFFSET(3) NUMBITS(1) []
    ],
    /// `MSR_POWER_CTL` (0x1fc).
    POWER_CTL [ C1E OFFSET(1) NUMBITS(1) [] ],
    /// `IA32_MISC_ENABLE` (0x1a0).
    MISC_ENABLE [
        EIST OFFSET(16) NUMBITS(1) [],
        TURBO_DISABLE OFFSET(38) NUMBITS(1) []
    ],
    /// `MSR_IACORE_RATIOS` (0x66a) and `MSR_IACORE_VIDS` (0x66b).
    pub IACORE [
        MIN OFFSET(0) NUMBITS(7) [],
        LFM OFFSET(8) NUMBITS(7) [],
        MAX OFFSET(16) NUMBITS(7) []
    ],
    /// `IA32_PERF_CTL` (0x199).
    PERF_CTL [ VID OFFSET(0) NUMBITS(8) [], RATIO OFFSET(8) NUMBITS(8) [] ],
    /// `MSR_PP1_POWER_LIMIT` (0x638).
    PP1_POWER_LIMIT [ LIMIT OFFSET(17) NUMBITS(7) [] ]
];

register_bitfields![u32,
    CPUID_6_EAX [ TURBO OFFSET(1) NUMBITS(1) [] ]
];

pub const MSR_BSEL_CR_OVERCLOCK_CONTROL: Msr<BSEL::Register> = Msr::new(0xcd);
pub const MSR_PLATFORM_INFO: Msr<PLATFORM_INFO::Register> = Msr::new(0xce);
const MSR_PKG_CST_CONFIG_CONTROL: Msr = Msr::new(0xe2);
const MSR_POWER_MISC: Msr<POWER_MISC::Register> = Msr::new(0x120);
const IA32_PERF_CTL: Msr<PERF_CTL::Register> = Msr::new(0x199);
const IA32_MISC_ENABLE: Msr<MISC_ENABLE::Register> = Msr::new(0x1a0);
const MSR_POWER_CTL: Msr<POWER_CTL::Register> = Msr::new(0x1fc);
const MSR_PKG_POWER_LIMIT: Msr = Msr::new(0x610);
const MSR_PP1_POWER_LIMIT: Msr<PP1_POWER_LIMIT::Register> = Msr::new(0x638);
pub const MSR_IACORE_RATIOS: Msr<IACORE::Register> = Msr::new(0x66a);
pub const MSR_IACORE_VIDS: Msr<IACORE::Register> = Msr::new(0x66b);
pub const MSR_IACORE_TURBO_RATIOS: Msr = Msr::new(0x66c);
const MSR_PKG_TURBO_CFG1: Msr = Msr::new(0x670);
const MSR_CPU_TURBO_WKLD_CFG1: Msr = Msr::new(0x671);
const MSR_CPU_TURBO_WKLD_CFG2: Msr = Msr::new(0x672);
const MSR_CPU_THERM_CFG1: Msr = Msr::new(0x673);
const MSR_CPU_THERM_CFG2: Msr = Msr::new(0x674);
const MSR_CPU_THERM_SENS_CFG: Msr = Msr::new(0x675);

/// Bus clock in kHz from `MSR_BSEL_CR_OVERCLOCK_CONTROL`.
#[must_use]
pub fn bus_freq_khz() -> u32 {
    // SAFETY: Silvermont implements MSR_BSEL_CR_OVERCLOCK_CONTROL.
    let cpu = unsafe { NativeMsr::new() };
    match cpu
        .reg(MSR_BSEL_CR_OVERCLOCK_CONTROL)
        .read_as_enum(BSEL::FREQ)
    {
        Some(BSEL::FREQ::Value::Mhz83) => 83_333,
        Some(BSEL::FREQ::Value::Mhz100) => 100_000,
        Some(BSEL::FREQ::Value::Mhz133) => 133_333,
        Some(BSEL::FREQ::Value::Mhz116) => 116_666,
        Some(BSEL::FREQ::Value::Mhz80) => 80_000,
        None => 0,
    }
}

/// TSC frequency in MHz: bus clock times the maximum non-turbo ratio.
#[must_use]
pub fn tsc_freq_mhz() -> u32 {
    // SAFETY: Silvermont implements MSR_PLATFORM_INFO.
    let ratio = unsafe { NativeMsr::new() }
        .reg(MSR_PLATFORM_INFO)
        .read(PLATFORM_INFO::MAX_NON_TURBO_RATIO) as u32;
    bus_freq_khz() * ratio / 1000
}

/// Enable EIST and request the guaranteed ratio and voltage (coreboot
/// `set_max_freq()`), on the executing core.
pub fn set_max_freq() {
    // SAFETY: Silvermont implements these MSRs.
    let cpu = unsafe { NativeMsr::new() };
    cpu.reg(IA32_MISC_ENABLE).modify(MISC_ENABLE::EIST::SET);
    let ratio = cpu.reg(MSR_IACORE_RATIOS).read(IACORE::MAX);
    let vid = cpu.reg(MSR_IACORE_VIDS).read(IACORE::MAX);
    cpu.reg(IA32_PERF_CTL)
        .write(PERF_CTL::RATIO.val(ratio) + PERF_CTL::VID.val(vid));
}

/// Clear the turbo disable of the executing core's building block when
/// CPUID hides turbo only because firmware disabled it (coreboot
/// `enable_turbo()`).
fn enable_turbo() {
    let (eax, _, _, _) = crate::x86::cpuid(6);
    let available = tock_registers::LocalRegisterCopy::<u32, CPUID_6_EAX::Register>::new(eax)
        .is_set(CPUID_6_EAX::TURBO);
    // SAFETY: IA32_MISC_ENABLE is architectural on this CPU.
    let cpu = unsafe { NativeMsr::new() };
    let misc = cpu.reg(IA32_MISC_ENABLE);
    if !available && misc.is_set(MISC_ENABLE::TURBO_DISABLE) {
        misc.modify(MISC_ENABLE::TURBO_DISABLE::CLEAR);
    }
}

static SILVERMONT_IDS: &[CpuIdMatch] = &[
    CpuIdMatch {
        vendor: CpuVendor::Intel,
        signature: 0x30673,
        mask: CpuIdMatch::EXACT_MASK,
    },
    CpuIdMatch {
        vendor: CpuVendor::Intel,
        signature: 0x30678,
        mask: CpuIdMatch::EXACT_MASK,
    },
    CpuIdMatch {
        vendor: CpuVendor::Intel,
        signature: 0x30679,
        mask: CpuIdMatch::EXACT_MASK,
    },
];

/// CPU driver for the Bay Trail Silvermont cores.
pub struct SilvermontCpuDriver {
    microcode: Option<&'static [u8]>,
}

impl SilvermontCpuDriver {
    #[must_use]
    pub const fn new(microcode: Option<&'static [u8]>) -> Self {
        Self { microcode }
    }
}

impl SmmCpu for SilvermontCpuDriver {
    fn smm_save_state_format(&self) -> X86SaveStateFormat {
        X86SaveStateFormat::IntelEm64t
    }

    fn smrr_pair(&self) -> Option<SmrrPair> {
        Some(SmrrPair::Architectural)
    }
}

impl CpuDriver for SilvermontCpuDriver {
    fn name(&self) -> &'static str {
        "Intel Silvermont (Bay Trail)"
    }

    fn id_table(&self) -> &'static [CpuIdMatch] {
        SILVERMONT_IDS
    }

    fn update_microcode(&self) {
        if let Some(blob) = self.microcode {
            let cpu = crate::x86::mp::current_cpu_index();
            let before = crate::x86::cpu::intel::microcode::current_revision();
            fstart_log::info!("microcode: cpu{} before rev={:#x}", cpu, before);
            // SAFETY: the blob is firmware-image-backed and stays mapped
            // throughout MP init.
            unsafe { crate::x86::cpu::intel::microcode::update_current_cpu_logged(blob) };
            let after = crate::x86::cpu::intel::microcode::current_revision();
            fstart_log::info!("microcode: cpu{} after rev={:#x}", cpu, after);
        }
    }

    /// Package power, turbo and thermal limits (coreboot `pre_mp_init`).
    fn pre_mp_init(&self) {
        // SAFETY: Silvermont implements the package MSRs.
        let cpu = unsafe { NativeMsr::new() };
        // Package TDP of about 7 W.
        cpu.reg(MSR_PKG_POWER_LIMIT).set(0x0038_80fa);
        cpu.reg(MSR_PP1_POWER_LIMIT)
            .modify(PP1_POWER_LIMIT::LIMIT.val(0));
        cpu.reg(MSR_PKG_TURBO_CFG1).set(0x702);
        cpu.reg(MSR_CPU_TURBO_WKLD_CFG1).set(0x200b);
        cpu.reg(MSR_CPU_TURBO_WKLD_CFG2).set(0);
        cpu.reg(MSR_CPU_THERM_CFG1).set(0x0000_0305);
        cpu.reg(MSR_CPU_THERM_CFG2).set(0x0405_500d);
        cpu.reg(MSR_CPU_THERM_SENS_CFG).set(0x27);
        // Turbo on the BSP and the siblings of its building block.
        enable_turbo();
    }

    fn init_cpu(&self) {
        // The turbo disable is scoped per building block, not per package:
        // cores outside the BSP's block enable their own.
        if crate::x86::lapic::Lapic::from_msr().id() != 0 {
            enable_turbo();
        }
        // SAFETY: Silvermont implements IA32_FEATURE_CONTROL.
        unsafe { feature_control::enable_and_lock(0) };
        // SAFETY: Silvermont implements the core power MSRs.
        let cpu = unsafe { NativeMsr::new() };
        // Dynamic L2 shrink enable and threshold; clear SINGLE_PCTL (bit 11).
        let cst = cpu.reg(MSR_PKG_CST_CONFIG_CONTROL);
        cst.set((cst.get() & !0x3f_080f) | 0xe_0008);
        cpu.reg(MSR_POWER_MISC)
            .modify(POWER_MISC::ENABLE_ULFM_AUTOCM::CLEAR + POWER_MISC::ENABLE_INDP_AUTOCM::CLEAR);
        // Disable C1E.
        cpu.reg(MSR_POWER_CTL).modify(POWER_CTL::C1E::CLEAR);
        let misc = cpu.reg(MSR_POWER_MISC);
        misc.set(misc.get() | 0x44);
        set_max_freq();
    }
}
