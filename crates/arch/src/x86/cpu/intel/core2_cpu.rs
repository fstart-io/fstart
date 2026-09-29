//! Intel Core/Core 2 CPU operations for GM965-era systems.
//!
//! Mirrors the per-CPU MSR setup in coreboot's `cpu/intel/model_6fx` driver
//! and optionally supplies an Intel microcode blob to [`crate::x86::mp`].

use crate::x86::cpu::intel::smm::{SmmCpu, SmrrPair, X86SaveStateFormat};
use crate::x86::cpu::intel::{common_power, feature_control};
use crate::x86::mp::{CpuDriver, CpuIdMatch, CpuIdentity, CpuVendor};
use crate::x86::msr::{rdmsr, wrmsr};
use crate::x86::mtrr;

const IA32_PECI_CTL: u32 = 0x5a0;
const IA32_PLATFORM_ID: u32 = 0x17;
const IA32_PERF_STATUS: u32 = 0x198;
const IA32_PERF_CTL: u32 = 0x199;
const PIC_SENS_CFG: u32 = 0x1aa;

fn configure_misc() {
    // SAFETY: these MSRs are defined for Intel Core/Core 2 CPUs.
    unsafe {
        common_power::configure_misc(
            common_power::MISC_ENABLE::C2E::SET.value
                | common_power::MISC_ENABLE::C4E::SET.value
                | common_power::MISC_ENABLE::HARD_C4E::SET.value
                | common_power::MISC_ENABLE::EMTTM::SET.value,
        );

        let status = rdmsr(IA32_PERF_STATUS);
        let busratio_max = (status >> 40) & 0x1f;
        let platform = rdmsr(IA32_PLATFORM_ID);
        let vid_max = platform & 0x3f;
        let mut perf_ctl = status & !0xffff;
        perf_ctl |= busratio_max << 8;
        perf_ctl |= vid_max;
        wrmsr(IA32_PERF_CTL, perf_ctl);

        let mut peci = rdmsr(IA32_PECI_CTL);
        peci |= 1;
        wrmsr(IA32_PECI_CTL, peci);
    }
}

fn configure_pic_thermal_sensors() {
    // SAFETY: PIC_SENS_CFG is defined for this CPU family.
    unsafe {
        let mut msr = rdmsr(PIC_SENS_CFG);
        msr |= 1 << 21; // inter-core lock TM1
        msr |= 1 << 4; // enable bypass filter
        wrmsr(PIC_SENS_CFG, msr);
    }
}

static CORE2_IDS: &[CpuIdMatch] = &[
    CpuIdMatch {
        vendor: CpuVendor::Intel,
        signature: 0x06f0,
        mask: CpuIdMatch::EXACT_MASK,
    },
    CpuIdMatch {
        vendor: CpuVendor::Intel,
        signature: 0x06f2,
        mask: CpuIdMatch::EXACT_MASK,
    },
    CpuIdMatch {
        vendor: CpuVendor::Intel,
        signature: 0x06f6,
        mask: CpuIdMatch::EXACT_MASK,
    },
    CpuIdMatch {
        vendor: CpuVendor::Intel,
        signature: 0x06f7,
        mask: CpuIdMatch::EXACT_MASK,
    },
    CpuIdMatch {
        vendor: CpuVendor::Intel,
        signature: 0x06fa,
        mask: CpuIdMatch::EXACT_MASK,
    },
    CpuIdMatch {
        vendor: CpuVendor::Intel,
        signature: 0x06fb,
        mask: CpuIdMatch::EXACT_MASK,
    },
    CpuIdMatch {
        vendor: CpuVendor::Intel,
        signature: 0x06fd,
        mask: CpuIdMatch::EXACT_MASK,
    },
    CpuIdMatch {
        vendor: CpuVendor::Intel,
        signature: 0x10661,
        mask: CpuIdMatch::EXACT_MASK,
    },
];

/// CPU driver for Intel Core/Core 2 family 6 model f/16h systems.
pub struct Core2CpuDriver {
    pmbase: u32,
    microcode: Option<&'static [u8]>,
}

impl Core2CpuDriver {
    /// Create Core 2 CPU ops with the southbridge PMBASE and optional ucode.
    pub fn new(pmbase: u32, microcode: Option<&'static [u8]>) -> Self {
        Self { pmbase, microcode }
    }
}

/// Model 0Fh (Merom/Conroe) has the alternative SMRR pair; model 16h
/// (Merom-L) the architectural one. Same split as coreboot's
/// `cpu_has_alternative_smrr()`.
fn smrr_pair_for(identity: CpuIdentity) -> SmrrPair {
    if identity.model() == 0x0f {
        SmrrPair::Core2Alternative
    } else {
        SmrrPair::Architectural
    }
}

impl SmmCpu for Core2CpuDriver {
    fn smm_save_state_format(&self) -> X86SaveStateFormat {
        X86SaveStateFormat::IntelEm64t
    }

    /// Assumes every package in the system is the same model as the BSP.
    fn smrr_pair(&self) -> Option<SmrrPair> {
        Some(smrr_pair_for(CpuIdentity::current()))
    }
}

impl CpuDriver for Core2CpuDriver {
    fn name(&self) -> &'static str {
        "Intel Core/Core 2"
    }

    fn id_table(&self) -> &'static [CpuIdMatch] {
        CORE2_IDS
    }

    fn update_microcode(&self) {
        if let Some(blob) = self.microcode {
            let cpu = crate::x86::mp::current_cpu_index();
            let before = crate::x86::cpu::intel::microcode::current_revision();
            fstart_log::info!("microcode: cpu{} before rev={:#x}", cpu, before);
            // SAFETY: board code supplies a firmware-image-backed Intel
            // microcode blob that remains reachable throughout MP init.
            unsafe { crate::x86::cpu::intel::microcode::update_current_cpu_logged(blob) };
            let after = crate::x86::cpu::intel::microcode::current_revision();
            fstart_log::info!("microcode: cpu{} after rev={:#x}", cpu, after);
        }
    }

    fn init_cpu(&self) {
        // SAFETY: MP init runs this on every active logical CPU. All CPUs
        // receive the same low-DRAM WB MTRR layout before OS handoff.
        unsafe { mtrr::setup_ram_wb() };
        // SAFETY: this CPU model implements these power-management MSRs.
        unsafe {
            common_power::configure_c_states(
                self.pmbase,
                common_power::CST::DEEPER_SLEEP::SET.value
                    | common_power::CST::DYNAMIC_L2::SET.value,
            );
        }
        configure_misc();
        configure_pic_thermal_sensors();
        let smrr = smrr_pair_for(CpuIdentity::current()).feature_control_bits();
        // SAFETY: Core/Core 2 CPUs implement IA32_FEATURE_CONTROL, and
        // `feature_control_bits` only names bits this model has.
        unsafe { feature_control::enable_and_lock(smrr) };
        fstart_log::info!("cpu: Core 2 MSR configuration complete");
    }
}
