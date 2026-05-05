//! SPD reading and DIMM configuration detection.

use super::{SysInfo, TOTAL_DIMMS};
use fstart_services::ServiceError;
use fstart_spd::ChipWidth;

const UBDIMM: u8 = 1;
const SODIMM: u8 = 2;

/// Read SPD data from all DIMMs and determine the memory configuration.
///
/// Ported from coreboot `sdram_read_spds()` + `decode_spd()` +
/// `find_ramconfig()`, with common DDR2 SPD parsing delegated to
/// `fstart-spd` so Pineview and GM965 share the same geometry/timing decode.
pub fn read_spds(
    si: &mut SysInfo,
    smbus: &mut dyn fstart_services::SmBus,
) -> Result<(), ServiceError> {
    si.dt0mode = 0;

    for i in 0..TOTAL_DIMMS {
        let addr = si.spd_map[i];
        if addr == 0 {
            si.dimms[i] = None;
            continue;
        }

        let Some(spd_buf) = fstart_spd::ddr2::read_spd(smbus, addr)? else {
            fstart_log::info!("raminit: DIMM {} (addr {:#x}) not present", i, addr);
            si.dimms[i] = None;
            continue;
        };

        let Some(mut info) = fstart_spd::ddr2::decode_dimm(&spd_buf) else {
            fstart_log::error!("raminit: DIMM {} is not valid DDR2", i);
            return Err(ServiceError::HardwareError);
        };

        si.spd_type = fstart_spd::DDR2;

        // Preserve Pineview's coreboot CAS mask policy: only CAS3..CAS6 are
        // considered, with a conservative CAS0..2 fallback if the advertised
        // mask is unusable.
        info.cas_latencies &= 0x78;
        if info.cas_latencies == 0 {
            info.cas_latencies = 7;
        }

        si.dt0mode |= (info.spd_data[49] & 0x2) >> 1;

        let dimm_type = match info.spd_data[20] {
            0x02 => UBDIMM,
            0x04 => SODIMM,
            _ => 0,
        };
        let type_str = if dimm_type == UBDIMM {
            "UB"
        } else if dimm_type == SODIMM {
            "SO"
        } else {
            "??"
        };
        fstart_log::info!(
            "raminit: {}-DIMM {} ranks={} banks={} rows={} cols={} width=x{} page={}B",
            type_str,
            i,
            info.ranks as u32,
            info.banks as u32,
            info.rows as u32,
            info.cols as u32,
            chip_width_bits(info.width) as u32,
            info.page_size,
        );

        si.dimms[i] = Some(info);
    }

    // Verify at least one DIMM is populated.
    let any_populated = si
        .dimms
        .iter()
        .any(|d| d.as_ref().is_some_and(|d| d.card_type != 0));
    if !any_populated {
        fstart_log::error!("raminit: no DIMMs detected");
        return Err(ServiceError::HardwareError);
    }

    // Determine DIMM configuration per channel (coreboot find_ramconfig).
    for chan in 0..super::TOTAL_CHANNELS {
        si.dimm_config[chan] = find_ramconfig(si, chan);
        fstart_log::info!("raminit: config[CH{}] = {}", chan, si.dimm_config[chan]);
    }

    Ok(())
}

fn chip_width_bits(width: ChipWidth) -> u8 {
    match width {
        ChipWidth::X4 => 4,
        ChipWidth::X8 => 8,
        ChipWidth::X16 => 16,
        ChipWidth::X32 => 32,
    }
}

/// Determine the DIMM configuration code for a channel.
///
/// RAM Config: DIMMB-DIMMA
///   0 = EMPTY-EMPTY, 1 = EMPTY-x16SS, 2 = EMPTY-x16DS,
///   3 = x16SS-x16SS, 4 = x16DS-x16DS, 5 = EMPTY-x8DS,
///   6 = x8DS-x8DS
fn find_ramconfig(si: &SysInfo, chan: usize) -> u8 {
    let a = &si.dimms[chan * 2];
    let b = &si.dimms[chan * 2 + 1];

    let a_sides = a.as_ref().map_or(0, |d| d.sides);
    let b_sides = b.as_ref().map_or(0, |d| d.sides);
    let a_x8 = a.as_ref().is_some_and(|d| d.width == ChipWidth::X8);
    let b_x8 = b.as_ref().is_some_and(|d| d.width == ChipWidth::X8);

    match (a_sides, b_sides) {
        (0, 0) => 0, // EMPTY-EMPTY
        (0, 1) => 1, // EMPTY-SS
        (0, s) if s > 1 => {
            if b_x8 {
                5
            } else {
                2
            }
        } // EMPTY-DS
        (1, 0) => 1, // SS-EMPTY
        (1, 1) => 3, // SS-SS (same width assumed)
        (s, 0) if s > 1 => {
            if a_x8 {
                5
            } else {
                4
            }
        } // DS-EMPTY
        (s1, s2) if s1 > 1 && s2 > 1 => {
            if a_x8 && b_x8 {
                6
            } else {
                4
            } // DS-DS
        }
        _ => 0,
    }
}
