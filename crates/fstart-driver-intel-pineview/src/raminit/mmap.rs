//! Memory map configuration: DRA/DRB, TOLUD/TOM/TOUUD/GBSM/BGSM/TSEG.
//!
//! Ported from coreboot `sdram_mmap()`, `sdram_dradrb()`,
//! `sdram_mmap_regs()`.

use super::SysInfo;
use fstart_ecam as ecam;
use fstart_pineview_regs::{hostbridge, mchbar, MchBar};

/// Initial memory map setup before JEDEC init.
///
/// Ported from coreboot `sdram_mmap()`. Uses lookup tables indexed
/// by dimm_config rather than computing from geometry.
pub fn sdram_mmap(si: &SysInfo, mch: &MchBar) {
    let hb = ecam::PciDevBdf::new(0, 0, 0);
    let cfg = si.dimm_config[0] as usize;

    static W260_MB: [u32; 7] = [
        0,
        0x0040_0001,
        0x00c0_0001,
        0x0050_0000,
        0x00f0_0000,
        0x00c0_0001,
        0x00f0_0000,
    ];
    static W208_MB: [u32; 7] = [
        0,
        0x0001_0000,
        0x0101_0000,
        0x0001_0001,
        0x0101_0101,
        0x0101_0000,
        0x0101_0101,
    ];
    static W200_MB: [u32; 7] = [0, 0, 0, 0x0002_0002, 0x0004_0002, 0, 0x0004_0002];
    static W204_MB: [u32; 7] = [
        0,
        0x0002_0002,
        0x0004_0002,
        0x0004_0004,
        0x0008_0006,
        0x0004_0002,
        0x0008_0006,
    ];

    static W260_DT: [u32; 16] = [
        0x000000, 0x100001, 0x300001, 0x100001, 0x400001, 0x500000, 0x700000, 0x500000, 0xc00001,
        0xd00000, 0xf00000, 0xd00000, 0x400001, 0x500000, 0x700000, 0x500000,
    ];
    static W208_DT: [u32; 16] = [
        0x00000000, 0x00000001, 0x00000101, 0x00000001, 0x00010000, 0x00010001, 0x00010101,
        0x00010001, 0x01010000, 0x01010001, 0x01010101, 0x01010001, 0x00010000, 0x00010001,
        0x00010101, 0x00010001,
    ];
    static W200_DT: [u32; 16] = [
        0x00000000, 0x00020002, 0x00040002, 0x00020002, 0x00000000, 0x00020002, 0x00040002,
        0x00020002, 0x00000000, 0x00020002, 0x00040002, 0x00020002, 0x00000000, 0x00020002,
        0x00040002, 0x00020002,
    ];
    static W204_DT: [u32; 16] = [
        0x00000000, 0x00020002, 0x00040004, 0x00020002, 0x00020002, 0x00040004, 0x00060006,
        0x00040004, 0x00040002, 0x00060004, 0x00080006, 0x00060004, 0x00020002, 0x00040004,
        0x00060006, 0x00040004,
    ];

    let w260 = if si.is_sodimm() {
        &W260_MB[..]
    } else {
        &W260_DT[..]
    };
    let w208 = if si.is_sodimm() {
        &W208_MB[..]
    } else {
        &W208_DT[..]
    };
    let w200 = if si.is_sodimm() {
        &W200_MB[..]
    } else {
        &W200_DT[..]
    };
    let w204 = if si.is_sodimm() {
        &W204_MB[..]
    } else {
        &W204_DT[..]
    };

    if si.is_sodimm() && cfg < 3 && si.dimm_populated(0) {
        let d = si.dimms[0].as_ref().expect("populated");
        if d.ranks > 1 {
            let v = mch.read32(mchbar::C0CKECTRL);
            mch.write32(mchbar::C0CKECTRL, (v & !1) | 0x0030_0001);
            mch.write32(mchbar::C0DRA01, 0x0000_0101);
            mch.write32(mchbar::C0DRB0, 0x0004_0002);
            mch.write32(mchbar::C0DRB2, w204[cfg]);
        } else {
            let v = mch.read32(mchbar::C0CKECTRL);
            mch.write32(mchbar::C0CKECTRL, (v & !1) | 0x0010_0001);
            mch.write32(mchbar::C0DRA01, 0x0000_0001);
            mch.write32(mchbar::C0DRB0, 0x0002_0002);
            mch.write32(mchbar::C0DRB2, w204[cfg]);
        }
    } else if si.is_sodimm() && cfg == 5 && si.dimm_populated(0) {
        let v = mch.read32(mchbar::C0CKECTRL);
        mch.write32(mchbar::C0CKECTRL, (v & !1) | 0x0030_0001);
        mch.write32(mchbar::C0DRA01, 0x0000_0101);
        mch.write32(mchbar::C0DRB0, 0x0004_0002);
        mch.write32(mchbar::C0DRB2, 0x0004_0004);
    } else {
        let v = mch.read32(mchbar::C0CKECTRL);
        mch.write32(mchbar::C0CKECTRL, (v & !1) | w260[cfg]);
        mch.write32(mchbar::C0DRA01, w208[cfg]);
        mch.write32(mchbar::C0DRB0, w200[cfg]);
        mch.write32(mchbar::C0DRB2, w204[cfg]);
    }

    static TOLUD_MB: [u16; 7] = [2048, 2048, 4096, 4096, 8192, 4096, 8192];
    static TOM_MB: [u16; 7] = [2, 2, 4, 4, 8, 4, 8];
    static TOUUD_MB: [u16; 7] = [128, 128, 256, 256, 512, 256, 512];
    static GBSM_MB: [u32; 7] = [
        1 << 27,
        1 << 27,
        1 << 28,
        1 << 27,
        1 << 29,
        1 << 28,
        1 << 29,
    ];
    static BGSM_MB: [u32; 7] = [
        1 << 27,
        1 << 27,
        1 << 28,
        1 << 27,
        1 << 29,
        1 << 28,
        1 << 29,
    ];
    static TSEGMB_MB: [u32; 7] = [
        1 << 27,
        1 << 27,
        1 << 28,
        1 << 27,
        1 << 29,
        1 << 28,
        1 << 29,
    ];
    static TOLUD_DT: [u16; 16] = [
        2048, 2048, 4096, 2048, 2048, 4096, 6144, 4096, 4096, 6144, 8192, 6144, 2048, 4096, 6144,
        4096,
    ];
    static TOM_DT: [u16; 16] = [2, 2, 4, 2, 2, 4, 6, 4, 4, 6, 8, 6, 2, 4, 6, 4];
    static TOUUD_DT: [u16; 16] = [
        128, 128, 256, 128, 128, 256, 384, 256, 256, 384, 512, 384, 128, 256, 384, 256,
    ];
    static GBSM_DT: [u32; 16] = [
        1 << 27,
        1 << 27,
        1 << 28,
        1 << 27,
        1 << 27,
        1 << 27,
        0x18000000,
        1 << 27,
        1 << 28,
        0x18000000,
        1 << 29,
        0x18000000,
        1 << 27,
        1 << 27,
        0x18000000,
        1 << 27,
    ];
    static BGSM_DT: [u32; 16] = [
        1 << 27,
        1 << 27,
        1 << 28,
        1 << 27,
        1 << 27,
        1 << 28,
        0x18000000,
        1 << 28,
        1 << 28,
        0x18000000,
        1 << 29,
        0x18000000,
        1 << 27,
        1 << 28,
        0x18000000,
        1 << 28,
    ];
    static TSEGMB_DT: [u32; 16] = [
        1 << 27,
        1 << 27,
        1 << 28,
        1 << 27,
        1 << 27,
        1 << 28,
        0x18000000,
        1 << 28,
        1 << 28,
        0x18000000,
        1 << 29,
        0x18000000,
        1 << 27,
        1 << 28,
        0x18000000,
        1 << 28,
    ];
    let tolud = if si.is_sodimm() {
        &TOLUD_MB[..]
    } else {
        &TOLUD_DT[..]
    };
    let tom = if si.is_sodimm() {
        &TOM_MB[..]
    } else {
        &TOM_DT[..]
    };
    let touud = if si.is_sodimm() {
        &TOUUD_MB[..]
    } else {
        &TOUUD_DT[..]
    };
    let gbsm = if si.is_sodimm() {
        &GBSM_MB[..]
    } else {
        &GBSM_DT[..]
    };
    let bgsm = if si.is_sodimm() {
        &BGSM_MB[..]
    } else {
        &BGSM_DT[..]
    };
    let tsegmb = if si.is_sodimm() {
        &TSEGMB_MB[..]
    } else {
        &TSEGMB_DT[..]
    };

    hb.write16(hostbridge::TOLUD, tolud[cfg]);
    hb.write16(hostbridge::TOM, tom[cfg]);
    hb.write16(hostbridge::TOUUD, touud[cfg]);
    hb.write32(hostbridge::GBSM, gbsm[cfg]);
    hb.write32(hostbridge::BGSM, bgsm[cfg]);
    hb.write32(hostbridge::TSEG, tsegmb[cfg]);

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
            [[0xFF, 0x01, 0xFF, 0xFF], [0xFF, 0x03, 0xFF, 0xFF]],
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
                let banks = usize::from(d.banks >= 8);
                let width = match d.width {
                    fstart_spd::ChipWidth::X16 | fstart_spd::ChipWidth::X32 => 1,
                    _ => 0,
                };
                let cols = (d.cols as usize).saturating_sub(9).min(1);
                let rows = (d.rows as usize).saturating_sub(12).min(3);
                let mut dra = DRATAB[banks][width][cols][rows];
                if d.banks >= 8 {
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
pub fn sdram_mmap_regs(si: &SysInfo, _mch: &MchBar) {
    let hb = ecam::PciDevBdf::new(0, 0, 0);
    let ggc = hb.read16(hostbridge::GGC);

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

    hb.write16(hostbridge::TOLUD, (tolud << 4) as u16);
    hb.write16(hostbridge::TOM, (tom >> 6) as u16);
    if reclaim {
        hb.write16(0x98, (reclaimbase >> 6) as u16);
        hb.write16(0x9A, (reclaimlimit >> 6) as u16);
    }
    hb.write16(hostbridge::TOUUD, touud as u16);
    hb.write32(hostbridge::GBSM, gfxbase << 20);
    hb.write32(hostbridge::BGSM, gttbase << 20);
    hb.write32(hostbridge::TSEG, tsegbase << 20);

    // ESMRAMC: 1M TSEG + enable.
    let v = hb.read8(hostbridge::ESMRAMC);
    hb.write8(hostbridge::ESMRAMC, (v & !0x07) | (1 << 0));

    fstart_log::info!(
        "raminit: mmap regs: TOLUD={} GBSM={} BGSM={} TSEG={} MiB",
        tolud,
        gfxbase,
        gttbase,
        tsegbase
    );
}
