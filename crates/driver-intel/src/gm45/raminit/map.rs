//! Cold DDR3 final decode, following coreboot gm45/raminit.c program_memory_map().
//! This is a fail-closed slice: mapping does not make DRAM usable by the boot flow.
use super::*;

#[derive(Debug, PartialEq, Eq)]
pub(super) struct FinalMap {
    pub bounds: [[u32; 2]; 2],
    pub pages: [u32; 2],
    pub interleaved: bool,
    pub ggc: u16,
    pub tom: u16,
    pub tolud: u16,
    pub touud: u16,
    pub remap_base: u16,
    pub remap_limit: u16,
}

/// `capid_high` is CAPID0+4, including IGD-disable and reclaim capability.
/// ME is detected independently of DEVEN, as in coreboot.
pub(super) fn compute(
    dimms: &Ddr3Channels,
    igd_requested: bool,
    capid_high: u32,
    me_active: bool,
) -> Result<FinalMap, ServiceError> {
    let bad = ServiceError::HardwareError;
    // igd_compute_ggc(): fixed 32 MiB UMA; 2 MiB GTT, plus 2 MiB
    // shadow GTT when VT-d is present. CMOS gfx_uma_size is not used.
    let ggc = if igd_requested && capid_high & (1 << 1) == 0 {
        if capid_high & (1 << 16) == 0 {
            0x0b50
        } else {
            0x0350
        }
    } else {
        2
    };
    let reserved_mb = super::super::IntelGm45::reserved_memory(ggc, 3).ok_or(bad)? >> 20;
    let mut bounds = [[0; 2]; 2];
    let mut pages = [0; 2];
    let interleaved = dimms.dimms.iter().all(Option::is_some);
    let mut base_mb = 0u32;
    let mut totals = [0u32; 2];
    for (ch, dimm) in dimms.dimms.iter().enumerate() {
        if interleaved {
            base_mb = 0;
        }
        let ranks = if let Some(d) = dimm {
            ddr3::validate_gm45(d).map_err(|_| bad)?;
            // SPD page_size is the *chip* page; CxDRA needs the rank page.
            let chips_per_rank = match d.width {
                crate::generic::spd::ChipWidth::X8 => 8,
                crate::generic::spd::ChipWidth::X16 => 4,
                _ => return Err(bad),
            };
            let page = d.page_size.checked_mul(chips_per_rank).ok_or(bad)?;
            if !page.is_power_of_two() || !(1024..=131072).contains(&page) {
                return Err(bad);
            }
            let rank_mb = d.rank_capacity_mb;
            if rank_mb == 0 || !rank_mb.is_multiple_of(32) {
                return Err(bad);
            }
            for rank in 0..usize::from(d.ranks) {
                pages[ch] |= (page.ilog2() - 10) << (rank * 4);
            }
            for rank in 0..4 {
                if rank < usize::from(d.ranks) {
                    base_mb = base_mb.checked_add(rank_mb).ok_or(bad)?;
                    totals[ch] = totals[ch].checked_add(rank_mb).ok_or(bad)?;
                }
                // CxDRBy_BOUND_MASK: bits [8:2] in units of 32 MiB.
                if base_mb > 16256 || base_mb & 127 != 0 {
                    return Err(bad);
                }
                bounds[ch][rank / 2] |= ((base_mb >> 5) & 0x1fc) << (rank % 2 * 16);
            }
            d.ranks
        } else {
            for rank in 0..4 {
                if base_mb > 16256 || base_mb & 127 != 0 {
                    return Err(bad);
                }
                bounds[ch][rank / 2] |= ((base_mb >> 5) & 0x1fc) << (rank % 2 * 16);
            }
            0
        };
        if ranks > 2 {
            return Err(bad);
        }
    }
    let tom = totals[0].checked_add(totals[1]).ok_or(bad)?;
    // Coreboot collect_dimm_config() always selects dual interleaved mode when
    // both channels are populated, including unequal capacities: both channels
    // count rank bounds from zero and the MCH interleaves the common part.
    if tom == 0 || !tom.is_multiple_of(128) {
        return Err(bad);
    }
    let used_me = if me_active {
        if totals[0] == totals[1] { 64 } else { 32 }
    } else {
        0
    };
    let mut tom_minus_me = tom.checked_sub(used_me).ok_or(bad)?;
    // coreboot's default 2048 MiB MMIO window, including reserved UMA/TSEG.
    let mmio_start = 2048 + reserved_mb;
    let mut tolud = tom_minus_me.min(mmio_start);
    let mut touud = tom_minus_me;
    let mut remap_base = 0xffc0; // disabled REMAPBASE encoding: 0x3ff
    let mut remap_limit = 0;
    if capid_high & (1 << 15) == 0 && tom_minus_me >= mmio_start + 64 {
        tom_minus_me &= !63;
        tolud &= !63;
        remap_base = tom_minus_me.max(4096);
        let remapped = if tom_minus_me > 4096 {
            4096u32.checked_sub(tolud).ok_or(bad)?
        } else {
            tom_minus_me.checked_sub(tolud).ok_or(bad)?
        };
        touud = remap_base.checked_add(remapped).ok_or(bad)?;
        remap_limit = touud.checked_sub(64).ok_or(bad)?;
    } else if tom_minus_me > 4096 {
        // Without reclaim, a single TOUUD cannot describe the hole below 4G.
        return Err(bad);
    }
    if tolud <= reserved_mb + 1
        || tom >> 7 > 0x1ff
        || tolud > 4095
        || touud > u16::MAX as u32
        || remap_base >> 6 > 0x3ff
        || remap_limit >> 6 > 0x3ff
        || (remap_base != 0xffc0
            && (!remap_base.is_multiple_of(64) || !remap_limit.is_multiple_of(64)))
    {
        return Err(bad);
    }
    Ok(FinalMap {
        bounds,
        pages,
        interleaved,
        ggc,
        tom: (tom >> 7) as u16,
        tolud: (tolud << 4) as u16,
        touud: touud as u16,
        remap_base: (remap_base >> 6) as u16,
        remap_limit: (remap_limit >> 6) as u16,
    })
}

pub(super) fn program(mch: &Mchbar, hb: &super::super::Gm45HostBridgePciConfig, map: &FinalMap) {
    for ch in 0..2 {
        for (pair, &bound) in map.bounds[ch].iter().enumerate() {
            mch.write32(channel_offset(ch, 0x1200 + pair * 4), bound);
        }
        mch.update32(channel_offset(ch, 0x1208), 0xffff, map.pages[ch]);
    }
    hb.esmramc.set((hb.esmramc.get() & !7) | 3); // 2 MiB TSEG
    hb.tom.set(map.tom);
    hb.tolud.set(map.tolud);
    hb.touud.set(map.touud);
    hb.remap_base.set(map.remap_base);
    hb.remap_limit.set(map.remap_limit);
    if map.interleaved {
        mch.regs()
            .dcc
            .set((mch.regs().dcc.get() & !((1 << 10) | (1 << 9))) | (1 << 1));
    } else {
        mch.regs().dcc.modify(Dcc::INTERLEAVED.val(0));
    }
    // GGC owns the graphics stolen decode even before init_igd's display setup.
    hb.ggc.set((hb.ggc.get() & 0xf00f) | map.ggc);
}
