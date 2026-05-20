//! DDR3 SPD byte offsets and decoding (JEDEC SPD revision 1.x).

use crate::{ChipCapacity, ChipWidth, DimmInfo, DDR3, SPD_MEMORY_TYPE};

/// SPD byte 4: SDRAM density and bank count.
pub const SPD_DDR3_DENSITY_BANKS: u8 = 4;
/// SPD byte 5: SDRAM addressing (row/column bits).
pub const SPD_DDR3_ADDRESSING: u8 = 5;
/// SPD byte 7: module organization (rank count and SDRAM device width).
pub const SPD_DDR3_MODULE_ORG: u8 = 7;
/// SPD byte 8: module memory bus width.
pub const SPD_DDR3_BUS_WIDTH: u8 = 8;
/// SPD byte 12: minimum cycle time, tCKmin, in MTB units.
pub const SPD_DDR3_TCK_MIN: u8 = 12;
/// SPD bytes 14..15: CAS latencies supported bitmap.
pub const SPD_DDR3_CAS_LATENCIES_LSB: u8 = 14;
pub const SPD_DDR3_CAS_LATENCIES_MSB: u8 = 15;
/// SPD byte 16: minimum CAS latency time, tAAmin, in MTB units.
pub const SPD_DDR3_TAA_MIN: u8 = 16;
/// SPD byte 17: minimum write recovery time, tWRmin, in MTB units.
pub const SPD_DDR3_TWR_MIN: u8 = 17;
/// SPD byte 18: minimum RAS-to-CAS delay, tRCDmin, in MTB units.
pub const SPD_DDR3_TRCD_MIN: u8 = 18;
/// SPD byte 19: minimum row-to-row delay, tRRDmin, in MTB units.
pub const SPD_DDR3_TRRD_MIN: u8 = 19;
/// SPD byte 20: minimum row precharge delay, tRPmin, in MTB units.
pub const SPD_DDR3_TRP_MIN: u8 = 20;
/// SPD byte 21: upper nibbles for tRAS/tRC.
pub const SPD_DDR3_TRAS_TRC_MSN: u8 = 21;
/// SPD byte 22: tRAS lower byte.
pub const SPD_DDR3_TRAS_LSB: u8 = 22;
/// SPD byte 23: tRC lower byte.
pub const SPD_DDR3_TRC_LSB: u8 = 23;
/// SPD bytes 24..25: tRFCmin in MTB units.
pub const SPD_DDR3_TRFC_LSB: u8 = 24;
pub const SPD_DDR3_TRFC_MSB: u8 = 25;
/// SPD byte 26: tWTRmin in MTB units.
pub const SPD_DDR3_TWTR_MIN: u8 = 26;
/// SPD byte 27: tRTPmin in MTB units.
pub const SPD_DDR3_TRTP_MIN: u8 = 27;
/// SPD byte 28: upper nibble for tFAW.
pub const SPD_DDR3_TFAW_MSN: u8 = 28;
/// SPD byte 29: tFAW lower byte.
pub const SPD_DDR3_TFAW_LSB: u8 = 29;
/// SPD byte 6: module nominal voltage bits.
pub const SPD_DDR3_NOMINAL_VOLTAGE: u8 = 6;
/// SPD byte 31: SDRAM optional features.
pub const SPD_DDR3_SDRAM_OPTIONAL_FEATURES: u8 = 31;
/// SPD byte 62: module reference card / rank address mirroring.
pub const SPD_DDR3_REFERENCE_CARD: u8 = 62;

/// Decoded DDR3 DIMM information using picoseconds for timing minima.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ddr3DimmInfo {
    pub ranks: u8,
    pub rows: u8,
    pub cols: u8,
    pub banks: u8,
    pub device_width_bits: u8,
    pub bus_width_bits: u8,
    pub rank_capacity_mb: u32,
    pub module_capacity_mb: u32,
    pub cas_latencies: u16,
    pub tck_min_ps: u32,
    pub taa_min_ps: u32,
    pub twr_min_ps: u32,
    pub trcd_min_ps: u32,
    pub trrd_min_ps: u32,
    pub trp_min_ps: u32,
    pub tras_min_ps: u32,
    pub trc_min_ps: u32,
    pub trfc_min_ps: u32,
    pub twtr_min_ps: u32,
    pub trtp_min_ps: u32,
    pub tfaw_min_ps: u32,
    pub supports_1_5v: bool,
    pub supports_auto_self_refresh: bool,
    pub rank1_mirrored: bool,
}

/// Decode a DDR3 SPD MTB field to picoseconds.
#[inline]
pub const fn mtb_ps(mtb_units: u16) -> u32 {
    // DDR3 bytes 10/11 define MTB dividend/divisor.  Sandy Bridge coreboot's
    // native path assumes standard DDR3 SPD MTB = 1/8 ns = 125 ps, which is
    // what X220 SO-DIMMs use.
    mtb_units as u32 * 125
}

/// Decode a raw DDR3 SPD image into [`Ddr3DimmInfo`].
pub fn decode_dimm(spd_data: &[u8; 256]) -> Option<Ddr3DimmInfo> {
    if spd_data[SPD_MEMORY_TYPE as usize] != DDR3 {
        return None;
    }

    let density_code = spd_data[SPD_DDR3_DENSITY_BANKS as usize] & 0x0f;
    if density_code > 6 {
        return None;
    }
    let sdram_capacity_mib = 256u32.checked_shl(u32::from(density_code))?;
    let bank_code = (spd_data[SPD_DDR3_DENSITY_BANKS as usize] >> 4) & 0x07;
    if bank_code > 1 {
        return None;
    }
    let banks = 8u8 << bank_code;

    let col_code = spd_data[SPD_DDR3_ADDRESSING as usize] & 0x07;
    let row_code = (spd_data[SPD_DDR3_ADDRESSING as usize] >> 3) & 0x07;
    if col_code > 3 || row_code > 4 {
        return None;
    }
    let cols = 9 + col_code;
    let rows = 12 + row_code;

    let width_code = spd_data[SPD_DDR3_MODULE_ORG as usize] & 0x07;
    if width_code > 3 {
        return None;
    }
    let device_width_bits = 4u8 << width_code;
    let ranks = ((spd_data[SPD_DDR3_MODULE_ORG as usize] >> 3) & 0x07) + 1;
    if ranks > 4 {
        return None;
    }

    let bus_width_code = spd_data[SPD_DDR3_BUS_WIDTH as usize] & 0x07;
    if bus_width_code > 3 {
        return None;
    }
    let bus_width_bits = 8u8 << bus_width_code;
    let rank_capacity_mb = (sdram_capacity_mib / 8) * u32::from(bus_width_bits / device_width_bits);
    let module_capacity_mb = rank_capacity_mb * u32::from(ranks);

    let tras_trc_msn = spd_data[SPD_DDR3_TRAS_TRC_MSN as usize];
    let tfaw_msn = spd_data[SPD_DDR3_TFAW_MSN as usize];
    let cas_latencies = u16::from(spd_data[SPD_DDR3_CAS_LATENCIES_LSB as usize])
        | (u16::from(spd_data[SPD_DDR3_CAS_LATENCIES_MSB as usize]) << 8);

    Some(Ddr3DimmInfo {
        ranks,
        rows,
        cols,
        banks,
        device_width_bits,
        bus_width_bits,
        rank_capacity_mb,
        module_capacity_mb,
        cas_latencies,
        tck_min_ps: mtb_ps(u16::from(spd_data[SPD_DDR3_TCK_MIN as usize])),
        taa_min_ps: mtb_ps(u16::from(spd_data[SPD_DDR3_TAA_MIN as usize])),
        twr_min_ps: mtb_ps(u16::from(spd_data[SPD_DDR3_TWR_MIN as usize])),
        trcd_min_ps: mtb_ps(u16::from(spd_data[SPD_DDR3_TRCD_MIN as usize])),
        trrd_min_ps: mtb_ps(u16::from(spd_data[SPD_DDR3_TRRD_MIN as usize])),
        trp_min_ps: mtb_ps(u16::from(spd_data[SPD_DDR3_TRP_MIN as usize])),
        tras_min_ps: mtb_ps(
            (u16::from(tras_trc_msn & 0x0f) << 8) | u16::from(spd_data[SPD_DDR3_TRAS_LSB as usize]),
        ),
        trc_min_ps: mtb_ps(
            (u16::from(tras_trc_msn >> 4) << 8) | u16::from(spd_data[SPD_DDR3_TRC_LSB as usize]),
        ),
        trfc_min_ps: mtb_ps(
            u16::from(spd_data[SPD_DDR3_TRFC_LSB as usize])
                | (u16::from(spd_data[SPD_DDR3_TRFC_MSB as usize]) << 8),
        ),
        twtr_min_ps: mtb_ps(u16::from(spd_data[SPD_DDR3_TWTR_MIN as usize])),
        trtp_min_ps: mtb_ps(u16::from(spd_data[SPD_DDR3_TRTP_MIN as usize])),
        tfaw_min_ps: mtb_ps(
            (u16::from(tfaw_msn & 0x0f) << 8) | u16::from(spd_data[SPD_DDR3_TFAW_LSB as usize]),
        ),
        supports_1_5v: (spd_data[SPD_DDR3_NOMINAL_VOLTAGE as usize] & 1) == 0,
        supports_auto_self_refresh: (spd_data[SPD_DDR3_SDRAM_OPTIONAL_FEATURES as usize]
            & (1 << 2))
            != 0,
        rank1_mirrored: (spd_data[SPD_DDR3_REFERENCE_CARD as usize] & 1) != 0,
    })
}

impl From<Ddr3DimmInfo> for DimmInfo {
    fn from(info: Ddr3DimmInfo) -> Self {
        let width = match info.device_width_bits {
            4 => ChipWidth::X4,
            8 => ChipWidth::X8,
            16 => ChipWidth::X16,
            32 => ChipWidth::X32,
            _ => ChipWidth::X8,
        };
        let chip_capacity = match (info.rank_capacity_mb
            / u32::from(info.bus_width_bits / info.device_width_bits))
            * 8
        {
            0..=256 => ChipCapacity::Cap256M,
            257..=512 => ChipCapacity::Cap512M,
            513..=1024 => ChipCapacity::Cap1G,
            1025..=2048 => ChipCapacity::Cap2G,
            2049..=4096 => ChipCapacity::Cap4G,
            4097..=8192 => ChipCapacity::Cap8G,
            _ => ChipCapacity::Cap16G,
        };

        Self {
            card_type: 1,
            mem_type: DDR3,
            width,
            chip_capacity,
            page_size: (1u32 << info.cols) * (u32::from(info.device_width_bits).max(8) / 8),
            sides: info.ranks,
            banks: info.banks,
            ranks: info.ranks,
            rows: info.rows,
            cols: info.cols,
            cas_latencies: (info.cas_latencies & 0xff) as u8,
            taa_min: (info.taa_min_ps / 125) as u8,
            tck_min: (info.tck_min_ps / 125) as u8,
            cycle_time_256ns: [0; 8],
            access_time_256ns: [0; 8],
            twr: (info.twr_min_ps / 125) as u8,
            trp: (info.trp_min_ps / 125) as u8,
            trcd: (info.trcd_min_ps / 125) as u8,
            tras: (info.tras_min_ps / 125) as u8,
            trfc: (info.trfc_min_ps / 125) as u16,
            twtr: (info.twtr_min_ps / 125) as u8,
            trrd: (info.trrd_min_ps / 125) as u8,
            trtp: (info.trtp_min_ps / 125) as u8,
            twr_256ns: (info.twr_min_ps << 8) / 1000,
            trp_256ns: (info.trp_min_ps << 8) / 1000,
            trcd_256ns: (info.trcd_min_ps << 8) / 1000,
            tras_256ns: (info.tras_min_ps << 8) / 1000,
            trfc_256ns: (info.trfc_min_ps << 8) / 1000,
            twtr_256ns: (info.twtr_min_ps << 8) / 1000,
            trrd_256ns: (info.trrd_min_ps << 8) / 1000,
            trtp_256ns: (info.trtp_min_ps << 8) / 1000,
            rank_capacity_mb: info.rank_capacity_mb,
            spd_data: [0; 256],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_common_x220_style_ddr3_sodimm() {
        let mut spd = [0u8; 256];
        spd[SPD_MEMORY_TYPE as usize] = DDR3;
        spd[SPD_DDR3_DENSITY_BANKS as usize] = 0x03; // 2 Gb, 8 banks
        spd[SPD_DDR3_ADDRESSING as usize] = 0x19; // 14 rows, 10 columns
        spd[SPD_DDR3_MODULE_ORG as usize] = 0x09; // 2 ranks, x8 devices
        spd[SPD_DDR3_BUS_WIDTH as usize] = 0x03; // 64-bit bus
        spd[SPD_DDR3_TCK_MIN as usize] = 10; // 1.25 ns, DDR3-1600 capable
        spd[SPD_DDR3_TAA_MIN as usize] = 107; // 13.375 ns
        spd[SPD_DDR3_TRCD_MIN as usize] = 107;
        spd[SPD_DDR3_TRP_MIN as usize] = 107;
        spd[SPD_DDR3_TRAS_TRC_MSN as usize] = 0x11;
        spd[SPD_DDR3_TRAS_LSB as usize] = 0x18;
        spd[SPD_DDR3_TRC_LSB as usize] = 0x7f;
        spd[SPD_DDR3_TRFC_LSB as usize] = 0x80;
        spd[SPD_DDR3_TRFC_MSB as usize] = 0x04;
        spd[SPD_DDR3_TFAW_LSB as usize] = 0xf0;
        spd[SPD_DDR3_SDRAM_OPTIONAL_FEATURES as usize] = 1 << 2;

        let info = decode_dimm(&spd).expect("valid DDR3 SPD");
        assert_eq!(info.module_capacity_mb, 4096);
        assert_eq!(info.ranks, 2);
        assert_eq!(info.banks, 8);
        assert_eq!(info.rows, 15);
        assert_eq!(info.cols, 10);
        assert_eq!(info.tck_min_ps, 1250);
        assert!(info.supports_auto_self_refresh);
    }
}
