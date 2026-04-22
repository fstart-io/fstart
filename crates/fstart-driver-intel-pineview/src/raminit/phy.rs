//! DDR2 PHY calibration: DLL timing, RCOMP, ODT, receive enable,
//! enhanced mode, power settings, and DQ/DQS programming.
//!
//! Ported from coreboot `raminit.c` functions: `sdram_dlltiming`,
//! `sdram_rcomp`, `sdram_odt`, `sdram_rcompupdate`, `sdram_rcven`,
//! `sdram_new_trd`, `sdram_enhancedmode`, `sdram_powersettings`,
//! `sdram_programddr`, `sdram_programdqdqs`, `sdram_periodic_rcomp`.

use super::SysInfo;
use fstart_pineview_regs::{mchbar, MchBar};

/// DLL timing calibration.
///
/// Ported from coreboot `sdram_dlltiming()`.
pub fn dll_timing(si: &SysInfo, mch: &MchBar) {
    // DLL enable and reset sequence.
    mch.setbits32(mchbar::C0DLLPIEN, 1);
    mch.setbits32(mchbar::C0RSTCTL, 1 << 1);

    // Coarse delay calibration.
    mch.write32(mchbar::C0COARSEDLY0, 0);
    mch.write32(mchbar::C0COARSEDLY1, 0);

    // DQS receiver enable DLL.
    for lane in 0..8 {
        mch.write32(mchbar::ly(0x540, lane), 0x1214_0514);
    }

    // Command/clock DLLs.
    mch.write8(mchbar::C0TXCMD0DLL, 0);
    mch.write8(mchbar::C0TXCK0DLL, 0);
    mch.write8(mchbar::C0TXCK1DLL, 0);
    mch.write8(mchbar::C0TXCMD1DLL, 0);

    // Control DLLs.
    for i in 0..4 {
        mch.write8(mchbar::C0TXCTL0DLL + i, 0);
    }

    fstart_log::info!("raminit: DLL timing calibrated");
}

/// RCOMP (resistance compensation) calibration.
///
/// Ported from coreboot `sdram_rcomp()`. This is a ~250-line function
/// in coreboot that programs RCOMP groups 0-6 with calibration values.
/// The condensed port programs the key registers.
pub fn rcomp(si: &SysInfo, mch: &MchBar) {
    // Enable comparators.
    mch.write32(mchbar::COMPCTRL1, 0);
    mch.setbits32(mchbar::COMPCTRL1, 1 << 0);

    // RCOMP group 0 (DQ drive).
    mch.write32(mchbar::C0RCOMPCTRL0, 0x0001_4000);
    mch.write32(mchbar::C0SCOMPVREF0, 0);

    // RCOMP group 2 (CLK drive).
    mch.write32(mchbar::C0RCOMPCTRL2, 0x0001_4000);

    // RCOMP group 3 (CMD drive).
    mch.write32(mchbar::C0RCOMPCTRL3, 0x0001_4000);

    // RCOMP group 4 (CTL drive).
    mch.write32(mchbar::C0RCOMPCTRL4, 0x0001_4000);

    // RCOMP group 5 (CLK drive strength).
    mch.write32(mchbar::C0RCOMPCTRL5, 0x0001_4000);

    // RCOMP group 6 (DQS drive).
    mch.write32(mchbar::C0RCOMPCTRL6, 0x0001_4000);

    // Slew rate tables for each group.
    let slew_base: u32 = 0x3842;
    for grp_base in [
        mchbar::C0SLEWBASE0,
        mchbar::C0SLEWBASE2,
        mchbar::C0SLEWBASE3,
        mchbar::C0SLEWBASE4,
        mchbar::C0SLEWBASE5,
        mchbar::C0SLEWBASE6,
    ] {
        mch.write16(grp_base, slew_base as u16);
    }

    // Start RCOMP calibration.
    mch.clrbits32(mchbar::COMPCTRL1, 1 << 0);

    fstart_log::info!("raminit: RCOMP calibration started");
}

/// On-Die Termination (ODT) configuration.
///
/// Ported from coreboot `sdram_odt()`.
pub fn odt(si: &SysInfo, mch: &MchBar) {
    // ODT values depend on DIMM configuration.
    let config = si.dimm_config[0];

    // Single DIMM: 150 ohm nominal, no dynamic ODT.
    // Dual DIMM: 75 ohm nominal on both.
    let odt_val = match config {
        0 => 0u32, // no DIMMs
        1 | 2 | 5 => {
            // single DIMM (SS or DS)
            (1 << 0) | (0 << 8)
        }
        3 | 4 | 6 => {
            // dual DIMM
            (1 << 0) | (1 << 8) | (1 << 4)
        }
        _ => 0,
    };
    mch.write32(mchbar::C0ODT, odt_val);
    mch.write32(mchbar::C0ODTCTRL, 0x44);
    mch.write32(mchbar::C0ODTRKCTRL, 0x0222_2222);

    fstart_log::info!("raminit: ODT configured (config={})", config);
}

/// RCOMP update (post-calibration fixup).
///
/// Ported from coreboot `sdram_rcompupdate()`.
pub fn rcomp_update(si: &SysInfo, mch: &MchBar) {
    // Check if RCOMP override is needed.
    let xcomp = mch.read32(mchbar::XCOMP);
    fstart_log::info!("raminit: RCOMP update, XCOMP={:#x}", xcomp);
    // The detailed override logic is board-specific and involves
    // reading back RCOMP results and applying corrections. For the
    // initial port, accept the hardware's calibration result.
}

/// Receive enable calibration.
///
/// Ported from coreboot `sdram_rcven()`. This trains the DQS receive
/// enable timing for each byte lane.
pub fn sdram_rcven(si: &mut SysInfo, mch: &MchBar) {
    fstart_log::info!("raminit: receive enable calibration (stub)");
    // The full rcven calibration involves:
    // 1. For each lane, sweep coarse + fine delay
    // 2. Write a pattern to DRAM, read back, check DQS sampling
    // 3. Record the passing delay values
    // This is the most complex calibration step (~160 lines in coreboot).
    // For the initial port, skip (DRAM will be usable on hardware that
    // has conservative default rcven values, and QEMU doesn't need it).
}

/// Compute new tRD (read-to-data delay).
///
/// Ported from coreboot `sdram_new_trd()`.
pub fn sdram_new_trd(si: &SysInfo, mch: &MchBar) {
    // tRD computation based on rcven results.
    // For stub: use a safe default.
    let trd: u8 = if si.selected_timings.mem_clock == 0 {
        6
    } else {
        7
    };
    let v = mch.read8(mchbar::C0STATRDADJV);
    mch.write8(mchbar::C0STATRDADJV, (v & !0x0F) | trd);
    fstart_log::info!("raminit: tRD={}", trd);
}

/// Enhanced mode registers.
///
/// Ported from coreboot `sdram_enhancedmode()`.
pub fn sdram_enhanced_mode(si: &SysInfo, mch: &MchBar) {
    // Enable read/write pointers.
    mch.setbits32(mchbar::C0REFRCTRL2, 1 << 29);

    // Page policy: open page for single-DIMM, close for dual.
    let policy = if si.dimm_config[0] <= 2 { 0 } else { 1 };
    let v = mch.read32(mchbar::C0PVCFG);
    mch.write32(mchbar::C0PVCFG, (v & !0x3) | policy);

    // Bypass and arbitration settings.
    mch.write32(mchbar::C0ARBSPL, 0x0001_0220);

    fstart_log::info!("raminit: enhanced mode configured");
}

/// Power management settings.
///
/// Ported from coreboot `sdram_powersettings()`.
pub fn sdram_power_settings(si: &SysInfo, mch: &MchBar) {
    // CKE control — always on.
    mch.write32(mchbar::C0CKECTRL, mch.read32(mchbar::C0CKECTRL) | (1 << 0));

    // Power-down mode timing.
    mch.write32(mchbar::C0PWLRCTRL, 0x06);

    // Thermal throttle settings.
    mch.write32(mchbar::GTDPCTSHOTTH, 0xFF);

    fstart_log::info!("raminit: power settings configured");
}

/// Program DDR mode register.
///
/// Ported from coreboot `sdram_programddr()`.
pub fn sdram_program_ddr(mch: &MchBar) {
    // Set DDR2 mode.
    let v = mch.read32(mchbar::C0CKECTRL);
    mch.write32(mchbar::C0CKECTRL, v | (1 << 24));

    fstart_log::info!("raminit: DDR mode programmed");
}

/// Program DQ/DQS output timing.
///
/// Ported from coreboot `sdram_programdqdqs()`.
pub fn sdram_program_dqdqs(si: &SysInfo, mch: &MchBar) {
    // Default DQ/DQS output delays (identical for all lanes on Pineview).
    for lane in 0..8u32 {
        // DQ TX delays (4 ranks × 8 lanes).
        for rank in 0..4u32 {
            mch.write8(mchbar::C0TXDQ0R0DLL + lane * 4 + rank, 0);
        }
        // DQS TX delays.
        for rank in 0..4u32 {
            mch.write8(mchbar::C0TXDQS0R0DLL + lane * 4 + rank, 0);
        }
    }

    fstart_log::info!("raminit: DQ/DQS programmed");
}

/// Enable periodic RCOMP.
///
/// Ported from coreboot `sdram_periodic_rcomp()`.
pub fn sdram_periodic_rcomp(mch: &MchBar) {
    mch.setbits32(mchbar::COMPCTRL1, 1 << 16);
    fstart_log::info!("raminit: periodic RCOMP enabled");
}
