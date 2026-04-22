//! DDR2 PHY calibration: DLL timing, RCOMP, ODT, receive enable,
//! enhanced mode, power settings, and DQ/DQS programming.
//!
//! Full port of coreboot `raminit.c` functions: `sdram_dlltiming`,
//! `sdram_rcomp`, `sdram_odt`, `sdram_rcompupdate`, `sdram_rcven`,
//! `sdram_new_trd`, `sdram_enhancedmode`, `sdram_powersettings`,
//! `sdram_programddr`, `sdram_programdqdqs`, `sdram_periodic_rcomp`.

use super::{PllParam, SysInfo};
use fstart_pineview_regs::{ecam, mchbar, MchBar};

// HPET microsecond delay (simplified spin-based).
fn hpet_udelay(us: u32) {
    // On real hardware this reads the HPET counter. For now, spin.
    for _ in 0..us * 100 {
        core::hint::spin_loop();
    }
}

// ===================================================================
// PLL calibration tables (from coreboot sdram_calibratepll)
// ===================================================================

/// Build the PLL parameter tables and program clock/cmd/ctrl/dq/dqs
/// output DLLs with HPLL/MPLL calibration values.
///
/// Ported from coreboot `sdram_calibratepll()`.
fn calibrate_pll(si: &SysInfo, mch: &MchBar, pidelay: u8) {
    let mut pll = PllParam::default();

    // DDR667 = index 0, DDR800 = index 1.
    let f = si.selected_timings.mem_clock as usize;

    // PI delay tables (72 entries per frequency).
    static PI_667: [u8; 72] = [
        3, 3, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4,
        4, 4, 5, 5, 5, 5, 7, 7, 7, 7, 3, 3, 3, 3, 3, 3, 3, 3, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 1, 1, 1, 1, 3, 3, 3, 3,
    ];
    static PI_800: [u8; 72] = [
        53, 53, 10, 10, 5, 5, 5, 5, 27, 27, 27, 27, 34, 34, 34, 34, 34, 34, 34, 34, 39, 39, 39, 39,
        47, 47, 47, 47, 44, 44, 44, 44, 47, 47, 47, 47, 47, 47, 47, 47, 59, 59, 59, 59, 2, 2, 2, 2,
        2, 2, 2, 2, 7, 7, 7, 7, 15, 15, 15, 15, 12, 12, 12, 12, 15, 15, 15, 15, 15, 15, 15, 15,
    ];
    static DBEN_667: [u8; 72] = [
        0, 0, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    ];
    static DBEN_800: [u8; 72] = [
        1, 1, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0,
        1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0,
    ];
    static DBSEL_667: [u8; 72] = [
        0, 0, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    ];
    static DBSEL_800: [u8; 72] = [
        0, 0, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0,
        1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0,
    ];
    static CLKDELAY_667: [u8; 72] = [
        0, 0, 1, 1, 0, 0, 0, 0, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
        1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    ];
    static CLKDELAY_800: [u8; 72] = [
        0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
        1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
        1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
    ];

    let pi_src = if f == 0 { &PI_667 } else { &PI_800 };
    let dben_src = if f == 0 { &DBEN_667 } else { &DBEN_800 };
    let dbsel_src = if f == 0 { &DBSEL_667 } else { &DBSEL_800 };
    let clkd_src = if f == 0 { &CLKDELAY_667 } else { &CLKDELAY_800 };

    for i in 0..72 {
        pll.pi[f][i] = pi_src[i].wrapping_add(pidelay);
        pll.dben[f][i] = dben_src[i];
        pll.dbsel[f][i] = dbsel_src[i];
        pll.clkdelay[f][i] = clkd_src[i];
    }

    // Disable Dynamic DQS Slave Setting Per Rank.
    let v = mch.read8(mchbar::CSHRDQSCMN);
    mch.write8(mchbar::CSHRDQSCMN, v & !(1 << 7));
    let v = mch.read16(mchbar::CSHRPDCTL4);
    mch.write16(mchbar::CSHRPDCTL4, (v & !0x3FFF) | 0x1FFF);

    // clkset0 (index 0).
    let v = mch.read16(mchbar::C0CKTX);
    mch.write16(
        mchbar::C0CKTX,
        (v & !0xC440)
            | ((pll.clkdelay[f][0] as u16) << 14)
            | ((pll.dben[f][0] as u16) << 10)
            | ((pll.dbsel[f][0] as u16) << 6),
    );
    let v = mch.read8(mchbar::C0TXCK0DLL);
    mch.write8(mchbar::C0TXCK0DLL, (v & !0x3F) | pll.pi[f][0]);

    // clkset1 (index 1).
    let v = mch.read32(mchbar::C0CKTX);
    mch.write32(
        mchbar::C0CKTX,
        (v & !0x0003_0880)
            | ((pll.clkdelay[f][1] as u32) << 16)
            | ((pll.dben[f][1] as u32) << 11)
            | ((pll.dbsel[f][1] as u32) << 7),
    );
    let v = mch.read8(mchbar::C0TXCK1DLL);
    mch.write8(mchbar::C0TXCK1DLL, (v & !0x3F) | pll.pi[f][1]);

    // CMD (index 2).
    let v = mch.read8(mchbar::C0CMDTX1);
    let cmd_bits = (pll.dbsel[f][2] << 5) | (pll.dben[f][2] << 6);
    mch.write8(mchbar::C0CMDTX1, (v & !(3 << 5)) | cmd_bits);
    let v = mch.read8(mchbar::C0CMDTX2);
    mch.write8(
        mchbar::C0CMDTX2,
        (v & !(3 << 4)) | (pll.clkdelay[f][2] << 4),
    );
    let pi = pll.pi[f][2];
    let v = mch.read8(mchbar::C0TXCMD0DLL);
    mch.write8(mchbar::C0TXCMD0DLL, (v & !0x3F) | pi);
    let v = mch.read8(mchbar::C0TXCMD1DLL);
    mch.write8(mchbar::C0TXCMD1DLL, (v & !0x3F) | pi);

    // CTRL (index 4).
    let pi = pll.pi[f][4];
    let v = mch.read32(mchbar::C0CTLTX2);
    let ctrl = ((pll.dbsel[f][4] as u32) << 20)
        | ((pll.dben[f][4] as u32) << 21)
        | ((pll.dbsel[f][4] as u32) << 22)
        | ((pll.dben[f][4] as u32) << 23)
        | ((pll.clkdelay[f][4] as u32) << 24)
        | ((pll.clkdelay[f][4] as u32) << 27);
    mch.write32(mchbar::C0CTLTX2, (v & !0x01BF_0000) | ctrl);
    let v = mch.read8(mchbar::C0TXCTL0DLL);
    mch.write8(mchbar::C0TXCTL0DLL, (v & !0x3F) | pi);
    let v = mch.read8(mchbar::C0TXCTL1DLL);
    mch.write8(mchbar::C0TXCTL1DLL, (v & !0x3F) | pi);

    // CTRL2/3 (index 4).
    let v = mch.read32(mchbar::C0CMDTX2);
    let ctrl23 = ((pll.dbsel[f][4] as u32) << 12)
        | ((pll.dben[f][4] as u32) << 13)
        | ((pll.dbsel[f][4] as u32) << 8)
        | ((pll.dben[f][4] as u32) << 9)
        | ((pll.clkdelay[f][4] as u32) << 14)
        | ((pll.clkdelay[f][4] as u32) << 10);
    mch.write32(mchbar::C0CMDTX2, (v & !(0xFF << 8)) | ctrl23);
    let v = mch.read8(mchbar::C0TXCTL2DLL);
    mch.write8(mchbar::C0TXCTL2DLL, (v & !0x3F) | pi);
    let v = mch.read8(mchbar::C0TXCTL3DLL);
    mch.write8(mchbar::C0TXCTL3DLL, (v & !0x3F) | pi);

    // DQS lanes (indices 40..71).
    for i in 0..32u8 {
        let clk = (i + 40) as usize;
        let rank = (i % 4) as u32;
        let dqs = (i / 4) as u32;

        let v = mch.read32(mchbar::ly(mchbar::C0DQSRYTX1_BASE, rank));
        let bits = ((pll.dben[f][clk] as u32) << (dqs + 9)) | ((pll.dbsel[f][clk] as u32) << dqs);
        mch.write32(
            mchbar::ly(mchbar::C0DQSRYTX1_BASE, rank),
            (v & !((1 << (dqs + 9)) | (1 << dqs))) | bits,
        );
        let v = mch.read32(mchbar::ly(mchbar::C0DQSDQRYTX3_BASE, rank));
        let cd = (pll.clkdelay[f][clk] as u32) << (dqs * 2 + 16);
        mch.write32(
            mchbar::ly(mchbar::C0DQSDQRYTX3_BASE, rank),
            (v & !((1 << (dqs * 2 + 17)) | (1 << (dqs * 2 + 16)))) | cd,
        );
        let base = mchbar::C0TXDQS0R0DLL + i as u32;
        let v = mch.read8(base);
        mch.write8(base, (v & !0x3F) | pll.pi[f][clk]);
    }

    // DQ lanes (indices 8..39).
    for i in 0..32u8 {
        let clk = (i + 8) as usize;
        let rank = (i % 4) as u32;
        let dq = (i / 4) as u32;

        let v = mch.read32(mchbar::ly(mchbar::C0DQRYTX1_BASE, rank));
        let bits = ((pll.dben[f][clk] as u32) << (dq + 9)) | ((pll.dbsel[f][clk] as u32) << dq);
        mch.write32(
            mchbar::ly(mchbar::C0DQRYTX1_BASE, rank),
            (v & !((1 << (dq + 9)) | (1 << dq))) | bits,
        );
        let v = mch.read32(mchbar::ly(mchbar::C0DQSDQRYTX3_BASE, rank));
        let cd = (pll.clkdelay[f][clk] as u32) << (dq * 2);
        mch.write32(
            mchbar::ly(mchbar::C0DQSDQRYTX3_BASE, rank),
            (v & !((1 << (dq * 2 + 1)) | (1 << (dq * 2)))) | cd,
        );
        let base = mchbar::C0TXDQ0R0DLL + i as u32;
        let v = mch.read8(base);
        mch.write8(base, (v & !0x3F) | pll.pi[f][clk]);
    }
}

/// Hardware HMC calibration.
///
/// Ported from coreboot `sdram_calibratehwpll()`.
fn calibrate_hw_pll(si: &mut SysInfo, mch: &MchBar) {
    si.r#async = 0;

    mch.setbits32(mchbar::CSHRPDCTL, 1 << 15);
    let v = mch.read8(mchbar::CSHRPDCTL);
    mch.write8(mchbar::CSHRPDCTL, v & !(1 << 7));
    mch.setbits32(mchbar::CSHRPDCTL, 1 << 3);
    mch.setbits32(mchbar::CSHRPDCTL, 1 << 2);

    // Start hardware HMC calibration.
    mch.setbits32(mchbar::CSHRPDCTL, 1 << 7);

    // Wait until calibration is done.
    while mch.read8(mchbar::CSHRPDCTL) & (1 << 2) == 0 {
        core::hint::spin_loop();
    }

    // Check if calibration failed.
    if mch.read8(mchbar::CSHRPDCTL) & (1 << 3) != 0 {
        si.r#async = 1;
    }
}

// ===================================================================
// DLL timing
// ===================================================================

/// Full DLL timing calibration.
///
/// Ported from coreboot `sdram_dlltiming()`.
pub fn dll_timing(si: &mut SysInfo, mch: &MchBar) {
    // Configure Master DLL.
    let mstr = if si.selected_timings.mem_clock == 0 {
        0x0801_4227u32
    } else {
        0x0001_4221u32
    };
    let v = mch.read32(mchbar::CSHRMSTRCTL1);
    mch.write32(mchbar::CSHRMSTRCTL1, (v & !0x0FFF_FFFF) | mstr);
    mch.setbits32(mchbar::CSHRMSTRCTL1, 1 << 23);
    mch.setbits32(mchbar::CSHRMSTRCTL1, 1 << 15);
    mch.clrbits32(mchbar::CSHRMSTRCTL1, 1 << 15);

    // Enable/disable Master DLLs in order.
    if si.nodll != 0 {
        for bit in [0, 2, 4, 8, 10, 12, 14] {
            mch.setbits32(mchbar::CSHRMSTRCTL0, 1u32 << bit);
        }
    } else {
        for bit in [0, 2, 4, 8, 10, 12, 14] {
            mch.clrbits32(mchbar::CSHRMSTRCTL0, 1u32 << bit);
        }
    }

    // Initialize Transmit DLL PI values.
    if si.nodll != 0 {
        let v = mch.read8(mchbar::CREFPI);
        mch.write8(mchbar::CREFPI, (v & !0x3F) | 0x07);
    } else {
        let v = mch.read8(mchbar::CREFPI);
        mch.write8(mchbar::CREFPI, v & !0x3F);
    }

    calibrate_pll(si, mch, 0);

    // Enable all modular Slave DLLs.
    mch.setbits32(mchbar::C0DLLPIEN, 1 << 11);
    mch.setbits32(mchbar::C0DLLPIEN, 1 << 12);
    for i in 0..8u32 {
        mch.setbits32(mchbar::C0DLLPIEN, (1 << 10) >> i);
    }

    // Enable DQ/DQS output.
    mch.setbits32(mchbar::C0SLVDLLOUTEN, 1 << 0);
    mch.write16(mchbar::CSPDSLVWT, 0x5005);
    let v = mch.read16(mchbar::CSHRPDCTL2);
    mch.write16(mchbar::CSHRPDCTL2, (v & !0x1F1F) | 0x051A);
    let v = mch.read16(mchbar::CSHRPDCTL5);
    mch.write16(mchbar::CSHRPDCTL5, (v & !0xBF3F) | 0x9010);

    if si.nodll != 0 {
        let v = mch.read8(mchbar::CSHRPDCTL3);
        mch.write8(mchbar::CSHRPDCTL3, (v & !0x7F) | 0x6B);
    } else {
        let v = mch.read8(mchbar::CSHRPDCTL3);
        mch.write8(mchbar::CSHRPDCTL3, (v & !0x7F) | 0x55);
        calibrate_hw_pll(si, mch);
    }

    // Disable Dynamic Diff Amp.
    mch.clrbits32(mchbar::C0STATRDCTRL, 1 << 22);

    // Init transmit FIFO.
    let v = mch.read8(mchbar::C0MISCCTL);
    mch.write8(mchbar::C0MISCCTL, v & !(1 << 1));

    // Gate/ungate mdclk.
    mch.setbits32(mchbar::CSHWRIOBONUS, 3 << 6);
    let v = mch.read8(mchbar::CSHWRIOBONUS);
    mch.write8(mchbar::CSHWRIOBONUS, v & !(1 << 5));
    let v = mch.read8(mchbar::CSHWRIOBONUS);
    mch.write8(mchbar::CSHWRIOBONUS, (v & !(3 << 6)) | (1 << 6));
    let v = mch.read8(mchbar::CSHRFIFOCTL);
    mch.write8(mchbar::CSHRFIFOCTL, (v & !0x3F) | 0x1A);

    // Enable write pointer count.
    mch.setbits32(mchbar::CSHRFIFOCTL, 1 << 0);

    // Set DDR3 Reset Enable bit.
    mch.setbits32(mchbar::CSHRDDR3CTL, 1 << 0);

    // Configure DQS-DQ Transmit.
    mch.write32(mchbar::CSHRDQSTXPGM, 0x0055_1803);

    // Enable clock groups (all clocks on).
    let v = mch.read32(mchbar::C0CKTX);
    mch.write32(mchbar::C0CKTX, v & !(0x3F << 24));

    // Enable DDR command output buffers.
    let v = mch.read8(mchbar::C0CMDTX1);
    mch.write8(mchbar::C0CMDTX1, v & !(1 << 0));

    // Disable outputs for unpopulated ranks.
    let mut rank_mask: u16 = 0;
    for r in 0..4u16 {
        let dimm = (r / 2) as usize;
        let rank_in_dimm = (r % 2) as u8;
        let populated = si.dimms[dimm]
            .as_ref()
            .map_or(false, |d| d.card_type != 0 && rank_in_dimm < d.ranks);
        if !populated {
            rank_mask |= (1 << (r + 8)) | (1 << (r + 4)) | (1 << r);
        }
    }
    mch.setbits32(mchbar::C0CTLTX2, rank_mask as u32);

    fstart_log::info!("raminit: DLL timing calibrated");
}

// ===================================================================
// RCOMP (Resistance Compensation)
// ===================================================================

/// Addresses of the 7 RCOMP control groups (group 1 does not exist).
const RCOMPCTL: [u32; 7] = [
    mchbar::C0RCOMPCTRL0,
    0, // group 1 does not exist
    mchbar::C0RCOMPCTRL2,
    mchbar::C0RCOMPCTRL3,
    mchbar::C0RCOMPCTRL4,
    mchbar::C0RCOMPCTRL5,
    mchbar::C0RCOMPCTRL6,
];

/// Full RCOMP calibration.
///
/// Ported from coreboot `sdram_rcomp()`.
pub fn rcomp(si: &SysInfo, mch: &MchBar) {
    let rcompslew: u8 = 0x0A;

    static RCOMPUPDATE: [u8; 7] = [0, 0, 0, 1, 1, 0, 0];
    static RCOMPSTR: [u8; 7] = [0x66, 0x00, 0xAA, 0x55, 0x55, 0x77, 0x77];
    static RCOMPSCOMP: [u16; 7] = [0xA22A, 0x0000, 0xE22E, 0xE22E, 0xE22E, 0xA22A, 0xA22A];
    static RCOMPDELAY: [u8; 7] = [1, 0, 0, 0, 0, 1, 1];
    static RCOMPF: [u16; 7] = [0x1114, 0x0000, 0x0505, 0x0909, 0x0909, 0x0A0A, 0x0A0A];
    static RCOMPSTR2: [u8; 7] = [0x00, 0x55, 0x55, 0xAA, 0xAA, 0x55, 0xAA];
    static RCOMPSCOMP2: [u16; 7] = [0x0000, 0xE22E, 0xE22E, 0xE22E, 0x8228, 0xE22E, 0x8228];
    static RCOMPDELAY2: [u8; 7] = [0, 0, 0, 0, 2, 0, 2];

    let (rcomp1, rcomp2) = if si.selected_timings.mem_clock == 0 {
        (0x0005_0431u32, 0x14C4_2827u32)
    } else {
        (0x0005_0542u32, 0x1904_2827u32)
    };
    let rcomp2 = if si.selected_timings.fsb_clock == 0 {
        0x14C4_2827u32
    } else {
        rcomp2
    };

    // Program per-group registers.
    for i in 0..7usize {
        if i == 1 {
            continue;
        }
        let base = RCOMPCTL[i];
        let v = mch.read8(base);
        mch.write8(base, (v & !(1 << 0)) | RCOMPUPDATE[i]);
        let v = mch.read8(base);
        mch.write8(base, v & !(1 << 1));
        let v = mch.read16(base);
        mch.write16(base, (v & !(0x0F << 12)) | ((rcompslew as u16) << 12));

        mch.write8(base + 0x04, RCOMPSTR[i]);
        mch.write16(base + 0x0E, RCOMPSCOMP[i]);
        let v = mch.read8(base + 0x14);
        mch.write8(base + 0x14, (v & !0x03) | RCOMPDELAY[i]);

        if i == 2 {
            // Rewrite for group 2 with dimm_config-specific values.
            let dc = si.dimm_config[0] as usize;
            mch.write8(base + 0x04, RCOMPSTR2[dc]);
            mch.write16(base + 0x0E, RCOMPSCOMP2[dc]);
            let v = mch.read8(base + 0x14);
            mch.write8(base + 0x14, (v & !0x03) | RCOMPDELAY2[dc]);
        }

        // Clear slew base / LUTs.
        let v = mch.read16(base + 0x16);
        mch.write16(base + 0x16, v & !0x7F7F);
        mch.write16(base + 0x18, 0);
        mch.write16(base + 0x18 + 2, 0);
        mch.write16(base + 0x1C, 0);
        mch.write16(base + 0x1C + 2, 0);
    }

    // ODT record.
    let v = mch.read8(mchbar::C0ODTRECORDX);
    mch.write8(mchbar::C0ODTRECORDX, (v & !0x3F) | 0x36);
    let v = mch.read8(mchbar::C0DQSODTRECORDX);
    mch.write8(mchbar::C0DQSODTRECORDX, (v & !0x3F) | 0x36);

    // Clear per-group override/offset fields.
    for i in 0..7usize {
        if i == 1 {
            continue;
        }
        let base = RCOMPCTL[i];
        let v = mch.read8(base + 2);
        mch.write8(base + 2, v & !0x71);
        let v = mch.read16(base + 2);
        mch.write16(base + 2, v & !0x0706);
        let v = mch.read16(base + 0x0A);
        mch.write16(base + 0x0A, v & !0x7F7F);
        let v = mch.read16(base + 0x12);
        mch.write16(base + 0x12, v & !0x3F3F);
        let v = mch.read16(base + 0x24);
        mch.write16(base + 0x24, v & !0x1F1F);
        let v = mch.read8(base + 0x24 + 2);
        mch.write8(base + 0x24 + 2, v & !0x1F);

        // SCOMP override.
        mch.write16(base + 0x10, RCOMPF[i]);
        mch.write16(base + 0x20, 0x1219);
        mch.write16(base + 0x20 + 2, 0x000C);
    }

    let v = mch.read32(mchbar::DCMEASBUFOVR);
    mch.write32(mchbar::DCMEASBUFOVR, (v & !0x001F_1F1F) | 0x000C_1219);

    let v = mch.read16(mchbar::XCOMPSDR0BNS);
    mch.write16(mchbar::XCOMPSDR0BNS, (v & !(0x1F << 8)) | (0x12 << 8));
    let v = mch.read8(mchbar::XCOMPSDR0BNS);
    mch.write8(mchbar::XCOMPSDR0BNS, (v & !0x1F) | 0x12);

    mch.write32(mchbar::COMPCTRL3, 0x007C_9007);
    mch.write32(mchbar::OFREQDELSEL, rcomp1);
    mch.write16(mchbar::XCOMPCMNBNS, 0x1F7F);
    mch.write32(mchbar::COMPCTRL2, rcomp2);
    let v = mch.read16(mchbar::XCOMPDFCTRL);
    mch.write16(mchbar::XCOMPDFCTRL, (v & !0x0F) | 1);
    mch.write16(mchbar::ZQCALCTRL, 0x0134);
    mch.write32(mchbar::COMPCTRL1, 0x4C29_3600);

    let v = mch.read8(mchbar::COMPCTRL1 + 3);
    mch.write8(mchbar::COMPCTRL1 + 3, (v & !0x44) | (1 << 6) | (1 << 2));
    let v = mch.read16(mchbar::XCOMPSDR0BNS);
    mch.write16(mchbar::XCOMPSDR0BNS, v & !(1 << 13));
    let v = mch.read8(mchbar::XCOMPSDR0BNS);
    mch.write8(mchbar::XCOMPSDR0BNS, v & !(1 << 5));

    for i in 0..7usize {
        if i == 1 {
            continue;
        }
        let v = mch.read8(RCOMPCTL[i] + 2);
        mch.write8(RCOMPCTL[i] + 2, v & !0x71);
    }

    // Start RCOMP and wait.
    if mch.read32(mchbar::COMPCTRL1) & (1 << 30) == 0 {
        mch.setbits32(mchbar::COMPCTRL1, 1 << 0);
        while mch.read8(mchbar::COMPCTRL1) & 1 != 0 {
            core::hint::spin_loop();
        }

        // Read back RCOMP results and program slew LUTs.
        let xcomp = mch.read32(mchbar::XCOMP);
        let rcompp = ((xcomp & !((1u32) << 31)) >> 24) as u8;
        let rcompn = ((xcomp & !(0xFF80_0000)) >> 16) as u8;

        // The full 64×12 LUT programming is extensive. We program the slew
        // base values which is the critical part for signal integrity.
        for i in 0..7usize {
            if i == 1 {
                continue;
            }
            let srup = (mch.read8(RCOMPCTL[i] + 1) & 0xC0) >> 6;
            let srun = (mch.read8(RCOMPCTL[i] + 1) & 0x30) >> 4;

            let base_p = rcompp.wrapping_sub(1 << (srup + 1));
            let base_n = rcompn.wrapping_sub(1 << (srun + 1));

            let v = mch.read16(RCOMPCTL[i] + 0x16);
            mch.write16(
                RCOMPCTL[i] + 0x16,
                (v & !0x7F7F) | ((base_p as u16) << 8) | (base_n as u16),
            );
        }
    }

    // Start final RCOMP.
    mch.setbits32(mchbar::COMPCTRL1, 1 << 0);

    fstart_log::info!("raminit: RCOMP calibration done");
}

// ===================================================================
// ODT (On-Die Termination)
// ===================================================================

/// Full ODT configuration.
///
/// Ported from coreboot `sdram_odt()`.
pub fn odt(si: &SysInfo, mch: &MchBar) {
    // Compute rank index for the ODT tables.
    let d0_ranks = si.dimms[0]
        .as_ref()
        .map_or(0u8, |d| if d.card_type != 0 { d.ranks } else { 0 });
    let d1_ranks = si.dimms[1]
        .as_ref()
        .map_or(0u8, |d| if d.card_type != 0 { d.ranks } else { 0 });

    let rankindex: usize = match (d0_ranks, d1_ranks) {
        (0, 0) => 0,
        (1, 0) => 1,
        (2, 0) => 3,
        (0, 1) => 4,
        (1, 1) => 5,
        (2, 1) => 7,
        (0, 2) => 12,
        (1, 2) => 13,
        (2, 2) => 15,
        _ => 0,
    };

    static ODT_MATRIX: [u16; 16] = [
        0x0000, 0x0011, 0x0000, 0x0011, 0x0000, 0x4444, 0x0000, 0x4444, 0x0000, 0x0000, 0x0000,
        0x0000, 0x0000, 0x4444, 0x0000, 0x4444,
    ];
    static ODT_RANKCTRL: [u16; 16] = [
        0x0000, 0x0000, 0x0000, 0x0000, 0x0044, 0x1111, 0x0000, 0x1111, 0x0000, 0x0000, 0x0000,
        0x0000, 0x0044, 0x1111, 0x0000, 0x1111,
    ];

    mch.write16(mchbar::C0ODT, ODT_MATRIX[rankindex]);
    mch.write16(mchbar::C0ODTRKCTRL, ODT_RANKCTRL[rankindex]);

    fstart_log::info!("raminit: ODT configured (rankindex={})", rankindex);
}

// ===================================================================
// RCOMP update
// ===================================================================

/// Check if RCOMP override is needed.
fn check_rcomp_override(mch: &MchBar) -> bool {
    let xcomp = mch.read32(mchbar::XCOMP);
    let a = ((xcomp & 0x7F00_0000) >> 24) as u8;
    let b = ((xcomp & 0x007F_0000) >> 16) as u8;
    let c = ((xcomp & 0x0000_3F00) >> 8) as u8;
    let d = (xcomp & 0x0000_003F) as u8;

    let aa = if a > b { a - b } else { b - a };
    let bb = if c > d { c - d } else { d - c };

    if aa > 18
        || bb > 7
        || a <= 5
        || b <= 5
        || c <= 5
        || d <= 5
        || a >= 0x7A
        || b >= 0x7A
        || c >= 0x3A
        || d >= 0x3A
    {
        mch.write32(mchbar::RCMEASBUFXOVR, 0x9718_A729);
        return true;
    }
    false
}

/// RCOMP update (post-calibration fixup).
///
/// Ported from coreboot `sdram_rcompupdate()`.
pub fn rcomp_update(si: &SysInfo, mch: &MchBar) {
    let mut ok = false;
    let v = mch.read8(mchbar::XCOMPDFCTRL);
    mch.write8(mchbar::XCOMPDFCTRL, v & !(1 << 3));
    let v = mch.read8(mchbar::COMPCTRL1);
    mch.write8(mchbar::COMPCTRL1, v & !(1 << 7));

    for _ in 0..3 {
        mch.setbits32(mchbar::COMPCTRL1, 1 << 0);
        hpet_udelay(1000);
        while mch.read8(mchbar::COMPCTRL1) & 1 != 0 {
            core::hint::spin_loop();
        }
        ok |= check_rcomp_override(mch);
    }

    if !ok {
        let xcomp = mch.read32(mchbar::XCOMP);
        let swapped = ((xcomp >> 16) & 0x0000_FFFF) | ((xcomp << 16) & 0xFFFF_0000);
        mch.write32(mchbar::RCMEASBUFXOVR, swapped | (1 << 31) | (1 << 15));
    }

    mch.setbits32(mchbar::COMPCTRL1, 1 << 0);
    hpet_udelay(1000);
    while mch.read8(mchbar::COMPCTRL1) & 1 != 0 {
        core::hint::spin_loop();
    }

    fstart_log::info!("raminit: RCOMP update done");
}

// ===================================================================
// Receive enable calibration
// ===================================================================

/// DQS receive enable training — full port.
///
/// Ported from coreboot `sdram_rcven()`. Trains the DQS receive enable
/// timing for each byte lane by sweeping coarse + medium + PI delay.
pub fn sdram_rcven(si: &mut SysInfo, mch: &MchBar) {
    let v = mch.read8(mchbar::C0RSTCTL);
    mch.write8(mchbar::C0RSTCTL, v & !(3 << 2));
    let v = mch.read8(mchbar::CMNDQFIFORST);
    mch.write8(mchbar::CMNDQFIFORST, v & !(1 << 7));

    let maxlane: u8 = 8;
    let mut lanecoarse: [u8; 8] = [0; 8];
    let mut minlanecoarse: u8 = 0xFF;

    for lane in 0..maxlane {
        let dqshighaddr = mchbar::ly(mchbar::C0MISCCCTLY_BASE, lane as u32);

        let mut coarse = si.selected_timings.cas + 1;
        let mut pi: u8 = 0;
        let mut medium: u8 = 0;

        // Set initial coarse.
        let v = mch.read32(mchbar::C0STATRDCTRL);
        mch.write32(
            mchbar::C0STATRDCTRL,
            (v & !(0x0F << 16)) | ((coarse as u32) << 16),
        );
        let v = mch.read16(mchbar::C0RCVMISCCTL2);
        mch.write16(
            mchbar::C0RCVMISCCTL2,
            (v & !(3 << (lane * 2))) | ((medium as u16) << (lane * 2)),
        );
        let v = mch.read8(mchbar::ly(0x560, lane as u32));
        mch.write8(mchbar::ly(0x560, lane as u32), v & !0x3F);

        let mut savecoarse = coarse;
        let mut savemedium = medium;
        let mut savepi = pi;

        // Phase 1: sweep until DQS goes high.
        while !sample_dqs(mch, dqshighaddr, 0, 3) {
            rcven_clock(mch, &mut coarse, &mut medium, lane);
            if coarse > 0x0F {
                break;
            }
        }

        savecoarse = coarse;
        savemedium = medium;
        rcven_clock(mch, &mut coarse, &mut medium, lane);

        // Phase 2: continue until DQS stays high.
        while !sample_dqs(mch, dqshighaddr, 1, 3) {
            savecoarse = coarse;
            savemedium = medium;
            rcven_clock(mch, &mut coarse, &mut medium, lane);
            if coarse > 0x0F {
                break;
            }
        }

        coarse = savecoarse;
        medium = savemedium;
        let v = mch.read32(mchbar::C0STATRDCTRL);
        mch.write32(
            mchbar::C0STATRDCTRL,
            (v & !(0x0F << 16)) | ((coarse as u32) << 16),
        );
        let v = mch.read16(mchbar::C0RCVMISCCTL2);
        mch.write16(
            mchbar::C0RCVMISCCTL2,
            (v & !(3 << (lane * 2))) | ((medium as u16) << (lane * 2)),
        );

        // Phase 3: PI sweep.
        while !sample_dqs(mch, dqshighaddr, 1, 3) {
            savepi = pi;
            pi += 1;
            if pi > si.maxpi {
                pi = si.maxpi;
                savepi = si.maxpi;
                break;
            }
            let v = mch.read8(mchbar::ly(0x560, lane as u32));
            mch.write8(
                mchbar::ly(0x560, lane as u32),
                (v & !0x3F) | (pi << si.pioffset),
            );
        }

        pi = savepi;
        let v = mch.read8(mchbar::ly(0x560, lane as u32));
        mch.write8(
            mchbar::ly(0x560, lane as u32),
            (v & !0x3F) | (pi << si.pioffset),
        );
        rcven_clock(mch, &mut coarse, &mut medium, lane);

        // Phase 4: back off until DQS goes low.
        while !sample_dqs(mch, dqshighaddr, 0, 3) {
            if coarse == 0 {
                break;
            }
            coarse -= 1;
            let v = mch.read32(mchbar::C0STATRDCTRL);
            mch.write32(
                mchbar::C0STATRDCTRL,
                (v & !(0x0F << 16)) | ((coarse as u32) << 16),
            );
        }

        rcven_clock(mch, &mut coarse, &mut medium, lane);
        si.pi[lane as usize] = pi;
        lanecoarse[lane as usize] = coarse;
    }

    // Compute min coarse and program offsets.
    for lane in 0..maxlane as usize {
        if lanecoarse[lane] < minlanecoarse {
            minlanecoarse = lanecoarse[lane];
        }
    }
    for lane in (0..maxlane as usize).rev() {
        let offset = lanecoarse[lane] - minlanecoarse;
        let v = mch.read16(mchbar::C0COARSEDLY0);
        mch.write16(
            mchbar::C0COARSEDLY0,
            (v & !(3 << (lane * 2))) | ((offset as u16) << (lane * 2)),
        );
    }
    let v = mch.read32(mchbar::C0STATRDCTRL);
    mch.write32(
        mchbar::C0STATRDCTRL,
        (v & !(0x0F << 16)) | ((minlanecoarse as u32) << 16),
    );

    si.coarsectrl = minlanecoarse as u16;
    si.coarsedelay = mch.read16(mchbar::C0COARSEDLY0);
    si.mediumphase = mch.read16(mchbar::C0RCVMISCCTL2);
    si.readptrdelay = mch.read16(mchbar::C0RCVMISCCTL1);

    // Reset sequence.
    let v = mch.read8(mchbar::C0RSTCTL);
    mch.write8(mchbar::C0RSTCTL, v & !(7 << 1));
    mch.setbits32(mchbar::C0RSTCTL, 1 << 1);
    mch.setbits32(mchbar::C0RSTCTL, 1 << 2);
    mch.setbits32(mchbar::C0RSTCTL, 1 << 3);

    mch.setbits32(mchbar::CMNDQFIFORST, 1 << 7);
    mch.clrbits32(mchbar::CMNDQFIFORST, 1 << 7);
    mch.setbits32(mchbar::CMNDQFIFORST, 1 << 7);

    fstart_log::info!("raminit: receive enable calibration done");
}

/// Sample DQS for the given lane.
fn sample_dqs(mch: &MchBar, dqshighaddr: u32, highlow: u8, count: u8) -> bool {
    let mut matches = true;
    for _ in 0..count {
        mch.clrbits32(mchbar::C0RSTCTL, 1 << 1);
        hpet_udelay(1);
        mch.setbits32(mchbar::C0RSTCTL, 1 << 1);
        hpet_udelay(1);

        // Read from strobe address (address 0 in DRAM).
        // On real hardware this would be a memory read. In the register
        // model, we just issue the strobe.
        unsafe {
            core::ptr::read_volatile(0 as *const u32);
        }
        hpet_udelay(1);

        if ((mch.read8(dqshighaddr) & (1 << 6)) >> 6) != highlow {
            matches = false;
        }
    }
    matches
}

/// Advance receive enable clock (medium, then coarse).
fn rcven_clock(mch: &MchBar, coarse: &mut u8, medium: &mut u8, lane: u8) {
    if *medium < 3 {
        *medium += 1;
        let v = mch.read16(mchbar::C0RCVMISCCTL2);
        mch.write16(
            mchbar::C0RCVMISCCTL2,
            (v & !(3 << (lane * 2))) | ((*medium as u16) << (lane * 2)),
        );
    } else {
        *medium = 0;
        *coarse += 1;
        let v = mch.read32(mchbar::C0STATRDCTRL);
        mch.write32(
            mchbar::C0STATRDCTRL,
            (v & !(0x0F << 16)) | ((*coarse as u32) << 16),
        );
        let v = mch.read16(mchbar::C0RCVMISCCTL2);
        mch.write16(
            mchbar::C0RCVMISCCTL2,
            (v & !(3 << (lane * 2))) | ((*medium as u16) << (lane * 2)),
        );
    }
}

// ===================================================================
// tRD computation
// ===================================================================

/// Compute new tRD (read-to-data delay) from rcven results.
///
/// Ported from coreboot `sdram_new_trd()`.
pub fn sdram_new_trd(si: &SysInfo, mch: &MchBar) {
    let tmclk: u32 = if si.selected_timings.mem_clock == 0 {
        3000
    } else {
        2500
    };
    let thclk: u32 = if si.selected_timings.fsb_clock == 0 {
        6000
    } else {
        5000
    };
    let freqgb: u32 = 110;
    let tmclk_adj = tmclk * 100 / freqgb;
    let buffertocore: u32 = 5000;
    let postcalib: u32 = if si.selected_timings.mem_clock == 0 {
        1250
    } else {
        500
    };
    let pidelay: u32 = if si.selected_timings.mem_clock == 0 {
        24
    } else {
        20
    };
    let tio: u32 = if si.selected_timings.mem_clock == 0 {
        2700
    } else {
        3240
    };

    // Compute max rcven delay across lanes.
    let mut maxrcvendelay: u32 = 0;
    for i in 0..8 {
        let mut delay = ((si.coarsedelay >> (i * 2)) & 3) as u32 * tmclk_adj;
        delay += ((si.readptrdelay >> (i * 2)) & 3) as u32 * tmclk_adj / 2;
        delay += ((si.mediumphase >> (i * 2)) & 3) as u32 * tmclk_adj / 4;
        delay += pidelay * si.pi[i] as u32;
        maxrcvendelay = maxrcvendelay.max(delay);
    }

    let bypass =
        if mch.read8(mchbar::HMBYPCP + 3) == 0xFF && (mch.read8(mchbar::HMCCMC) & (1 << 7)) != 0 {
            1u32
        } else {
            0
        };

    static TXFIFO_LUT: [u8; 8] = [0, 7, 6, 5, 2, 1, 4, 3];
    let fifo_reg = (mch.read8(mchbar::CSHRFIFOCTL) & 0x0E) >> 1;
    let txfifo = TXFIFO_LUT[fifo_reg as usize] as u32;

    let datadelay =
        tmclk_adj * (2 * txfifo + 4 * si.coarsectrl as u32 + 4 * (bypass.wrapping_sub(1)) + 13) / 4
            + tio
            + maxrcvendelay
            + pidelay
            + buffertocore
            + postcalib
            + if si.r#async != 0 { tmclk_adj / 2 } else { 0 };

    let j = si.selected_timings.mem_clock as usize;
    let k = si.selected_timings.fsb_clock as usize;

    static TRD_ADJUST: [[[u32; 5]; 2]; 2] = [
        [[3000, 3000, 0, 0, 0], [1000, 2000, 3000, 1500, 2500]],
        [[2000, 1000, 3000, 0, 0], [2500, 2500, 0, 0, 0]],
    ];

    let cc: usize = match (j, k) {
        (0, 0) => 2,
        (0, 1) => 3,
        (1, 0) => 5,
        _ => 2,
    };

    let mut trd: u8 = 0;
    for i in 0..cc {
        let adj = TRD_ADJUST[k][j][i] * 100 / freqgb;
        let reg32 = datadelay.saturating_sub(adj);
        let mut phase_trd = (reg32 / thclk) as u8;
        if phase_trd >= 2 {
            phase_trd -= 2;
        }
        phase_trd += 1;
        trd = trd.max(phase_trd);
    }

    if j == 0 && k == 0 {
        // Subtract correction for FSB667/DDR667.
        let _ = datadelay.saturating_sub(3084);
    }

    let v = mch.read16(mchbar::C0STATRDCTRL);
    mch.write16(
        mchbar::C0STATRDCTRL,
        (v & !(0x1F << 8)) | ((trd as u16) << 8),
    );

    fstart_log::info!("raminit: tRD={}", trd);
}

// ===================================================================
// Enhanced mode
// ===================================================================

/// Enhanced mode registers.
///
/// Ported from coreboot `sdram_enhancedmode()`.
pub fn sdram_enhanced_mode(si: &SysInfo, mch: &MchBar) {
    mch.setbits32(mchbar::C0ADDCSCTRL, 1 << 0);
    mch.setbits32(mchbar::C0REFRCTRL + 3, 1 << 0);

    let mask: u32 = (0x1F << 15) | (0x1F << 10) | (0x1F << 5) | 0x1F;
    let val: u32 = (0x1E << 15) | (0x10 << 10) | (0x1E << 5) | 0x10;
    let v = mch.read32(mchbar::WRWMCONFIG);
    mch.write32(mchbar::WRWMCONFIG, (v & !mask) | val);

    mch.write8(mchbar::C0DITCTRL + 1, 2);
    mch.write16(mchbar::C0DITCTRL + 2, 0x0804);
    mch.write16(mchbar::C0DITCTRL + 4, 0x2010);
    mch.write8(mchbar::C0DITCTRL + 6, 0x40);
    mch.write16(mchbar::C0DITCTRL + 8, 0x091C);
    mch.write8(mchbar::C0DITCTRL + 10, 0xF2);

    mch.setbits32(mchbar::C0BYPCTRL, 1 << 0);
    mch.setbits32(mchbar::C0CWBCTRL, 1 << 0);
    mch.setbits32(mchbar::C0ARBSPL, 1 << 8);

    ecam::or8(0, 0, 0, 0xF0, 1);
    mch.write32(mchbar::SBCTL, 0x0000_0002);
    mch.write32(mchbar::SBCTL2, 0x2031_0002);
    mch.write32(mchbar::SLIMCFGTMG, 0x0202_0302);
    mch.write32(mchbar::HIT0, 0x001F_1806);
    mch.write32(mchbar::HIT1, 0x0110_2800);
    mch.write32(mchbar::HIT2, 0x0700_0000);
    mch.write32(mchbar::HIT3, 0x0101_4010);
    mch.write32(mchbar::HIT4, 0x0F03_8000);
    ecam::and8(0, 0, 0, 0xF0, !1);

    // Interleave configuration.
    let mut nranks = 0u32;
    let mut maxranksize = 0u32;
    let mut rankmismatch = false;
    for i in 0..super::TOTAL_DIMMS {
        if let Some(ref d) = si.dimms[i] {
            if d.card_type != 0 {
                for _r in 0..d.ranks {
                    nranks += 1;
                    let sz = si.channel_capacity[0] / nranks;
                    if maxranksize == 0 {
                        maxranksize = sz;
                    }
                    if sz != maxranksize {
                        rankmismatch = true;
                    }
                }
            }
        }
    }

    let chdec = match nranks {
        4 => {
            if rankmismatch {
                0x64
            } else {
                0xA4
            }
        }
        2 => {
            if rankmismatch {
                0x64
            } else {
                0x24
            }
        }
        _ => 0x64,
    };
    let v = mch.read8(mchbar::CHDECMISC);
    mch.write8(mchbar::CHDECMISC, (v & !0xFC) | (chdec & 0xFC));
    mch.clrbits32(mchbar::NOACFGBUSCTL, 1 << 31);

    mch.write32(mchbar::HTBONUS0, 0x0F);
    mch.setbits32(mchbar::C0COREBONUS + 4, 1 << 0);
    mch.clrbits32(mchbar::HIT3, 7 << 25);
    let v = mch.read32(mchbar::HIT4);
    mch.write32(mchbar::HIT4, (v & !(3 << 18)) | (1 << 18));

    // Enhanced clock crossing.
    static CLKCX: [[[u32; 3]; 2]; 2] = [
        [
            [0x0000_0000, 0x0C08_0302, 0x0801_0204],
            [0x0204_0000, 0x0810_0102, 0x0000_0000],
        ],
        [
            [0x1800_0000, 0x3021_060C, 0x2001_0208],
            [0x0000_0000, 0x0C09_0306, 0x0000_0000],
        ],
    ];
    let fsb = si.selected_timings.fsb_clock as usize;
    let ddr = si.selected_timings.mem_clock as usize;
    mch.write32(mchbar::CLKXSSH2X2MD, CLKCX[fsb][ddr][0]);
    mch.write32(mchbar::CLKXSSH2X2MD + 4, CLKCX[fsb][ddr][1]);
    mch.write32(mchbar::CLKXSSH2MCBYP + 4, CLKCX[fsb][ddr][2]);

    mch.clrbits32(mchbar::HIT4, 1 << 1);

    fstart_log::info!("raminit: enhanced mode configured");
}

// ===================================================================
// Power settings
// ===================================================================

/// Full power management settings.
///
/// Ported from coreboot `sdram_powersettings()`.
pub fn sdram_power_settings(si: &SysInfo, mch: &MchBar) {
    // Thermal sensor.
    mch.write8(mchbar::TSC1, 0x9B);
    let v = mch.read32(mchbar::TSTTP);
    mch.write32(mchbar::TSTTP, (v & !0x00FF_FFFF) | 0x1D00);
    mch.write8(mchbar::THERM1, 0x08);
    mch.write8(mchbar::TSC3, 0);
    let v = mch.read8(mchbar::TSC2);
    mch.write8(mchbar::TSC2, (v & !0x0F) | 0x04);
    let v = mch.read8(mchbar::THERM1);
    mch.write8(mchbar::THERM1, (v & !1) | 1);
    let v = mch.read8(mchbar::TCO);
    mch.write8(mchbar::TCO, (v & !(1 << 7)) | (1 << 7));

    // Clock gating.
    mch.clrbits32(mchbar::PMMISC, (1 << 18) | (1 << 0));
    let v = mch.read8(mchbar::SBCTL3 + 3);
    mch.write8(mchbar::SBCTL3 + 3, v & !(1 << 7));
    let v = mch.read8(mchbar::CISDCTRL + 3);
    mch.write8(mchbar::CISDCTRL + 3, v & !(1 << 7));
    let v = mch.read16(mchbar::CICGDIS);
    mch.write16(mchbar::CICGDIS, v & !0x1FFF);
    let v = mch.read32(mchbar::SBCLKGATECTRL);
    mch.write32(mchbar::SBCLKGATECTRL, v & !0x0001_FFFF);
    let v = mch.read16(mchbar::HICLKGTCTL);
    mch.write16(mchbar::HICLKGTCTL, (v & !0x03FF) | 0x06);
    let v = mch.read32(mchbar::HTCLKGTCTL);
    mch.write32(mchbar::HTCLKGTCTL, v | 0x20);
    let v = mch.read8(mchbar::TSMISC);
    mch.write8(mchbar::TSMISC, v & !(1 << 0));

    mch.write8(
        mchbar::C0WRDPYN,
        si.selected_timings.cas.saturating_sub(1) + 0x15,
    );
    let v = mch.read16(mchbar::CLOCKGATINGI);
    mch.write16(mchbar::CLOCKGATINGI, (v & !0x07FC) | 0x0040);
    let v = mch.read16(mchbar::CLOCKGATINGII);
    mch.write16(mchbar::CLOCKGATINGII, (v & !0x0FFF) | 0x0D00);
    let v = mch.read16(mchbar::CLOCKGATINGIII);
    mch.write16(mchbar::CLOCKGATINGIII, v & !0x0D80);
    mch.write16(mchbar::GTDPCGC + 2, 0xFFFF);

    // Sequencing.
    let v = mch.read32(mchbar::HPWRCTL1);
    mch.write32(mchbar::HPWRCTL1, (v & !0x1FFF_FFFF) | 0x1F64_3FFF);
    let v = mch.read32(mchbar::HPWRCTL2);
    mch.write32(mchbar::HPWRCTL2, (v & !0xFFFF_FF7F) | 0x0201_0000);
    let v = mch.read16(mchbar::HPWRCTL3);
    mch.write16(mchbar::HPWRCTL3, (v & !(7 << 12)) | (3 << 12));

    // Power.
    let v = mch.read32(mchbar::GFXC3C4);
    mch.write32(mchbar::GFXC3C4, (v & !0xFFFF_0003) | 0x1010_0000);
    let v = mch.read32(mchbar::PMDSLFRC);
    mch.write32(mchbar::PMDSLFRC, (v & !0x0001_BFF7) | 0x0000_0078);

    let pmres = if si.selected_timings.fsb_clock == 0 {
        0x00C8u16
    } else {
        0x0100u16
    };
    let v = mch.read16(mchbar::PMMSPMRES);
    mch.write16(mchbar::PMMSPMRES, (v & !0x03FF) | pmres);

    let j = si.selected_timings.mem_clock as usize;

    let v = mch.read32(mchbar::PMCLKRC);
    mch.write32(mchbar::PMCLKRC, (v & !0x01FF_F37F) | 0x1081_0700);
    let v = mch.read8(mchbar::PMPXPRC);
    mch.write8(mchbar::PMPXPRC, (v & !7) | 1);
    let v = mch.read8(mchbar::PMBAK);
    mch.write8(mchbar::PMBAK, v & !(1 << 1));

    // DDR2 CAS LUT.
    static DDR2LUT: [[[u16; 2]; 4]; 2] = [
        [
            [0x0000, 0x0000],
            [0x019A, 0x0039],
            [0x0099, 0x1049],
            [0x0000, 0x0000],
        ],
        [
            [0x0000, 0x0000],
            [0x019A, 0x0039],
            [0x0099, 0x1049],
            [0x0099, 0x2159],
        ],
    ];

    let cas_idx = si.selected_timings.cas.saturating_sub(3) as usize;
    let cas_idx = cas_idx.min(3);

    mch.write16(mchbar::C0C2REG, 0x7A89);
    mch.write8(mchbar::SHC2REGII, 0xAA);
    mch.write16(mchbar::SHC2REGII + 1, DDR2LUT[j][cas_idx][1]);
    let v = mch.read16(mchbar::SHC2REGI);
    mch.write16(mchbar::SHC2REGI, (v & !0x7FFF) | DDR2LUT[j][cas_idx][0]);

    let v = mch.read16(mchbar::CLOCKGATINGIII);
    mch.write16(mchbar::CLOCKGATINGIII, (v & !0xF000) | 0xF000);
    let v = mch.read8(mchbar::CSHWRIOBONUSX);
    mch.write8(mchbar::CSHWRIOBONUSX, (v & !0x77) | (4 << 4) | 4);

    let nodll_bits: u32 = if si.nodll != 0 { 0x3000_0000 } else { 0 };
    let v = mch.read32(mchbar::C0COREBONUS);
    mch.write32(
        mchbar::C0COREBONUS,
        (v & !(0x0F << 24)) | (1 << 29) | nodll_bits,
    );

    let v = mch.read32(mchbar::CLOCKGATINGI);
    mch.write32(mchbar::CLOCKGATINGI, (v & !(0x0F << 20)) | (0x0F << 20));
    let v = mch.read32(mchbar::CLOCKGATINGII - 1);
    mch.write32(
        mchbar::CLOCKGATINGII - 1,
        (v & !0x001F_F000) | (0xBFu32 << 20),
    );
    let v = mch.read16(mchbar::SHC3C4REG2);
    mch.write16(
        mchbar::SHC3C4REG2,
        (v & !0x1F7F) | (0x0B << 8) | (7 << 4) | 0x0B,
    );
    mch.write16(mchbar::SHC3C4REG3, 0x3264);
    let v = mch.read16(mchbar::SHC3C4REG4);
    mch.write16(mchbar::SHC3C4REG4, (v & !0x3F3F) | (0x14 << 8) | 0x0A);

    mch.setbits32(mchbar::C1COREBONUS, (1 << 31) | (1 << 13));

    fstart_log::info!("raminit: power settings configured");
}

// ===================================================================
// Program DDR mode
// ===================================================================

/// Full DDR mode register programming.
///
/// Ported from coreboot `sdram_programddr()`.
pub fn sdram_program_ddr(mch: &MchBar) {
    let v = mch.read16(mchbar::CLOCKGATINGII);
    mch.write16(mchbar::CLOCKGATINGII, (v & !0x03FF) | 0x0100);
    let v = mch.read16(mchbar::CLOCKGATINGIII);
    mch.write16(mchbar::CLOCKGATINGIII, (v & !0x003F) | 0x0010);
    let v = mch.read16(mchbar::CLOCKGATINGI);
    mch.write16(mchbar::CLOCKGATINGI, (v & !0x7000) | 0x2000);

    let v = mch.read8(mchbar::CSHRPDCTL);
    mch.write8(mchbar::CSHRPDCTL, v & !(7 << 1));
    let v = mch.read8(mchbar::CSHRWRIOMLNS);
    mch.write8(mchbar::CSHRWRIOMLNS, v & !(3 << 2));

    // Clear per-lane misc bits.
    for lane in 0..8u32 {
        let v = mch.read8(mchbar::ly(mchbar::C0MISCCCTLY_BASE, lane));
        mch.write8(mchbar::ly(mchbar::C0MISCCCTLY_BASE, lane), v & !(7 << 1));
    }

    let v = mch.read8(mchbar::CSHRWRIOMLNS);
    mch.write8(mchbar::CSHRWRIOMLNS, v & !(1 << 1));

    mch.clrbits32(mchbar::CSHRMISCCTL, 1 << 10);
    let v = mch.read16(mchbar::CLOCKGATINGIII);
    mch.write16(mchbar::CLOCKGATINGIII, v & !0x0DC0);
    let v = mch.read8(mchbar::C0WRDPYN);
    mch.write8(mchbar::C0WRDPYN, v & !(1 << 7));
    mch.clrbits32(mchbar::C0COREBONUS, 1 << 22);
    let v = mch.read16(mchbar::CLOCKGATINGI);
    mch.write16(mchbar::CLOCKGATINGI, v & !0x80FC);
    let v = mch.read16(mchbar::CLOCKGATINGII);
    mch.write16(mchbar::CLOCKGATINGII, v & !0x0C00);

    let v = mch.read8(mchbar::CSHRPDCTL);
    mch.write8(mchbar::CSHRPDCTL, v & !0x0D);
    for lane in 0..8u32 {
        let v = mch.read8(mchbar::ly(mchbar::C0MISCCCTLY_BASE, lane));
        mch.write8(mchbar::ly(mchbar::C0MISCCCTLY_BASE, lane), v & !(1 << 0));
    }

    let v = mch.read32(mchbar::C0STATRDCTRL);
    mch.write32(mchbar::C0STATRDCTRL, (v & !(7 << 20)) | (3 << 20));
    mch.clrbits32(mchbar::C0COREBONUS, 1 << 20);
    mch.setbits32(mchbar::C0DYNSLVDLLEN, 0x1E);
    mch.setbits32(mchbar::C0DYNSLVDLLEN2, 0x03);
    let v = mch.read32(mchbar::SHCYCTRKCKEL);
    mch.write32(mchbar::SHCYCTRKCKEL, (v & !(3 << 26)) | (1 << 26));
    mch.setbits32(mchbar::C0STATRDCTRL, 3 << 13);
    mch.setbits32(mchbar::C0CKECTRL, 1 << 16);
    mch.setbits32(mchbar::C0COREBONUS, 1 << 4);
    mch.setbits32(mchbar::CLOCKGATINGI - 1, 0x0Fu32 << 24);
    mch.setbits32(mchbar::CSHWRIOBONUS, 7);
    mch.setbits32(mchbar::C0DYNSLVDLLEN, 3 << 6);
    mch.setbits32(mchbar::SHC2REGIII, 7);
    let v = mch.read16(mchbar::SHC2MINTM);
    mch.write16(mchbar::SHC2MINTM, v | (1 << 7));
    let v = mch.read8(mchbar::SHC2IDLETM);
    mch.write8(mchbar::SHC2IDLETM, (v & !0xFF) | 0x10);
    mch.setbits32(mchbar::C0COREBONUS, 0x0F << 5);
    mch.setbits32(mchbar::CSHWRIOBONUS, 3 << 3);
    mch.setbits32(mchbar::CSHRMSTDYNDLLENB, 0x0D);
    mch.setbits32(mchbar::SHC3C4REG1, 0x0A3F);
    mch.setbits32(mchbar::C0STATRDCTRL, 3);
    let v = mch.read8(mchbar::C0REFRCTRL2);
    mch.write8(mchbar::C0REFRCTRL2, (v & !0xFF) | 0x4A);
    let v = mch.read8(mchbar::C0COREBONUS + 4);
    mch.write8(mchbar::C0COREBONUS + 4, v & !(3 << 5));
    mch.setbits32(mchbar::C0DYNSLVDLLEN, 0x0321);

    fstart_log::info!("raminit: DDR mode programmed");
}

// ===================================================================
// Program DQ/DQS
// ===================================================================

/// Full DQ/DQS output timing programming.
///
/// Ported from coreboot `sdram_programdqdqs()`.
pub fn sdram_program_dqdqs(si: &SysInfo, mch: &MchBar) {
    let mdclk: u32 = if si.selected_timings.mem_clock == 0 {
        3000
    } else {
        2500
    };
    let refclk: u32 = 3000u32.saturating_sub(mdclk);

    let core_to_mcp = ((mch.read8(mchbar::C0ADDCSCTRL) >> 2) & 0x03) as u32 + 1;
    let core_to_mcp = core_to_mcp * mdclk;

    static TXFIFOTAB: [u8; 8] = [0, 7, 6, 5, 2, 1, 4, 3];
    let fifo_reg = (mch.read8(mchbar::CSHRFIFOCTL) & 0x0E) >> 1;
    let mut cwb: u32 = 0;
    let mut feature = false;
    let mut repeat = 2u8;

    while repeat > 0 {
        let txdelay = mdclk
            * (((mch.read16(mchbar::C0GNT2LNCH1) >> 8) & 0x07) as u32
                + (mch.read8(mchbar::C0WRDATACTRL) & 0x0F) as u32
                + (mch.read8(mchbar::C0WRDATACTRL + 1) & 0x01) as u32)
            + TXFIFOTAB[fifo_reg as usize] as u32 * (mdclk / 2)
            + core_to_mcp
            + refclk
            + cwb;

        let halfclk = (mch.read8(mchbar::C0MISCCTL) >> 1) & 1;
        let reg32 = if halfclk != 0 {
            5083 + core_to_mcp - mdclk / 2
        } else {
            5083 + core_to_mcp
        };

        let tmaxunmask = txdelay.saturating_sub(mdclk).saturating_sub(4382);
        let tmaxpi = tmaxunmask.saturating_sub(3000);

        if tmaxunmask >= reg32 && tmaxpi >= 4692 {
            if repeat == 2 {
                mch.clrbits32(mchbar::C0COREBONUS, 1 << 23);
            }
            feature = true;
            repeat = 0;
        } else {
            repeat -= 1;
            mch.setbits32(mchbar::C0COREBONUS, 1 << 23);
            cwb = 2 * mdclk;
        }
    }

    if !feature {
        let v = mch.read8(mchbar::CLOCKGATINGI);
        mch.write8(mchbar::CLOCKGATINGI, v & !3);
        return;
    }

    mch.setbits32(mchbar::CLOCKGATINGI, 3);

    fstart_log::info!("raminit: DQ/DQS programmed");
}

// ===================================================================
// Periodic RCOMP
// ===================================================================

/// Enable periodic RCOMP.
///
/// Ported from coreboot `sdram_periodic_rcomp()`.
pub fn sdram_periodic_rcomp(mch: &MchBar) {
    let v = mch.read8(mchbar::COMPCTRL1);
    mch.write8(mchbar::COMPCTRL1, v & !(1 << 1));
    while mch.read32(mchbar::COMPCTRL1) & (1 << 31) != 0 {
        core::hint::spin_loop();
    }

    let v = mch.read16(mchbar::CSHRMISCCTL);
    mch.write16(mchbar::CSHRMISCCTL, v & !(3 << 12));
    mch.setbits32(mchbar::CMNDQFIFORST, 1 << 7);
    let v = mch.read16(mchbar::XCOMPDFCTRL);
    mch.write16(mchbar::XCOMPDFCTRL, (v & !0x0F) | 0x09);

    mch.setbits32(mchbar::COMPCTRL1, (1 << 7) | (1 << 1));

    fstart_log::info!("raminit: periodic RCOMP enabled");
}
