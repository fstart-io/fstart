//! DDR2 SPD byte offsets and decoding (JEDEC SPD revision 1.2a).

use crate::{ChipCapacity, ChipWidth, DimmInfo};
use fstart_services::{ServiceError, SmBus};

// ===================================================================
// DDR2 SPD byte offset constants
// ===================================================================

/// Maximum DDR2 SPD payload size used by coreboot's common DDR2 decoder.
pub const SPD_SIZE_MAX_DDR2: usize = 128;
/// SPD byte 3: number of row address bits.
pub const SPD_NUM_ROWS: u8 = 3;
/// SPD byte 4: number of column address bits.
pub const SPD_NUM_COLUMNS: u8 = 4;
/// SPD byte 5: number of DIMM ranks (physical banks).
pub const SPD_NUM_DIMM_BANKS: u8 = 5;
/// SPD byte 6: module data width (LSB).
pub const SPD_MODULE_DATA_WIDTH_LSB: u8 = 6;
/// SPD byte 7: module data width (MSB).
pub const SPD_MODULE_DATA_WIDTH_MSB: u8 = 7;
/// SPD byte 9: minimum cycle time at maximum supported CAS latency.
pub const SPD_MIN_CYCLE_TIME_AT_CAS_MAX: u8 = 9;
/// SPD byte 10: access time from clock.
pub const SPD_ACCESS_TIME_FROM_CLOCK: u8 = 10;
/// SPD byte 11: DIMM configuration type (ECC, parity, etc.).
pub const SPD_DIMM_CONFIG_TYPE: u8 = 11;
/// SPD byte 13: primary SDRAM width.
pub const SPD_PRIMARY_SDRAM_WIDTH: u8 = 13;
/// SPD byte 17: number of banks per SDRAM device.
pub const SPD_NUM_BANKS_PER_SDRAM: u8 = 17;
/// SPD byte 18: supported CAS latencies (bitmask).
pub const SPD_SUPPORTED_CAS_LATENCIES: u8 = 18;
/// SPD byte 23: minimum cycle time at CAS-1.
pub const SPD_MIN_CYCLE_TIME_AT_CAS_MINUS_1: u8 = 23;
/// SPD byte 24: access time at CAS-1.
pub const SPD_ACCESS_TIME_FROM_CLOCK_CAS_MINUS_1: u8 = 24;
/// SPD byte 25: minimum cycle time at CAS-2.
pub const SPD_MIN_CYCLE_TIME_AT_CAS_MINUS_2: u8 = 25;
/// SPD byte 26: access time at CAS-2.
pub const SPD_ACCESS_TIME_FROM_CLOCK_CAS_MINUS_2: u8 = 26;
/// SPD byte 27: minimum row precharge time (tRP).
pub const SPD_MIN_ROW_PRECHARGE_TIME: u8 = 27;
/// SPD byte 28: minimum RAS-to-RAS delay (tRRD).
pub const SPD_MIN_RAS_TO_RAS_DELAY: u8 = 28;
/// SPD byte 29: minimum RAS-to-CAS delay (tRCD).
pub const SPD_MIN_RAS_TO_CAS_DELAY: u8 = 29;
/// SPD byte 30: minimum active-to-precharge delay (tRAS).
pub const SPD_MIN_ACTIVE_TO_PRECHARGE_DELAY: u8 = 30;
/// SPD byte 31: rank density bitfield.
pub const SPD_RANK_DENSITY: u8 = 31;
/// SPD byte 36: minimum write recovery time (tWR).
pub const SPD_MIN_WRITE_RECOVERY_TIME: u8 = 36;
/// SPD byte 37: minimum write-to-read delay (tWTR).
pub const SPD_MIN_WRITE_TO_READ_DELAY: u8 = 37;
/// SPD byte 38: minimum read-to-precharge (tRTP).
pub const SPD_MIN_READ_TO_PRECHARGE: u8 = 38;
/// SPD byte 40: tRC/tRFC fractional/high bits.
pub const SPD_TRC_TRFC_EXT: u8 = 40;
/// SPD byte 42: tRFC integer byte.
pub const SPD_TRFC_LO: u8 = 42;
/// SPD byte 62: DDR2 SPD revision.
pub const SPD_REVISION: u8 = 62;

/// DDR2 memory type identifier (SPD byte 2).
pub const DDR2: u8 = 0x08;

/// Read the DDR2 SPD payload from `addr` into a 256-byte scratch buffer.
///
/// DDR2 SPD data is 128 bytes. The returned buffer is zero-filled above byte
/// 127 so callers can keep using the project-wide [`DimmInfo::spd_data`] shape.
/// A failure on byte 0 is treated as an unpopulated slot.
pub fn read_spd(smbus: &mut dyn SmBus, addr: u8) -> Result<Option<[u8; 256]>, ServiceError> {
    let mut spd = [0u8; 256];
    for byte in 0..SPD_SIZE_MAX_DDR2 as u8 {
        match smbus.read_byte(addr, byte) {
            Ok(v) => spd[byte as usize] = v,
            Err(_) if byte == 0 => return Ok(None),
            Err(e) => return Err(e),
        }
    }

    if spd[..SPD_SIZE_MAX_DDR2].iter().all(|b| *b == 0) {
        return Ok(None);
    }

    Ok(Some(spd))
}

/// Return the index of the most-significant set bit in `value`.
pub fn msb_index(value: u8) -> Option<u8> {
    if value == 0 {
        None
    } else {
        Some(7 - value.leading_zeros() as u8)
    }
}

/// Decode DDR2 tCK encoding to units of 1/256 ns.
pub fn decode_tck_256ns(raw: u8) -> Option<u32> {
    let high = raw >> 4;
    let low = match raw & 0x0f {
        0x0..=0x9 => (raw & 0x0f) * 10,
        0x0a => 25,
        0x0b => 33,
        0x0c => 66,
        0x0d => 75,
        _ => return None,
    };

    Some((((high as u32) * 100 + low as u32) << 8) / 100)
}

/// Decode DDR2 BCD timing encoding to units of 1/256 ns.
pub fn decode_bcd_256ns(raw: u8) -> Option<u32> {
    let high = raw >> 4;
    let low = raw & 0x0f;
    if high >= 10 || low >= 10 {
        return None;
    }
    Some((((high as u32) * 10 + low as u32) << 8) / 100)
}

/// Decode DDR2 quarter-ns timing encoding to units of 1/256 ns.
pub fn decode_quarter_256ns(raw: u8) -> u32 {
    let high = raw >> 2;
    let low = 25 * (raw & 0x03);
    (((high as u32) * 100 + low as u32) << 8) / 100
}

fn decode_trfc_256ns(spd_data: &[u8; 256]) -> u32 {
    let b40 = spd_data[SPD_TRC_TRFC_EXT as usize];
    let b42 = spd_data[SPD_TRFC_LO as usize];

    let mut trfc = (b42 as u32) * 100;
    if b40 & 0x01 != 0 {
        trfc += 256 * 100;
    }

    trfc += match (b40 >> 1) & 0x07 {
        1 => 25,
        2 => 33,
        3 => 50,
        4 => 66,
        5 => 75,
        _ => 0,
    };

    (trfc << 8) / 100
}

fn rank_density_mb(spd_data: &[u8; 256]) -> u32 {
    let density = spd_data[SPD_RANK_DENSITY as usize].rotate_left(3);
    if density == 0 {
        0
    } else {
        128 * density as u32
    }
}

/// Decode DDR2 raw SPD data into a [`DimmInfo`].
///
/// Returns `None` if the memory type is not DDR2 or the data looks
/// unpopulated/invalid. Timing fields are decoded in the same 1/256 ns units
/// as coreboot's common DDR2 SPD library.
pub fn decode_dimm(spd_data: &[u8; 256]) -> Option<DimmInfo> {
    let mem_type = spd_data[crate::SPD_MEMORY_TYPE as usize];
    if mem_type != DDR2 {
        return None;
    }

    let revision = spd_data[SPD_REVISION as usize];
    if revision == 0 {
        return None;
    }

    let rows = spd_data[SPD_NUM_ROWS as usize] & 0x1F;
    let cols = spd_data[SPD_NUM_COLUMNS as usize] & 0x0F;
    if rows == 0 || cols == 0 {
        return None;
    }

    // DDR2 SPD byte 5 bits[2:0] = "number of ranks minus 1".
    // Value 0 → 1 rank, 1 → 2 ranks, 3 → 4 ranks.
    let ranks = (spd_data[SPD_NUM_DIMM_BANKS as usize] & 0x07).saturating_add(1);
    let banks = spd_data[SPD_NUM_BANKS_PER_SDRAM as usize];
    let primary_width = spd_data[SPD_PRIMARY_SDRAM_WIDTH as usize];
    if banks == 0 || primary_width == 0 {
        return None;
    }

    let width = match primary_width {
        4 => ChipWidth::X4,
        8 => ChipWidth::X8,
        16 => ChipWidth::X16,
        32 => ChipWidth::X32,
        _ => ChipWidth::X8,
    };

    // Chip capacity in bits = 2^rows * 2^cols * banks * chip width.
    let chip_cap_bits = (1u64 << rows as u64)
        .saturating_mul(1u64 << cols as u64)
        .saturating_mul(banks as u64)
        .saturating_mul(primary_width as u64);
    let chip_capacity = match chip_cap_bits {
        0..=0x0FFF_FFFF => ChipCapacity::Cap256M,
        0x1000_0000..=0x1FFF_FFFF => ChipCapacity::Cap512M,
        0x2000_0000..=0x3FFF_FFFF => ChipCapacity::Cap1G,
        0x4000_0000..=0x7FFF_FFFF => ChipCapacity::Cap2G,
        0x8000_0000..=0xFFFF_FFFF => ChipCapacity::Cap4G,
        0x1_0000_0000..=0x1_FFFF_FFFF => ChipCapacity::Cap8G,
        _ => ChipCapacity::Cap16G,
    };

    // Module data width (typically 64 for non-ECC, 72 for ECC).
    let module_width = spd_data[SPD_MODULE_DATA_WIDTH_LSB as usize] as u32
        | ((spd_data[SPD_MODULE_DATA_WIDTH_MSB as usize] as u32) << 8);
    if module_width == 0 {
        return None;
    }

    // Page size in bytes = 2^cols * chip_width_bytes. This is the value used
    // by Intel DDR2 controllers for page-width timing/address-decode choices.
    let page_size = (1u32 << cols as u32) * ((primary_width as u32).max(8) / 8);

    let rank_capacity_mb = match rank_density_mb(spd_data) {
        0 => {
            ((1u64 << rows as u64)
                .saturating_mul(1u64 << cols as u64)
                .saturating_mul(banks as u64)
                .saturating_mul(module_width as u64)
                / 8
                / 1024
                / 1024) as u32
        }
        mb => mb,
    };

    let cas_latencies = spd_data[SPD_SUPPORTED_CAS_LATENCIES as usize];
    let mut cycle_time_256ns = [0u32; 8];
    let mut access_time_256ns = [0u32; 8];
    if let Some(max_cas) = msb_index(cas_latencies) {
        cycle_time_256ns[max_cas as usize] =
            decode_tck_256ns(spd_data[SPD_MIN_CYCLE_TIME_AT_CAS_MAX as usize]).unwrap_or(0);
        access_time_256ns[max_cas as usize] =
            decode_bcd_256ns(spd_data[SPD_ACCESS_TIME_FROM_CLOCK as usize]).unwrap_or(0);

        if max_cas >= 1 && (cas_latencies & (1 << (max_cas - 1))) != 0 {
            cycle_time_256ns[(max_cas - 1) as usize] =
                decode_tck_256ns(spd_data[SPD_MIN_CYCLE_TIME_AT_CAS_MINUS_1 as usize]).unwrap_or(0);
            access_time_256ns[(max_cas - 1) as usize] =
                decode_bcd_256ns(spd_data[SPD_ACCESS_TIME_FROM_CLOCK_CAS_MINUS_1 as usize])
                    .unwrap_or(0);
        }

        if max_cas >= 2 && (cas_latencies & (1 << (max_cas - 2))) != 0 {
            cycle_time_256ns[(max_cas - 2) as usize] =
                decode_tck_256ns(spd_data[SPD_MIN_CYCLE_TIME_AT_CAS_MINUS_2 as usize]).unwrap_or(0);
            access_time_256ns[(max_cas - 2) as usize] =
                decode_bcd_256ns(spd_data[SPD_ACCESS_TIME_FROM_CLOCK_CAS_MINUS_2 as usize])
                    .unwrap_or(0);
        }
    }

    // tRFC: keep the compact raw field for legacy users, and expose decoded ns.
    let trfc = spd_data[SPD_TRFC_LO as usize] as u16
        | (((spd_data[SPD_TRC_TRFC_EXT as usize] & 0x01) as u16) << 8);

    Some(DimmInfo {
        card_type: revision,
        mem_type,
        width,
        chip_capacity,
        page_size,
        sides: if ranks > 1 { 2 } else { 1 },
        banks,
        ranks,
        rows,
        cols,
        cas_latencies,
        taa_min: spd_data[SPD_ACCESS_TIME_FROM_CLOCK as usize],
        tck_min: spd_data[SPD_MIN_CYCLE_TIME_AT_CAS_MAX as usize],
        cycle_time_256ns,
        access_time_256ns,
        twr: spd_data[SPD_MIN_WRITE_RECOVERY_TIME as usize],
        trp: spd_data[SPD_MIN_ROW_PRECHARGE_TIME as usize],
        trcd: spd_data[SPD_MIN_RAS_TO_CAS_DELAY as usize],
        tras: spd_data[SPD_MIN_ACTIVE_TO_PRECHARGE_DELAY as usize],
        trfc,
        twtr: spd_data[SPD_MIN_WRITE_TO_READ_DELAY as usize],
        trrd: spd_data[SPD_MIN_RAS_TO_RAS_DELAY as usize],
        trtp: spd_data[SPD_MIN_READ_TO_PRECHARGE as usize],
        twr_256ns: decode_quarter_256ns(spd_data[SPD_MIN_WRITE_RECOVERY_TIME as usize]),
        trp_256ns: decode_quarter_256ns(spd_data[SPD_MIN_ROW_PRECHARGE_TIME as usize]),
        trcd_256ns: decode_quarter_256ns(spd_data[SPD_MIN_RAS_TO_CAS_DELAY as usize]),
        tras_256ns: (spd_data[SPD_MIN_ACTIVE_TO_PRECHARGE_DELAY as usize] as u32) << 8,
        trfc_256ns: decode_trfc_256ns(spd_data),
        twtr_256ns: decode_quarter_256ns(spd_data[SPD_MIN_WRITE_TO_READ_DELAY as usize]),
        trrd_256ns: decode_quarter_256ns(spd_data[SPD_MIN_RAS_TO_RAS_DELAY as usize]),
        trtp_256ns: decode_quarter_256ns(spd_data[SPD_MIN_READ_TO_PRECHARGE as usize]),
        rank_capacity_mb,
        spd_data: *spd_data,
    })
}
