//! DDR2 SPD byte offsets and decoding (JEDEC SPD revision 1.2a).

use super::{ChipCapacity, ChipWidth, DimmInfo};
use fstart_core::services::{ServiceError, SmBus};

// ===================================================================
// DDR2 SPD byte offset constants
// ===================================================================

/// Maximum DDR2 SPD payload size used by coreboot's common DDR2 decoder.
pub const SPD_SIZE_MAX_DDR2: usize = 128;
/// SPD byte 0: number of bytes written by the SPD manufacturer.
pub const SPD_BYTES_WRITTEN: u8 = 0;
/// SPD byte 1: log2 of the EEPROM size in bytes.
pub const SPD_EEPROM_SIZE: u8 = 1;
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
/// SPD byte 8: nominal module voltage interface level.
pub const SPD_MODULE_VOLTAGE: u8 = 8;
/// SPD byte 9: minimum cycle time at maximum supported CAS latency.
pub const SPD_MIN_CYCLE_TIME_AT_CAS_MAX: u8 = 9;
/// SPD byte 10: access time from clock.
pub const SPD_ACCESS_TIME_FROM_CLOCK: u8 = 10;
/// SPD byte 11: DIMM configuration type (ECC, parity, etc.).
pub const SPD_DIMM_CONFIG_TYPE: u8 = 11;
/// SPD byte 12: refresh rate encoding, with self-refresh capability in bit 7.
pub const SPD_REFRESH_RATE: u8 = 12;
/// SPD byte 13: primary SDRAM width.
pub const SPD_PRIMARY_SDRAM_WIDTH: u8 = 13;
/// SPD byte 16: supported burst lengths (bit 3 = BL8).
pub const SPD_BURST_LENGTHS: u8 = 16;
/// SPD byte 17: number of banks per SDRAM device.
pub const SPD_NUM_BANKS_PER_SDRAM: u8 = 17;
/// SPD byte 18: supported CAS latencies (bitmask).
pub const SPD_SUPPORTED_CAS_LATENCIES: u8 = 18;
/// SPD byte 20: DIMM type (registered variants). Only the low 6 bits
/// carry the type; see `is_registered_ddr2`.
pub const SPD_DIMM_TYPE: u8 = 20;
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
/// SPD byte 63: checksum of bytes 0 through 62.
pub const SPD_CHECKSUM: u8 = 63;
/// SPD bytes 64-71: JEP106 manufacturer id, 0x7f continuation codes first.
pub const SPD_MANUFACTURER_ID: u8 = 64;
/// SPD bytes 73-90: module part number, space padded.
pub const SPD_PART_NUMBER: u8 = 73;
pub const SPD_PART_NUMBER_LEN: usize = 18;
/// SPD bytes 95-98: module serial number.
pub const SPD_SERIAL_NUMBER: u8 = 95;
/// End of the identity bytes SMBIOS needs (exclusive).
pub const SPD_IDENTITY_END: u8 = SPD_SERIAL_NUMBER + 4;

/// DDR2 memory type identifier (SPD byte 2).
pub const DDR2: u8 = 0x08;

/// Registered DDR2 DIMM types (SPD byte 20, low 6 bits), matching
/// coreboot `spd_dimm_is_registered_ddr2`.
#[must_use]
pub const fn is_registered_ddr2(dimm_type: u8) -> bool {
    matches!(dimm_type & 0x3f, 0x01 | 0x07 | 0x10)
}

/// Read the DDR2 SPD payload from `addr` into a 256-byte scratch buffer.
///
/// DDR2 SPD data is 128 bytes. The returned buffer is zero-filled above byte
/// 127 so callers can keep using the project-wide [`DimmInfo::spd_data`] shape.
/// Only a device NAK denotes an unpopulated slot; bus/controller failures
/// must never be cached as a topology change.
pub fn read_spd<B: SmBus + ?Sized>(
    smbus: &mut B,
    addr: u8,
) -> Result<Option<[u8; 256]>, ServiceError> {
    let mut spd = [0u8; 256];
    if !read_eeprom(smbus, addr, 0, &mut spd[..SPD_SIZE_MAX_DDR2])? {
        return Ok(None);
    }
    if spd[..SPD_SIZE_MAX_DDR2].iter().all(|b| *b == 0) {
        return Ok(None);
    }
    Ok(Some(spd))
}

/// SPD bytes that identify a DIMM: the checksum over the bytes before it,
/// then the manufacturer, location, part number, revision, date and serial.
const SPD_IDENTITY: core::ops::Range<usize> = SPD_CHECKSUM as usize..SPD_IDENTITY_END as usize;
const SPD_IDENTITY_LEN: usize = SPD_IDENTITY.end - SPD_IDENTITY.start;

/// Like [`read_spd`], but reuse `cached` (a copy of this slot's SPD kept by
/// an earlier boot) when the DIMM still reports the same identity bytes (63..=98).
///
/// coreboot's `spd_cache` makes the same trade with the serial number alone:
/// a few bytes on the bus instead of the whole EEPROM. An all-zero `cached`
/// slot means no DIMM was there, and the SPD is read in full.
pub fn read_spd_cached<B: SmBus + ?Sized>(
    smbus: &mut B,
    addr: u8,
    cached: Option<&[u8; SPD_SIZE_MAX_DDR2]>,
) -> Result<Option<[u8; 256]>, ServiceError> {
    let Some(cached) = cached.filter(|spd| spd.iter().any(|b| *b != 0)) else {
        return read_spd(smbus, addr);
    };
    let mut identity = [0u8; SPD_IDENTITY_LEN];
    if !read_eeprom(smbus, addr, SPD_CHECKSUM, &mut identity)? {
        return Ok(None);
    }
    if identity[..] != cached[SPD_IDENTITY] {
        fstart_log::info!("spd: DIMM at {:#x} changed, reading its SPD", addr);
        return read_spd(smbus, addr);
    }
    let mut spd = [0u8; 256];
    spd[..SPD_SIZE_MAX_DDR2].copy_from_slice(cached);
    Ok(Some(spd))
}

/// Fill `buf` from the EEPROM at `addr`; `false` when nothing answers.
///
/// Prefers the controller's I2C sequential read and, like coreboot, falls
/// back to byte reads when that fails.
fn read_eeprom<B: SmBus + ?Sized>(
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

fn checksum_valid(spd_data: &[u8; 256]) -> bool {
    spd_data[..SPD_CHECKSUM as usize]
        .iter()
        .copied()
        .fold(0u8, u8::wrapping_add)
        == spd_data[SPD_CHECKSUM as usize]
}

fn sizes_valid(spd_data: &[u8; 256]) -> bool {
    let spd_size = usize::from(spd_data[SPD_BYTES_WRITTEN as usize]).min(SPD_SIZE_MAX_DDR2);
    let eeprom_size = match spd_data[SPD_EEPROM_SIZE as usize] {
        0 => 0,
        exponent @ 1..=0x0e => 1usize << exponent,
        _ => 0x3fff,
    };
    spd_size >= 64 && eeprom_size >= 64
}

/// Decode DDR2 raw SPD data into a [`DimmInfo`].
///
/// Returns `None` if the memory type is not DDR2 or the data looks
/// unpopulated/invalid. Timing fields are decoded in the same 1/256 ns units
/// as coreboot's common DDR2 SPD library.
pub fn decode_dimm(spd_data: &[u8; 256]) -> Option<DimmInfo> {
    let mem_type = spd_data[super::SPD_MEMORY_TYPE as usize];
    if mem_type != DDR2 || !sizes_valid(spd_data) {
        return None;
    }

    let revision = spd_data[SPD_REVISION as usize];
    if revision & 0xf0 != 0x10 || !checksum_valid(spd_data) {
        return None;
    }

    if spd_data[SPD_MODULE_VOLTAGE as usize] > 0x05 {
        return None;
    }

    // Coreboot rejects reserved refresh encodings instead of assuming a rate.
    if spd_data[SPD_REFRESH_RATE as usize] & 0x7f > 5 {
        return None;
    }

    let rows = spd_data[SPD_NUM_ROWS as usize];
    let cols = spd_data[SPD_NUM_COLUMNS as usize];
    if rows == 0 || rows > 31 || (revision < 0x13 && rows > 15) || cols == 0 || cols > 15 {
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
        _ => return None,
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

    let rank_capacity_mb = rank_density_mb(spd_data);
    if rank_capacity_mb == 0 {
        return None;
    }

    let cas_latencies = spd_data[SPD_SUPPORTED_CAS_LATENCIES as usize];
    if cas_latencies == 0
        || cas_latencies & 0x03 != 0
        || (revision < 0x13 && cas_latencies & 0x80 != 0)
        || (revision < 0x12 && cas_latencies & 0x40 != 0)
    {
        return None;
    }

    let mut cycle_time_256ns = [0u32; 8];
    let mut access_time_256ns = [0u32; 8];
    if let Some(max_cas) = msb_index(cas_latencies) {
        cycle_time_256ns[max_cas as usize] =
            decode_tck_256ns(spd_data[SPD_MIN_CYCLE_TIME_AT_CAS_MAX as usize])?;
        access_time_256ns[max_cas as usize] =
            decode_bcd_256ns(spd_data[SPD_ACCESS_TIME_FROM_CLOCK as usize])?;

        if max_cas >= 1 && (cas_latencies & (1 << (max_cas - 1))) != 0 {
            cycle_time_256ns[(max_cas - 1) as usize] =
                decode_tck_256ns(spd_data[SPD_MIN_CYCLE_TIME_AT_CAS_MINUS_1 as usize])?;
            access_time_256ns[(max_cas - 1) as usize] =
                decode_bcd_256ns(spd_data[SPD_ACCESS_TIME_FROM_CLOCK_CAS_MINUS_1 as usize])?;
        }

        if max_cas >= 2 && (cas_latencies & (1 << (max_cas - 2))) != 0 {
            cycle_time_256ns[(max_cas - 2) as usize] =
                decode_tck_256ns(spd_data[SPD_MIN_CYCLE_TIME_AT_CAS_MINUS_2 as usize])?;
            access_time_256ns[(max_cas - 2) as usize] =
                decode_bcd_256ns(spd_data[SPD_ACCESS_TIME_FROM_CLOCK_CAS_MINUS_2 as usize])?;
        }
    }

    // tRFC: keep the compact raw field for legacy users, and expose decoded ns.
    let trfc = spd_data[SPD_TRFC_LO as usize] as u16
        | (((spd_data[SPD_TRC_TRFC_EXT as usize] & 0x01) as u16) << 8);

    Some(DimmInfo {
        card_type: spd_data[SPD_DIMM_TYPE as usize],
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
        is_ecc: spd_data[SPD_DIMM_CONFIG_TYPE as usize] & 0x02 != 0,
        is_registered: is_registered_ddr2(spd_data[SPD_DIMM_TYPE as usize]),
        is_stacked: spd_data[SPD_NUM_DIMM_BANKS as usize] & 0x10 != 0,
        supports_bl8: spd_data[SPD_BURST_LENGTHS as usize] & 0x08 != 0,
        spd_data: *spd_data,
    })
}

/// Describe a decoded DDR2 module for the SMBIOS memory inventory.
///
/// `configured_mts` is the data rate raminit programmed. Identity fields come
/// from SPD bytes 64-98, which the caller must have read.
pub fn memory_device(
    dimm: &DimmInfo,
    channel: u8,
    slot: u8,
    configured_mts: u16,
) -> fstart_core::memory_info::MemoryDevice {
    use fstart_core::memory_info as mi;
    let spd = &dimm.spd_data;
    let manufacturer = &spd[SPD_MANUFACTURER_ID as usize..SPD_MANUFACTURER_ID as usize + 8];
    let jedec_bank = manufacturer
        .iter()
        .take_while(|byte| **byte == 0x7f)
        .count();
    // DDR2 SPD byte 20 module type: RDIMM, UDIMM, SO-DIMM, Micro, Mini-R, Mini-U.
    let (form_factor, buffering) = match spd[SPD_DIMM_TYPE as usize] & 0x3f {
        0x01 | 0x10 => (mi::FORM_FACTOR_DIMM, mi::TYPE_DETAIL_REGISTERED),
        0x02 | 0x20 => (mi::FORM_FACTOR_DIMM, mi::TYPE_DETAIL_UNBUFFERED),
        0x04 => (mi::FORM_FACTOR_SODIMM, mi::TYPE_DETAIL_UNBUFFERED),
        0x08 => (mi::FORM_FACTOR_DIMM, 0),
        _ => (mi::FORM_FACTOR_UNKNOWN, 0),
    };
    let total_width = u16::from(spd[SPD_MODULE_DATA_WIDTH_LSB as usize])
        | u16::from(spd[SPD_MODULE_DATA_WIDTH_MSB as usize]) << 8;
    let max_mts = decode_tck_256ns(spd[SPD_MIN_CYCLE_TIME_AT_CAS_MAX as usize])
        .filter(|tck| *tck != 0)
        // Two transfers per clock: MT/s = 2 * 1000 / tCK[ns].
        .map_or(0, |tck| ((512_000 + tck / 2) / tck) as u16);
    let mut part_number = [b' '; 20];
    part_number[..SPD_PART_NUMBER_LEN].copy_from_slice(
        &spd[SPD_PART_NUMBER as usize..SPD_PART_NUMBER as usize + SPD_PART_NUMBER_LEN],
    );
    mi::MemoryDevice {
        size_mib: dimm
            .rank_capacity_mb
            .saturating_mul(u32::from(dimm.ranks))
            .into(),
        configured_mts: configured_mts.into(),
        max_mts: max_mts.into(),
        // DDR2 modules run at 1.8 V (SSTL_18).
        voltage_mv: 1800.into(),
        data_width: total_width
            .saturating_sub(if dimm.is_ecc { 8 } else { 0 })
            .into(),
        total_width: total_width.into(),
        type_detail: (mi::TYPE_DETAIL_SYNCHRONOUS | buffering).into(),
        jedec_bank: jedec_bank as u8,
        jedec_id: manufacturer.get(jedec_bank).copied().unwrap_or(0),
        memory_type: mi::MEMORY_TYPE_DDR2,
        form_factor,
        ranks: dimm.ranks,
        channel,
        slot,
        serial: spd[SPD_SERIAL_NUMBER as usize..SPD_IDENTITY_END as usize]
            .try_into()
            .expect("four serial bytes"),
        part_number,
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn memory_device_decodes_identity_and_speed() {
        let mut spd = valid_spd();
        spd[SPD_DIMM_TYPE as usize] = 0x04;
        spd[SPD_MANUFACTURER_ID as usize..SPD_MANUFACTURER_ID as usize + 2]
            .copy_from_slice(&[0x7f, 0x98]);
        spd[SPD_PART_NUMBER as usize..SPD_PART_NUMBER as usize + SPD_PART_NUMBER_LEN]
            .copy_from_slice(b"KVR667D2S5/1G     ");
        spd[SPD_SERIAL_NUMBER as usize..SPD_IDENTITY_END as usize].copy_from_slice(&[1, 2, 3, 4]);
        update_checksum(&mut spd);
        let dimm = decode_dimm(&spd).unwrap();
        let device = memory_device(&dimm, 1, 0, 533);
        assert_eq!(device.size_mib.get(), dimm.rank_capacity_mb);
        assert_eq!(
            (device.configured_mts.get(), device.max_mts.get()),
            (533, 667)
        );
        assert_eq!(
            device.form_factor,
            fstart_core::memory_info::FORM_FACTOR_SODIMM
        );
        assert_eq!(device.manufacturer(), Some("Kingston"));
        assert_eq!(device.part_number(), Some("KVR667D2S5/1G"));
        assert_eq!(device.serial, [1, 2, 3, 4]);
        assert_eq!(
            (device.data_width.get(), device.total_width.get()),
            (64, 64)
        );
    }

    pub(crate) fn valid_spd() -> [u8; 256] {
        let mut spd = [0u8; 256];
        spd[SPD_BYTES_WRITTEN as usize] = 128;
        spd[SPD_EEPROM_SIZE as usize] = 8;
        spd[super::super::SPD_MEMORY_TYPE as usize] = DDR2;
        spd[SPD_NUM_ROWS as usize] = 13;
        spd[SPD_NUM_COLUMNS as usize] = 10;
        spd[SPD_MODULE_DATA_WIDTH_LSB as usize] = 64;
        spd[SPD_MODULE_VOLTAGE as usize] = 5;
        spd[SPD_MIN_CYCLE_TIME_AT_CAS_MAX as usize] = 0x30;
        spd[SPD_ACCESS_TIME_FROM_CLOCK as usize] = 0x45;
        spd[SPD_PRIMARY_SDRAM_WIDTH as usize] = 8;
        spd[SPD_NUM_BANKS_PER_SDRAM as usize] = 8;
        spd[SPD_SUPPORTED_CAS_LATENCIES as usize] = 1 << 5;
        spd[SPD_DIMM_TYPE as usize] = 2;
        spd[SPD_RANK_DENSITY as usize] = 1;
        spd[SPD_REVISION as usize] = 0x12;
        update_checksum(&mut spd);
        spd
    }

    fn update_checksum(spd: &mut [u8; 256]) {
        spd[SPD_CHECKSUM as usize] = spd[..SPD_CHECKSUM as usize]
            .iter()
            .copied()
            .fold(0u8, u8::wrapping_add);
    }

    #[test]
    fn rejects_malformed_spd_geometry_and_sizes() {
        let mut spd = valid_spd();
        spd[SPD_BYTES_WRITTEN as usize] = 0;
        update_checksum(&mut spd);
        assert!(decode_dimm(&spd).is_none());

        let mut spd = valid_spd();
        spd[SPD_NUM_COLUMNS as usize] = 0x1c;
        update_checksum(&mut spd);
        assert!(decode_dimm(&spd).is_none());

        let mut spd = valid_spd();
        spd[SPD_RANK_DENSITY as usize] = 0;
        update_checksum(&mut spd);
        assert!(decode_dimm(&spd).is_none());
    }

    #[test]
    fn decodes_only_defined_chip_widths() {
        for raw in 0..=u8::MAX {
            let mut spd = valid_spd();
            spd[SPD_PRIMARY_SDRAM_WIDTH as usize] = raw;
            update_checksum(&mut spd);
            let expected = match raw {
                4 => Some(ChipWidth::X4),
                8 => Some(ChipWidth::X8),
                16 => Some(ChipWidth::X16),
                32 => Some(ChipWidth::X32),
                _ => None,
            };
            assert_eq!(decode_dimm(&spd).map(|d| d.width), expected, "width {raw}");
        }
    }

    #[test]
    fn rejects_reserved_refresh_encodings() {
        for refresh in 0..=u8::MAX {
            let mut spd = valid_spd();
            spd[SPD_REFRESH_RATE as usize] = refresh;
            update_checksum(&mut spd);
            assert_eq!(
                decode_dimm(&spd).is_some(),
                matches!(refresh, 0..=5 | 0x80..=0x85),
                "refresh encoding {refresh:#04x}"
            );
        }
    }

    #[test]
    fn rejects_bad_checksum_and_timing_encoding() {
        let mut spd = valid_spd();
        spd[SPD_CHECKSUM as usize] ^= 1;
        assert!(decode_dimm(&spd).is_none());

        let mut spd = valid_spd();
        spd[SPD_MIN_CYCLE_TIME_AT_CAS_MAX as usize] = 0x2e;
        update_checksum(&mut spd);
        assert!(decode_dimm(&spd).is_none());
    }

    /// One EEPROM behind a controller without an I2C read, counting the
    /// bytes it hands out.
    struct Eeprom {
        addr: u8,
        bytes: [u8; 128],
        reads: usize,
    }

    impl SmBus for Eeprom {
        fn read_byte(&mut self, addr: u8, cmd: u8) -> Result<u8, ServiceError> {
            if addr != self.addr {
                return Err(ServiceError::NoDevice);
            }
            self.reads += 1;
            Ok(self.bytes[cmd as usize])
        }
        fn write_byte(&mut self, _: u8, _: u8, _: u8) -> Result<(), ServiceError> {
            Err(ServiceError::HardwareError)
        }
    }

    #[test]
    fn cached_spd_is_reused_only_for_the_same_dimm() {
        let mut bytes: [u8; 128] = core::array::from_fn(|i| i as u8 | 1);
        let mut bus = Eeprom {
            addr: 0x50,
            bytes,
            reads: 0,
        };

        let spd = read_spd_cached(&mut bus, 0x50, Some(&bytes))
            .unwrap()
            .unwrap();
        assert_eq!(spd[..128], bytes);
        assert_eq!(bus.reads, SPD_IDENTITY_LEN);

        // A different serial number means another DIMM: read it in full.
        bytes[98] ^= 0xff;
        bus.reads = 0;
        let spd = read_spd_cached(&mut bus, 0x50, Some(&bytes))
            .unwrap()
            .unwrap();
        assert_eq!(spd[..128], bus.bytes);
        assert_eq!(bus.reads, SPD_IDENTITY_LEN + 128);

        // An empty cached slot reads the SPD; an empty slot reports nothing.
        bus.reads = 0;
        assert!(
            read_spd_cached(&mut bus, 0x50, Some(&[0; 128]))
                .unwrap()
                .is_some()
        );
        assert_eq!(bus.reads, 128);
        assert!(
            read_spd_cached(&mut bus, 0x51, Some(&bytes))
                .unwrap()
                .is_none()
        );
    }
}
