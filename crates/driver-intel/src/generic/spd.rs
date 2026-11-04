//! SPD (Serial Presence Detect) reader and decoder.
//!
//! Reads raw SPD EEPROM data over SMBus and decodes it into typed
//! [`DimmInfo`] structs.  Per-DDR-generation decoding lives in
//! submodules (`ddr2`, future `ddr3`, `ddr4`, `ddr5`).

use fstart_core::services::{ServiceError, SmBus};

pub mod ddr2;
pub mod ddr3;

// ===================================================================
// Shared SPD constants
// ===================================================================

/// SPD byte 2: memory type (shared across all DDR generations).
pub const SPD_MEMORY_TYPE: u8 = 2;

// ===================================================================
// Shared types
// ===================================================================

pub use fstart_driver_spd::{ChipCapacity, ChipWidth};

/// FSB clock frequency.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsbClock {
    Fsb800 = 0,
    Fsb1066 = 1,
}

/// Memory clock frequency.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum MemClock {
    #[default]
    Ddr667 = 0,
    Ddr800 = 1,
}

/// Decoded DIMM information from SPD data.
///
/// Contains the geometry, timing, and electrical characteristics
/// needed by the DRAM init code.
#[derive(Debug, Clone)]
pub struct DimmInfo {
    /// Module/card type from the technology-specific SPD field.
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
    /// Decoded tCK per CAS level, in units of 1/256 ns.
    pub cycle_time_256ns: [u32; 8],
    /// Decoded access time per CAS level, in units of 1/256 ns.
    pub access_time_256ns: [u32; 8],
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
    /// Decoded tWR, in units of 1/256 ns.
    pub twr_256ns: u32,
    /// Decoded tRP, in units of 1/256 ns.
    pub trp_256ns: u32,
    /// Decoded tRCD, in units of 1/256 ns.
    pub trcd_256ns: u32,
    /// Decoded tRAS, in units of 1/256 ns.
    pub tras_256ns: u32,
    /// Decoded tRFC, in units of 1/256 ns.
    pub trfc_256ns: u32,
    /// Decoded tWTR, in units of 1/256 ns.
    pub twtr_256ns: u32,
    /// Decoded tRRD, in units of 1/256 ns.
    pub trrd_256ns: u32,
    /// Decoded tRTP, in units of 1/256 ns.
    pub trtp_256ns: u32,
    /// Rank capacity in MiB.
    pub rank_capacity_mb: u32,
    /// ECC DIMM (SPD byte 11, any ECC bit set).
    pub is_ecc: bool,
    /// Registered DIMM (SPD byte 20 type, coreboot registered set).
    pub is_registered: bool,
    /// Stacked package (SPD byte 5, bit 4).
    pub is_stacked: bool,
    /// Burst length 8 supported (SPD byte 16, bit 3).
    pub supports_bl8: bool,
    /// Raw 256-byte SPD data (kept for direct access to uncommonly-used bytes).
    pub spd_data: [u8; 256],
}

// ===================================================================
// Shared functions
// ===================================================================

/// Read 256 bytes of SPD data from a DIMM at `addr` over SMBus.
pub fn read_spd<B: SmBus>(
    smbus: &mut B,
    addr: u8,
    buf: &mut [u8; 256],
) -> Result<(), ServiceError> {
    for i in 0..=255u8 {
        buf[i as usize] = smbus.read_byte(addr, i)?;
    }
    Ok(())
}

/// Fill `buf` from the EEPROM at `addr`; `false` when nothing answers.
///
/// Prefers the controller's I2C sequential read and, like coreboot, falls
/// back to byte reads when that fails.
pub(crate) fn read_eeprom<B: SmBus + ?Sized>(
    smbus: &mut B,
    addr: u8,
    offset: u8,
    buf: &mut [u8],
) -> Result<bool, ServiceError> {
    match smbus.i2c_eeprom_read(addr, offset, buf) {
        Ok(()) => return Ok(true),
        Err(ServiceError::NoDevice) => return Ok(false),
        Err(error) => fstart_log::warn!(
            "spd: I2C read at {:#x} failed (code {}), reading byte by byte",
            addr,
            error as u8
        ),
    }
    for (byte, cmd) in buf.iter_mut().zip(offset..=u8::MAX) {
        match smbus.read_byte(addr, cmd) {
            Ok(value) => *byte = value,
            Err(ServiceError::NoDevice) if cmd == offset => return Ok(false),
            Err(error) => return Err(error),
        }
    }
    Ok(true)
}

/// Check if a DIMM slot is populated.
pub fn dimm_is_populated(info: &DimmInfo) -> bool {
    info.card_type != 0
}
