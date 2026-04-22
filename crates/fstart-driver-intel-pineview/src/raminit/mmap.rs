//! Memory map configuration: DRA/DRB, TOLUD/TOM/TOUUD/GBSM/BGSM/TSEG.
//!
//! Ported from coreboot `sdram_mmap()`, `sdram_dradrb()`,
//! `sdram_mmap_regs()`.

use super::SysInfo;
use fstart_pineview_regs::{hostbridge, mchbar, EcamPci, MchBar};

/// Initial memory map setup before JEDEC init.
///
/// Ported from coreboot `sdram_mmap()`. Uses lookup tables indexed
/// by dimm_config rather than computing from geometry.
pub fn sdram_mmap(si: &SysInfo, mch: &MchBar) {
    let cfg = si.dimm_config[0] as usize;

    static W260: [u32; 7] = [
        0,
        0x0040_0001,
        0x00C0_0001,
        0x0050_0000,
        0x00F0_0000,
        0x00C0_0001,
        0x00F0_0000,
    ];
    static W208: [u32; 7] = [
        0,
        0x0001_0000,
        0x0101_0000,
        0x0001_0001,
        0x0101_0101,
        0x0101_0000,
        0x0101_0101,
    ];
    static W200: [u32; 7] = [0, 0, 0, 0x0002_0002, 0x0004_0002, 0, 0x0004_0002];
    static W204: [u32; 7] = [
        0,
        0x0002_0002,
        0x0004_0002,
        0x0004_0004,
        0x0008_0006,
        0x0004_0002,
        0x0008_0006,
    ];

    if cfg < 3 && si.dimm_populated(0) {
        let d = si.dimms[0].as_ref().expect("populated");
        if d.ranks > 1 {
            // 2R/NC
            let v = mch.read32(mchbar::C0CKECTRL);
            mch.write32(mchbar::C0CKECTRL, (v & !1) | 0x0030_0001);
            mch.write32(mchbar::C0DRA01, 0x0000_0101);
            mch.write32(mchbar::C0DRB0, 0x0004_0002);
            mch.write32(mchbar::C0DRB2, W204[cfg]);
        } else {
            // 1R/NC
            let v = mch.read32(mchbar::C0CKECTRL);
            mch.write32(mchbar::C0CKECTRL, (v & !1) | 0x0010_0001);
            mch.write32(mchbar::C0DRA01, 0x0000_0001);
            mch.write32(mchbar::C0DRB0, 0x0002_0002);
            mch.write32(mchbar::C0DRB2, W204[cfg]);
        }
    } else if cfg == 5 && si.dimm_populated(0) {
        let v = mch.read32(mchbar::C0CKECTRL);
        mch.write32(mchbar::C0CKECTRL, (v & !1) | 0x0030_0001);
        mch.write32(mchbar::C0DRA01, 0x0000_0101);
        mch.write32(mchbar::C0DRB0, 0x0004_0002);
        mch.write32(mchbar::C0DRB2, 0x0004_0004);
    } else {
        let v = mch.read32(mchbar::C0CKECTRL);
        mch.write32(mchbar::C0CKECTRL, (v & !1) | W260[cfg]);
        mch.write32(mchbar::C0DRA01, W208[cfg]);
        mch.write32(mchbar::C0DRB0, W200[cfg]);
        mch.write32(mchbar::C0DRB2, W204[cfg]);
    }

    static TOLUD_TAB: [u16; 7] = [2048, 2048, 4096, 4096, 8192, 4096, 8192];
    static TOM_TAB: [u16; 7] = [2, 2, 4, 4, 8, 4, 8];
    static TOUUD_TAB: [u16; 7] = [128, 128, 256, 256, 512, 256, 512];

    let ecam = EcamPci::new(0xE000_0000); // ECAM base is already live at this point.
    ecam.write16(0, 0, 0, hostbridge::TOLUD, TOLUD_TAB[cfg] << 4);
    ecam.write16(0, 0, 0, hostbridge::TOM, TOM_TAB[cfg] >> 6);
    ecam.write16(0, 0, 0, hostbridge::TOUUD, TOUUD_TAB[cfg]);

    fstart_log::info!("raminit: memory map configured (dimm_config={})", cfg);
}

/// Program DRA (DRAM Row Attributes) and DRB (DRAM Row Boundary).
///
/// Ported from coreboot `sdram_dradrb()`.
pub fn sdram_dradrb(si: &mut SysInfo, mch: &MchBar) {
    // DRA lookup table: [banks][width][cols-9][rows-12] → dra encoding.
    static DRATAB: [[[[u8; 4]; 2]; 2]; 2] = [
        [
            [[0xFF, 0xFF, 0xFF, 0xFF], [0xFF, 0x00, 0x02, 0xFF]],
            [[0xFF, 0x01, 0xFF, 0xFF], [0xFF, 0x03, 0xFF, 0x06]],
        ],
        [
            [[0xFF, 0xFF, 0xFF, 0xFF], [0xFF, 0x04, 0x06, 0x08]],
            [[0xFF, 0xFF, 0xFF, 0xFF], [0x05, 0x07, 0x09, 0xFF]],
        ],
    ];

    // DRB size table indexed by DRA encoding.
    static DRADRB: [[u8; 6]; 10] = [
        [0x01, 0x01, 0x00, 0x08, 0, 0x04],
        [0x01, 0x00, 0x00, 0x10, 0, 0x02],
        [0x02, 0x01, 0x00, 0x08, 1, 0x08],
        [0x01, 0x01, 0x00, 0x10, 1, 0x04],
        [0x01, 0x01, 0x01, 0x08, 1, 0x08],
        [0x00, 0x01, 0x01, 0x10, 1, 0x04],
        [0x02, 0x01, 0x01, 0x08, 2, 0x10],
        [0x01, 0x01, 0x01, 0x10, 2, 0x08],
        [0x03, 0x01, 0x01, 0x08, 3, 0x20],
        [0x02, 0x01, 0x01, 0x10, 3, 0x10],
    ];

    let mut c0dra: u32 = 0;
    for r in 0..super::RANKS_PER_CHANNEL {
        let dimm_idx = r / 2;
        let rank_in_dimm = (r % 2) as u8;
        if let Some(ref d) = si.dimms[dimm_idx] {
            if d.card_type != 0 && rank_in_dimm < d.ranks {
                let banks = (d.banks as usize).min(1);
                let width = match d.width {
                    fstart_spd::ChipWidth::X16 | fstart_spd::ChipWidth::X32 => 1,
                    _ => 0,
                };
                let cols = (d.cols as usize).saturating_sub(9).min(1);
                let rows = (d.rows as usize).saturating_sub(12).min(3);
                let mut dra = DRATAB[banks][width][cols][rows];
                if d.banks == 1 {
                    dra |= 1 << 7;
                }
                c0dra |= (dra as u32) << (r * 8);
            }
        }
    }
    mch.write32(mchbar::C0DRA01, c0dra);

    // Program CKE for populated ranks.
    let mut rank_bits: u8 = 0;
    for r in 0..super::RANKS_PER_CHANNEL {
        let dimm_idx = r / 2;
        let rank_in_dimm = (r % 2) as u8;
        if let Some(ref d) = si.dimms[dimm_idx] {
            if d.card_type != 0 && rank_in_dimm < d.ranks {
                rank_bits |= 1 << r;
            }
        }
    }
    let v = mch.read8(mchbar::C0CKECTRL + 2);
    mch.write8(mchbar::C0CKECTRL + 2, (v & !0xF0) | (rank_bits << 4));

    // Single-DIMM CKE optimization.
    let only_a = si.dimm_populated(0) && !si.dimm_populated(1);
    let only_b = !si.dimm_populated(0) && si.dimm_populated(1);
    if only_a || only_b {
        mch.setbits32(mchbar::C0CKECTRL, 1 << 0);
    }

    // Program DRBs.
    let mut c0drb: u16 = 0;
    si.channel_capacity[0] = 0;
    for r in 0..super::RANKS_PER_CHANNEL {
        let dimm_idx = r / 2;
        let rank_in_dimm = (r % 2) as u8;
        if let Some(ref d) = si.dimms[dimm_idx] {
            if d.card_type != 0 && rank_in_dimm < d.ranks {
                let ind = ((c0dra >> (8 * r)) & 0x7F) as usize;
                if ind < 10 {
                    c0drb += DRADRB[ind][5] as u16;
                    si.channel_capacity[0] += (DRADRB[ind][5] as u32) << 6;
                }
            }
        }
        let addr = mchbar::C0DRB0 + (r as u32) * 2;
        mch.write16(addr, c0drb);
    }

    fstart_log::info!(
        "raminit: DRA/DRB done, total = {} MiB",
        si.channel_capacity[0]
    );
}

/// Program memory-mapped registers: TOLUD, TOM, TOUUD, GBSM, BGSM, TSEG.
///
/// Full port from coreboot `sdram_mmap_regs()`.
pub fn sdram_mmap_regs(si: &SysInfo, mch: &MchBar, ecam: &EcamPci) {
    let ggc = ecam.read16(0, 0, 0, hostbridge::GGC);

    static GGC_TO_UMA: [u16; 10] = [0, 1, 4, 8, 16, 32, 48, 64, 128, 256];
    static GGC_TO_GTT: [u8; 4] = [0, 1, 0, 0];

    let gfxsize = GGC_TO_UMA[((ggc & 0x00F0) >> 4) as usize] as u32;
    let gttsize = GGC_TO_GTT[((ggc & 0x0300) >> 8) as usize] as u32;
    let tom = si.channel_capacity[0];
    let tsegsize: u32 = 1;
    let mmiosize: u32 = 1024;

    let mut tolud = (4096u32.saturating_sub(mmiosize)).min(tom);
    let reclaim = (tom.saturating_sub(tolud)) > 64;

    let (reclaimbase, reclaimlimit);
    if reclaim {
        tolud &= !0x3F;
        let tom_aligned = tom & !0x3F;
        reclaimbase = 4096u32.max(tom_aligned);
        reclaimlimit =
            reclaimbase + (4096u32.min(tom_aligned).saturating_sub(tolud)).saturating_sub(0x40);
    } else {
        reclaimbase = 0;
        reclaimlimit = 0;
    }

    let touud = if reclaim { reclaimlimit + 64 } else { tom };
    let gfxbase = tolud.saturating_sub(gfxsize);
    let gttbase = gfxbase.saturating_sub(gttsize);
    let tsegbase = gttbase.saturating_sub(tsegsize);

    ecam.write16(0, 0, 0, hostbridge::TOLUD, (tolud << 4) as u16);
    ecam.write16(0, 0, 0, hostbridge::TOM, (tom >> 6) as u16);
    if reclaim {
        ecam.write16(0, 0, 0, 0x98, (reclaimbase >> 6) as u16);
        ecam.write16(0, 0, 0, 0x9A, (reclaimlimit >> 6) as u16);
    }
    ecam.write16(0, 0, 0, hostbridge::TOUUD, touud as u16);
    ecam.write32(0, 0, 0, hostbridge::GBSM, gfxbase << 20);
    ecam.write32(0, 0, 0, hostbridge::BGSM, gttbase << 20);
    ecam.write32(0, 0, 0, hostbridge::TSEG, tsegbase << 20);

    // ESMRAMC: 1M TSEG + enable.
    let v = ecam.read8(0, 0, 0, hostbridge::ESMRAMC);
    ecam.write8(0, 0, 0, hostbridge::ESMRAMC, (v & !0x07) | (1 << 0));

    fstart_log::info!(
        "raminit: mmap regs: TOLUD={} GBSM={} BGSM={} TSEG={} MiB",
        tolud,
        gfxbase,
        gttbase,
        tsegbase
    );
}
