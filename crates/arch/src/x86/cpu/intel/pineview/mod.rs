//! Intel Atom Pineview CPU support.
//!
//! The Pineview is a single- or dual-core Atom with up to 4 threads
//! (D510 = 2C/4T).  Per-CPU configuration involves:
//!
//! - C-state configuration (C3 support via `MSR_PKG_CST_CONFIG_CONTROL`)
//! - SpeedStep / EIST (via `IA32_MISC_ENABLE`)
//! - Thermal monitoring (TM1/TM2 via `IA32_MISC_ENABLE`)
//!
//! This matches coreboot's `cpu/intel/model_106cx/model_106cx_init.c`.

use crate::x86::cpu::intel::smm::{SmmCpu, SmrrPair, X86SaveStateFormat};
use crate::x86::cpu::intel::{common_power, feature_control};
use crate::x86::mp::{CpuDriver, CpuIdMatch, CpuVendor};
use crate::x86::mtrr;

fn mtrr_type_name(ty: u64) -> &'static str {
    match ty {
        0x00 => "UC",
        0x01 => "WC",
        0x04 => "WT",
        0x05 => "WP",
        0x06 => "WB",
        _ => "unknown",
    }
}

fn log_variable_mtrr(index: u32) {
    // SAFETY: `index` is bounded by IA32_MTRR_CAP.VCNT in the caller.
    let (base_raw, mask_raw) = unsafe { mtrr::read_variable(index) };
    if !mtrr::is_valid_mask(mask_raw) {
        return;
    }

    let ty = mtrr::decode_type(base_raw);
    fstart_log::info!(
        "mtrr{}: base={:#x} size={:#x} type={} ({}) base_msr={:#x} mask_msr={:#x}",
        index,
        mtrr::decode_base(base_raw),
        mtrr::decode_size(mask_raw),
        ty,
        mtrr_type_name(ty),
        base_raw,
        mask_raw
    );
}

fn log_mtrr_solution(label: &str) {
    // SAFETY: reading IA32_MTRR_CAP is valid on Pineview.
    let count = unsafe { mtrr::variable_count() };
    let fixed = unsafe { mtrr::fixed_supported() };
    fstart_log::info!(
        "mtrr solution: {} (variable_count={} fixed_supported={})",
        label,
        count,
        fixed
    );
    if fixed {
        fstart_log::info!("fixed mtrr: 0x00000-0x9ffff WB, 0xa0000-0xfffff UC");
    }
    for index in 0..count {
        log_variable_mtrr(index);
    }
}

// ---------------------------------------------------------------------------
// CpuDriver implementation
// ---------------------------------------------------------------------------

static PINEVIEW_IDS: &[CpuIdMatch] = &[
    CpuIdMatch {
        vendor: CpuVendor::Intel,
        signature: 0x106c0,
        mask: CpuIdMatch::EXACT_MASK,
    },
    // Atom 230 stepping 2 (D945GCLF Socket 441 reports 0x106c2).
    CpuIdMatch {
        vendor: CpuVendor::Intel,
        signature: 0x106c2,
        mask: CpuIdMatch::EXACT_MASK,
    },
    CpuIdMatch {
        vendor: CpuVendor::Intel,
        signature: 0x106ca,
        mask: CpuIdMatch::EXACT_MASK,
    },
];

/// CPU driver for Intel Atom Pineview (family 6, model 1Ch/26h).
///
/// Configures C-states, Enhanced SpeedStep, and thermal monitoring
/// on every logical CPU during MP initialization.
pub struct PineviewCpuDriver {
    /// PM base I/O port (programmed by the ICH7 southbridge).
    pmbase: u32,
    /// Optional concatenated Intel microcode blob packaged in FFS.
    microcode: Option<&'static [u8]>,
}

impl PineviewCpuDriver {
    /// Create with the southbridge's PM base I/O address and optional microcode blob.
    pub fn new(pmbase: u32, microcode: Option<&'static [u8]>) -> Self {
        Self { pmbase, microcode }
    }
}

impl SmmCpu for PineviewCpuDriver {
    fn smm_save_state_format(&self) -> X86SaveStateFormat {
        X86SaveStateFormat::IntelEm64t
    }

    /// Model 1Ch Atoms use the alternative SMRR pair (coreboot `model_106cx`).
    fn smrr_pair(&self) -> Option<SmrrPair> {
        Some(SmrrPair::Core2Alternative)
    }
}

impl CpuDriver for PineviewCpuDriver {
    fn name(&self) -> &'static str {
        "Intel Atom Pineview (106cx)"
    }

    fn id_table(&self) -> &'static [CpuIdMatch] {
        PINEVIEW_IDS
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
        // SAFETY: MP init runs this on every active logical CPU.  All CPUs
        // receive the same low-DRAM WB MTRR layout before OS handoff.
        unsafe { mtrr::setup_ram_wb() };
        log_mtrr_solution("per-CPU ramstage layout");
        // SAFETY: Pineview implements these MSRs; no Core 2-only bits are set.
        unsafe {
            common_power::configure_c_states(self.pmbase, 0);
            common_power::configure_misc(0);
        }
        let smrr = SmrrPair::Core2Alternative.feature_control_bits();
        // SAFETY: model 1Ch Atoms implement IA32_FEATURE_CONTROL, and
        // `feature_control_bits` only names bits this model has.
        unsafe { feature_control::enable_and_lock(smrr) };
        fstart_log::info!("cpu: Pineview MSR configuration complete");
    }

    fn post_mp_init(&self) {
        fstart_log::info!("cpu: Pineview post-MP init complete");
    }
}
