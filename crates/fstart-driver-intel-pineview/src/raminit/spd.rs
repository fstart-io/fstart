//! SPD reading and DIMM configuration detection.

use super::{SysInfo, TOTAL_DIMMS};
use fstart_services::ServiceError;

const UBDIMM: u8 = 1;
const SODIMM: u8 = 2;

/// Read SPD data from all DIMMs and determine the memory configuration.
///
/// Ported from coreboot `sdram_read_spds()` + `decode_spd()` +
/// `find_ramconfig()`.
pub fn read_spds(
    si: &mut SysInfo,
    smbus: &mut dyn fstart_services::SmBus,
) -> Result<(), ServiceError> {
    si.dt0mode = 0;

    for i in 0..TOTAL_DIMMS {
        let addr = si.spd_map[i];
        if addr == 0 {
            si.dimms[i] = Default::default();
            continue;
        }

        // Read first 64 bytes of SPD (enough for DDR2 decode).
        let mut spd_buf = [0u8; 256];
        let mut ok = true;
        for byte in 0..64u8 {
            match smbus.read_byte(addr, byte) {
                Ok(v) => spd_buf[byte as usize] = v,
                Err(_) => {
                    ok = false;
                    break;
                }
            }
        }

        if !ok {
            fstart_log::info!("raminit: DIMM {} (addr {:#x}) not present", i, addr);
            si.dimms[i] = None;
            continue;
        }

        let card_type = spd_buf[62] & 0x1F;
        if card_type == 0 {
            si.dimms[i] = None;
            continue;
        }

        // Verify DDR2.
        if spd_buf[2] != fstart_spd::DDR2 {
            fstart_log::error!("raminit: DIMM {} is not DDR2 (type={:#x})", i, spd_buf[2]);
            return Err(ServiceError::HardwareError);
        }
        si.spd_type = fstart_spd::DDR2;

        // Decode SPD fields (coreboot decode_spd).
        let dimm_type = match spd_buf[20] {
            0x02 => UBDIMM,
            0x04 => SODIMM,
            _ => 0,
        };
        let sides = (spd_buf[5] & 0x7) + 1;
        let banks = (spd_buf[17] >> 2).wrapping_sub(1);
        let rows = spd_buf[3];
        let cols = spd_buf[4];
        let cas_latencies = 0x78 & spd_buf[18];
        let cas_latencies = if cas_latencies == 0 { 7 } else { cas_latencies };
        let width_raw = (spd_buf[13] >> 3).wrapping_sub(1);
        let page_size = ((width_raw as u32) + 1) * (1u32 << cols as u32);

        let info = fstart_spd::DimmInfo {
            card_type,
            mem_type: fstart_spd::DDR2,
            width: match width_raw {
                0 => fstart_spd::ChipWidth::X8,
                1 => fstart_spd::ChipWidth::X16,
                _ => fstart_spd::ChipWidth::X8,
            },
            chip_capacity: match banks {
                0 => fstart_spd::ChipCapacity::Cap256M,
                1 => fstart_spd::ChipCapacity::Cap512M,
                2 => fstart_spd::ChipCapacity::Cap1G,
                3 => fstart_spd::ChipCapacity::Cap2G,
                _ => fstart_spd::ChipCapacity::Cap1G,
            },
            page_size,
            sides,
            banks,
            ranks: sides,
            rows,
            cols,
            cas_latencies,
            taa_min: spd_buf[26],
            tck_min: spd_buf[25],
            twr: spd_buf[36],
            trp: spd_buf[27],
            trcd: spd_buf[29],
            tras: spd_buf[30],
            trfc: spd_buf[42] as u16 | ((spd_buf[40] as u16 & 0xF) << 8),
            twtr: spd_buf[37],
            trrd: spd_buf[28],
            trtp: spd_buf[38],
            rank_capacity_mb: 0, // computed later in mmap
            spd_data: spd_buf,
        };

        si.dt0mode |= (spd_buf[49] & 0x2) >> 1;

        let type_str = if dimm_type == UBDIMM {
            "UB"
        } else if dimm_type == SODIMM {
            "SO"
        } else {
            "??"
        };
        fstart_log::info!(
            "raminit: {}-DIMM {} sides={} banks={} rows={} cols={} width={}",
            type_str,
            i,
            sides,
            banks,
            rows,
            cols,
            (width_raw + 1) * 8
        );

        si.dimms[i] = Some(info);
    }

    // Verify at least one DIMM is populated.
    let any_populated = si
        .dimms
        .iter()
        .any(|d| d.as_ref().map_or(false, |d| d.card_type != 0));
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
    let a_width = a.as_ref().map_or(0, |d| d.width as u8);
    let b_width = b.as_ref().map_or(0, |d| d.width as u8);

    match (a_sides, b_sides) {
        (0, 0) => 0, // EMPTY-EMPTY
        (0, 1) => 1, // EMPTY-SS
        (0, s) if s > 1 => {
            if b_width == 0 {
                5
            } else {
                2
            }
        } // EMPTY-DS
        (1, 0) => 1, // SS-EMPTY
        (1, 1) => 3, // SS-SS (same width assumed)
        (s, 0) if s > 1 => {
            if a_width == 0 {
                5
            } else {
                4
            }
        } // DS-EMPTY
        (s1, s2) if s1 > 1 && s2 > 1 => {
            if a_width == 0 && b_width == 0 {
                6
            } else {
                4
            } // DS-DS
        }
        _ => 0,
    }
}
