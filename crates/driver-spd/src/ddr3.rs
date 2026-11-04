//! DDR3 module facts, JEDEC 21-C Annex K, SPD revision 1.x.
//!
//! [`decode`] is coreboot's `spd_decode_ddr3()`: geometry, supported CAS
//! latencies and the minimum timings in 1/256 ns, including the fine
//! timebase corrections. [`inventory`] turns the same bytes into an SMBIOS
//! memory device.

use crate::crc16;
use fstart_core::memory_info::{
    FORM_FACTOR_DIMM, FORM_FACTOR_SODIMM, FORM_FACTOR_UNKNOWN, MEMORY_TYPE_DDR3, MemoryDevice,
    TYPE_DETAIL_REGISTERED, TYPE_DETAIL_SYNCHRONOUS, TYPE_DETAIL_UNBUFFERED,
};
use tock_registers::{LocalRegisterCopy, register_bitfields};

/// SPD bytes a DDR3 EEPROM exposes.
pub const SPD_SIZE: usize = 256;

register_bitfields![u8,
    /// Byte 0: CRC coverage and SPD size.
    Header [ CRC_TO_116 OFFSET(7) NUMBITS(1) [] ],
    /// Byte 3: module type.
    Module [ KIND OFFSET(0) NUMBITS(4) [] ],
    /// Byte 4: SDRAM density and banks.
    Density [ CAPACITY OFFSET(0) NUMBITS(4) [], BANKS OFFSET(4) NUMBITS(3) [] ],
    /// Byte 5: addressing.
    Addressing [ COLUMNS OFFSET(0) NUMBITS(3) [], ROWS OFFSET(3) NUMBITS(3) [] ],
    /// Byte 6: nominal voltage.
    Voltage [ NOT_1V5 OFFSET(0) NUMBITS(1) [], V1_35 OFFSET(1) NUMBITS(1) [], V1_25 OFFSET(2) NUMBITS(1) [] ],
    /// Byte 7: module organization.
    Organization [ WIDTH OFFSET(0) NUMBITS(3) [], RANKS OFFSET(3) NUMBITS(3) [] ],
    /// Byte 8: module bus width.
    BusWidth [ PRIMARY OFFSET(0) NUMBITS(3) [], EXTENSION OFFSET(3) NUMBITS(2) [] ],
    /// Byte 9: fine timebase dividend/divisor.
    FineTimebase [ DIVISOR OFFSET(0) NUMBITS(4) [], DIVIDEND OFFSET(4) NUMBITS(4) [] ]
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    TooShort,
    NotDdr3,
    Crc,
    /// A geometry field holds a reserved encoding.
    InvalidField,
}

impl Error {
    pub const fn name(self) -> &'static str {
        match self {
            Error::TooShort => "too short",
            Error::NotDdr3 => "not DDR3",
            Error::Crc => "CRC mismatch",
            Error::InvalidField => "invalid field",
        }
    }
}

/// DDR3 module facts. Times are in 1/256 ns.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Ddr3Attributes {
    /// Module type (SPD byte 3, bits 3:0).
    pub module_type: u8,
    /// Supported CAS latencies: bit n means CL n + 4.
    pub cas_supported: u16,
    /// SDRAM device width in bits.
    pub width: u8,
    pub ranks: u8,
    pub row_bits: u8,
    pub col_bits: u8,
    /// Module data width in bits.
    pub bus_width: u8,
    pub ecc: bool,
    /// Supply voltages in mV: lowest and highest the module operates at.
    pub min_voltage: u16,
    pub max_voltage: u16,
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
    /// Rank 1 address bits are mirrored.
    pub mirrored: bool,
    pub reference_card: u8,
    pub manufacturer_id: u16,
}

impl Ddr3Attributes {
    /// Supported data rate in MT/s from tCKmin.
    #[must_use]
    pub fn max_mts(&self) -> u32 {
        // tCK [1/256 ns]: MT/s = 2 * 1000 / tCK[ns].
        (2 * 1000 * 256u32).checked_div(self.t_ck).unwrap_or(0)
    }
}

/// The CRC the SPD covers: bytes 0..117 or 0..126 (byte 0 bit 7).
fn crc_ok(spd: &[u8]) -> bool {
    let header = LocalRegisterCopy::<u8, Header::Register>::new(spd[0]);
    let len = if header.is_set(Header::CRC_TO_116) {
        117
    } else {
        126
    };
    crc16(&spd[..len]) == u16::from_le_bytes([spd[126], spd[127]])
}

/// Decode a DDR3 SPD copy (at least the first 128 bytes; bytes 117..145
/// are identity and optional).
pub fn decode(spd: &[u8]) -> Result<Ddr3Attributes, Error> {
    let spd = spd.get(..128).ok_or(Error::TooShort)?;
    if spd[2] != 0x0b {
        return Err(Error::NotDdr3);
    }
    if !crc_ok(spd) {
        return Err(Error::Crc);
    }
    decode_without_crc(spd)
}

/// [`decode`] for parts that ship with a bad SPD CRC: the fields are still
/// range checked.
pub fn decode_without_crc(spd: &[u8]) -> Result<Ddr3Attributes, Error> {
    let spd = spd.get(..128).ok_or(Error::TooShort)?;
    if spd[2] != 0x0b {
        return Err(Error::NotDdr3);
    }
    let density = LocalRegisterCopy::<u8, Density::Register>::new(spd[4]);
    let addressing = LocalRegisterCopy::<u8, Addressing::Register>::new(spd[5]);
    let voltage = LocalRegisterCopy::<u8, Voltage::Register>::new(spd[6]);
    let org = LocalRegisterCopy::<u8, Organization::Register>::new(spd[7]);
    let bus = LocalRegisterCopy::<u8, BusWidth::Register>::new(spd[8]);
    let capacity_shift = density.read(Density::CAPACITY);
    if density.read(Density::BANKS) > 3
        || capacity_shift > 6
        || addressing.read(Addressing::ROWS) > 4
        || addressing.read(Addressing::COLUMNS) > 3
        || org.read(Organization::RANKS) > 3
        || org.read(Organization::WIDTH) > 3
        || bus.read(BusWidth::EXTENSION) > 1
        || bus.read(BusWidth::PRIMARY) > 3
    {
        return Err(Error::InvalidField);
    }
    let (mut min_voltage, mut max_voltage) = (u16::MAX, 0u16);
    for (supported, mv) in [
        (voltage.is_set(Voltage::V1_25), 1250),
        (voltage.is_set(Voltage::V1_35), 1350),
        (!voltage.is_set(Voltage::NOT_1V5), 1500),
    ] {
        if supported {
            min_voltage = min_voltage.min(mv);
            max_voltage = max_voltage.max(mv);
        }
    }
    let ranks = org.read(Organization::RANKS) + 1;
    let width = 4u8 << org.read(Organization::WIDTH);
    let bus_width = 8u8 << bus.read(BusWidth::PRIMARY);
    // 256 Mbit << capacity_shift per device, times devices per rank.
    let size_mb = ((1u32 << (capacity_shift + 5)) * u32::from(bus_width) * u32::from(ranks))
        / u32::from(width);
    // A zero medium-timebase dividend or divisor makes every timing zero.
    if spd[10] == 0 || spd[11] == 0 {
        return Err(Error::InvalidField);
    }
    let mtb = (u32::from(spd[10]) << 8) / u32::from(spd[11]);
    let t = |byte: u8| u32::from(byte) * mtb;
    let mut dimm = Ddr3Attributes {
        module_type: LocalRegisterCopy::<u8, Module::Register>::new(spd[3]).read(Module::KIND),
        cas_supported: u16::from_le_bytes([spd[14], spd[15]]),
        width,
        ranks,
        row_bits: addressing.read(Addressing::ROWS) + 12,
        col_bits: addressing.read(Addressing::COLUMNS) + 9,
        bus_width,
        ecc: bus.read(BusWidth::EXTENSION) != 0,
        min_voltage: if max_voltage == 0 { 0 } else { min_voltage },
        max_voltage,
        size_mb,
        t_ck: t(spd[12]),
        t_aa: t(spd[16]),
        t_wr: t(spd[17]),
        t_rcd: t(spd[18]),
        t_rrd: t(spd[19]),
        t_rp: t(spd[20]),
        t_ras: ((u32::from(spd[21] & 0x0f) << 8) + u32::from(spd[22])) * mtb,
        t_rc: ((u32::from(spd[21] & 0xf0) << 4) + u32::from(spd[23])) * mtb,
        t_rfc: u32::from(u16::from_le_bytes([spd[24], spd[25]])) * mtb,
        t_wtr: t(spd[26]),
        t_rtp: t(spd[27]),
        t_faw: ((u32::from(spd[28] & 0x0f) << 8) + u32::from(spd[29])) * mtb,
        mirrored: spd[63] & 1 != 0,
        reference_card: spd[62] & 0x1f,
        manufacturer_id: 0,
    };
    // Fine timebase corrections, SPD revision 1.1 and later. The MTB part
    // is rounded up and the FTB part negative, so rounding down is safe.
    let ftb_reg = LocalRegisterCopy::<u8, FineTimebase::Register>::new(spd[9]);
    if spd[1] >= 0x11 && ftb_reg.read(FineTimebase::DIVISOR) != 0 {
        let ftb = (i32::from(ftb_reg.read(FineTimebase::DIVIDEND)) << 8)
            / i32::from(ftb_reg.read(FineTimebase::DIVISOR));
        let correct = |value: &mut u32, byte: u8| {
            let delta = (i32::from(byte as i8) * ftb - 500) / 1000;
            *value = value.wrapping_add_signed(delta);
        };
        correct(&mut dimm.t_ck, spd[34]);
        correct(&mut dimm.t_aa, spd[35]);
        correct(&mut dimm.t_rcd, spd[36]);
        correct(&mut dimm.t_rp, spd[37]);
        correct(&mut dimm.t_rc, spd[38]);
    }
    if let Some(id) = spd.get(117..119) {
        dimm.manufacturer_id = u16::from_le_bytes([id[0], id[1]]);
    }
    Ok(dimm)
}

/// An SMBIOS memory device for a DDR3 module. Zero `configured_mts` means
/// unknown.
pub fn inventory(
    spd: &[u8],
    configured_mts: u16,
    channel: u8,
    slot: u8,
) -> Result<MemoryDevice, Error> {
    let dimm = decode(spd)?;
    let form_factor = match dimm.module_type {
        1 | 2 => FORM_FACTOR_DIMM,
        3 | 4 | 8 | 9 | 0xc | 0xd => FORM_FACTOR_SODIMM,
        _ => FORM_FACTOR_UNKNOWN,
    };
    let buffering = if matches!(dimm.module_type, 1 | 5 | 8) {
        TYPE_DETAIL_REGISTERED
    } else {
        TYPE_DETAIL_UNBUFFERED
    };
    let mut device = MemoryDevice {
        size_mib: dimm.size_mb.into(),
        configured_mts: configured_mts.into(),
        max_mts: (dimm.max_mts().min(u32::from(u16::MAX)) as u16).into(),
        voltage_mv: 0.into(),
        data_width: u16::from(dimm.bus_width).into(),
        total_width: (u16::from(dimm.bus_width) + if dimm.ecc { 8 } else { 0 }).into(),
        type_detail: (TYPE_DETAIL_SYNCHRONOUS | buffering).into(),
        memory_type: MEMORY_TYPE_DDR3,
        form_factor,
        ranks: dimm.ranks,
        channel,
        slot,
        ..MemoryDevice::default()
    };
    if let Some(identity) = spd.get(117..146) {
        device.jedec_bank = identity[0] & 0x7f;
        device.jedec_id = identity[1];
        device.serial.copy_from_slice(&identity[5..9]);
        device.part_number[..18].copy_from_slice(&identity[11..29]);
        device.part_number[18..].fill(b' ');
    }
    Ok(device)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Kingston D2516EC4BXGGB-U as soldered on the MinnowBoard Max
    /// (coreboot `mb/intel/minnowmax/spd`).
    const KINGSTON_2GB: [u8; 128] = [
        0x92, 0x11, 0x0b, 0x03, 0x04, 0x19, 0x02, 0x02, 0x03, 0x11, 0x01, 0x08, 0x0a, 0x00, 0xfe,
        0x00, 0x69, 0x78, 0x69, 0x3c, 0x69, 0x11, 0x18, 0x86, 0x20, 0x08, 0x3c, 0x3c, 0x01, 0x40,
        0x83, 0x05, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x0f, 0x01, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x98, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x19, 0xd2,
    ];

    #[test]
    fn decodes_geometry_and_timings_in_quarter_ns_units() {
        let dimm = decode(&KINGSTON_2GB).unwrap();
        assert_eq!((dimm.width, dimm.ranks, dimm.bus_width), (16, 1, 64));
        assert_eq!(dimm.size_mb, 2048);
        // tCKmin 10 MTB of 1/8 ns = 1.25 ns: DDR3-1600.
        assert_eq!(dimm.t_ck, 320);
        assert_eq!(dimm.max_mts(), 1600);
        assert_eq!(dimm.t_aa, 0x69 * 32);
        assert_eq!(dimm.cas_supported, 0xfe);
        assert!(dimm.min_voltage == 1350 && dimm.max_voltage == 1500);
    }

    #[test]
    fn rejects_a_broken_crc_and_other_memory_types() {
        let mut spd = KINGSTON_2GB;
        spd[20] ^= 1;
        assert_eq!(decode(&spd), Err(Error::Crc));
        spd[2] = 0x0c;
        assert_eq!(decode(&spd), Err(Error::NotDdr3));
    }
}
