//! SPD reading and DIMM configuration detection.

use super::{SysInfo, TOTAL_DIMMS};
use crate::generic::spd::ChipWidth;
use fstart_core::services::ServiceError;

use crate::pineview::raminit::{DIMM_TYPE_SODIMM, DIMM_TYPE_UBDIMM};

/// Read SPD data from all DIMMs and determine the memory configuration.
///
/// Ported from coreboot `sdram_read_spds()` + `decode_spd()` +
/// `find_ramconfig()`, with common DDR2 SPD parsing delegated to
/// `crate::spd` so Pineview and GM965 share the same geometry/timing decode.
/// `cached` holds the SPD of each slot from the training record, so a DIMM
/// that still reports the same identity is not read again.
pub fn read_spds<B: fstart_core::services::SmBus + ?Sized>(
    si: &mut SysInfo,
    smbus: &mut B,
    cached: Option<&[[u8; 128]; TOTAL_DIMMS]>,
) -> Result<(), ServiceError> {
    si.dt0mode = 0;

    for i in 0..TOTAL_DIMMS {
        let addr = si.spd_map[i];
        if addr == 0 {
            si.dimms[i] = None;
            continue;
        }

        fstart_log::info!("raminit: probing DIMM {} SPD at {:#x}", i, addr);
        let Some(spd_buf) =
            crate::generic::spd::ddr2::read_spd_cached(smbus, addr, cached.map(|spd| &spd[i]))?
        else {
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

        let Some(info) = crate::generic::spd::ddr2::decode_dimm(&spd_buf) else {
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

        // Pineview only supports a subset of DDR2 geometries (coreboot
        // `decode_spd`); anything else would be silently folded onto a
        // neighbouring DRA/DRB entry.
        validate_pineview_spd(&info, i)?;

        si.spd_type = crate::generic::spd::ddr2::DDR2;

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

fn validate_pineview_spd(
    info: &crate::generic::spd::DimmInfo,
    idx: usize,
) -> Result<(), ServiceError> {
    if info.is_ecc {
        fstart_log::error!("raminit: DIMM {} uses unsupported ECC DDR2", idx);
        return Err(ServiceError::HardwareError);
    }
    if !matches!(info.banks, 4 | 8)
        || !matches!(info.width, ChipWidth::X8 | ChipWidth::X16)
        || !matches!(info.ranks, 1 | 2)
        || !matches!(info.sides, 1 | 2)
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
/// This implementation has two incompatible encodings. Desktop/UDIMM uses
/// the vendor-derived 4-bit DIMMA/DIMMB matrix. Mobile/SO-DIMM uses
/// the vendor-derived 0..6 encoding, matching coreboot.
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

    // Use the coreboot-derived mobile/SO-DIMM encoding, while normalizing a
    // single populated socket regardless of whether it is DIMMA or DIMMB.
    // For two populated sockets DIMMA determines the dual-rank/x8 special
    // case.
    match (a.as_ref(), b.as_ref()) {
        (None, None) => Ok(0),
        (Some(a), Some(_b)) => {
            let mut cfg = 3;
            if a.sides > 1 {
                cfg += 1;
                if a.width == ChipWidth::X8 {
                    cfg = 6;
                }
            }
            Ok(cfg)
        }
        (Some(a), None) | (None, Some(a)) => {
            let mut cfg = 1;
            if a.sides > 1 {
                cfg += 1;
                if a.width == ChipWidth::X8 {
                    cfg = 5;
                }
            }
            Ok(cfg)
        }
    }
}

fn dimm_config_desktop(d: &crate::generic::spd::DimmInfo) -> Result<u8, ServiceError> {
    if d.card_type == 0 {
        return Ok(0);
    }
    match (d.ranks, d.width) {
        (1, ChipWidth::X8) => Ok(1),
        (2, ChipWidth::X8) => Ok(2),
        (1, ChipWidth::X16) => Ok(3),
        _ => {
            fstart_log::error!("raminit: unsupported UDIMM rank/width config");
            Err(ServiceError::HardwareError)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generic::spd::ddr2;

    #[test]
    fn desktop_configurations_match_rank_and_width() {
        let mut info = ddr2::decode_dimm(&ddr2::tests::valid_spd()).unwrap();
        for (ranks, width, config) in [
            (1, ChipWidth::X8, Some(1)),
            (2, ChipWidth::X8, Some(2)),
            (1, ChipWidth::X16, Some(3)),
            (2, ChipWidth::X16, None),
            (1, ChipWidth::X4, None),
        ] {
            info.ranks = ranks;
            info.width = width;
            assert_eq!(dimm_config_desktop(&info).ok(), config);
        }
    }

    #[test]
    fn rejects_unsupported_pineview_geometry() {
        let info = ddr2::decode_dimm(&ddr2::tests::valid_spd()).unwrap();
        assert!(validate_pineview_spd(&info, 0).is_ok());
        for invalid in [
            crate::generic::spd::DimmInfo {
                is_ecc: true,
                ..info.clone()
            },
            crate::generic::spd::DimmInfo {
                width: ChipWidth::X4,
                ..info.clone()
            },
            crate::generic::spd::DimmInfo {
                ranks: 3,
                ..info.clone()
            },
        ] {
            assert!(validate_pineview_spd(&invalid, 0).is_err());
        }
    }
}
