//! Pineview DDR2 raminit — ported from coreboot `raminit.c`.
//!
//! This module implements the complete DDR2 SDRAM initialization sequence
//! for the Intel Atom D4xx/D5xx (Pineview) memory controller. The code
//! is a line-by-line port of coreboot's ~2600-line `raminit.c`, adapted
//! to Rust idioms and the fstart register access model.
//!
//! ## Boot paths
//!
//! - **Normal**: full SPD read → timing selection → PHY training.
//! - **Reset**: skip DLL timing and RCOMP (already calibrated).
//! - **Resume (S3)**: skip JEDEC init and some calibration.
//!
//! ## Entry point
//!
//! [`sdram_initialize`] is called from `IntelPineview::init()` with the
//! MCHBAR accessor, ECAM handle, and SPD addresses.

mod jedec;
mod mmap;
mod phy;
mod spd;
mod timing;

use fstart_pineview_regs::{mchbar, EcamPci, MchBar};
use fstart_services::ServiceError;
use fstart_spd::DimmInfo;

// ===================================================================
// Constants
// ===================================================================

pub const TOTAL_CHANNELS: usize = 1;
pub const TOTAL_DIMMS: usize = 2;
pub const RANKS_PER_CHANNEL: usize = 4;

const BOOT_PATH_NORMAL: u8 = 0;
const BOOT_PATH_RESET: u8 = 1;
const BOOT_PATH_RESUME: u8 = 2;

// ===================================================================
// Sysinfo — raminit state
// ===================================================================

/// Selected memory timings (in clock cycles).
#[derive(Debug, Default, Clone, Copy)]
pub struct Timings {
    pub cas: u8,
    pub fsb_clock: u8,
    pub mem_clock: u8,
    pub tras: u8,
    pub trp: u8,
    pub trcd: u8,
    pub twr: u8,
    pub trfc: u8,
    pub twtr: u8,
    pub trrd: u8,
    pub trtp: u8,
}

/// PLL parameters for DQS/DQ calibration.
#[derive(Debug, Clone)]
pub struct PllParam {
    pub kcoarse: [[u8; 72]; 2],
    pub pi: [[u8; 72]; 2],
    pub dben: [[u8; 72]; 2],
    pub dbsel: [[u8; 72]; 2],
    pub clkdelay: [[u8; 72]; 2],
}

impl Default for PllParam {
    fn default() -> Self {
        Self {
            kcoarse: [[0; 72]; 2],
            pi: [[0; 72]; 2],
            dben: [[0; 72]; 2],
            dbsel: [[0; 72]; 2],
            clkdelay: [[0; 72]; 2],
        }
    }
}

/// Complete raminit state, analogous to coreboot's `struct sysinfo`.
#[derive(Debug)]
pub struct SysInfo {
    pub boot_path: u8,
    pub spd_map: [u8; 4],
    pub dimms: [Option<DimmInfo>; TOTAL_DIMMS * TOTAL_CHANNELS],
    pub dimm_config: [u8; TOTAL_CHANNELS],
    pub spd_type: u8,
    pub selected_timings: Timings,
    pub channel_capacity: [u32; TOTAL_CHANNELS],

    // DLL / calibration state
    pub maxpi: u8,
    pub pioffset: u8,
    pub pi: [u8; 8],
    pub coarsectrl: u16,
    pub coarsedelay: u16,
    pub mediumphase: u16,
    pub readptrdelay: u16,
    pub nodll: u8,
    pub r#async: u8,
    pub dt0mode: u8,
    pub ggc: u16,
}

impl SysInfo {
    pub fn new(boot_path: u8, spd_map: [u8; 4]) -> Self {
        Self {
            boot_path,
            spd_map,
            dimms: [None, None],
            dimm_config: [0],
            spd_type: 0,
            selected_timings: Timings::default(),
            channel_capacity: [0],
            maxpi: 0,
            pioffset: 0,
            pi: [0; 8],
            coarsectrl: 0,
            coarsedelay: 0,
            mediumphase: 0,
            readptrdelay: 0,
            nodll: 0,
            r#async: 0,
            dt0mode: 0,
            ggc: 0,
        }
    }

    /// Check if a DIMM slot is populated.
    pub fn dimm_populated(&self, idx: usize) -> bool {
        self.dimms[idx].as_ref().map_or(false, |d| d.card_type != 0)
    }
}

// ===================================================================
// Top-level entry point
// ===================================================================

/// Initialize DDR2 SDRAM.
///
/// This is the main raminit entry point, equivalent to coreboot's
/// `sdram_initialize()`. Called from `IntelPineview::init()`.
///
/// # Arguments
/// * `mch` — MCHBAR MMIO accessor
/// * `ecam` — ECAM PCI config accessor
/// * `smbus` — SMBus controller for SPD reads
/// * `boot_path` — 0 = normal, 1 = reset, 2 = S3 resume
/// * `spd_addresses` — SMBus addresses of DIMM SPD EEPROMs (e.g., [0x50, 0x51, 0, 0])
pub fn sdram_initialize(
    mch: &MchBar,
    ecam: &EcamPci,
    smbus: &mut dyn fstart_services::SmBus,
    boot_path: u8,
    spd_addresses: &[u8; 4],
) -> Result<u64, ServiceError> {
    fstart_log::info!("raminit: starting DDR2 initialization");

    let mut si = SysInfo::new(boot_path, *spd_addresses);

    // 1. Read SPD data from DIMMs.
    spd::read_spds(&mut si, smbus)?;

    // 2. Detect RAM speed (common frequency).
    timing::detect_ram_speed(&mut si, mch, ecam);

    // 3. Detect smallest common timings.
    timing::detect_smallest_params(&mut si);

    // 4. Enable HPET.
    // (Handled by platform code, not raminit.)

    // 5. Clock crossing.
    mch.setbits32(mchbar::CPCTL, 1 << 15);
    timing::clk_crossing(&si, mch);

    // 6. Check for reset.
    timing::check_reset(&si, ecam);

    // 7. Clock mode.
    timing::clkmode(&si, mch);

    // 8. Program timings.
    timing::sdram_timings(&si, mch);

    // 9. DLL timing (skip on reset path).
    if si.boot_path != BOOT_PATH_RESET {
        phy::dll_timing(&mut si, mch);
    }

    // 10. RCOMP (skip on reset path).
    if si.boot_path != BOOT_PATH_RESET {
        phy::rcomp(&si, mch);
    }

    // 11. ODT.
    phy::odt(&si, mch);

    // 12. Wait for RCOMP completion (skip on reset path).
    if si.boot_path != BOOT_PATH_RESET {
        let mut timeout = 1_000_000u32;
        while (mch.read8(mchbar::COMPCTRL1) & 1) != 0 {
            timeout -= 1;
            if timeout == 0 {
                fstart_log::error!("raminit: RCOMP timeout");
                return Err(ServiceError::Timeout);
            }
            core::hint::spin_loop();
        }
    }

    // 13. Memory map.
    mmap::sdram_mmap(&si, mch);

    // 14. Enable DDR IO buffer.
    let iobuf = mch.read8(mchbar::C0IOBUFACTCTL);
    mch.write8(mchbar::C0IOBUFACTCTL, (iobuf & !0x3F) | 0x08);
    mch.setbits32(mchbar::C0RSTCTL, 1 << 0);

    // 15. RCOMP update.
    phy::rcomp_update(&si, mch);

    mch.setbits32(mchbar::HIT4, 1 << 1);

    // 16. JEDEC init (skip on S3 resume).
    if si.boot_path != BOOT_PATH_RESUME {
        mch.setbits32(mchbar::C0CKECTRL, 1 << 27);
        jedec::jedec_init(&si, mch);
    }

    // 17. Misc.
    jedec::sdram_misc(&si, mch);

    // 18. ZQCL.
    jedec::sdram_zqcl(&si, mch);

    // 19. Refresh control (skip on resume).
    if si.boot_path != BOOT_PATH_RESUME {
        mch.setbits32(mchbar::C0REFRCTRL2, 3 << 30);
    }

    // 20. DRA/DRB.
    mmap::sdram_dradrb(&mut si, mch);

    // 21. Receive enable calibration.
    phy::sdram_rcven(&mut si, mch);

    // 22. New tRD.
    phy::sdram_new_trd(&si, mch);

    // 23. Memory map registers.
    mmap::sdram_mmap_regs(&si, mch, ecam);

    // 24. Enhanced mode.
    phy::sdram_enhanced_mode(&si, mch, ecam);

    // 25. Power settings.
    phy::sdram_power_settings(&si, mch);

    // 26. Program DDR.
    phy::sdram_program_ddr(mch);

    // 27. Program DQDQS.
    phy::sdram_program_dqdqs(&si, mch);

    // 28. Periodic RCOMP.
    phy::sdram_periodic_rcomp(mch);

    // 29. Set init done.
    mch.setbits32(mchbar::C0REFRCTRL2, 1 << 30);

    // 30. Tell ICH7 and northbridge we're done.
    ecam.and8(0, 0x1f, 0, 0xA2, !(1 << 7));
    ecam.or8(0, 0, 0, 0xF4, 1);

    // Compute total DRAM size from channel capacity.
    let total_mb = si.channel_capacity.iter().sum::<u32>();
    let total_bytes = (total_mb as u64) * 1024 * 1024;

    fstart_log::info!(
        "raminit: DDR2 initialization complete, {} MiB detected",
        total_mb
    );
    Ok(total_bytes)
}
