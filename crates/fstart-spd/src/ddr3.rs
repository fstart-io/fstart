//! DDR3 SPD byte offsets and decoding (JEDEC SPD revision 1.x).
//!
//! The GM45 raminit code needs a compact, no-alloc view of DDR3 SPD data:
//! geometry, CAS support, selected timing minima in 1/256 ns units, and the
//! raw-card encoding used by Intel's memory-controller programming tables.
//! The validation helpers mirror the constraints enforced by coreboot's
//! `northbridge/intel/gm45/raminit.c::verify_ddr3_dimm()`.

use serde::{Deserialize, Serialize};

use crate::{ChipCapacity, ChipWidth, DDR3, SPD_MEMORY_TYPE};
use fstart_services::{ServiceError, SmBus};

/// Maximum DDR3 SPD payload size. DDR3 SPD EEPROMs expose 256 bytes.
pub const SPD_SIZE_MAX_DDR3: usize = 256;
/// SPD byte 1: SPD revision.
pub const SPD_REVISION: u8 = 1;
/// SPD byte 3: module type.
pub const SPD_MODULE_TYPE: u8 = 3;
/// SPD byte 4: SDRAM density and bank count.
pub const SPD_DENSITY_BANKS: u8 = 4;
/// SPD byte 5: SDRAM row/column addressing.
pub const SPD_ADDRESSING: u8 = 5;
/// SPD byte 6: nominal voltage.
pub const SPD_NOMINAL_VOLTAGE: u8 = 6;
/// SPD byte 7: module organization (device width and ranks).
pub const SPD_MODULE_ORGANIZATION: u8 = 7;
/// SPD byte 8: module bus width.
pub const SPD_MODULE_BUS_WIDTH: u8 = 8;
/// SPD byte 10: medium timebase dividend. GM45/coreboot requires 1.
pub const SPD_MTB_DIVIDEND: u8 = 10;
/// SPD byte 11: medium timebase divisor. GM45/coreboot requires 8.
pub const SPD_MTB_DIVISOR: u8 = 11;
/// SPD byte 12: minimum cycle time (tCKmin), in MTB units.
pub const SPD_TCK_MIN: u8 = 12;
/// SPD byte 14: CAS latency bitmap, bits CL4..CL11.
pub const SPD_CAS_LATENCIES_LSB: u8 = 14;
/// SPD byte 15: CAS latency bitmap, bits CL12..CL18.
pub const SPD_CAS_LATENCIES_MSB: u8 = 15;
/// SPD byte 16: minimum CAS access time (tAAmin), in MTB units.
pub const SPD_TAA_MIN: u8 = 16;
/// SPD byte 17: minimum write recovery time (tWRmin), in MTB units.
pub const SPD_TWR_MIN: u8 = 17;
/// SPD byte 18: minimum RAS-to-CAS delay (tRCDmin), in MTB units.
pub const SPD_TRCD_MIN: u8 = 18;
/// SPD byte 19: minimum row-to-row delay (tRRDmin), in MTB units.
pub const SPD_TRRD_MIN: u8 = 19;
/// SPD byte 20: minimum row precharge delay (tRPmin), in MTB units.
pub const SPD_TRP_MIN: u8 = 20;
/// SPD byte 21: upper nibbles for tRAS/tRC.
pub const SPD_TRAS_TRC_EXT: u8 = 21;
/// SPD byte 22: minimum active-to-precharge delay (tRASmin) low byte.
pub const SPD_TRAS_MIN_LSB: u8 = 22;
/// SPD byte 23: minimum active-to-active/refresh delay (tRCmin) low byte.
pub const SPD_TRC_MIN_LSB: u8 = 23;
/// SPD byte 24: minimum refresh recovery delay (tRFCmin) low byte.
pub const SPD_TRFC_MIN_LSB: u8 = 24;
/// SPD byte 25: minimum refresh recovery delay (tRFCmin) high byte.
pub const SPD_TRFC_MIN_MSB: u8 = 25;
/// SPD byte 26: minimum internal write-to-read delay (tWTRmin), in MTB units.
pub const SPD_TWTR_MIN: u8 = 26;
/// SPD byte 27: minimum internal read-to-precharge delay (tRTPmin), in MTB units.
pub const SPD_TRTP_MIN: u8 = 27;
/// SPD byte 28: upper nibble for tFAW.
pub const SPD_TFAW_EXT: u8 = 28;
/// SPD byte 29: minimum four activate window (tFAWmin) low byte.
pub const SPD_TFAW_MIN_LSB: u8 = 29;
/// SPD byte 60: module height and raw-card extension bits.
pub const SPD_MODULE_HEIGHT_RAW_CARD_EXT: u8 = 60;
/// SPD byte 62: reference raw card used by GM45 routing tables.
pub const SPD_REFERENCE_RAW_CARD: u8 = 62;
/// SPD bytes 117..118: module manufacturer ID, continuation then bank/code.
pub const SPD_MODULE_MANUFACTURER_ID_LSB: u8 = 117;
/// SPD bytes 117..118: module manufacturer ID, continuation then bank/code.
pub const SPD_MODULE_MANUFACTURER_ID_MSB: u8 = 118;
/// SPD bytes 122..125: module serial number.
pub const SPD_SERIAL_NUMBER: u8 = 122;
/// SPD bytes 128..145: module part number.
pub const SPD_PART_NUMBER: u8 = 128;
/// DDR3 serial number length in bytes.
pub const SPD_DDR3_SERIAL_LEN: usize = 4;
/// DDR3 part number length in bytes.
pub const SPD_DDR3_PART_LEN: usize = 18;

const MTB_1_8_NS_TO_256NS: u32 = 32;

/// DDR3 module type from SPD byte 3 bits [3:0].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Ddr3ModuleType {
    /// Raw card with no module type specified.
    Undefined,
    /// Registered DIMM.
    Rdimm,
    /// Unbuffered DIMM.
    Udimm,
    /// SO-DIMM.
    Sodimm,
    /// Micro-DIMM.
    MicroDimm,
    /// Mini-RDIMM.
    MiniRdimm,
    /// Mini-UDIMM.
    MiniUdimm,
    /// 72b SO-RDIMM.
    SoRdimm72b,
    /// 72b SO-UDIMM.
    SoUdimm72b,
    /// 16b SO-DIMM.
    SoDimm16b,
    /// 32b SO-DIMM.
    SoDimm32b,
    /// Unknown or reserved module type code.
    Unknown(u8),
}

impl Ddr3ModuleType {
    /// Decode a raw SPD module type byte.
    pub const fn from_spd(raw: u8) -> Self {
        match raw & 0x0f {
            0x00 => Self::Undefined,
            0x01 => Self::Rdimm,
            0x02 => Self::Udimm,
            0x03 => Self::Sodimm,
            0x04 => Self::MicroDimm,
            0x05 => Self::MiniRdimm,
            0x06 => Self::MiniUdimm,
            0x08 => Self::SoRdimm72b,
            0x09 => Self::SoUdimm72b,
            0x0c => Self::SoDimm16b,
            0x0d => Self::SoDimm32b,
            code => Self::Unknown(code),
        }
    }
}

/// Validation error from DDR3 SPD decoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Ddr3SpdError {
    /// The SPD EEPROM was absent or returned all zeroes.
    NotPopulated,
    /// SPD byte 2 was not DDR3 (`0x0b`).
    WrongMemoryType(u8),
    /// SPD module type is reserved or not useful for GM45 raminit.
    UnsupportedModuleType(u8),
    /// The bank-count encoding is unsupported by GM45.
    UnsupportedBanks(u8),
    /// The SDRAM device width is unsupported by GM45.
    UnsupportedDeviceWidth(u8),
    /// The SDRAM density is unsupported by GM45.
    UnsupportedChipCapacity(u8),
    /// The DIMM rank count is unsupported by GM45.
    UnsupportedRanks(u8),
    /// GM45/coreboot DDR3 code assumes MTB = 1/8 ns.
    UnsupportedTimebase { dividend: u8, divisor: u8 },
    /// The raw-card routing table is unsupported by GM45.
    UnsupportedRawCard(u8),
    /// Row/column/bus-width fields were invalid or out of range.
    InvalidGeometry,
    /// Required timing fields were zero or otherwise unusable.
    InvalidTiming,
}

/// Decoded DDR3 DIMM information.
///
/// This struct intentionally stores only compact decoded fields and small ID
/// arrays so it remains serde-friendly in firmware contexts.  Callers that need
/// byte-for-byte SPD data can keep the `[u8; 256]` returned by [`read_spd`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ddr3DimmInfo {
    /// SPD revision byte.
    pub revision: u8,
    /// Module type.
    pub module_type: Ddr3ModuleType,
    /// SDRAM density code from SPD byte 4 bits [3:0].
    pub density_code: u8,
    /// Raw-card code from SPD byte 62 bits [4:0].
    pub raw_card: u8,
    /// Raw-card code including extension bits used by coreboot validation.
    pub raw_card_with_extension: u8,
    /// Memory type, always `0x0b` for a decoded DDR3 DIMM.
    pub mem_type: u8,
    /// Chip width (x4/x8/x16/x32).
    pub width: ChipWidth,
    /// Chip capacity.
    pub chip_capacity: ChipCapacity,
    /// Page size in bytes.
    pub page_size: u32,
    /// Number of sides (1 = single-rank, 2 = dual-rank for GM45-supported DIMMs).
    pub sides: u8,
    /// Banks per SDRAM device.
    pub banks: u8,
    /// Number of ranks.
    pub ranks: u8,
    /// Row address bits.
    pub rows: u8,
    /// Column address bits.
    pub cols: u8,
    /// Primary module bus width in bits, excluding ECC extension.
    pub primary_bus_width_bits: u16,
    /// Total module bus width in bits, including ECC extension if present.
    pub bus_width_bits: u16,
    /// Supported CAS latencies. Bit `n` means CAS `n` is supported.
    pub cas_latencies: u32,
    /// tCKmin in units of 1/256 ns.
    pub tck_min_256ns: u32,
    /// tAAmin in units of 1/256 ns.
    pub taa_min_256ns: u32,
    /// tWRmin in units of 1/256 ns.
    pub twr_256ns: u32,
    /// tRCDmin in units of 1/256 ns.
    pub trcd_256ns: u32,
    /// tRRDmin in units of 1/256 ns.
    pub trrd_256ns: u32,
    /// tRPmin in units of 1/256 ns.
    pub trp_256ns: u32,
    /// tRASmin in units of 1/256 ns.
    pub tras_256ns: u32,
    /// tRCmin in units of 1/256 ns.
    pub trc_256ns: u32,
    /// tRFCmin in units of 1/256 ns.
    pub trfc_256ns: u32,
    /// tWTRmin in units of 1/256 ns.
    pub twtr_256ns: u32,
    /// tRTPmin in units of 1/256 ns.
    pub trtp_256ns: u32,
    /// tFAWmin in units of 1/256 ns.
    pub tfaw_256ns: u32,
    /// Capacity of one rank in MiB.
    pub rank_capacity_mb: u32,
    /// Module manufacturer ID bytes as a little-endian u16.
    pub module_manufacturer_id: u16,
    /// Module serial number bytes.
    pub serial: [u8; SPD_DDR3_SERIAL_LEN],
    /// Module part number bytes, ASCII padded with spaces by SPD convention.
    pub part_number: [u8; SPD_DDR3_PART_LEN],
}

/// Read the DDR3 SPD payload from `addr` into a 256-byte scratch buffer.
///
/// A failure on byte 0 is treated as an unpopulated slot, matching the DDR2
/// reader and the way firmware probes optional SPD EEPROMs.
pub fn read_spd(smbus: &mut dyn SmBus, addr: u8) -> Result<Option<[u8; 256]>, ServiceError> {
    let mut spd = [0u8; 256];
    for byte in 0..SPD_SIZE_MAX_DDR3 as u16 {
        let cmd = byte as u8;
        match smbus.read_byte(addr, cmd) {
            Ok(v) => spd[byte as usize] = v,
            Err(_) if byte == 0 => return Ok(None),
            Err(e) => return Err(e),
        }
    }

    if spd.iter().all(|b| *b == 0) {
        return Ok(None);
    }

    Ok(Some(spd))
}

/// Decode the raw JEDEC CAS bitmap into the project convention.
///
/// DDR3 SPD bytes 14..15 encode CL4 in bit 0, CL5 in bit 1, and so on.  GM45
/// raminit, like coreboot, uses bit `n` to mean CAS `n`, so the bitmap is
/// shifted left by four.
pub const fn decode_cas_mask(spd_data: &[u8; 256]) -> u32 {
    let raw = (spd_data[SPD_CAS_LATENCIES_LSB as usize] as u32)
        | ((spd_data[SPD_CAS_LATENCIES_MSB as usize] as u32) << 8);
    raw << 4
}

/// Convert a timing byte in DDR3 MTB units to 1/256 ns.
///
/// GM45/coreboot accepts only MTB = 1/8 ns (SPD bytes 10/11 = 1/8), so each
/// MTB unit is exactly 32 units of 1/256 ns.
pub const fn mtb_to_256ns(value: u8) -> u32 {
    (value as u32) * MTB_1_8_NS_TO_256NS
}

fn decode_capacity(code: u8) -> Option<ChipCapacity> {
    match code {
        0 => Some(ChipCapacity::Cap256M),
        1 => Some(ChipCapacity::Cap512M),
        2 => Some(ChipCapacity::Cap1G),
        3 => Some(ChipCapacity::Cap2G),
        4 => Some(ChipCapacity::Cap4G),
        5 => Some(ChipCapacity::Cap8G),
        6 => Some(ChipCapacity::Cap16G),
        _ => None,
    }
}

fn decode_width(code: u8) -> Option<(ChipWidth, u16)> {
    match code {
        0 => Some((ChipWidth::X4, 4)),
        1 => Some((ChipWidth::X8, 8)),
        2 => Some((ChipWidth::X16, 16)),
        3 => Some((ChipWidth::X32, 32)),
        _ => None,
    }
}

fn decode_primary_bus_width(code: u8) -> Option<u16> {
    match code {
        0 => Some(8),
        1 => Some(16),
        2 => Some(32),
        3 => Some(64),
        _ => None,
    }
}

fn decode_bus_extension_width(code: u8) -> Option<u16> {
    match code {
        0 => Some(0),
        1 => Some(8),
        _ => None,
    }
}

fn decode_banks(code: u8) -> Option<u8> {
    match code {
        0 => Some(8),
        1 => Some(16),
        _ => None,
    }
}

fn decode_extended_mtb(low: u8, ext: u8, shift: u8) -> u32 {
    let upper = ((ext >> shift) as u16) & 0x0f;
    ((upper << 8) | low as u16) as u32 * MTB_1_8_NS_TO_256NS
}

/// Decode raw DDR3 SPD data into [`Ddr3DimmInfo`].
///
/// This is a general decoder for JEDEC fields that GM45 needs.  Use
/// [`validate_gm45`] (or [`decode_dimm_gm45`]) to enforce the stricter Intel
/// GM45 limitations from coreboot.
pub fn decode_dimm(spd_data: &[u8; 256]) -> Result<Ddr3DimmInfo, Ddr3SpdError> {
    if spd_data.iter().all(|b| *b == 0) {
        return Err(Ddr3SpdError::NotPopulated);
    }

    let mem_type = spd_data[SPD_MEMORY_TYPE as usize];
    if mem_type != DDR3 {
        return Err(Ddr3SpdError::WrongMemoryType(mem_type));
    }

    let module_type_raw = spd_data[SPD_MODULE_TYPE as usize] & 0x0f;
    let module_type = Ddr3ModuleType::from_spd(module_type_raw);
    if matches!(module_type, Ddr3ModuleType::Unknown(_)) {
        return Err(Ddr3SpdError::UnsupportedModuleType(module_type_raw));
    }

    let density_code = spd_data[SPD_DENSITY_BANKS as usize] & 0x0f;
    let chip_capacity =
        decode_capacity(density_code).ok_or(Ddr3SpdError::UnsupportedChipCapacity(density_code))?;
    let bank_code = (spd_data[SPD_DENSITY_BANKS as usize] >> 4) & 0x07;
    let banks = decode_banks(bank_code).ok_or(Ddr3SpdError::UnsupportedBanks(bank_code))?;

    let cols = (spd_data[SPD_ADDRESSING as usize] & 0x07) + 9;
    let rows = ((spd_data[SPD_ADDRESSING as usize] >> 3) & 0x07) + 12;
    if !(9..=12).contains(&cols) || !(12..=18).contains(&rows) {
        return Err(Ddr3SpdError::InvalidGeometry);
    }

    let organization = spd_data[SPD_MODULE_ORGANIZATION as usize];
    let width_code = organization & 0x07;
    let (width, width_bits) =
        decode_width(width_code).ok_or(Ddr3SpdError::UnsupportedDeviceWidth(width_code))?;
    let ranks = ((organization >> 3) & 0x07) + 1;
    if ranks == 0 {
        return Err(Ddr3SpdError::InvalidGeometry);
    }

    let bus = spd_data[SPD_MODULE_BUS_WIDTH as usize];
    let primary_bus_width_bits =
        decode_primary_bus_width(bus & 0x07).ok_or(Ddr3SpdError::InvalidGeometry)?;
    let extension_bits =
        decode_bus_extension_width((bus >> 3) & 0x03).ok_or(Ddr3SpdError::InvalidGeometry)?;
    let bus_width_bits = primary_bus_width_bits + extension_bits;

    let dividend = spd_data[SPD_MTB_DIVIDEND as usize];
    let divisor = spd_data[SPD_MTB_DIVISOR as usize];
    if dividend != 1 || divisor != 8 {
        return Err(Ddr3SpdError::UnsupportedTimebase { dividend, divisor });
    }

    let raw_card = spd_data[SPD_REFERENCE_RAW_CARD as usize] & 0x1f;
    let raw_card_with_extension = spd_data[SPD_REFERENCE_RAW_CARD as usize] & 0x9f;
    let page_size = (u32::from(width_bits) / 8) * (1u32 << cols);
    if page_size == 0 || primary_bus_width_bits == 0 || width_bits == 0 {
        return Err(Ddr3SpdError::InvalidGeometry);
    }

    let rank_capacity_mb = (1u64 << rows)
        .saturating_mul(1u64 << cols)
        .saturating_mul(u64::from(banks))
        .saturating_mul(u64::from(primary_bus_width_bits))
        / 8
        / 1024
        / 1024;

    let tck_min_256ns = mtb_to_256ns(spd_data[SPD_TCK_MIN as usize]);
    let taa_min_256ns = mtb_to_256ns(spd_data[SPD_TAA_MIN as usize]);
    let twr_256ns = mtb_to_256ns(spd_data[SPD_TWR_MIN as usize]);
    let trcd_256ns = mtb_to_256ns(spd_data[SPD_TRCD_MIN as usize]);
    let trrd_256ns = mtb_to_256ns(spd_data[SPD_TRRD_MIN as usize]);
    let trp_256ns = mtb_to_256ns(spd_data[SPD_TRP_MIN as usize]);
    let tras_256ns = decode_extended_mtb(
        spd_data[SPD_TRAS_MIN_LSB as usize],
        spd_data[SPD_TRAS_TRC_EXT as usize],
        0,
    );
    let trc_256ns = decode_extended_mtb(
        spd_data[SPD_TRC_MIN_LSB as usize],
        spd_data[SPD_TRAS_TRC_EXT as usize],
        4,
    );
    let trfc_256ns = (u32::from(spd_data[SPD_TRFC_MIN_LSB as usize])
        | (u32::from(spd_data[SPD_TRFC_MIN_MSB as usize]) << 8))
        * MTB_1_8_NS_TO_256NS;
    let twtr_256ns = mtb_to_256ns(spd_data[SPD_TWTR_MIN as usize]);
    let trtp_256ns = mtb_to_256ns(spd_data[SPD_TRTP_MIN as usize]);
    let tfaw_256ns = decode_extended_mtb(
        spd_data[SPD_TFAW_MIN_LSB as usize],
        spd_data[SPD_TFAW_EXT as usize],
        0,
    );

    if tck_min_256ns == 0
        || taa_min_256ns == 0
        || twr_256ns == 0
        || trcd_256ns == 0
        || trp_256ns == 0
        || tras_256ns == 0
        || trfc_256ns == 0
    {
        return Err(Ddr3SpdError::InvalidTiming);
    }

    let mut serial = [0u8; SPD_DDR3_SERIAL_LEN];
    serial.copy_from_slice(
        &spd_data[SPD_SERIAL_NUMBER as usize..SPD_SERIAL_NUMBER as usize + SPD_DDR3_SERIAL_LEN],
    );
    let mut part_number = [0u8; SPD_DDR3_PART_LEN];
    part_number.copy_from_slice(
        &spd_data[SPD_PART_NUMBER as usize..SPD_PART_NUMBER as usize + SPD_DDR3_PART_LEN],
    );
    let module_manufacturer_id = u16::from(spd_data[SPD_MODULE_MANUFACTURER_ID_LSB as usize])
        | (u16::from(spd_data[SPD_MODULE_MANUFACTURER_ID_MSB as usize]) << 8);

    Ok(Ddr3DimmInfo {
        revision: spd_data[SPD_REVISION as usize],
        module_type,
        density_code,
        raw_card,
        raw_card_with_extension,
        mem_type,
        width,
        chip_capacity,
        page_size,
        sides: if ranks > 1 { 2 } else { 1 },
        banks,
        ranks,
        rows,
        cols,
        primary_bus_width_bits,
        bus_width_bits,
        cas_latencies: decode_cas_mask(spd_data),
        tck_min_256ns,
        taa_min_256ns,
        twr_256ns,
        trcd_256ns,
        trrd_256ns,
        trp_256ns,
        tras_256ns,
        trc_256ns,
        trfc_256ns,
        twtr_256ns,
        trtp_256ns,
        tfaw_256ns,
        rank_capacity_mb: rank_capacity_mb as u32,
        module_manufacturer_id,
        serial,
        part_number,
    })
}

/// Validate a decoded DDR3 DIMM against GM45/coreboot limits.
pub fn validate_gm45(info: &Ddr3DimmInfo) -> Result<(), Ddr3SpdError> {
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
    if !matches!(info.raw_card_with_extension, 0 | 1 | 2 | 3 | 5) {
        return Err(Ddr3SpdError::UnsupportedRawCard(
            info.raw_card_with_extension,
        ));
    }
    Ok(())
}

/// Decode raw DDR3 SPD and enforce the GM45/coreboot limitations.
pub fn decode_dimm_gm45(spd_data: &[u8; 256]) -> Result<Ddr3DimmInfo, Ddr3SpdError> {
    let info = decode_dimm(spd_data)?;
    validate_gm45(&info)?;
    Ok(info)
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use fstart_services::ServiceError;

    struct FakeSmBus {
        data: [u8; 256],
        fail_byte0: bool,
    }

    impl SmBus for FakeSmBus {
        fn read_byte(&mut self, _addr: u8, cmd: u8) -> Result<u8, ServiceError> {
            if self.fail_byte0 && cmd == 0 {
                Err(ServiceError::HardwareError)
            } else {
                Ok(self.data[cmd as usize])
            }
        }

        fn write_byte(&mut self, _addr: u8, _cmd: u8, _value: u8) -> Result<(), ServiceError> {
            Ok(())
        }
    }

    fn synthetic_ddr3_spd() -> [u8; 256] {
        let mut spd = [0u8; 256];
        spd[SPD_REVISION as usize] = 0x11;
        spd[SPD_MEMORY_TYPE as usize] = DDR3;
        spd[SPD_MODULE_TYPE as usize] = 0x03; // SO-DIMM
        spd[SPD_DENSITY_BANKS as usize] = 0x02; // 1Gb, 8 banks
        spd[SPD_ADDRESSING as usize] = (2 << 3) | 1; // 14 rows, 10 cols
        spd[SPD_MODULE_ORGANIZATION as usize] = (1 << 3) | 1; // 2 ranks, x8
        spd[SPD_MODULE_BUS_WIDTH as usize] = 0x03; // 64-bit primary bus
        spd[SPD_MTB_DIVIDEND as usize] = 1;
        spd[SPD_MTB_DIVISOR as usize] = 8;
        spd[SPD_TCK_MIN as usize] = 10; // 1.25ns => DDR3-800
        spd[SPD_CAS_LATENCIES_LSB as usize] = 0b0011_1000; // CL7, CL8, CL9
        spd[SPD_CAS_LATENCIES_MSB as usize] = 0;
        spd[SPD_TAA_MIN as usize] = 110; // 13.75ns
        spd[SPD_TWR_MIN as usize] = 120; // 15ns
        spd[SPD_TRCD_MIN as usize] = 110;
        spd[SPD_TRRD_MIN as usize] = 48; // 6ns
        spd[SPD_TRP_MIN as usize] = 110;
        spd[SPD_TRAS_TRC_EXT as usize] = (1 << 4) | 1;
        spd[SPD_TRAS_MIN_LSB as usize] = 0x18; // 280 MTB => 35ns
        spd[SPD_TRC_MIN_LSB as usize] = 0x86; // 390 MTB => 48.75ns
        spd[SPD_TRFC_MIN_LSB as usize] = 0xa0;
        spd[SPD_TRFC_MIN_MSB as usize] = 0x04; // 1184 MTB => 148ns
        spd[SPD_TWTR_MIN as usize] = 60;
        spd[SPD_TRTP_MIN as usize] = 60;
        spd[SPD_TFAW_EXT as usize] = 0;
        spd[SPD_TFAW_MIN_LSB as usize] = 0xf0; // 30ns
        spd[SPD_REFERENCE_RAW_CARD as usize] = 0x02;
        spd[SPD_MODULE_MANUFACTURER_ID_LSB as usize] = 0x80;
        spd[SPD_MODULE_MANUFACTURER_ID_MSB as usize] = 0x2c;
        spd[SPD_SERIAL_NUMBER as usize..SPD_SERIAL_NUMBER as usize + SPD_DDR3_SERIAL_LEN]
            .copy_from_slice(&[1, 2, 3, 4]);
        spd[SPD_PART_NUMBER as usize..SPD_PART_NUMBER as usize + SPD_DDR3_PART_LEN]
            .copy_from_slice(b"FSTART-DDR3-TEST  ");
        spd
    }

    #[test]
    fn decode_synthetic_ddr3_geometry_and_timings() {
        let spd = synthetic_ddr3_spd();
        let info = decode_dimm_gm45(&spd).expect("valid synthetic DDR3 SPD");

        assert_eq!(info.module_type, Ddr3ModuleType::Sodimm);
        assert_eq!(info.mem_type, DDR3);
        assert_eq!(info.width, ChipWidth::X8);
        assert_eq!(info.chip_capacity, ChipCapacity::Cap1G);
        assert_eq!(info.banks, 8);
        assert_eq!(info.ranks, 2);
        assert_eq!(info.rows, 14);
        assert_eq!(info.cols, 10);
        assert_eq!(info.page_size, 1024);
        assert_eq!(info.rank_capacity_mb, 1024);
        assert_eq!(info.cas_latencies, (0b0011_1000u32) << 4);
        assert_eq!(info.tck_min_256ns, 10 * 32);
        assert_eq!(info.taa_min_256ns, 110 * 32);
        assert_eq!(info.tras_256ns, 0x118 * 32);
        assert_eq!(info.trc_256ns, 0x186 * 32);
        assert_eq!(info.trfc_256ns, 0x04a0 * 32);
        assert_eq!(info.tfaw_256ns, 0xf0 * 32);
        assert_eq!(info.raw_card, 2);
        assert_eq!(info.serial, [1, 2, 3, 4]);
        assert_eq!(&info.part_number, b"FSTART-DDR3-TEST  ");
    }

    #[test]
    fn rejects_non_ddr3_and_unsupported_gm45_bank_count() {
        let mut spd = synthetic_ddr3_spd();
        spd[SPD_MEMORY_TYPE as usize] = 0x08;
        assert_eq!(decode_dimm(&spd), Err(Ddr3SpdError::WrongMemoryType(0x08)));

        let mut spd = synthetic_ddr3_spd();
        spd[SPD_DENSITY_BANKS as usize] = 1 << 4; // 16 banks
        let info = decode_dimm(&spd).expect("general decoder accepts 16-bank DDR3");
        assert_eq!(
            validate_gm45(&info),
            Err(Ddr3SpdError::UnsupportedBanks(16))
        );
    }

    #[test]
    fn read_spd_returns_none_for_absent_slot_and_reads_256_bytes() {
        let data = synthetic_ddr3_spd();
        let mut smbus = FakeSmBus {
            data,
            fail_byte0: false,
        };
        let read = read_spd(&mut smbus, 0x50)
            .expect("smbus read succeeds")
            .expect("slot populated");
        assert_eq!(read[SPD_MEMORY_TYPE as usize], DDR3);
        assert_eq!(read[SPD_PART_NUMBER as usize], b'F');

        let mut smbus = FakeSmBus {
            data: [0; 256],
            fail_byte0: true,
        };
        assert!(read_spd(&mut smbus, 0x50)
            .expect("absent slot is not fatal")
            .is_none());
    }
}
