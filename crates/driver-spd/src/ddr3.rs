//! DDR3 SPD byte offsets and decoding (JEDEC SPD revision 1.x).
//!
//! A compact, no-alloc view of DDR3 SPD data for memory-controller drivers:
//! geometry, CAS support, timing minima in 1/256 ns units, the raw-card
//! encoding and the SMBIOS Type 17 inventory. Controller limits (coreboot
//! `verify_ddr3_dimm()` and the like) belong to the controller's driver.

use crate::{ChipCapacity, ChipWidth};

/// SPD byte 2: memory type (shared across all DDR generations).
pub const SPD_MEMORY_TYPE: u8 = 2;
/// DDR3 memory type identifier.
pub const DDR3: u8 = 0x0B;

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
/// SPD byte 9: fine timebase (FTB) dividend (bits 7:4) and divisor
/// (bits 3:0), in ps.
pub const SPD_FTB: u8 = 9;
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
/// SPD byte 34: fine offset of tCKmin, in signed FTB units. The fine
/// offsets (bytes 34..38) are absent before SPD revision 1.1.
pub const SPD_TCK_MIN_FINE: u8 = 34;
/// SPD byte 35: fine offset of tAAmin, in signed FTB units.
pub const SPD_TAA_MIN_FINE: u8 = 35;
/// SPD byte 36: fine offset of tRCDmin, in signed FTB units.
pub const SPD_TRCD_MIN_FINE: u8 = 36;
/// SPD byte 37: fine offset of tRPmin, in signed FTB units.
pub const SPD_TRP_MIN_FINE: u8 = 37;
/// SPD byte 38: fine offset of tRCmin, in signed FTB units.
pub const SPD_TRC_MIN_FINE: u8 = 38;
/// SPD byte 60: module height and raw-card extension bits.
pub const SPD_MODULE_HEIGHT_RAW_CARD_EXT: u8 = 60;
/// SPD byte 62: reference raw card used by GM45 routing tables.
pub const SPD_REFERENCE_RAW_CARD: u8 = 62;
/// SPD byte 117: module manufacturer ID, continuation count and parity.
pub const SPD_MODULE_MANUFACTURER_ID_LSB: u8 = 117;
/// SPD byte 118: module manufacturer ID, JEDEC code in that bank.
pub const SPD_MODULE_MANUFACTURER_ID_MSB: u8 = 118;
/// SPD bytes 122..125: module serial number.
pub const SPD_SERIAL_NUMBER: u8 = 122;
/// SPD bytes 128..145: module part number.
pub const SPD_PART_NUMBER: u8 = 128;
/// DDR3 serial number length in bytes.
pub const SPD_DDR3_SERIAL_LEN: usize = 4;
/// DDR3 part number length in bytes.
pub const SPD_DDR3_PART_LEN: usize = 18;

/// DDR3 module type from SPD byte 3 bits [3:0].
#[derive(Debug, Clone, Copy, PartialEq, Eq, ufmt::derive::uDebug)]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, ufmt::derive::uDebug)]
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
/// This struct stores compact decoded fields and small IDs. Callers that need
/// byte-for-byte SPD data can keep the `[u8; 256]` returned by [`read_spd`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    /// The module asks for mirrored address lines on odd ranks
    /// (module-specific SPD byte 63 bit 0, as coreboot's MRC reads it).
    pub mirrored: bool,
    /// Module manufacturer ID bytes as a little-endian u16.
    pub module_manufacturer_id: u16,
    /// Module serial number bytes.
    pub serial: [u8; SPD_DDR3_SERIAL_LEN],
    /// Module part number bytes, ASCII padded with spaces by SPD convention.
    pub part_number: [u8; SPD_DDR3_PART_LEN],
}

impl Ddr3DimmInfo {
    /// Capacity of the whole module in MiB.
    #[must_use]
    pub const fn size_mb(&self) -> u32 {
        self.rank_capacity_mb * self.ranks as u32
    }
}

/// The SMBIOS Type 17 record of a decoded DIMM (coreboot
/// `gm45/raminit_meminfo.c`). `spd` holds at least SPD bytes 0..13;
/// `configured_mts` is the data rate raminit programmed.
pub fn memory_device(
    dimm: &Ddr3DimmInfo,
    spd: &[u8],
    channel: u8,
    slot: u8,
    configured_mts: u16,
) -> fstart_core::memory_info::MemoryDevice {
    use fstart_core::memory_info as mi;
    let (form_factor, buffering) = match dimm.module_type {
        Ddr3ModuleType::Rdimm | Ddr3ModuleType::MiniRdimm => {
            (mi::FORM_FACTOR_DIMM, mi::TYPE_DETAIL_REGISTERED)
        }
        Ddr3ModuleType::Udimm | Ddr3ModuleType::MiniUdimm | Ddr3ModuleType::MicroDimm => {
            (mi::FORM_FACTOR_DIMM, mi::TYPE_DETAIL_UNBUFFERED)
        }
        Ddr3ModuleType::Sodimm
        | Ddr3ModuleType::SoUdimm72b
        | Ddr3ModuleType::SoDimm16b
        | Ddr3ModuleType::SoDimm32b => (mi::FORM_FACTOR_SODIMM, mi::TYPE_DETAIL_UNBUFFERED),
        Ddr3ModuleType::SoRdimm72b => (mi::FORM_FACTOR_SODIMM, mi::TYPE_DETAIL_REGISTERED),
        _ => (mi::FORM_FACTOR_UNKNOWN, 0),
    };
    let byte = |index: u8| spd.get(usize::from(index)).copied().unwrap_or(0);
    // Byte 8: primary bus width 8 << [2:0], ECC extension 8 * [4:3].
    let bus_width = byte(SPD_MODULE_BUS_WIDTH);
    let data_width = 8u16 << (bus_width & 7).min(3);
    let total_width = data_width + 8 * u16::from((bus_width >> 3) & 3);
    // tCKmin in 1/8 ns medium timebase units: MT/s = 2000 / tCK[ns].
    let max_mts = match byte(SPD_TCK_MIN) {
        0 => 0,
        tck => (16_000 / u32::from(tck)) as u16,
    };
    let [jedec_bank, jedec_id] = dimm.module_manufacturer_id.to_le_bytes();
    let mut part_number = [b' '; 30];
    part_number[..SPD_DDR3_PART_LEN].copy_from_slice(&dimm.part_number);
    mi::MemoryDevice {
        size_mib: dimm
            .rank_capacity_mb
            .saturating_mul(u32::from(dimm.ranks))
            .into(),
        configured_mts: configured_mts.into(),
        max_mts: max_mts.into(),
        // Byte 6 bit 0 clear: 1.5 V operable, which the GM45 drives.
        voltage_mv: 1500.into(),
        data_width: data_width.into(),
        total_width: total_width.into(),
        type_detail: (mi::TYPE_DETAIL_SYNCHRONOUS | buffering).into(),
        // Byte 117: continuation codes (bit 7 is parity); byte 118: the id.
        jedec_bank: jedec_bank & 0x7f,
        jedec_id,
        memory_type: mi::MEMORY_TYPE_DDR3,
        form_factor,
        ranks: dimm.ranks,
        channel,
        slot,
        serial: dimm.serial,
        part_number,
    }
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
/// A timing byte in the module's own medium-timebase units, in 1/256 ns.
///
/// The SPD gives the MTB as dividend/divisor ns: 1/8 ns (32 here) is the
/// common case, not the only one.
pub const fn mtb_to_256ns(value: u8, mtb_256ns: u32) -> u32 {
    (value as u32) * mtb_256ns
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

fn decode_extended_mtb(low: u8, ext: u8, shift: u8, mtb_256ns: u32) -> u32 {
    let upper = ((ext >> shift) as u16) & 0x0f;
    ((upper << 8) | low as u16) as u32 * mtb_256ns
}

/// The fine-timebase correction of one timing byte, in 1/256 ns.
///
/// The FTB (SPD byte 9) is dividend/divisor in ps and the offset byte is a
/// signed count of those units. The result is rounded *up*, so a corrected
/// minimum time is never shorter than the module's: underestimating one
/// would program a timing the module does not meet.
fn fine_correction(raw: u8, ftb_dividend: u32, ftb_divisor: u32) -> i32 {
    let units = i32::from(raw as i8) * ftb_dividend as i32 * 256;
    let denominator = ftb_divisor as i32 * 1000;
    (units + denominator - 1).div_euclid(denominator)
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
    if !(9..=12).contains(&cols) || !(12..=16).contains(&rows) {
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
    // MTB is dividend/divisor ns, in 1/256 ns. Every JEDEC timing is a
    // whole number of MTB units, so a timebase that rounds to nothing is
    // unusable rather than merely unusual.
    let mtb_256ns = (u32::from(dividend) * 256).div_ceil(u32::from(divisor).max(1));
    if dividend == 0 || divisor == 0 || mtb_256ns == 0 {
        return Err(Ddr3SpdError::UnsupportedTimebase { dividend, divisor });
    }
    // Fine offsets exist from SPD revision 1.1 and need an FTB divisor.
    let ftb_dividend = u32::from(spd_data[SPD_FTB as usize] >> 4);
    let ftb_divisor = u32::from(spd_data[SPD_FTB as usize] & 0x0f);
    let fine = |byte: u8| {
        if spd_data[SPD_REVISION as usize] >= 0x11 && ftb_divisor != 0 {
            fine_correction(spd_data[usize::from(byte)], ftb_dividend, ftb_divisor)
        } else {
            0
        }
    };
    let corrected = |value: u32, byte: u8| {
        i64::from(value)
            .saturating_add(i64::from(fine(byte)))
            .clamp(0, i64::from(u32::MAX)) as u32
    };

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

    let tck_min_256ns = corrected(
        mtb_to_256ns(spd_data[SPD_TCK_MIN as usize], mtb_256ns),
        SPD_TCK_MIN_FINE,
    );
    let taa_min_256ns = corrected(
        mtb_to_256ns(spd_data[SPD_TAA_MIN as usize], mtb_256ns),
        SPD_TAA_MIN_FINE,
    );
    let twr_256ns = mtb_to_256ns(spd_data[SPD_TWR_MIN as usize], mtb_256ns);
    let trcd_256ns = corrected(
        mtb_to_256ns(spd_data[SPD_TRCD_MIN as usize], mtb_256ns),
        SPD_TRCD_MIN_FINE,
    );
    let trrd_256ns = mtb_to_256ns(spd_data[SPD_TRRD_MIN as usize], mtb_256ns);
    let trp_256ns = corrected(
        mtb_to_256ns(spd_data[SPD_TRP_MIN as usize], mtb_256ns),
        SPD_TRP_MIN_FINE,
    );
    let tras_256ns = decode_extended_mtb(
        spd_data[SPD_TRAS_MIN_LSB as usize],
        spd_data[SPD_TRAS_TRC_EXT as usize],
        0,
        mtb_256ns,
    );
    let trc_256ns = corrected(
        decode_extended_mtb(
            spd_data[SPD_TRC_MIN_LSB as usize],
            spd_data[SPD_TRAS_TRC_EXT as usize],
            4,
            mtb_256ns,
        ),
        SPD_TRC_MIN_FINE,
    );
    let trfc_256ns = (u32::from(spd_data[SPD_TRFC_MIN_LSB as usize])
        | (u32::from(spd_data[SPD_TRFC_MIN_MSB as usize]) << 8))
        * mtb_256ns;
    let twtr_256ns = mtb_to_256ns(spd_data[SPD_TWTR_MIN as usize], mtb_256ns);
    let trtp_256ns = mtb_to_256ns(spd_data[SPD_TRTP_MIN as usize], mtb_256ns);
    let tfaw_256ns = decode_extended_mtb(
        spd_data[SPD_TFAW_MIN_LSB as usize],
        spd_data[SPD_TFAW_EXT as usize],
        0,
        mtb_256ns,
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
        mirrored: spd_data[63] & 1 != 0,
        module_manufacturer_id,
        serial,
        part_number,
    })
}

/// SPD bytes 117..127 identify the module independently of its timings.
const UNIQUE_ID: core::ops::Range<usize> = 117..128;

/// Optional DDR3 features and thermal options (coreboot `dimm_flags_ddr3_st`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Ddr3Flags {
    pub operable_1_50v: bool,
    pub operable_1_35v: bool,
    pub operable_1_25v: bool,
    pub dll_off_mode: bool,
    pub rzq6_supported: bool,
    pub rzq7_supported: bool,
    pub pasr: bool,
    pub odts: bool,
    pub asr: bool,
    pub ext_temp_range: bool,
    pub ext_temp_refresh: bool,
    pub therm_sensor: bool,
    pub is_ecc: bool,
    pub pins_mirrored: bool,
}

/// XMP profile selector (coreboot `ddr3_xmp_profile`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XmpProfile {
    One,
    Two,
}

/// DDR3 timing attributes in 1/256 ns with fine-timebase corrections.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ddr3Attr {
    pub dimm_type: u8,
    /// Supported CAS latencies; bit 0 is CL4.
    pub cas_supported: u16,
    pub flags: Ddr3Flags,
    pub width: u8,
    pub ranks: u8,
    pub row_bits: u8,
    pub col_bits: u8,
    pub size_mb: u32,
    pub t_ck: u32,
    pub t_aa: u32,
    pub t_wr: u32,
    pub t_rcd: u32,
    pub t_rrd: u32,
    pub t_rp: u32,
    pub t_ras: u32,
    pub t_rc: u32,
    pub t_rfc: u32,
    pub t_wtr: u32,
    pub t_rtp: u32,
    pub t_faw: u32,
    /// Only XMP profiles specify tCWL and command rate; zero otherwise.
    pub t_cwl: u32,
    pub t_cmd: u32,
    pub reference_card: u8,
    pub voltage: u16,
    pub dimms_per_channel: u8,
}

/// Why an SPD cannot describe a usable DDR3 module (coreboot `SPD_STATUS_*`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ddr3AttrError {
    NotDdr3,
    Crc,
    InvalidField,
    NoXmp,
}

/// Manufacturing-data CRC used to detect a changed DIMM.
#[must_use]
pub fn unique_crc(spd: &[u8; 256]) -> u16 {
    crate::crc16(&spd[UNIQUE_ID])
}

fn spd_crc_matches(spd: &[u8; 256]) -> bool {
    let covered = if spd[0] & 0x80 != 0 { 117 } else { 126 };
    crate::crc16(&spd[..covered]) == u16::from_le_bytes([spd[126], spd[127]])
}

/// Medium timebase in 1/256 ns: dividend / divisor ns.
fn mtb(dividend: u8, divisor: u8) -> Option<u32> {
    (divisor != 0).then(|| (u32::from(dividend) << 8) / u32::from(divisor))
}

fn ftb_correct(value: u32, correction: u8, ftb: u32) -> u32 {
    let delta = (i32::from(correction as i8) * ftb as i32 - 500) / 1000;
    value.wrapping_add_signed(delta)
}

/// Decode JEDEC timings as coreboot `spd_decode_ddr3` does. Unlike
/// [`decode_dimm`], CRC and field errors may accompany decoded attributes;
/// the controller decides whether those attributes are safe to use.
pub fn decode_attr(spd: &[u8; 256]) -> Result<(Ddr3Attr, Option<Ddr3AttrError>), Ddr3AttrError> {
    if spd[SPD_MEMORY_TYPE as usize] != DDR3 {
        return Err(Ddr3AttrError::NotDdr3);
    }
    let mut status = (!spd_crc_matches(spd)).then_some(Ddr3AttrError::Crc);
    let mut invalid = |bad: bool| {
        if bad {
            status = Some(Ddr3AttrError::InvalidField);
        }
    };
    invalid((spd[4] >> 4) & 7 > 3);
    let capacity_shift = spd[4] & 0x0f;
    invalid(capacity_shift > 6);
    let row_code = (spd[5] >> 3) & 7;
    let col_code = spd[5] & 7;
    invalid(row_code > 4 || col_code > 3);
    let mut flags = Ddr3Flags::default();
    let mut voltage = 0;
    if spd[6] & (1 << 2) != 0 {
        flags.operable_1_25v = true;
        voltage = 1250;
    }
    if spd[6] & (1 << 1) != 0 {
        flags.operable_1_35v = true;
        voltage = 1300;
    }
    if spd[6] & 1 == 0 {
        flags.operable_1_50v = true;
        voltage = 1500;
    }
    let rank_code = (spd[7] >> 3) & 7;
    invalid(rank_code > 3);
    let width_code = spd[7] & 7;
    invalid(width_code > 3);
    let ranks = rank_code + 1;
    let width = 4u8.wrapping_shl(u32::from(width_code));
    let extension = (spd[8] >> 3) & 3;
    invalid(extension > 1);
    flags.is_ecc = extension != 0;
    let bus_code = spd[8] & 7;
    invalid(bus_code > 3);
    let bus_width = 8u32 << bus_code.min(7);
    let size_mb = if width == 0 {
        0
    } else {
        ((1u32 << (u32::from(capacity_shift.min(15)) + 5)) * bus_width * u32::from(ranks))
            / u32::from(width)
    };
    let mtb = mtb(spd[10], spd[11]).ok_or(Ddr3AttrError::InvalidField)?;
    let byte = |index: usize| u32::from(spd[index]);
    let mut attr = Ddr3Attr {
        dimm_type: spd[3] & 0x0f,
        cas_supported: u16::from_le_bytes([spd[14], spd[15]]),
        flags,
        width,
        ranks,
        row_bits: row_code + 12,
        col_bits: col_code + 9,
        size_mb,
        t_ck: byte(12) * mtb,
        t_aa: byte(16) * mtb,
        t_wr: byte(17) * mtb,
        t_rcd: byte(18) * mtb,
        t_rrd: byte(19) * mtb,
        t_rp: byte(20) * mtb,
        t_ras: (((byte(21) & 0x0f) << 8) + byte(22)) * mtb,
        t_rc: (((byte(21) & 0xf0) << 4) + byte(23)) * mtb,
        t_rfc: ((byte(25) << 8) + byte(24)) * mtb,
        t_wtr: byte(26) * mtb,
        t_rtp: byte(27) * mtb,
        t_faw: (((byte(28) & 0x0f) << 8) + byte(29)) * mtb,
        t_cwl: 0,
        t_cmd: 0,
        reference_card: spd[62] & 0x1f,
        voltage,
        dimms_per_channel: 3,
    };
    if spd[1] >= 0x11 && spd[9] & 0x0f != 0 {
        let ftb = ((u32::from(spd[9]) & 0xf0) << 4) / (u32::from(spd[9]) & 0x0f);
        attr.t_ck = ftb_correct(attr.t_ck, spd[34], ftb);
        attr.t_aa = ftb_correct(attr.t_aa, spd[35], ftb);
        attr.t_rcd = ftb_correct(attr.t_rcd, spd[36], ftb);
        attr.t_rp = ftb_correct(attr.t_rp, spd[37], ftb);
        attr.t_rc = ftb_correct(attr.t_rc, spd[38], ftb);
    }
    let features = spd[30];
    attr.flags.dll_off_mode = features & 0x80 != 0;
    attr.flags.rzq7_supported = features & 0x02 != 0;
    attr.flags.rzq6_supported = features & 0x01 != 0;
    let thermal = spd[31];
    attr.flags.pasr = thermal & 0x80 != 0;
    attr.flags.odts = thermal & 0x08 != 0;
    attr.flags.asr = thermal & 0x04 != 0;
    attr.flags.ext_temp_range = thermal & 0x02 != 0;
    attr.flags.ext_temp_refresh = thermal & 0x01 != 0;
    attr.flags.therm_sensor = spd[32] & 0x80 != 0;
    attr.flags.pins_mirrored = spd[63] & 0x01 != 0;
    Ok((attr, status))
}

/// Decode an enabled XMP profile on top of valid JEDEC attributes.
pub fn decode_xmp(spd: &[u8; 256], profile: XmpProfile) -> Result<Ddr3Attr, Ddr3AttrError> {
    let (mut attr, status) = decode_attr(spd)?;
    if let Some(error) = status {
        return Err(error);
    }
    if spd[176] != 0x0c || spd[177] != 0x4a {
        return Err(Ddr3AttrError::NoXmp);
    }
    let (enable, xmp, mtb, dimms) = match profile {
        XmpProfile::One => (
            spd[178] & 1,
            185,
            mtb(spd[180], spd[181]),
            (spd[178] >> 2) & 3,
        ),
        XmpProfile::Two => (
            spd[178] & 2,
            220,
            mtb(spd[182], spd[183]),
            (spd[178] >> 4) & 3,
        ),
    };
    if enable == 0 {
        return Err(Ddr3AttrError::NoXmp);
    }
    let mtb = mtb.ok_or(Ddr3AttrError::NoXmp)?;
    let x = |index: usize| u32::from(spd[xmp + index]);
    attr.dimms_per_channel = dimms + 1;
    let v = spd[xmp];
    attr.voltage =
        u16::from(v & 1) * 50 + u16::from((v >> 1) & 0xf) * 100 + u16::from((v >> 5) & 3) * 1000;
    attr.t_ck = x(1) * mtb;
    attr.cas_supported = u16::from_le_bytes([spd[xmp + 3], spd[xmp + 4]]) & 0x7fff;
    attr.t_aa = x(2) * mtb;
    attr.t_wr = x(8) * mtb;
    attr.t_rcd = x(7) * mtb;
    attr.t_rrd = x(17) * mtb;
    attr.t_rp = x(6) * mtb;
    attr.t_ras = (((x(9) & 0x0f) << 8) + x(10)) * mtb;
    attr.t_rc = (((x(9) & 0xf0) << 4) + x(11)) * mtb;
    attr.t_rfc = ((x(15) << 8) + x(14)) * mtb;
    attr.t_wtr = x(20) * mtb;
    attr.t_rtp = x(16) * mtb;
    attr.t_faw = (((x(18) & 0x0f) << 8) + x(19)) * mtb;
    attr.t_cwl = x(5) * mtb;
    attr.t_cmd = x(23) * mtb;
    Ok(attr)
}

/// A synthetic DDR3-800 SO-DIMM SPD (2 ranks of x8 1 Gbit chips), for the
/// tests of controller drivers.
#[cfg(any(test, feature = "testing"))]
pub mod testing {
    use super::*;

    #[must_use]
    pub fn synthetic_ddr3_spd() -> [u8; 256] {
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
}

#[cfg(test)]
mod tests {
    use super::testing::synthetic_ddr3_spd;
    use super::*;

    fn with_crc(mut spd: [u8; 256]) -> [u8; 256] {
        let covered = if spd[0] & 0x80 != 0 { 117 } else { 126 };
        let crc = crate::crc16(&spd[..covered]);
        spd[126..128].copy_from_slice(&crc.to_le_bytes());
        spd
    }

    #[test]
    fn timing_attributes_preserve_coreboot_crc_tolerance() {
        let spd = with_crc(synthetic_ddr3_spd());
        let (attr, status) = decode_attr(&spd).unwrap();
        assert_eq!(status, None);
        assert_eq!((attr.size_mb, attr.ranks, attr.width), (2048, 2, 8));
        assert_eq!(attr.t_ck, 320);
        assert_eq!(attr.cas_supported, 0b0011_1000);
        assert!(attr.flags.operable_1_50v);
        let mut corrupt = spd;
        corrupt[126] ^= 1;
        assert_eq!(decode_attr(&corrupt), Ok((attr, Some(Ddr3AttrError::Crc))));
    }

    #[test]
    fn timing_attributes_apply_signed_fine_timebase() {
        let mut spd = synthetic_ddr3_spd();
        spd[9] = 0x11; // 1 ps
        spd[34] = (-10i8) as u8;
        spd[35] = 10;
        let (attr, status) = decode_attr(&with_crc(spd)).unwrap();
        assert_eq!(status, None);
        // Signed division truncates toward zero, as in coreboot ddr3.c.
        assert_eq!(attr.t_ck, 320 - 3);
        assert_eq!(attr.t_aa, 110 * 32 + 2);
        spd[11] = 0;
        assert_eq!(decode_attr(&spd), Err(Ddr3AttrError::InvalidField));
    }

    #[test]
    fn xmp_requires_valid_base_spd_and_enabled_profile() {
        let mut spd = with_crc(synthetic_ddr3_spd());
        assert_eq!(decode_xmp(&spd, XmpProfile::One), Err(Ddr3AttrError::NoXmp));
        spd[176..182].copy_from_slice(&[0x0c, 0x4a, 0x05, 0, 1, 8]);
        spd[185] = 0x2a; // 1500 mV: 1 V + 5 * 100 mV
        spd[186] = 12; // 1.5 ns
        spd[188] = 0x38;
        let attr = decode_xmp(&spd, XmpProfile::One).unwrap();
        assert_eq!(
            (attr.voltage, attr.dimms_per_channel, attr.t_ck),
            (1500, 2, 384)
        );
        assert_eq!(attr.cas_supported, 0x38);
        assert_eq!(decode_xmp(&spd, XmpProfile::Two), Err(Ddr3AttrError::NoXmp));
        spd[126] ^= 1;
        assert_eq!(decode_xmp(&spd, XmpProfile::One), Err(Ddr3AttrError::Crc));
    }

    #[test]
    fn module_identity_crc_ignores_timings_but_detects_replacement() {
        let mut spd = synthetic_ddr3_spd();
        let identity = unique_crc(&spd);
        spd[12] += 1;
        assert_eq!(unique_crc(&spd), identity);
        spd[122] ^= 1;
        assert_ne!(unique_crc(&spd), identity);
    }

    #[test]
    fn rejects_non_ddr3() {
        let mut spd = synthetic_ddr3_spd();
        spd[SPD_MEMORY_TYPE as usize] = 0x08;
        assert_eq!(decode_dimm(&spd), Err(Ddr3SpdError::WrongMemoryType(0x08)));
        let mut spd = synthetic_ddr3_spd();
        spd[SPD_DENSITY_BANKS as usize] = 1 << 4; // 16 banks
        assert_eq!(decode_dimm(&spd).map(|info| info.banks), Ok(16));
    }

    #[test]
    fn decode_synthetic_ddr3_geometry_and_timings() {
        let spd = synthetic_ddr3_spd();
        let info = decode_dimm(&spd).expect("valid synthetic DDR3 SPD");

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
    fn a_non_default_timebase_decodes_the_same_times() {
        // 2/16 ns is the same timebase as the usual 1/8 ns.
        let mut spd = synthetic_ddr3_spd();
        spd[SPD_MTB_DIVIDEND as usize] = 2;
        spd[SPD_MTB_DIVISOR as usize] = 16;
        let scaled = decode_dimm(&spd).expect("2/16 ns is a valid timebase");
        let plain = decode_dimm(&synthetic_ddr3_spd()).expect("1/8 ns");
        assert_eq!(scaled.tck_min_256ns, plain.tck_min_256ns);
        assert_eq!(scaled.taa_min_256ns, plain.taa_min_256ns);
        assert_eq!(scaled.tras_256ns, plain.tras_256ns);
        assert_eq!(scaled.tfaw_256ns, plain.tfaw_256ns);
    }

    #[test]
    fn fine_offsets_shift_the_minimum_times() {
        // FTB 1/1 = 1 ps per unit; the offsets are signed counts of those.
        let mut spd = synthetic_ddr3_spd();
        spd[SPD_FTB as usize] = 0x11;
        spd[SPD_TCK_MIN_FINE as usize] = 8; // +8 ps, rounded up to 3/256 ns
        spd[SPD_TAA_MIN_FINE as usize] = 0xf8; // -8 ps, rounded up to -2
        let dimm = decode_dimm(&spd).expect("fine offsets are valid");
        let plain = decode_dimm(&synthetic_ddr3_spd()).expect("no offsets");
        assert_eq!(dimm.tck_min_256ns, plain.tck_min_256ns + 3);
        assert_eq!(dimm.taa_min_256ns, plain.taa_min_256ns - 2);
        // Timings without a fine offset are untouched.
        assert_eq!(dimm.trcd_256ns, plain.trcd_256ns);
        assert_eq!(dimm.trp_256ns, plain.trp_256ns);
    }

    #[test]
    fn fine_offsets_need_revision_1_1_and_a_divisor() {
        let mut spd = synthetic_ddr3_spd();
        spd[SPD_FTB as usize] = 0x11;
        spd[SPD_TCK_MIN_FINE as usize] = 8;
        spd[SPD_REVISION as usize] = 0x10;
        let dimm = decode_dimm(&spd).expect("revision 1.0 ignores the offsets");
        assert_eq!(
            dimm.tck_min_256ns,
            decode_dimm(&synthetic_ddr3_spd()).unwrap().tck_min_256ns
        );
        spd[SPD_REVISION as usize] = 0x11;
        spd[SPD_FTB as usize] = 0x10; // divisor 0
        let dimm = decode_dimm(&spd).expect("a zero FTB divisor means no offsets");
        assert_eq!(
            dimm.tck_min_256ns,
            decode_dimm(&synthetic_ddr3_spd()).unwrap().tck_min_256ns
        );
    }

    #[test]
    fn an_unusable_timebase_is_refused() {
        let mut spd = synthetic_ddr3_spd();
        spd[SPD_MTB_DIVISOR as usize] = 0;
        assert_eq!(
            decode_dimm(&spd),
            Err(Ddr3SpdError::UnsupportedTimebase {
                dividend: 1,
                divisor: 0
            })
        );
    }

    #[test]
    fn ddr3_addressing_encodings_are_bounded_by_jedec_geometry() {
        for row_code in 0..=7 {
            for col_code in 0..=7 {
                let mut spd = synthetic_ddr3_spd();
                spd[SPD_ADDRESSING as usize] = (row_code << 3) | col_code;
                if row_code <= 4 && col_code <= 3 {
                    let info = decode_dimm(&spd).expect("valid DDR3 address bits");
                    assert_eq!(info.rows, row_code + 12);
                    assert_eq!(info.cols, col_code + 9);
                } else {
                    assert_eq!(decode_dimm(&spd), Err(Ddr3SpdError::InvalidGeometry));
                }
            }
        }
    }

    #[test]
    fn memory_device_reports_the_smbios_type17_fields() {
        let mut spd = synthetic_ddr3_spd();
        spd[SPD_MODULE_BUS_WIDTH as usize] = 0x03; // 64 bits, no ECC
        spd[SPD_TCK_MIN as usize] = 15; // 1.875 ns: DDR3-1066
        spd[SPD_MODULE_MANUFACTURER_ID_LSB as usize] = 0x80; // 0 continuations, parity
        spd[SPD_MODULE_MANUFACTURER_ID_MSB as usize] = 0xce;
        let dimm = decode_dimm(&spd).unwrap();
        let device = memory_device(&dimm, &spd, 1, 0, 1067);
        assert_eq!(
            device.memory_type,
            fstart_core::memory_info::MEMORY_TYPE_DDR3
        );
        assert_eq!(
            device.form_factor,
            fstart_core::memory_info::FORM_FACTOR_SODIMM
        );
        assert_eq!(device.data_width.get(), 64);
        assert_eq!(device.total_width.get(), 64);
        assert_eq!(device.max_mts.get(), 1066);
        assert_eq!(device.configured_mts.get(), 1067);
        assert_eq!((device.jedec_bank, device.jedec_id), (0, 0xce));
        assert_eq!(device.channel, 1);
        assert_eq!(
            device.size_mib.get(),
            dimm.rank_capacity_mb * u32::from(dimm.ranks)
        );
    }
}
