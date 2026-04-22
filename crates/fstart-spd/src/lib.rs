//! SPD (Serial Presence Detect) reader and decoder for DDR2 DIMMs.
//!
//! Reads raw SPD EEPROM data over SMBus and decodes it into a typed
//! [`DimmInfo`] struct containing the timing and geometry information
//! needed by the DDR2 raminit code.
//!
//! SPD byte offsets and DDR2 decoding rules follow JEDEC standards
//! (SPD revision 1.2a for DDR2).

#![no_std]

use fstart_services::{ServiceError, SmBus};

// ===================================================================
// SPD byte offset constants (JEDEC standard)
// ===================================================================

/// SPD byte 2: memory type (0x08 = DDR2, 0x0B = DDR3).
pub const SPD_MEMORY_TYPE: u8 = 2;
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
/// SPD byte 18: supported CAS latencies bitmask.
pub const SPD_SUPPORTED_CAS_LATENCIES: u8 = 18;
/// SPD byte 27: minimum row precharge time (tRP).
pub const SPD_MIN_ROW_PRECHARGE_TIME: u8 = 27;
/// SPD byte 28: minimum row-active-to-row-active delay (tRRD).
pub const SPD_MIN_RAS_TO_RAS_DELAY: u8 = 28;
/// SPD byte 29: minimum RAS to CAS delay (tRCD).
pub const SPD_MIN_RAS_TO_CAS_DELAY: u8 = 29;
/// SPD byte 30: minimum active-to-precharge delay (tRAS).
pub const SPD_MIN_ACTIVE_TO_PRECHARGE_DELAY: u8 = 30;
/// SPD byte 36: minimum write recovery time (tWR).
pub const SPD_MIN_WRITE_RECOVERY_TIME: u8 = 36;
/// SPD byte 37: minimum write-to-read delay (tWTR).
pub const SPD_MIN_WRITE_TO_READ_DELAY: u8 = 37;
/// SPD byte 38: minimum internal read-to-precharge (tRTP).
pub const SPD_MIN_READ_TO_PRECHARGE: u8 = 38;
/// SPD byte 42: minimum refresh recovery delay (tRFC), low byte.
pub const SPD_TRFC_LO: u8 = 42;
/// SPD byte 40: tRFC high nibble (in upper 4 bits).
pub const SPD_TRFC_HI: u8 = 40;
/// SPD byte 62: raw card type.
pub const SPD_RAW_CARD: u8 = 62;

/// DDR2 memory type identifier.
pub const DDR2: u8 = 0x08;
/// DDR3 memory type identifier.
pub const DDR3: u8 = 0x0B;

// ===================================================================
// Enums
// ===================================================================

/// DDR chip width classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChipWidth {
    X4 = 0,
    X8 = 1,
    X16 = 2,
    X32 = 3,
}

/// DDR chip capacity classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChipCapacity {
    Cap256M = 0,
    Cap512M = 1,
    Cap1G = 2,
    Cap2G = 3,
    Cap4G = 4,
    Cap8G = 5,
    Cap16G = 6,
}

/// Front-side bus clock frequency.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsbClock {
    Fsb667MHz = 0,
    Fsb800MHz = 1,
}

/// Memory clock frequency.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemClock {
    Mem667MHz = 0,
    Mem800MHz = 1,
}

// ===================================================================
// DimmInfo
// ===================================================================

/// Decoded DIMM information from SPD data.
#[derive(Debug, Clone)]
pub struct DimmInfo {
    /// Raw card type from SPD byte 62.
    pub card_type: u8,
    /// Memory type (should be [`DDR2`]).
    pub mem_type: u8,
    /// SDRAM device width.
    pub width: ChipWidth,
    /// SDRAM chip capacity.
    pub chip_capacity: ChipCapacity,
    /// DIMM page size in bytes.
    pub page_size: u32,
    /// Number of sides (1 or 2).
    pub sides: u8,
    /// Banks per SDRAM device (4 or 8).
    pub banks: u8,
    /// Number of ranks.
    pub ranks: u8,
    /// Row address bits.
    pub rows: u8,
    /// Column address bits.
    pub cols: u8,
    /// Supported CAS latency bitmask.
    pub cas_latencies: u8,
    /// Minimum access time at CAS max (raw SPD).
    pub taa_min: u8,
    /// Minimum cycle time at CAS max (raw SPD).
    pub tck_min: u8,
    /// Minimum write recovery time (tWR) in clock-cycle units.
    pub twr: u8,
    /// Minimum row precharge time (tRP).
    pub trp: u8,
    /// Minimum RAS-to-CAS delay (tRCD).
    pub trcd: u8,
    /// Minimum active-to-precharge (tRAS).
    pub tras: u8,
    /// Minimum refresh recovery (tRFC) in ns.
    pub trfc: u16,
    /// Minimum write-to-read delay (tWTR).
    pub twtr: u8,
    /// Minimum row-active-to-row-active (tRRD).
    pub trrd: u8,
    /// Minimum read-to-precharge (tRTP).
    pub trtp: u8,
    /// Capacity per rank in megabytes.
    pub rank_capacity_mb: u32,
    /// Raw SPD data.
    pub spd_data: [u8; 256],
}

// ===================================================================
// Functions
// ===================================================================

/// Read 256 bytes of SPD data from a DIMM at `addr` over SMBus.
pub fn read_spd(smbus: &mut dyn SmBus, addr: u8, buf: &mut [u8; 256]) -> Result<(), ServiceError> {
    for i in 0..=255u8 {
        buf[i as usize] = smbus.read_byte(addr, i)?;
    }
    Ok(())
}

/// Check if a DIMM slot is populated.
pub fn dimm_is_populated(info: &DimmInfo) -> bool {
    info.card_type != 0
}

/// Decode raw SPD data into a [`DimmInfo`] struct.
///
/// Returns `None` if the memory type is not DDR2 or the data looks
/// unpopulated (all zeros).
pub fn decode_dimm(spd_data: &[u8; 256]) -> Option<DimmInfo> {
    let mem_type = spd_data[SPD_MEMORY_TYPE as usize];
    if mem_type != DDR2 {
        return None;
    }

    let card_type = spd_data[SPD_RAW_CARD as usize] & 0x1F;
    if card_type == 0 {
        return None;
    }

    let rows = spd_data[SPD_NUM_ROWS as usize] & 0x1F;
    let cols = spd_data[SPD_NUM_COLUMNS as usize] & 0x0F;
    if rows == 0 || cols == 0 {
        return None;
    }

    let ranks = (spd_data[SPD_NUM_DIMM_BANKS as usize] & 0x07).max(1);
    let banks = spd_data[SPD_NUM_BANKS_PER_SDRAM as usize];
    let primary_width = spd_data[SPD_PRIMARY_SDRAM_WIDTH as usize];

    let width = match primary_width {
        4 => ChipWidth::X4,
        8 => ChipWidth::X8,
        16 => ChipWidth::X16,
        32 => ChipWidth::X32,
        _ => ChipWidth::X8,
    };

    // Chip capacity in bits = 2^rows * 2^cols * banks * width
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

    // Page size in bytes = 2^cols * (module_width / 8).
    let page_size = (1u32 << cols as u32) * (module_width / 8);

    // Rank capacity in MB.
    let rank_capacity_mb = ((1u64 << rows as u64)
        .saturating_mul(1u64 << cols as u64)
        .saturating_mul(banks as u64)
        .saturating_mul(module_width as u64)
        / 8
        / 1024
        / 1024) as u32;

    // tRFC: byte 42 is low byte, byte 40 upper nibble has high 4 bits.
    let trfc = spd_data[SPD_TRFC_LO as usize] as u16
        | (((spd_data[SPD_TRFC_HI as usize] >> 4) as u16) << 8);

    Some(DimmInfo {
        card_type,
        mem_type,
        width,
        chip_capacity,
        page_size,
        sides: if ranks > 1 { 2 } else { 1 },
        banks,
        ranks,
        rows,
        cols,
        cas_latencies: spd_data[SPD_SUPPORTED_CAS_LATENCIES as usize],
        taa_min: spd_data[SPD_ACCESS_TIME_FROM_CLOCK as usize],
        tck_min: spd_data[SPD_MIN_CYCLE_TIME_AT_CAS_MAX as usize],
        twr: spd_data[SPD_MIN_WRITE_RECOVERY_TIME as usize],
        trp: spd_data[SPD_MIN_ROW_PRECHARGE_TIME as usize],
        trcd: spd_data[SPD_MIN_RAS_TO_CAS_DELAY as usize],
        tras: spd_data[SPD_MIN_ACTIVE_TO_PRECHARGE_DELAY as usize],
        trfc,
        twtr: spd_data[SPD_MIN_WRITE_TO_READ_DELAY as usize],
        trrd: spd_data[SPD_MIN_RAS_TO_RAS_DELAY as usize],
        trtp: spd_data[SPD_MIN_READ_TO_PRECHARGE as usize],
        rank_capacity_mb,
        spd_data: *spd_data,
    })
}
