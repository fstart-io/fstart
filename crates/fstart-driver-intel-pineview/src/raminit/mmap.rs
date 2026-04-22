//! Memory map configuration: DRA/DRB, memory-mapped register
//! programming, TOLUD/TOM/TOUUD.
//!
//! Ported from coreboot `sdram_mmap()`, `sdram_dradrb()`,
//! `sdram_mmap_regs()`.

use super::SysInfo;
use fstart_pineview_regs::{hostbridge, mchbar, EcamPci, MchBar};

/// Initial memory map setup (before JEDEC init).
///
/// Ported from coreboot `sdram_mmap()`.
pub fn sdram_mmap(si: &SysInfo, mch: &MchBar) {
    // Calculate total capacity from populated DIMMs.
    let mut total_mb: u32 = 0;
    for i in 0..super::TOTAL_DIMMS {
        if let Some(ref d) = si.dimms[i] {
            if d.card_type != 0 {
                // rank_capacity = page_size * rows * banks * ranks
                let rows = d.rows as u32;
                let banks = d.banks as u32;
                let ranks = d.ranks as u32;
                let page = d.page_size;
                let cap = (page * (1 << rows) * banks * ranks) / (1024 * 1024);
                total_mb += cap;
                fstart_log::info!("raminit: DIMM {} capacity = {} MiB", i, cap);
            }
        }
    }
    fstart_log::info!("raminit: total DRAM = {} MiB", total_mb);
}

/// Program DRA (DRAM Row Attributes) and DRB (DRAM Row Boundary)
/// registers.
///
/// Ported from coreboot `sdram_dradrb()`.
pub fn sdram_dradrb(si: &SysInfo, mch: &MchBar) {
    let mut cumul_mb: u16 = 0;

    for rank in 0..super::RANKS_PER_CHANNEL {
        let dimm_idx = rank / 2;
        let rank_in_dimm = rank % 2;

        let rank_mb = if let Some(ref d) = si.dimms[dimm_idx] {
            if d.card_type != 0 && rank_in_dimm < d.ranks as usize {
                let rows = d.rows as u32;
                let banks = d.banks as u32;
                let page = d.page_size;
                (page * (1 << rows) * banks) / (1024 * 1024)
            } else {
                0
            }
        } else {
            0
        };

        cumul_mb += rank_mb as u16;

        // DRB register: cumulative size in 32-MiB granularity.
        let drb = cumul_mb / 32;
        let drb_reg = mchbar::C0DRB0 + (rank as u32) * 2;
        mch.write16(drb_reg, drb);
    }

    // DRA registers encode row/col geometry per rank pair.
    for dimm in 0..super::TOTAL_DIMMS {
        if let Some(ref d) = si.dimms[dimm] {
            if d.card_type != 0 {
                // DRA: rows[3:0] | cols[7:4] for each DIMM pair.
                let rows_enc = d.rows.saturating_sub(12) & 0x0F;
                let cols_enc = d.cols.saturating_sub(9) & 0x0F;
                let dra = (cols_enc << 4) | rows_enc;
                let dra_reg = mchbar::C0DRA01 + (dimm as u32) * 2;
                mch.write8(dra_reg, dra);
            }
        }
    }

    fstart_log::info!("raminit: DRA/DRB programmed, {} MiB cumulative", cumul_mb);
}

/// Program memory-mapped registers: TOLUD, TOM, TOUUD.
///
/// Ported from coreboot `sdram_mmap_regs()`.
pub fn sdram_mmap_regs(si: &SysInfo, mch: &MchBar, ecam: &EcamPci) {
    // Read DRB3 (last rank boundary) for total size.
    let drb3 = mch.read16(mchbar::C0DRB0 + 6);
    let total_mb = (drb3 as u32) * 32;

    // TOLUD: Top of Low Usable DRAM (in 16 MiB granularity).
    // Must be below 4 GiB. For Pineview, typically DRAM ≤ 2 GiB.
    let tolud = total_mb.min(0x1000); // cap at 4 GiB
    ecam.write16(0, 0, 0, hostbridge::TOLUD, (tolud >> 4) as u16);

    // TOM: Top of Memory (in 64 MiB granularity).
    ecam.write16(0, 0, 0, hostbridge::TOM, (total_mb >> 6) as u16);

    // TOUUD: Top of Upper Usable DRAM.
    ecam.write16(0, 0, 0, hostbridge::TOUUD, (total_mb >> 6) as u16);

    fstart_log::info!("raminit: TOLUD={} MiB, TOM={} MiB", tolud, total_mb);
}
