//! [`CpuOps`] implementation for Intel Atom Pineview (model 106cx).
//!
//! The Pineview is a single- or dual-core Atom with up to 4 threads
//! (D510 = 2C/4T).  Per-CPU configuration involves:
//!
//! - C-state configuration (C3 support via `MSR_PKG_CST_CONFIG_CONTROL`)
//! - SpeedStep / EIST (via `IA32_MISC_ENABLE`)
//! - Thermal monitoring (TM1/TM2 via `IA32_MISC_ENABLE`)
//!
//! This matches coreboot's `cpu/intel/model_106cx/model_106cx_init.c`.

#![no_std]

use fstart_arch::msr::{rdmsr, wrmsr};
use fstart_arch::mtrr;
use fstart_mp::CpuOps;

// ---------------------------------------------------------------------------
// MSR indices
// ---------------------------------------------------------------------------

/// Package C-state configuration control.
const MSR_PKG_CST_CONFIG_CONTROL: u32 = 0xE2;
/// Processor MWAIT IO base address.
const MSR_PMG_IO_BASE_ADDR: u32 = 0xE4;
/// C-state latency control (IO capture address).
const MSR_PMG_IO_CAPTURE_ADDR: u32 = 0xE7;
/// Miscellaneous feature enable.
const IA32_MISC_ENABLE: u32 = 0x1A0;

/// Maximum supported C-state level.
const HIGHEST_CLEVEL: u32 = 3;

// ---------------------------------------------------------------------------
// Per-CPU configuration functions
// ---------------------------------------------------------------------------

/// Configure C-state support.
///
/// Sets up `MSR_PKG_CST_CONFIG_CONTROL` for C3 support with I/O-based
/// C-state transitions redirected to MWAIT, and configures the MWAIT
/// I/O base/capture addresses.
///
/// Matches coreboot `configure_c_states()` in model_106cx_init.c.
fn configure_c_states(pmbase: u32) {
    // SAFETY: these MSRs are architecturally defined for Atom Pineview.
    unsafe {
        let mut cst = rdmsr(MSR_PKG_CST_CONFIG_CONTROL);

        cst |= 1 << 15; // Lock configuration
        cst |= 1 << 10; // Redirect IO-based CState transitions to MWAIT
        cst &= !(1 << 9); // Single stop grant cycle on stpclk
        cst = (cst & !7) | (HIGHEST_CLEVEL as u64); // Support C3

        wrmsr(MSR_PKG_CST_CONFIG_CONTROL, cst);

        // MWAIT IO base address (P_BLK = PMBASE + 4).
        let io_base = ((pmbase + 4) & 0xFFFF) as u64;
        wrmsr(MSR_PMG_IO_BASE_ADDR, io_base);

        // C-level controls: IO port + (highest_clevel - 2) in bits [18:16].
        let io_capture = ((pmbase + 4) as u64) | (((HIGHEST_CLEVEL - 2) as u64) << 16);
        wrmsr(MSR_PMG_IO_CAPTURE_ADDR, io_capture);
    }
}

/// Configure Enhanced SpeedStep and thermal monitoring.
///
/// Enables TM1, TM2, bidirectional PROCHOT#, FERR# multiplexing,
/// and Enhanced SpeedStep (EIST).  Locks EIST enable.
///
/// Matches coreboot `configure_misc()` in model_106cx_init.c.
fn configure_misc() {
    // SAFETY: IA32_MISC_ENABLE is architecturally defined for this CPU.
    unsafe {
        let mut misc = rdmsr(IA32_MISC_ENABLE);

        misc |= 1 << 3; // TM1 enable
        misc |= 1 << 13; // TM2 enable
        misc |= 1 << 17; // Bidirectional PROCHOT#
        misc |= 1 << 10; // FERR# multiplexing
        misc |= 1 << 16; // Enhanced SpeedStep enable

        wrmsr(IA32_MISC_ENABLE, misc);

        // Lock EIST enable.
        misc |= 1 << 20;
        wrmsr(IA32_MISC_ENABLE, misc);
    }
}

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
// CpuOps implementation
// ---------------------------------------------------------------------------

/// CPU operations for Intel Atom Pineview (family 6, model 1Ch/26h).
///
/// Configures C-states, Enhanced SpeedStep, and thermal monitoring
/// on every logical CPU during MP initialization.
pub struct PineviewCpuOps {
    /// PM base I/O port (programmed by the ICH7 southbridge).
    pmbase: u32,
}

impl PineviewCpuOps {
    /// Create with the southbridge's PM base I/O address.
    pub fn new(pmbase: u32) -> Self {
        Self { pmbase }
    }
}

impl CpuOps for PineviewCpuOps {
    const NAME: &'static str = "Intel Atom Pineview (106cx)";

    fn init_cpu(&self) {
        // SAFETY: MP init runs this on every active logical CPU.  All CPUs
        // receive the same low-DRAM WB MTRR layout before OS handoff.
        unsafe { mtrr::setup_low_1g_wb() };
        log_mtrr_solution("per-CPU ramstage layout");
        configure_c_states(self.pmbase);
        configure_misc();
        fstart_log::info!("cpu: Pineview MSR configuration complete");
    }

    fn pre_mp_init(&self) {
        fstart_log::info!("cpu: Pineview pre-MP init (BSP ROM MTRR)");
        // SAFETY: this runs on the BSP only.  The ROM WP MTRR speeds reads
        // from memory-mapped SPI flash during signature verification and
        // payload loading.  It is cleared before jumping to Linux.
        unsafe { mtrr::set_boot_rom_wp(true) };
        log_mtrr_solution("BSP temporary ROM WP enabled");
    }

    fn post_mp_init(&self) {
        fstart_log::info!("cpu: Pineview post-MP init complete");
    }
}
