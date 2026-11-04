//! Coreboot GM45 `verify_ddr3_dimm()`: the DDR3 DIMMs the GM45 raminit can
//! train.

use fstart_driver_spd::ChipWidth;
use fstart_driver_spd::ddr3::{Ddr3DimmInfo, Ddr3ModuleType, Ddr3SpdError, decode_dimm};

/// Validate a decoded DDR3 DIMM against GM45/coreboot limits.
pub fn validate(info: &Ddr3DimmInfo) -> Result<(), Ddr3SpdError> {
    // Coreboot GM45 `verify_ddr3_dimm()` accepts only DDR3 SO-DIMMs and
    // rejects ECC/extended bus width before training.
    if info.module_type != Ddr3ModuleType::Sodimm {
        return Err(Ddr3SpdError::UnsupportedModuleType(
            match info.module_type {
                Ddr3ModuleType::Undefined => 0x00,
                Ddr3ModuleType::Rdimm => 0x01,
                Ddr3ModuleType::Udimm => 0x02,
                Ddr3ModuleType::Sodimm => 0x03,
                Ddr3ModuleType::MicroDimm => 0x04,
                Ddr3ModuleType::MiniRdimm => 0x05,
                Ddr3ModuleType::MiniUdimm => 0x06,
                Ddr3ModuleType::SoRdimm72b => 0x08,
                Ddr3ModuleType::SoUdimm72b => 0x09,
                Ddr3ModuleType::SoDimm16b => 0x0c,
                Ddr3ModuleType::SoDimm32b => 0x0d,
                Ddr3ModuleType::Unknown(code) => code,
            },
        ));
    }
    if info.bus_width_bits != info.primary_bus_width_bits {
        return Err(Ddr3SpdError::InvalidGeometry);
    }
    if info.banks != 8 {
        return Err(Ddr3SpdError::UnsupportedBanks(info.banks));
    }
    if !matches!(info.width, ChipWidth::X8 | ChipWidth::X16) {
        return Err(Ddr3SpdError::UnsupportedDeviceWidth(info.width as u8));
    }
    if info.ranks != 1 && info.ranks != 2 {
        return Err(Ddr3SpdError::UnsupportedRanks(info.ranks));
    }
    if info.density_code > 3 {
        return Err(Ddr3SpdError::UnsupportedChipCapacity(info.density_code));
    }
    // The rank boundaries must not be derived from geometry that contradicts
    // the SPD density. Check this before JEDEC/training, not only at final map.
    let chip_width = match info.width {
        ChipWidth::X8 => 8,
        ChipWidth::X16 => 16,
        _ => unreachable!(),
    };
    if !info.primary_bus_width_bits.is_multiple_of(chip_width)
        || info.rank_capacity_mb
            != (32u32 << info.density_code) * u32::from(info.primary_bus_width_bits / chip_width)
    {
        return Err(Ddr3SpdError::InvalidGeometry);
    }
    if !matches!(info.raw_card_with_extension, 0 | 1 | 2 | 3 | 5) {
        return Err(Ddr3SpdError::UnsupportedRawCard(
            info.raw_card_with_extension,
        ));
    }
    Ok(())
}

/// Decode raw DDR3 SPD and enforce the GM45/coreboot limitations.
pub fn decode(spd_data: &[u8; 256]) -> Result<Ddr3DimmInfo, Ddr3SpdError> {
    let info = decode_dimm(spd_data)?;
    validate(&info)?;
    Ok(info)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fstart_driver_spd::ddr3::testing::synthetic_ddr3_spd;
    use fstart_driver_spd::ddr3::{SPD_ADDRESSING, SPD_DENSITY_BANKS};

    #[test]
    fn accepts_the_synthetic_so_dimm() {
        assert!(decode(&synthetic_ddr3_spd()).is_ok());
    }

    #[test]
    fn rejects_unsupported_bank_count() {
        let mut spd = synthetic_ddr3_spd();
        spd[SPD_DENSITY_BANKS as usize] = 1 << 4; // 16 banks
        let info = decode_dimm(&spd).expect("general decoder accepts 16-bank DDR3");
        assert_eq!(validate(&info), Err(Ddr3SpdError::UnsupportedBanks(16)));
    }

    #[test]
    fn rejects_density_geometry_mismatch_before_gm45_training() {
        let mut spd = synthetic_ddr3_spd();
        // The density still says 1 Gbit, but 15 rows describe a 2 Gbit chip.
        spd[SPD_ADDRESSING as usize] = (3 << 3) | 1;
        let info = decode_dimm(&spd).expect("generic decoder retains SPD geometry");
        assert_eq!(info.rank_capacity_mb, 2048);
        assert_eq!(validate(&info), Err(Ddr3SpdError::InvalidGeometry));
        assert_eq!(decode(&spd), Err(Ddr3SpdError::InvalidGeometry));
    }
}
