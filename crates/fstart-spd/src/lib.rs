//! SPD (Serial Presence Detect) reader and decoder.
//!
//! Reads raw SPD EEPROM data over SMBus and decodes it into typed
//! [`DimmInfo`] structs.  Per-DDR-generation decoding lives in
//! submodules (`ddr2`, future `ddr3`, `ddr4`, `ddr5`).

#![no_std]

use fstart_services::{ServiceError, SmBus};

pub mod ddr2;

// Re-export DDR2 for backward compatibility — existing callers use
// `fstart_spd::decode_dimm`, `fstart_spd::SPD_NUM_ROWS`, etc.
pub use ddr2::*;

// ===================================================================
// Shared SPD constants
// ===================================================================

/// SPD byte 2: memory type (shared across all DDR generations).
pub const SPD_MEMORY_TYPE: u8 = 2;

/// DDR3 memory type identifier.
pub const DDR3: u8 = 0x0B;

// ===================================================================
// Shared types
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

/// FSB clock frequency.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsbClock {
    Fsb800 = 0,
    Fsb1066 = 1,
}

/// Memory clock frequency.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemClock {
    Ddr667 = 0,
    Ddr800 = 1,
}

/// Decoded DIMM information from SPD data.
///
/// Contains the geometry, timing, and electrical characteristics
/// needed by the DRAM init code.
#[derive(Debug, Clone)]
pub struct DimmInfo {
    /// Raw card type (from SPD).
    pub card_type: u8,
    /// Memory type (0x08 = DDR2, 0x0B = DDR3).
    pub mem_type: u8,
    /// Chip width (x4/x8/x16/x32).
    pub width: ChipWidth,
    /// Chip capacity.
    pub chip_capacity: ChipCapacity,
    /// Page size in bytes.
    pub page_size: u32,
    /// Number of sides (1 = single-sided, 2 = double-sided).
    pub sides: u8,
    /// Banks per SDRAM device.
    pub banks: u8,
    /// Number of ranks.
    pub ranks: u8,
    /// Row address bits.
    pub rows: u8,
    /// Column address bits.
    pub cols: u8,
    /// Supported CAS latencies (bitmask).
    pub cas_latencies: u8,
    /// Minimum access time from clock (tAA), raw SPD encoding.
    pub taa_min: u8,
    /// Minimum cycle time at max CAS (tCK), raw SPD encoding.
    pub tck_min: u8,
    /// tWR (write recovery).
    pub twr: u8,
    /// tRP (row precharge).
    pub trp: u8,
    /// tRCD (RAS-to-CAS delay).
    pub trcd: u8,
    /// tRAS (active-to-precharge).
    pub tras: u8,
    /// tRFC (refresh cycle time) in raw SPD units.
    pub trfc: u16,
    /// tWTR (write-to-read delay).
    pub twtr: u8,
    /// tRRD (row-to-row delay).
    pub trrd: u8,
    /// tRTP (read-to-precharge).
    pub trtp: u8,
    /// Rank capacity in MiB.
    pub rank_capacity_mb: u32,
    /// Raw 256-byte SPD data (kept for direct access to uncommonly-used bytes).
    pub spd_data: [u8; 256],
}

// ===================================================================
// Shared functions
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
