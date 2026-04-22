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

/// PM base I/O port (programmed by ICH7 southbridge).
const PMB0_BASE: u32 = 0x0500;

/// Maximum supported C-state level.
const HIGHEST_CLEVEL: u32 = 3;

// ---------------------------------------------------------------------------
// MSR helpers
// ---------------------------------------------------------------------------

/// Read a 64-bit MSR.
///
/// # Safety
///
/// Caller must ensure `msr` is a valid MSR for this CPU.
#[inline]
unsafe fn rdmsr(msr: u32) -> u64 {
    let lo: u32;
    let hi: u32;
    core::arch::asm!(
        "rdmsr",
        in("ecx") msr,
        out("eax") lo,
        out("edx") hi,
        options(nomem, nostack),
    );
    ((hi as u64) << 32) | (lo as u64)
}

/// Write a 64-bit MSR.
///
/// # Safety
///
/// Caller must ensure `msr` and `val` are valid for this CPU.
#[inline]
unsafe fn wrmsr(msr: u32, val: u64) {
    let lo = val as u32;
    let hi = (val >> 32) as u32;
    core::arch::asm!(
        "wrmsr",
        in("ecx") msr,
        in("eax") lo,
        in("edx") hi,
        options(nomem, nostack),
    );
}

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
fn configure_c_states() {
    // SAFETY: these MSRs are architecturally defined for Atom Pineview.
    unsafe {
        let mut cst = rdmsr(MSR_PKG_CST_CONFIG_CONTROL);

        cst |= 1 << 15; // Lock configuration
        cst |= 1 << 10; // Redirect IO-based CState transitions to MWAIT
        cst &= !(1 << 9); // Single stop grant cycle on stpclk
        cst = (cst & !7) | (HIGHEST_CLEVEL as u64); // Support C3

        wrmsr(MSR_PKG_CST_CONFIG_CONTROL, cst);

        // MWAIT IO base address (P_BLK).
        let io_base = ((PMB0_BASE + 4) & 0xFFFF) as u64;
        wrmsr(MSR_PMG_IO_BASE_ADDR, io_base);

        // C-level controls: IO port + (highest_clevel - 2) in bits [18:16].
        let io_capture = ((PMB0_BASE + 4) as u64) | (((HIGHEST_CLEVEL - 2) as u64) << 16);
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

// ---------------------------------------------------------------------------
// CpuOps implementation
// ---------------------------------------------------------------------------

/// CPU operations for Intel Atom Pineview (family 6, model 1Ch/26h).
///
/// Configures C-states, Enhanced SpeedStep, and thermal monitoring
/// on every logical CPU during MP initialization.
pub struct PineviewCpuOps;

impl CpuOps for PineviewCpuOps {
    const NAME: &'static str = "Intel Atom Pineview (106cx)";

    fn init_cpu(&self) {
        configure_c_states();
        configure_misc();
        fstart_log::info!("cpu: Pineview MSR configuration complete");
    }

    fn pre_mp_init(&self) {
        fstart_log::info!("cpu: Pineview pre-MP init (MTRR setup)");
        // Future: x86_setup_mtrrs_with_detect(), microcode load.
        // For now, MTRRs are configured by CAR setup and the BSP
        // mirrors them to APs via the SIPI parameter block.
    }

    fn post_mp_init(&self) {
        fstart_log::info!("cpu: Pineview post-MP init complete");
    }
}
