//! SPD reading and DIMM configuration detection.

use super::{SysInfo, TOTAL_DIMMS};
use fstart_services::ServiceError;
use fstart_spd::ChipWidth;

use crate::raminit::{DIMM_TYPE_SODIMM, DIMM_TYPE_UBDIMM};

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

        fstart_log::info!("raminit: probing DIMM {} SPD at {:#x}", i, addr);
        let Some(spd_buf) = fstart_spd::ddr2::read_spd(smbus, addr)? else {
            fstart_log::info!("raminit: DIMM {} (addr {:#x}) not present", i, addr);
            si.dimms[i] = None;
            continue;
        };
        fstart_log::info!(
            "raminit: DIMM {} SPD header [{:#x}, {:#x}, {:#x}, {:#x}]",
            i,
            spd_buf[0] as u32,
            spd_buf[1] as u32,
            spd_buf[2] as u32,
            spd_buf[3] as u32,
        );

        let Some(mut info) = fstart_spd::ddr2::decode_dimm(&spd_buf) else {
            fstart_log::error!(
                "raminit: DIMM {} is not valid DDR2 (bytes: [{:#x}, {:#x}, {:#x}, {:#x}], type={:#x}, rev={:#x})",
                i,
                spd_buf[0] as u32,
                spd_buf[1] as u32,
                spd_buf[2] as u32,
                spd_buf[3] as u32,
                spd_buf[20] as u32,
                spd_buf[62] as u32,
            );
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

        validate_pineview_spd(&info, i)?;

        si.dt0mode |= (info.spd_data[49] & 0x2) >> 1;

        let dimm_type = match info.spd_data[20] {
            0x02 => DIMM_TYPE_UBDIMM,
            0x04 => DIMM_TYPE_SODIMM,
            _ => {
                fstart_log::error!("raminit: DIMM {} has unsupported DDR2 module type", i);
                return Err(ServiceError::HardwareError);
            }
        };
        if si.dimm_type == super::DIMM_TYPE_NONE {
            si.dimm_type = dimm_type;
        } else if si.dimm_type != dimm_type {
            fstart_log::error!("raminit: mixed SO-DIMM/UDIMM configurations are unsupported");
            return Err(ServiceError::HardwareError);
        }
        let type_str = if dimm_type == DIMM_TYPE_UBDIMM {
            "UB"
        } else {
            "SO"
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
        si.dimm_config[chan] = find_ramconfig(si, chan)?;
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

fn validate_pineview_spd(info: &fstart_spd::DimmInfo, idx: usize) -> Result<(), ServiceError> {
    if info.spd_data[11] & 0x02 != 0 {
        fstart_log::error!(
            "raminit: DIMM {} uses unsupported DDR2 module configuration bits",
            idx
        );
        return Err(ServiceError::HardwareError);
    }
    if !matches!(info.banks, 4 | 8)
        || !matches!(info.width, ChipWidth::X8 | ChipWidth::X16)
        || info.ranks > 2
        || info.sides > 2
        || !(12..=15).contains(&info.rows)
        || !(9..=10).contains(&info.cols)
    {
        fstart_log::error!(
            "raminit: DIMM {} unsupported geometry banks={} width=x{} ranks={} sides={} rows={} cols={}",
            idx,
            info.banks as u32,
            chip_width_bits(info.width) as u32,
            info.ranks as u32,
            info.sides as u32,
            info.rows as u32,
            info.cols as u32,
        );
        return Err(ServiceError::HardwareError);
    }
    Ok(())
}

/// Determine the DIMM configuration code for a channel.
///
/// Pineview has two incompatible encodings.  Desktop/UDIMM uses the
/// vendor-MRC 4-bit DIMMA/DIMMB matrix.  Mobile/SO-DIMM keeps the older
/// 0..6 encoding used by coreboot for DDR2 SO-DIMMs.
fn find_ramconfig(si: &SysInfo, chan: usize) -> Result<u8, ServiceError> {
    let dimma = chan * 2;
    let dimmb = dimma + 1;
    let a = &si.dimms[dimma];
    let b = &si.dimms[dimmb];

    if !si.is_sodimm() {
        let a_cfg = a.as_ref().map_or(Ok(0), dimm_config_desktop)?;
        let b_cfg = b.as_ref().map_or(Ok(0), dimm_config_desktop)?;
        return Ok(a_cfg | (b_cfg << 2));
    }

    let a_pop = a.as_ref().is_some_and(|d| d.card_type != 0);
    let b_pop = b.as_ref().is_some_and(|d| d.card_type != 0);

    if a_pop && b_pop {
        let Some(dimma) = a.as_ref() else {
            return Ok(0);
        };
        let mut config = 3;
        if dimma.sides > 1 {
            config += 1;
        }
        if dimma.width == ChipWidth::X8 && dimma.sides > 1 {
            config = 6;
        }
        return Ok(config);
    }

    if a_pop || b_pop {
        let a_sides = a.as_ref().map_or(0, |d| d.sides);
        let b_sides = b.as_ref().map_or(0, |d| d.sides);
        let a_x8 = a.as_ref().is_none_or(|d| d.width == ChipWidth::X8);
        let b_x8 = b.as_ref().is_none_or(|d| d.width == ChipWidth::X8);
        let mut config = 1;
        if a_sides > 1 || b_sides > 1 {
            config += 1;
            if a_x8 || b_x8 {
                config = 5;
            }
        }
        return Ok(config);
    }

    Ok(0)
}

fn dimm_config_desktop(d: &fstart_spd::DimmInfo) -> Result<u8, ServiceError> {
    if d.card_type == 0 {
        return Ok(0);
    }
    let x8 = d.width == ChipWidth::X8;
    let x16 = matches!(d.width, ChipWidth::X16 | ChipWidth::X32);
    match (d.ranks, x8, x16) {
        (1, true, _) => Ok(1),
        (2, true, _) => Ok(2),
        (1, _, true) => Ok(3),
        _ => {
            fstart_log::error!("raminit: unsupported UDIMM rank/width config");
            Err(ServiceError::HardwareError)
        }
    }
}
