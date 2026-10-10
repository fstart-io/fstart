//! Intel Core/Core 2 CPU operations for mobile and LGA775 systems.
//!
//! Mirrors the per-CPU MSR setup in coreboot's `cpu/intel/model_6ex` (Core
//! Solo/Duo, Yonah) and `model_6fx` (Core 2) drivers and optionally supplies
//! an Intel microcode blob to [`crate::x86::mp`].

use super::msr_register::Msr;
use crate::x86::cpu::intel::smm::{SmmCpu, SmrrPair, X86SaveStateFormat};
use crate::x86::cpu::intel::{common_power, feature_control};
use crate::x86::mp::{CpuDriver, CpuIdMatch, CpuIdentity, CpuVendor};
use crate::x86::msr::{rdmsr, wrmsr};
use tock_registers::{LocalRegisterCopy, register_bitfields};

register_bitfields![u64,
    FSB_SELECTION [
        CODE OFFSET(0) NUMBITS(3) [],
        BSEL0 OFFSET(0) NUMBITS(1) [],
        BSEL1 OFFSET(1) NUMBITS(1) [],
        BSEL2 OFFSET(2) NUMBITS(1) []
    ],
    PECI_CONTROL [ ENABLE OFFSET(0) NUMBITS(1) [] ]
];

/// CPU-requested front-side-bus strap levels, independent of board wiring.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FsbBusSelect {
    pub bsel0_high: bool,
    pub bsel1_high: bool,
    pub bsel2_high: bool,
}

fn bus_select_from_msr(value: u64) -> Option<FsbBusSelect> {
    let value = LocalRegisterCopy::<u64, FSB_SELECTION::Register>::new(value);
    (value.read(FSB_SELECTION::CODE) != 7).then(|| FsbBusSelect {
        bsel0_high: value.is_set(FSB_SELECTION::BSEL0),
        bsel1_high: value.is_set(FSB_SELECTION::BSEL1),
        bsel2_high: value.is_set(FSB_SELECTION::BSEL2),
    })
}

const IA32_PECI_CTL: u32 = 0x5a0;
const IA32_PLATFORM_ID: u32 = 0x17;
const IA32_PERF_STATUS: u32 = 0x198;
const IA32_PERF_CTL: u32 = 0x199;
const PIC_SENS_CFG: u32 = 0x1aa;

/// Core Solo/Duo (Yonah, model 0Eh) lacks Deeper Sleep, EMTTM, PECI, SMRR
/// and the VMX feature-control lock that model 0Fh/16h receive.
fn is_yonah(identity: CpuIdentity) -> bool {
    identity.model() == 0x0e
}

fn configure_misc(yonah: bool) {
    let emttm = if yonah {
        0
    } else {
        common_power::MISC_ENABLE::EMTTM::SET.value
    };
    // SAFETY: these MSRs are defined for Intel Core/Core 2 CPUs.
    unsafe {
        common_power::configure_misc(
            common_power::MISC_ENABLE::C2E::SET.value
                | common_power::MISC_ENABLE::C4E::SET.value
                | common_power::MISC_ENABLE::HARD_C4E::SET.value
                | emttm,
        );

        let status = rdmsr(IA32_PERF_STATUS);
        let busratio_max = (status >> 40) & 0x1f;
        let platform = rdmsr(IA32_PLATFORM_ID);
        let vid_max = platform & 0x3f;
        let mut perf_ctl = status & !0xffff;
        perf_ctl |= busratio_max << 8;
        perf_ctl |= vid_max;
        wrmsr(IA32_PERF_CTL, perf_ctl);

        if yonah {
            return;
        }
        configure_peci();
    }
}

fn configure_peci() {
    let peci = Msr::<PECI_CONTROL::Register>::new(IA32_PECI_CTL);
    // SAFETY: model-6FX implements IA32_PECI_CTL (coreboot model_6fx).
    unsafe {
        let mut value = peci.read();
        value.modify(PECI_CONTROL::ENABLE::SET);
        peci.write(value.get());
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
        signature: 0x06e0,
        mask: CpuIdMatch::EXACT_MASK,
    },
    CpuIdMatch {
        vendor: CpuVendor::Intel,
        signature: 0x06e8,
        mask: CpuIdMatch::EXACT_MASK,
    },
    CpuIdMatch {
        vendor: CpuVendor::Intel,
        signature: 0x06ec,
        mask: CpuIdMatch::EXACT_MASK,
    },
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

/// CPU driver for Intel Core/Core 2 family 6 model e/f/16h systems.
pub struct Core2CpuDriver {
    pmbase: u32,
    microcode: Option<&'static [u8]>,
    desktop: bool,
}

impl Core2CpuDriver {
    /// Create Core 2 CPU ops with the southbridge PMBASE and optional ucode.
    pub fn new(pmbase: u32, microcode: Option<&'static [u8]>) -> Self {
        Self {
            pmbase,
            microcode,
            desktop: false,
        }
    }

    /// Desktop model-6FX initialization, without mobile C-state or VID policy.
    pub fn new_desktop(pmbase: u32, microcode: Option<&'static [u8]>) -> Self {
        Self {
            pmbase,
            microcode,
            desktop: true,
        }
    }

    /// Read BSEL only for supported Core 2 models; never probe a foreign MSR.
    /// Netburst and Enhanced Core model 17h require their own CPU drivers.
    pub fn bus_select() -> Option<FsbBusSelect> {
        let identity = CpuIdentity::current();
        if !matches!(identity.model(), 0x0f | 0x16)
            || !CORE2_IDS.iter().any(|entry| entry.matches(identity))
        {
            return None;
        }
        // SAFETY: MSR_FSB_FREQ exists on the selected model-6FX CPUs.
        bus_select_from_msr(
            unsafe { Msr::<FSB_SELECTION::Register>::new(crate::x86::MSR_FSB_FREQ).read() }.get(),
        )
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

// Both assume every package in the system is the same model as the BSP.
impl SmmCpu for Core2CpuDriver {
    /// Yonah has no EM64T and writes the legacy 32-bit save state.
    fn smm_save_state_format(&self) -> X86SaveStateFormat {
        if is_yonah(CpuIdentity::current()) {
            X86SaveStateFormat::IntelLegacy
        } else {
            X86SaveStateFormat::IntelEm64t
        }
    }

    fn smrr_pair(&self) -> Option<SmrrPair> {
        let identity = CpuIdentity::current();
        (!is_yonah(identity)).then(|| smrr_pair_for(identity))
    }
}

impl CpuDriver for Core2CpuDriver {
    fn name(&self) -> &'static str {
        "Intel Core/Core 2"
    }

    fn id_table(&self) -> &'static [CpuIdMatch] {
        if self.desktop {
            // The first three entries are Yonah, which cannot use LGA775.
            &CORE2_IDS[3..]
        } else {
            CORE2_IDS
        }
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
        let identity = CpuIdentity::current();
        let yonah = is_yonah(identity);
        let deeper_sleep = if yonah {
            0
        } else {
            common_power::CST::DEEPER_SLEEP::SET.value
        };
        if self.desktop {
            let (_, _, features, _) = crate::x86::cpuid(1);
            // SAFETY: the desktop ID table contains only model-6FX CPUs.
            // Do not request mobile C-states or overwrite CPU VID/ratio.
            unsafe { common_power::configure_desktop_misc(features) };
            configure_peci();
        } else {
            // SAFETY: these mobile models implement the power-management MSRs.
            unsafe {
                common_power::configure_c_states(
                    self.pmbase,
                    deeper_sleep | common_power::CST::DYNAMIC_L2::SET.value,
                );
            }
            configure_misc(yonah);
        }
        configure_pic_thermal_sensors();
        if !yonah {
            let smrr = smrr_pair_for(identity).feature_control_bits();
            // SAFETY: this is Intel; `feature_control_bits` checks SMRR
            // capability. The helper gates the MSR read on VMX/SMX or SMRR.
            unsafe { feature_control::enable_and_lock(smrr) };
        }
        fstart_log::info!("cpu: Core/Core 2 MSR configuration complete");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fsb_selection_ignores_unrelated_msr_fields_and_rejects_reserved_code() {
        for code in 0..7 {
            assert_eq!(
                bus_select_from_msr(0xffff_ffff_ffff_fff8 | code),
                Some(FsbBusSelect {
                    bsel0_high: code & 1 != 0,
                    bsel1_high: code & 2 != 0,
                    bsel2_high: code & 4 != 0,
                })
            );
        }
        assert_eq!(bus_select_from_msr(7), None);
    }

    #[test]
    fn desktop_cpu_selection_excludes_mobile_yonah_and_unimplemented_families() {
        let driver = Core2CpuDriver::new_desktop(0x500, None);
        let accepts = |signature| {
            driver.id_table().iter().any(|entry| {
                entry.matches(CpuIdentity {
                    vendor: CpuVendor::Intel,
                    signature,
                })
            })
        };
        assert!(accepts(0x6f6));
        assert!(accepts(0x10661));
        assert!(!accepts(0x6e8));
        assert!(!accepts(0xf41));
        assert!(!accepts(0x10676));
        assert!(driver.id_table().iter().all(|entry| matches!(CpuIdentity {
            vendor: entry.vendor,
            signature: entry.signature,
        }.model(), 0x0f | 0x16)));
    }
}
