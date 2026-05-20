//! Intel Sandy Bridge / Ivy Bridge CPU operations (model 206Ax / 306Ax).
//!
//! Mirrors the essential pieces of coreboot's `cpu/intel/model_206ax` MP
//! initialization: per-CPU MTRR publication, C-state MSRs, thermal/EIST setup,
//! energy-performance bias, Turbo enable, and serialized microcode updates on
//! Hyper-Threading-capable parts.

use fstart_arch_x86::mtrr;
use fstart_arch_x86::x86::msr::{rdmsr, wrmsr};
use fstart_arch_x86::{cpuid, cpuid_count};
use fstart_mp::CpuOps;

const MSR_PKG_CST_CONFIG_CONTROL: u32 = 0x00e2;
const MSR_MISC_PWR_MGMT: u32 = 0x01aa;
const MSR_POWER_CTL: u32 = 0x01fc;
const MSR_PKGC3_IRTL: u32 = 0x0060;
const MSR_PKGC6_IRTL: u32 = 0x0061;
const MSR_PKGC7_IRTL: u32 = 0x0062;
const MSR_PLATFORM_INFO: u32 = 0x00ce;
const MSR_TURBO_RATIO_LIMIT: u32 = 0x01ad;
const IA32_MISC_ENABLE: u32 = 0x01a0;
const IA32_THERM_INTERRUPT: u32 = 0x019b;
const IA32_PACKAGE_THERM_INTERRUPT: u32 = 0x01b2;
const IA32_PERF_CTL: u32 = 0x0199;
const IA32_ENERGY_PERF_BIAS: u32 = 0x01b0;
const IA32_FEATURE_CONTROL: u32 = 0x003a;

const IRTL_VALID: u64 = 1 << 15;
const IRTL_1024_NS: u64 = 2 << 10;
const ENERGY_POLICY_NORMAL: u64 = 6;

fn is_ivy_bridge() -> bool {
    let (eax, _, _, _) = cpuid(1);
    let family = ((eax >> 8) & 0x0f) + ((eax >> 20) & 0xff);
    let model = ((eax >> 4) & 0x0f) + ((eax >> 12) & 0xf0);
    family == 6 && model == 0x3a
}

fn ht_supported() -> bool {
    let (_, _, _, edx) = cpuid(1);
    (edx & (1 << 28)) != 0
}

fn configure_c_states() {
    // SAFETY: these MSRs are defined for Sandy Bridge/Ivy Bridge CPUs.
    unsafe {
        let mut cst = rdmsr(MSR_PKG_CST_CONFIG_CONTROL);
        cst |= 1 << 28; // C1 auto-undemotion
        cst |= 1 << 27; // C3 auto-undemotion
        cst |= 1 << 26; // C1 auto-demotion
        cst |= 1 << 25; // C3 auto-demotion
        cst &= !(1 << 10); // disable I/O MWAIT redirection
        cst = (cst & !7) | 7; // no package C-state limit
        cst |= 1 << 15; // lock
        wrmsr(MSR_PKG_CST_CONFIG_CONTROL, cst);

        let mut misc_pwr = rdmsr(MSR_MISC_PWR_MGMT);
        misc_pwr &= !1; // hardware all-P-state coordination
        wrmsr(MSR_MISC_PWR_MGMT, misc_pwr);

        let mut power_ctl = rdmsr(MSR_POWER_CTL);
        power_ctl |= 1 << 18; // enable energy-performance bias MSR
        power_ctl |= 1 << 1; // C1E
        power_ctl |= 1 << 0; // bidirectional PROCHOT#
        wrmsr(MSR_POWER_CTL, power_ctl);

        let (c3, c6, c7) = if is_ivy_bridge() {
            (0x3b, 0x50, 0x57)
        } else {
            (0x50, 0x68, 0x6d)
        };
        wrmsr(MSR_PKGC3_IRTL, IRTL_VALID | IRTL_1024_NS | c3);
        wrmsr(MSR_PKGC6_IRTL, IRTL_VALID | IRTL_1024_NS | c6);
        wrmsr(MSR_PKGC7_IRTL, IRTL_VALID | IRTL_1024_NS | c7);
    }
}

fn configure_misc() {
    // SAFETY: these MSRs are defined for Sandy Bridge/Ivy Bridge CPUs.
    unsafe {
        let mut misc = rdmsr(IA32_MISC_ENABLE);
        misc |= 1 << 0; // fast strings
        misc |= 1 << 3; // thermal monitor
        misc |= 1 << 16; // Enhanced SpeedStep
        wrmsr(IA32_MISC_ENABLE, misc);

        // Disable per-core thermal interrupts.  Package critical interrupt is
        // harmless to program identically on every logical CPU.
        wrmsr(IA32_THERM_INTERRUPT, 0);
        wrmsr(IA32_PACKAGE_THERM_INTERRUPT, 1 << 4);

        wrmsr(IA32_ENERGY_PERF_BIAS, ENERGY_POLICY_NORMAL);

        let mut feature_control = rdmsr(IA32_FEATURE_CONTROL);
        if (feature_control & 1) == 0 {
            // Leave VMX disabled for now but lock the register like coreboot's
            // Kconfig-disabled VMX path.
            feature_control |= 1;
            wrmsr(IA32_FEATURE_CONTROL, feature_control);
        }
    }
}

fn enable_turbo_and_set_ratio() {
    // SAFETY: IA32_PERF_CTL, PLATFORM_INFO, and TURBO_RATIO_LIMIT are defined
    // for the target CPU family.
    unsafe {
        let turbo_ratio = rdmsr(MSR_TURBO_RATIO_LIMIT) & 0xff;
        let ratio = if turbo_ratio != 0 {
            turbo_ratio
        } else {
            (rdmsr(MSR_PLATFORM_INFO) >> 8) & 0xff
        };
        if ratio != 0 {
            wrmsr(IA32_PERF_CTL, ratio << 8);
        }
    }
}

/// CPU operations for Intel Sandy Bridge/Ivy Bridge client CPUs.
pub struct SandybridgeCpuOps {
    microcode: Option<&'static [u8]>,
}

impl SandybridgeCpuOps {
    /// Create CPU ops with an optional concatenated Intel microcode blob.
    pub fn with_microcode(microcode: Option<&'static [u8]>) -> Self {
        Self { microcode }
    }
}

impl CpuOps for SandybridgeCpuOps {
    const NAME: &'static str = "Intel Sandy Bridge/Ivy Bridge (206ax/306ax)";

    fn pre_mp_init(&self) {
        let (eax, ebx, ecx, edx) = cpuid(1);
        fstart_log::info!(
            "cpu: cpuid.1 eax={:#x} ebx={:#x} ecx={:#x} edx={:#x}",
            eax,
            ebx,
            ecx,
            edx
        );
        let (_, threads_cores, _, _) = cpuid_count(0x0b, 0);
        fstart_log::info!("cpu: x2APIC topology leaf ebx={:#x}", threads_cores);
    }

    fn init_cpu(&self) {
        // SAFETY: MP init runs this on every active logical CPU. All CPUs
        // receive the same low-DRAM WB MTRR layout before OS handoff.
        unsafe { mtrr::setup_ram_wb() };
        configure_c_states();
        configure_misc();
        enable_turbo_and_set_ratio();
        fstart_log::info!("cpu: Sandy Bridge MSR configuration complete");
    }

    fn post_mp_init(&self) {
        fstart_log::info!("cpu: Sandy Bridge post-MP init complete");
    }

    fn microcode(&self) -> Option<(&[u8], bool)> {
        // coreboot returns `parallel = !intel_ht_supported()` for model_206ax.
        self.microcode.map(|blob| (blob, !ht_supported()))
    }
}
