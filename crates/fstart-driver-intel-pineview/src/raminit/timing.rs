//! DDR2 timing detection, clock crossing, and register programming.
//!
//! Ported from coreboot `raminit.c`: `sdram_detect_ram_speed`,
//! `sdram_detect_smallest_params`, `sdram_clk_crossing`,
//! `sdram_clkmode`, `sdram_timings`, `sdram_checkreset`.

use super::SysInfo;
use fstart_ecam as ecam;
use fstart_pineview_regs::{hostbridge, ich7, mchbar, MchBar};

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

/// Detect FSB and DDR frequency from host bridge PCI config,
/// then find the common CAS latency across all populated DIMMs.
///
/// Ported from coreboot `sdram_detect_ram_speed()`.
pub fn detect_ram_speed(si: &mut SysInfo, mch: &MchBar) {
    // --- Read FSB frequency from host bridge register 0xE3 ---
    let hb = ecam::PciDevBdf::new(0, 0, 0);
    let e3 = hb.read8(0xE3);
    let fsb_raw = (e3 & 0x70) >> 4;
    let fsb: u8 = if fsb_raw != 0 {
        // 5 - fsb_raw: 4→1(800), 3→2(invalid), 2→3(invalid), 1→4(invalid)
        // In practice only 4 (=800MHz) and 0 (=800MHz default) appear on Pineview.
        (5u8.saturating_sub(fsb_raw)).min(1)
    } else {
        1 // FSB_CLOCK_800MHz
    };

    // --- Read DDR frequency from host bridge registers 0xE3/0xE4 ---
    let freq_raw = ((e3 & 0x80) >> 7) | ((hb.read8(0xE4) & 0x03) << 1);
    let mut freq: u8 = if freq_raw != 0 {
        // 6 - freq_raw: 5→1(800), 4→2(invalid), ... Only 5 (=800) and 0 used.
        (6u8.saturating_sub(freq_raw)).min(1)
    } else {
        1 // MEM_CLOCK_800MHz
    };

    si.selected_timings.fsb_clock = fsb;
    fstart_log::info!(
        "raminit: HW strap: FSB={}MHz, DDR={}MHz",
        if fsb == 1 { 800 } else { 667 },
        if freq == 1 { 800 } else { 667 }
    );

    // --- Detect common CAS latency (SPD byte 18) ---
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
    let lowcas = (lsb.max(5)) as u8;
    let mut cas: u8 = 0;

    // --- CAS / frequency negotiation loop ---
    // For each populated DIMM, check if its tCK (SPD[9]) and
    // tAA (SPD[10]) meet the current frequency's requirements.
    while cas == 0 && highcas >= lowcas {
        let mut all_ok = true;
        for i in 0..super::TOTAL_DIMMS {
            if !si.dimm_populated(i) {
                continue;
            }
            let d = si.dimms[i].as_ref().expect("populated");
            let (max_tck, max_taa) = if freq == 1 {
                (0x25u8, 0x40u8) // 800 MHz: tCK ≤ 2.5 ns, tAA ≤ 10 ns
            } else {
                (0x30u8, 0x45u8) // 667 MHz: tCK ≤ 3.0 ns, tAA ≤ ~11 ns
            };
            if d.spd_data[9] > max_tck || d.spd_data[10] > max_taa {
                all_ok = false;
                break;
            }
        }
        if all_ok {
            cas = highcas;
        } else {
            if highcas == 0 {
                break;
            }
            highcas -= 1;
        }
    }

    // If no CAS works at 800 MHz, drop to 667 MHz and retry.
    if cas == 0 && freq == 1 {
        freq = 0;
        fstart_log::warn!("raminit: dropping to 667 MHz due to timing constraints");
        highcas = msb as u8;
        while cas == 0 && highcas >= lowcas {
            let mut all_ok = true;
            for i in 0..super::TOTAL_DIMMS {
                if !si.dimm_populated(i) {
                    continue;
                }
                let d = si.dimms[i].as_ref().expect("populated");
                if d.spd_data[9] > 0x30 || d.spd_data[10] > 0x45 {
                    all_ok = false;
                    break;
                }
            }
            if all_ok {
                cas = highcas;
            } else {
                if highcas == 0 {
                    break;
                }
                highcas -= 1;
            }
        }
    }

    if cas == 0 {
        fstart_log::error!("raminit: no valid CAS latency found, defaulting to 5");
        cas = 5;
    }

    si.selected_timings.cas = cas;
    si.selected_timings.mem_clock = freq;
    si.selected_timings.fsb_clock = fsb;

    // --- Program the selected frequency into MCHBAR CLKCFG ---
    if si.boot_path != super::BOOT_PATH_RESET {
        mch.setbits32(mchbar::PMSTS, 1 << 0);

        let clkcfg = mch.read32(mchbar::CLKCFG) & !0x70;
        let freq_bits: u32 = if freq == 1 { 3 } else { 2 }; // 800→3, 667→2
        mch.write32(mchbar::CLKCFG, clkcfg | (1 << 10) | (freq_bits << 4));

        // Read back the MCH-validated frequency.
        let validated = ((mch.read32(mchbar::CLKCFG) >> 4) & 0x07).wrapping_sub(2) as u8;
        si.selected_timings.mem_clock = validated.min(1);

        if si.selected_timings.mem_clock == 1 {
            fstart_log::info!("raminit: MCH validated at 800MHz");
            si.nodll = 0;
            si.maxpi = 63;
            si.pioffset = 0;
        } else {
            fstart_log::info!("raminit: MCH validated at 667MHz");
            si.nodll = 1;
            si.maxpi = 15;
            si.pioffset = 1;
        }
    }

    fstart_log::info!(
        "raminit: DDR {}MHz, CAS={}, FSB={}",
        if si.selected_timings.mem_clock == 1 {
            800
        } else {
            667
        },
        cas,
        if si.selected_timings.fsb_clock == 1 {
            800
        } else {
            667
        }
    );
}

/// Detect the smallest common timing parameters across all DIMMs.
///
/// Ported from coreboot `sdram_detect_smallest_params()`.
pub fn detect_smallest_params(si: &mut SysInfo) {
    // Cycle time in ps for DDR667 and DDR800.
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
        "raminit: tRAS={} tRP={} tRCD={} tWR={} tRFC={} tWTR={} tRRD={} tRTP={}",
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
            [0xFFFF_FFFF, 0x0503_0305, 0x0000_FFFF, 0x0000_0000],
            [0x1F1F_1F1F, 0x2A1F_1FA5, 0x0000_0000, 0x0500_0002],
        ],
        [
            [0x1F1F_1F1F, 0x0D07_070B, 0x0000_0000, 0x0000_0000],
            [0xFFFF_FFFF, 0x0503_0305, 0x0000_FFFF, 0x0000_0000],
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

    let mpll_ctl: u16 = if si.selected_timings.mem_clock == 0 {
        1 // 667 MHz
    } else {
        (1 << 8) | (1 << 5) // 800 MHz
    };

    if si.boot_path != super::BOOT_PATH_RESET {
        let v = mch.read16(mchbar::MPLLCTL);
        mch.write16(mchbar::MPLLCTL, (v & !0x033F) | mpll_ctl);
    }

    mch.write32(mchbar::C0GNT2LNCH1, 0x5800_1117);
    mch.setbits32(mchbar::C0STATRDCTRL, 1 << 23);

    static CAS_TO_REG: [[u32; 4]; 2] = [
        [0x0000_0000, 0x0003_0100, 0x0C24_0201, 0x0000_0000],
        [0x0000_0000, 0x0003_0100, 0x0C24_0201, 0x1045_0302],
    ];

    let ddr_freq = si.selected_timings.mem_clock as usize;
    let cas_idx = si.selected_timings.cas.saturating_sub(3) as usize;
    if cas_idx < 4 {
        mch.write32(mchbar::C0GNT2LNCH2, CAS_TO_REG[ddr_freq][cas_idx]);
    }

    fstart_log::info!("raminit: clock mode configured");
}

// ===================================================================
// Check reset — with actual cf9 reset trigger
// ===================================================================

/// Check for warm reset condition. If the PMCON bits indicate a
/// reset is needed (first pass of raminit on fresh power), trigger
/// a full platform reset.
///
/// Ported from coreboot `sdram_checkreset()`.
pub fn check_reset(si: &SysInfo) {
    let lpc = ecam::PciDevBdf::new(0, ich7::LPC_DEV, ich7::LPC_FUNC);

    let mut pmcon2 = lpc.read8(0xA2);
    let mut pmcon3 = lpc.read8(0xA4);
    pmcon3 &= !0x02;

    let reset = if pmcon2 & 0x80 != 0 {
        pmcon2 &= !0x80;
        true
    } else {
        pmcon2 |= 0x80;
        false
    };

    if pmcon2 & 0x04 != 0 {
        pmcon2 |= 0x04;
        pmcon3 = (pmcon3 & !0x30) | 0x30;
        pmcon3 |= 1 << 3;
    }

    lpc.write8(0xA2, pmcon2);
    lpc.write8(0xA4, pmcon3);

    if reset {
        fstart_log::info!("raminit: triggering full reset (PMCON2 bit 7 set)");
        // Write 0x0E to CF9 to trigger full reset.
        #[cfg(target_arch = "x86_64")]
        unsafe {
            fstart_pio::outb(0xCF9, 0x0E);
        }
        // Should not reach here after reset.
        loop {
            core::hint::spin_loop();
        }
    }
}

// ===================================================================
// Full timing register programming
// ===================================================================

/// Program detailed timing registers into MCHBAR.
///
/// This is a faithful line-by-line port of coreboot `sdram_timings()`.
pub fn sdram_timings(si: &SysInfo, mch: &MchBar) {
    let t = &si.selected_timings;
    let wl = t.cas.saturating_sub(1);
    let flag = if t.mem_clock == 0 { 0usize } else { 1usize };

    // Detect bank/page geometry for timing adjustments.
    let mut trp_adj: u8 = 0;
    let mut bank: u8 = 1;
    let mut page: usize = 0;
    for i in 0..super::TOTAL_DIMMS {
        if let Some(ref d) = si.dimms[i] {
            if d.card_type != 0 {
                if d.banks == 1 {
                    trp_adj = 1;
                    bank = 0;
                }
                if d.page_size == 2048 {
                    page = 1;
                }
            }
        }
    }

    static PAGETAB: [[u8; 2]; 2] = [[0x0E, 0x12], [0x10, 0x14]];

    // C0LATCTRL: write latency and CAS.
    mch.write8(
        mchbar::C0LATCTRL,
        ((wl.saturating_sub(3)) << 4) | (t.cas.saturating_sub(3)),
    );

    mch.setbits32(mchbar::C0PVCFG, 3);

    // C0CYCTRKPCHG: precharge tracking.
    let pchg = ((wl as u16 + 4 + t.twr as u16) << 6) | ((2 + (t.trtp as u16).max(2)) << 2) | 1;
    mch.write16(mchbar::C0CYCTRKPCHG, pchg);

    // C0CYCTRKACT: activate tracking.
    let mut act = ((bank as u32) << 21)
        | ((t.trrd as u32) << 17)
        | ((t.trp as u32) << 13)
        | (((t.trp + trp_adj) as u32) << 9)
        | (t.trfc as u32);
    if bank == 0 {
        act |= (PAGETAB[flag][page] as u32) << 22;
    }
    mch.write16(mchbar::C0CYCTRKACT, act as u16);
    mch.write16(mchbar::C0CYCTRKACT + 2, (act >> 16) as u16);

    // SHCYCTRKCKEL.
    let shckel_bits = (mch.read16(mchbar::C0CYCTRKACT + 2) & 0x0FC0) >> 6;
    let v = mch.read16(mchbar::SHCYCTRKCKEL);
    mch.write16(
        mchbar::SHCYCTRKCKEL,
        (v & !(0x3F << 7)) | (shckel_bits << 7),
    );

    // C0CYCTRKWR: write tracking.
    let wrtrk = ((t.trcd as u16) << 12) | (4 << 8) | (6 << 4) | 8;
    mch.write16(mchbar::C0CYCTRKWR, wrtrk);

    // C0CYCTRKRD: read tracking.
    let rdtrk = ((t.trcd as u32) << 17)
        | ((wl as u32 + 4 + t.twtr as u32) << 12)
        | ((t.cas as u32) << 8)
        | (4 << 4)
        | 6;
    mch.write32(mchbar::C0CYCTRKRD, rdtrk);

    // C0CYCTRKREFR: refresh tracking.
    let refr = (((t.trp + trp_adj) as u16) << 9) | (t.trfc as u16);
    mch.write8(mchbar::C0CYCTRKREFR, refr as u8);
    mch.write8(mchbar::C0CYCTRKREFR + 1, (refr >> 8) as u8);

    // C0CKECTRL: CKE idle timer.
    let v = mch.read16(mchbar::C0CKECTRL);
    mch.write16(mchbar::C0CKECTRL, (v & !(0x1FF << 1)) | (100 << 1));

    // C0CYCTRKPCHG2: tRAS.
    let v = mch.read8(mchbar::C0CYCTRKPCHG2);
    mch.write8(mchbar::C0CYCTRKPCHG2, (v & !0x3F) | t.tras);

    // Arbitration control.
    mch.write16(mchbar::C0ARBCTRL, 0x2310);
    let v = mch.read8(mchbar::C0ADDCSCTRL);
    mch.write8(mchbar::C0ADDCSCTRL, (v & !0x1F) | 1);

    // C0STATRDCTRL: static read control.
    let reg32_ddr = if t.mem_clock == 0 { 3000u32 } else { 2500 };
    let reg32_fsb = if si.selected_timings.fsb_clock == 0 {
        6000u32
    } else {
        5000
    };
    let stat = (((t.cas as u32 + 7) * reg32_ddr / reg32_fsb) as u16) << 8;
    let v = mch.read16(mchbar::C0STATRDCTRL);
    mch.write16(mchbar::C0STATRDCTRL, (v & !(0x1F << 8)) | stat);

    // C0WRDATACTRL: write data control.
    let wl_flag: u8 = if wl > 2 { 1 } else { 0 };
    let wdat_lo = wl.saturating_sub(1).saturating_sub(wl_flag);
    let wdat = (wdat_lo as u16) | ((wdat_lo as u16) << 4) | ((wl_flag as u16) << 8);
    let v = mch.read16(mchbar::C0WRDATACTRL);
    mch.write16(mchbar::C0WRDATACTRL, (v & !0x01FF) | wdat);

    mch.write16(mchbar::C0RDQCTRL, 0x1585);
    let v = mch.read8(mchbar::C0PWLRCTRL);
    mch.write8(mchbar::C0PWLRCTRL, v & !0x1F);

    // rdmodwr_window.
    let v = mch.read16(mchbar::C0PWLRCTRL);
    mch.write16(
        mchbar::C0PWLRCTRL,
        (v & !(0x3F << 8)) | (((t.cas as u16) + 9) << 8),
    );

    // Refresh control.
    let (refctrl16, refctrl32) = if t.mem_clock == 0 {
        (0x0514u16, 0x0A28u32)
    } else {
        (0x0618u16, 0x0C30u32)
    };
    let v = mch.read32(mchbar::C0REFRCTRL2);
    mch.write32(
        mchbar::C0REFRCTRL2,
        (v & !(0xFFFFF << 8)) | (0x3F << 22) | (refctrl32 << 8),
    );
    mch.write8(mchbar::C0REFRCTRL + 3, 0);
    let v = mch.read16(mchbar::C0REFCTRL);
    mch.write16(mchbar::C0REFCTRL, (v & !0x3FFF) | refctrl16);

    // NPUT static mode.
    mch.setbits32(mchbar::C0DYNRDCTRL, 1 << 0);

    let v = mch.read32(mchbar::C0STATRDCTRL);
    mch.write32(mchbar::C0STATRDCTRL, (v & !(0x7F << 24)) | (0x0B << 25));
    if si.selected_timings.mem_clock > si.selected_timings.fsb_clock {
        mch.setbits32(mchbar::C0STATRDCTRL, 1 << 24);
    }

    let v = mch.read8(mchbar::C0RDFIFOCTRL);
    mch.write8(mchbar::C0RDFIFOCTRL, v & !0x03);

    let v = mch.read16(mchbar::C0WRDATACTRL);
    mch.write16(
        mchbar::C0WRDATACTRL,
        (v & !(0x1F << 10)) | (((wl as u16) + 10) << 10),
    );

    let v = mch.read32(mchbar::C0CKECTRL);
    mch.write32(
        mchbar::C0CKECTRL,
        (v & !(7 << 24 | 7 << 17)) | (3 << 24) | (3 << 17),
    );

    // C0REFRCTRL + 4 (16-bit).
    let refctrl4 = (0x15u16 << 6) | 0x1F | (0x06 << 12);
    let v = mch.read16(mchbar::C0REFRCTRL + 4);
    mch.write16(mchbar::C0REFRCTRL + 4, (v & !0x7FFF) | refctrl4);

    // C0REFRCTRL2 upper bits.
    let reg32 = (0x06u32 << 27) | (1 << 25);
    let v = mch.read32(mchbar::C0REFRCTRL2);
    mch.write32(mchbar::C0REFRCTRL2, (v & !(3 << 28)) | (reg32 << 8));
    let v = mch.read8(mchbar::C0REFRCTRL + 3);
    mch.write8(mchbar::C0REFRCTRL + 3, (v & !0xFA) | ((reg32 >> 24) as u8));

    let v = mch.read8(mchbar::C0JEDEC);
    mch.write8(mchbar::C0JEDEC, v & !(1 << 7));
    let v = mch.read8(mchbar::C0DYNRDCTRL);
    mch.write8(mchbar::C0DYNRDCTRL, v & !(3 << 1));

    // Write watermark flush (64-bit register).
    let wmflsh = ((6u32 & 3) << 30) | (4 << 25) | (1 << 20) | (8 << 15) | (6 << 10) | (4 << 5) | 1;
    mch.write32(mchbar::C0WRWMFLSH, wmflsh);
    let v = mch.read16(mchbar::C0WRWMFLSH + 4);
    mch.write16(mchbar::C0WRWMFLSH + 4, (v & !0x01FF) | (8 << 3) | (6 >> 2));

    mch.setbits32(mchbar::SHPENDREG, 0x1C00 | (0x1F << 5));

    let v = mch.read8(mchbar::SHPAGECTRL);
    mch.write8(mchbar::SHPAGECTRL, (v & !0xFF) | 0x40);
    let v = mch.read8(mchbar::SHPAGECTRL + 1);
    mch.write8(mchbar::SHPAGECTRL + 1, (v & !0x07) | 0x05);
    let v = mch.read8(mchbar::SHCMPLWRCMD);
    mch.write8(mchbar::SHCMPLWRCMD, v | 0x1F);

    let bonus = (3u8 << 6) | (si.dt0mode << 4) | 0x0C;
    let v = mch.read8(mchbar::SHBONUSREG);
    mch.write8(mchbar::SHBONUSREG, (v & !0xDF) | bonus);

    let v = mch.read8(mchbar::CSHRWRIOMLNS);
    mch.write8(mchbar::CSHRWRIOMLNS, v & !(1 << 1));
    let v = mch.read8(mchbar::C0MISCTM);
    mch.write8(mchbar::C0MISCTM, (v & !0x07) | 0x02);
    let v = mch.read16(mchbar::C0BYPCTRL);
    mch.write16(mchbar::C0BYPCTRL, (v & !(0xFF << 2)) | (4 << 2));

    // WRWMCONFIG: kN=2 (2N command rate).
    let wrwm = (2u32 << 29) | (1 << 28) | (1 << 23);
    let v = mch.read32(mchbar::WRWMCONFIG);
    mch.write32(mchbar::WRWMCONFIG, (v & !(0xFFB << 20)) | wrwm);

    // BYPACTSF: extract from CYCTRKACT.
    let actlo = mch.read16(mchbar::C0CYCTRKACT);
    let acthi = mch.read16(mchbar::C0CYCTRKACT + 2);
    let byp_act = ((actlo >> 13) & 0x07) as u8 | (((acthi & 1) as u8) << 3);
    let v = mch.read8(mchbar::BYPACTSF);
    mch.write8(mchbar::BYPACTSF, (v & !0xF0) | (byp_act << 4));

    let rdtrk_bits = ((mch.read32(mchbar::C0CYCTRKRD) & 0x000F_0000) >> 17) as u8;
    let v = mch.read8(mchbar::BYPACTSF);
    mch.write8(mchbar::BYPACTSF, (v & !0x0F) | rdtrk_bits);

    // Clear bypass knobs.
    let v = mch.read8(mchbar::BYPKNRULE);
    mch.write8(mchbar::BYPKNRULE, v & !0xFC);
    let v = mch.read8(mchbar::BYPKNRULE);
    mch.write8(mchbar::BYPKNRULE, v & !0x03);
    let v = mch.read8(mchbar::SHBONUSREG);
    mch.write8(mchbar::SHBONUSREG, v & !0x03);
    mch.setbits32(mchbar::C0BYPCTRL, 1 << 0);
    mch.setbits32(mchbar::CSHRMISCCTL1, 1 << 9);

    // DLL receive control per lane.
    for i in 0..8u32 {
        let v = mch.read32(mchbar::ly(0x540, i));
        mch.write32(mchbar::ly(0x540, i), (v & !0x3F3F_3F3F) | 0x0C0C_0C0C);
    }

    // RDCS to RCVEN delay: coarse = CAS + 1.
    let v = mch.read32(mchbar::C0STATRDCTRL);
    mch.write32(
        mchbar::C0STATRDCTRL,
        (v & !(0x0F << 16)) | (((t.cas + 1) as u32) << 16),
    );

    // Program RCVEN delay with DLL-safe settings (all zero).
    for i in 0..8u32 {
        let v = mch.read8(mchbar::ly(0x560, i));
        mch.write8(mchbar::ly(0x560, i), v & !0x3F);
        let v = mch.read16(mchbar::C0RCVMISCCTL2);
        mch.write16(mchbar::C0RCVMISCCTL2, v & !(3 << (i * 2)));
        let v = mch.read16(mchbar::C0RCVMISCCTL1);
        mch.write16(mchbar::C0RCVMISCCTL1, v & !(3 << (i * 2)));
        let v = mch.read16(mchbar::C0COARSEDLY0);
        mch.write16(mchbar::C0COARSEDLY0, v & !(3 << (i * 2)));
    }

    // Power up DLL.
    let v = mch.read8(mchbar::C0DLLPIEN);
    mch.write8(mchbar::C0DLLPIEN, v & !(1 << 0));
    mch.setbits32(mchbar::C0DLLPIEN, 1 << 1);
    mch.setbits32(mchbar::C0DLLPIEN, 1 << 2);

    mch.setbits32(mchbar::C0COREBONUS, 0x000C_0400);
    mch.setbits32(mchbar::C0CMDTX1, 1 << 31);

    fstart_log::info!("raminit: timing registers programmed (WL={})", wl);
}
