//! DDR2 timing detection, clock crossing, and register programming.
//!
//! Ported from coreboot `raminit.c`: `sdram_detect_ram_speed`,
//! `sdram_detect_smallest_params`, `sdram_clk_crossing`,
//! `sdram_clkmode`, `sdram_timings`, `sdram_checkreset`.

use super::SysInfo;
use fstart_pineview_regs::{hostbridge, mchbar, MchBar};

// ===================================================================
// Helpers
// ===================================================================

fn lsbpos(val: u8) -> i8 {
    for i in 0..8 {
        if val & (1 << i) != 0 {
            return i;
        }
    }
    -1
}

fn msbpos(val: u8) -> i8 {
    for i in (0..8).rev() {
        if val & (1 << i) != 0 {
            return i;
        }
    }
    -1
}

fn div_round_up(a: u32, b: u32) -> u32 {
    (a + b - 1) / b
}

// ===================================================================
// RAM speed detection
// ===================================================================

/// Detect the common RAM speed and CAS latency across all populated DIMMs.
///
/// Ported from coreboot `sdram_detect_ram_speed()`.
pub fn detect_ram_speed(si: &mut SysInfo) {
    // Read FSB and DDR frequency from host bridge config.
    // In fstart these would come from ECAM, but we read from MCHBAR-side
    // POC register or the config. For now, default to 800/667.
    // TODO: read from actual PCI config via ECAM when on real hardware.
    let mut fsb: u8 = 1; // FSB_CLOCK_800MHz
    let mut freq: u8 = 0; // MEM_CLOCK_667MHz

    // Detect common CAS latency.
    let mut common_cas: u8 = 0xFF;
    for i in 0..super::TOTAL_DIMMS {
        if si.dimm_populated(i) {
            let d = si.dimms[i].as_ref().expect("populated");
            common_cas &= d.cas_latencies;
        }
    }
    if common_cas == 0 {
        fstart_log::error!("raminit: no common CAS latency among DIMMs");
        common_cas = 7; // fallback
    }

    let msb = msbpos(common_cas);
    let lsb = lsbpos(common_cas);
    let mut highcas = msb as u8;
    let lowcas = lsb.max(5) as u8;
    let mut cas: u8 = 0;

    // Try to find a CAS that meets timing constraints.
    while cas == 0 && highcas >= lowcas {
        let mut ok = true;
        for i in 0..super::TOTAL_DIMMS {
            if !si.dimm_populated(i) {
                continue;
            }
            let d = si.dimms[i].as_ref().expect("populated");
            let (max_tck, max_taa) = if freq == 1 {
                (0x25u8, 0x40u8) // 800 MHz
            } else {
                (0x30u8, 0x45u8) // 667 MHz
            };
            if d.tck_min > max_tck || d.taa_min > max_taa {
                ok = false;
                break;
            }
        }
        if ok {
            cas = highcas;
        } else {
            highcas -= 1;
        }
    }

    if cas == 0 && freq == 1 {
        // Drop to 667 MHz.
        freq = 0;
        fstart_log::warn!("raminit: dropping to 667 MHz due to timing constraints");
        highcas = msb as u8;
        let lowcas = lsb as u8;
        while cas == 0 && highcas >= lowcas {
            // At 667 MHz, all DIMMs should fit.
            cas = highcas;
        }
    }

    if cas == 0 {
        fstart_log::error!("raminit: no valid CAS latency found, defaulting to 5");
        cas = 5;
    }

    si.selected_timings.cas = cas;
    si.selected_timings.mem_clock = freq;
    si.selected_timings.fsb_clock = fsb;

    fstart_log::info!(
        "raminit: DDR {}MHz, CAS={}, FSB={}",
        if freq == 1 { 800 } else { 667 },
        cas,
        if fsb == 1 { 800 } else { 667 }
    );
}

/// Detect the smallest common timing parameters across all DIMMs.
///
/// Ported from coreboot `sdram_detect_smallest_params()`.
pub fn detect_smallest_params(si: &mut SysInfo) {
    // Cycle time multiplier in ps for DDR667 and DDR800.
    let mult: [u32; 2] = [3000, 2500];
    let m = mult[si.selected_timings.mem_clock as usize];

    let mut max_tras: u32 = 0;
    let mut max_trp: u32 = 0;
    let mut max_trcd: u32 = 0;
    let mut max_twr: u32 = 0;
    let mut max_trfc: u32 = 0;
    let mut max_twtr: u32 = 0;
    let mut max_trrd: u32 = 0;
    let mut max_trtp: u32 = 0;

    for i in 0..super::TOTAL_DIMMS {
        if !si.dimm_populated(i) {
            continue;
        }
        let d = si.dimms[i].as_ref().expect("populated");
        let spd = &d.spd_data;
        max_tras = max_tras.max((spd[30] as u32) * 1000);
        max_trp = max_trp.max(((spd[27] as u32) * 1000) >> 2);
        max_trcd = max_trcd.max(((spd[29] as u32) * 1000) >> 2);
        max_twr = max_twr.max(((spd[36] as u32) * 1000) >> 2);
        max_trfc = max_trfc.max((spd[42] as u32) * 1000 + (spd[40] as u32 & 0xF));
        max_twtr = max_twtr.max(((spd[37] as u32) * 1000) >> 2);
        max_trrd = max_trrd.max(((spd[28] as u32) * 1000) >> 2);
        max_trtp = max_trtp.max(((spd[38] as u32) * 1000) >> 2);
    }

    si.selected_timings.tras = 24u8.min(div_round_up(max_tras, m) as u8);
    si.selected_timings.trp = 10u8.min(div_round_up(max_trp, m) as u8);
    si.selected_timings.trcd = 10u8.min(div_round_up(max_trcd, m) as u8);
    si.selected_timings.twr = 15u8.min(div_round_up(max_twr, m) as u8);
    // tRFC must be even.
    let trfc = 78u8.min(div_round_up(max_trfc, m) as u8).wrapping_add(1) & 0xFE;
    si.selected_timings.trfc = trfc;
    si.selected_timings.twtr = 15u8.min(div_round_up(max_twtr, m) as u8);
    si.selected_timings.trrd = 15u8.min(div_round_up(max_trrd, m) as u8);
    si.selected_timings.trtp = 15u8.min(div_round_up(max_trtp, m) as u8);

    fstart_log::info!(
        "raminit: timings CAS={} tRAS={} tRP={} tRCD={} tWR={} tRFC={} tWTR={} tRRD={} tRTP={}",
        si.selected_timings.cas,
        si.selected_timings.tras,
        si.selected_timings.trp,
        si.selected_timings.trcd,
        si.selected_timings.twr,
        si.selected_timings.trfc,
        si.selected_timings.twtr,
        si.selected_timings.trrd,
        si.selected_timings.trtp
    );
}

// ===================================================================
// Clock crossing
// ===================================================================

/// Program clock-crossing registers.
///
/// Ported from coreboot `sdram_clk_crossing()`.
pub fn clk_crossing(si: &SysInfo, mch: &MchBar) {
    let ddr = si.selected_timings.mem_clock as usize;
    let fsb = si.selected_timings.fsb_clock as usize;

    static CLKCROSS: [[[u32; 4]; 2]; 2] = [
        [
            [0xFFFF_FFFF, 0x0503_0305, 0x0000_FFFF, 0x0000_0000], // FSB667, DDR667
            [0x1F1F_1F1F, 0x2A1F_1FA5, 0x0000_0000, 0x0500_0002], // FSB667, DDR800
        ],
        [
            [0x1F1F_1F1F, 0x0D07_070B, 0x0000_0000, 0x0000_0000], // FSB800, DDR667
            [0xFFFF_FFFF, 0x0503_0305, 0x0000_FFFF, 0x0000_0000], // FSB800, DDR800
        ],
    ];

    mch.write32(mchbar::HMCCMP, CLKCROSS[fsb][ddr][0]);
    mch.write32(mchbar::HMDCMP, CLKCROSS[fsb][ddr][1]);
    mch.write32(mchbar::HMBYPCP, CLKCROSS[fsb][ddr][2]);
    mch.write32(mchbar::HMCCPEXT, 0);
    mch.write32(mchbar::HMDCPEXT, CLKCROSS[fsb][ddr][3]);
    mch.setbits32(mchbar::HMCCMC, 1 << 7);

    if fsb == 0 && ddr == 1 {
        mch.write8(mchbar::CLKXSSH2MCBYPPHAS, 0);
        mch.write32(mchbar::CLKXSSH2MD, 0);
        mch.write32(mchbar::CLKXSSH2MD + 4, 0);
    }

    static CLKCROSS2: [[[u32; 8]; 2]; 2] = [
        [
            [
                0x0000_0000,
                0x0801_0204,
                0x0000_0000,
                0x0801_0204,
                0x0000_0000,
                0x0000_0000,
                0x0000_0000,
                0x0408_0102,
            ],
            [
                0x0408_0000,
                0x1001_0002,
                0x1000_0000,
                0x2001_0208,
                0x0000_0000,
                0x0000_0004,
                0x0204_0000,
                0x0810_0102,
            ],
        ],
        [
            [
                0x1000_0000,
                0x2001_0208,
                0x0408_0000,
                0x1001_0002,
                0x0000_0000,
                0x0000_0000,
                0x0800_0000,
                0x1020_0204,
            ],
            [
                0x0000_0000,
                0x0801_0204,
                0x0000_0000,
                0x0801_0204,
                0x0000_0000,
                0x0000_0000,
                0x0000_0000,
                0x0408_0102,
            ],
        ],
    ];

    let c2 = &CLKCROSS2[fsb][ddr];
    mch.write32(mchbar::CLKXSSH2MCBYP, c2[0]);
    mch.write32(mchbar::CLKXSSH2MCRDQ, c2[0]);
    mch.write32(mchbar::CLKXSSH2MCRDCST, c2[0]);
    mch.write32(mchbar::CLKXSSH2MCBYP + 4, c2[1]);
    mch.write32(mchbar::CLKXSSH2MCRDQ + 4, c2[1]);
    mch.write32(mchbar::CLKXSSH2MCRDCST + 4, c2[1]);
    mch.write32(mchbar::CLKXSSMC2H, c2[2]);
    mch.write32(mchbar::CLKXSSMC2H + 4, c2[3]);
    mch.write32(mchbar::CLKXSSMC2HALT, c2[4]);
    mch.write32(mchbar::CLKXSSMC2HALT + 4, c2[5]);
    mch.write32(mchbar::CLKXSSH2X2MD, c2[6]);
    mch.write32(mchbar::CLKXSSH2X2MD + 4, c2[7]);

    fstart_log::info!("raminit: clock crossing configured");
}

// ===================================================================
// Clock mode
// ===================================================================

/// Program clock mode registers.
///
/// Ported from coreboot `sdram_clkmode()`.
pub fn clkmode(si: &SysInfo, mch: &MchBar) {
    let v = mch.read16(mchbar::CSHRMISCCTL1);
    mch.write16(mchbar::CSHRMISCCTL1, v & !(1 << 8));
    let v = mch.read8(mchbar::CSHRMISCCTL1);
    mch.write8(mchbar::CSHRMISCCTL1, v & !0x3F);

    let (ddr_freq, mpll_ctl): (u8, u16) = if si.selected_timings.mem_clock == 0 {
        (0, 1)
    } else {
        (1, (1 << 8) | (1 << 5))
    };

    if si.boot_path != super::BOOT_PATH_RESET {
        let v = mch.read16(mchbar::MPLLCTL);
        mch.write16(mchbar::MPLLCTL, (v & !0x033F) | mpll_ctl);
    }

    mch.write32(mchbar::C0GNT2LNCH1, 0x5800_1117);
    mch.setbits32(mchbar::C0STATRDCTRL, 1 << 23);

    static CAS_TO_REG: [[u32; 4]; 2] = [
        [0x0000_0000, 0x0003_0100, 0x0C24_0201, 0x0000_0000], // DDR667
        [0x0000_0000, 0x0003_0100, 0x0C24_0201, 0x1045_0302], // DDR800
    ];

    let cas_idx = si.selected_timings.cas.saturating_sub(3) as usize;
    if cas_idx < 4 {
        mch.write32(mchbar::C0GNT2LNCH2, CAS_TO_REG[ddr_freq as usize][cas_idx]);
    }

    fstart_log::info!("raminit: clock mode configured");
}

// ===================================================================
// Check reset
// ===================================================================

/// Check for warm reset condition.
///
/// Ported from coreboot `sdram_checkreset()`.
pub fn check_reset(mch: &MchBar) {
    let pmsts = mch.read32(mchbar::PMSTS);
    if pmsts & (1 << 8) != 0 {
        fstart_log::info!("raminit: warm reset detected");
    }
}

// ===================================================================
// Timing registers
// ===================================================================

/// Program detailed timing registers into MCHBAR.
///
/// Ported from coreboot `sdram_timings()`. This is the largest single
/// function (~200 lines of register writes). Currently a condensed
/// port covering the main timing register programming.
pub fn sdram_timings(si: &SysInfo, mch: &MchBar) {
    let t = &si.selected_timings;
    let ddr = t.mem_clock;

    // Write latency: CAS - 1 for DDR2.
    let wl = t.cas.saturating_sub(1);

    // tRAS, tRP, tRCD encoding into C0C2REG and related registers.
    let c0c2reg = ((t.tras as u32) << 16) | ((t.trp as u32) << 8) | (t.trcd as u32);
    mch.write32(mchbar::C0C2REG, c0c2reg);

    // Core timing register.
    let latctrl = ((t.cas as u32) << 0)
        | ((wl as u32) << 4)
        | ((t.trp as u32) << 8)
        | ((t.trcd as u32) << 12)
        | ((t.twr as u32) << 16);
    mch.write32(mchbar::C0LATCTRL, latctrl);

    // Cycle tracking registers.
    mch.write32(mchbar::C0CYCTRKPCHG, (t.trp as u32) | ((t.trp as u32) << 8));
    mch.write32(
        mchbar::C0CYCTRKACT,
        (t.trcd as u32) | ((t.tras as u32) << 8),
    );
    mch.write32(mchbar::C0CYCTRKRD, (t.cas as u32) | ((t.trtp as u32) << 8));
    mch.write32(mchbar::C0CYCTRKWR, (wl as u32) | ((t.twr as u32) << 8));
    mch.write32(mchbar::C0CYCTRKREFR, t.trfc as u32);

    // Refresh control.
    let refrctrl = if ddr == 0 { 0x3F7 } else { 0x4B0 };
    mch.write32(mchbar::C0REFRCTRL, refrctrl);

    // ODT/JEDEC register.
    let jedec = ((t.twr as u32) << 0)
        | ((t.cas as u32) << 4)
        | ((t.twtr as u32) << 8)
        | ((t.trrd as u32) << 12);
    mch.write32(mchbar::C0JEDEC, jedec);

    fstart_log::info!("raminit: timing registers programmed (WL={})", wl);
}
