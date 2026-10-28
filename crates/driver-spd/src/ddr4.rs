//! DDR4 module inventory, JEDEC 21-C Annex L, SPD encoding revision 1.x.
//!
//! Bytes 0..125 have a CRC16 at 126..127. Manufacturing identity at
//! 320..348 is *not* covered by that CRC. Unsupported organizations are
//! rejected rather than guessing a capacity; the caller can publish a
//! present/unknown module using `MemoryDevice::unknown`.

use crate::crc16;
use fstart_core::memory_info::{
    FORM_FACTOR_DIMM, FORM_FACTOR_SODIMM, MEMORY_TYPE_DDR4, MemoryDevice, TYPE_DETAIL_LRDIMM,
    TYPE_DETAIL_REGISTERED, TYPE_DETAIL_SYNCHRONOUS, TYPE_DETAIL_UNBUFFERED,
};
use tock_registers::{LocalRegisterCopy, register_bitfields};

register_bitfields![u8,
    /// Byte 0: bytes used and total SPD size.
    Header [
        USED OFFSET(0) NUMBITS(4) [],
        TOTAL OFFSET(4) NUMBITS(3) [],
        RESERVED OFFSET(7) NUMBITS(1) []
    ],
    /// Byte 1: SPD encoding revision.
    Revision [
        MAJOR OFFSET(4) NUMBITS(4) []
    ],
    /// Byte 3: module type.
    Module [
        KIND OFFSET(0) NUMBITS(4) [],
        HYBRID_TYPE OFFSET(4) NUMBITS(3) [],
        HYBRID OFFSET(7) NUMBITS(1) []
    ],
    /// Byte 4: SDRAM density and banks.
    Density [
        CAPACITY OFFSET(0) NUMBITS(4) []
    ],
    /// Byte 6: primary SDRAM package type.
    Package [
        LOADING OFFSET(0) NUMBITS(2) [],
        DIES OFFSET(4) NUMBITS(3) [],
        STACKED OFFSET(7) NUMBITS(1) []
    ],
    /// Byte 12: module organization.
    Organization [
        WIDTH OFFSET(0) NUMBITS(3) [],
        RANKS OFFSET(3) NUMBITS(3) [],
        ASYMMETRIC OFFSET(6) NUMBITS(1) []
    ],
    /// Byte 13: module memory bus width.
    BusWidth [
        PRIMARY OFFSET(0) NUMBITS(3) [],
        EXTENSION OFFSET(3) NUMBITS(2) []
    ]
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    TooShort,
    NotDdr4,
    Header,
    Revision,
    Crc,
    ModuleType,
    Organization,
    Timebase,
    Clock,
}

/// Decode a DDR4 SPD copy. Requires the base block; identity is optional
/// when the transport only supplied 128/256 bytes. Zero configured MT/s
/// means unknown, never the SPD maximum. No slot wiring is inferred.
pub fn inventory(
    spd: &[u8],
    configured_mts: u16,
    channel: u8,
    slot: u8,
) -> Result<MemoryDevice, Error> {
    let base = spd.get(..128).ok_or(Error::TooShort)?;
    if base[2] != 0x0c {
        return Err(Error::NotDdr4);
    }
    if LocalRegisterCopy::<u8, Revision::Register>::new(base[1]).read(Revision::MAJOR) != 1 {
        return Err(Error::Revision);
    }
    if crc16(&base[..126]) != u16::from_le_bytes([base[126], base[127]]) {
        return Err(Error::Crc);
    }
    // Hybrid/NVDIMM devices need a persistent-memory inventory adapter;
    // publishing their full capacity as ordinary volatile DDR4 is wrong.
    let module = LocalRegisterCopy::<u8, Module::Register>::new(base[3]);
    if module.is_set(Module::HYBRID) || module.read(Module::HYBRID_TYPE) != 0 {
        return Err(Error::ModuleType);
    }
    let header = LocalRegisterCopy::<u8, Header::Register>::new(base[0]);
    let used_bytes = match header.read(Header::USED) {
        1..=4 => usize::from(header.read(Header::USED)) * 128,
        _ => return Err(Error::Header),
    };
    let total_bytes = match header.read(Header::TOTAL) {
        1 => 256,
        2 => 512,
        _ => return Err(Error::Header),
    };
    if header.is_set(Header::RESERVED) || used_bytes > total_bytes {
        return Err(Error::Header);
    }
    let (form_factor, buffering) = match module.read(Module::KIND) {
        1 => (FORM_FACTOR_DIMM, TYPE_DETAIL_REGISTERED),
        2 => (FORM_FACTOR_DIMM, TYPE_DETAIL_UNBUFFERED),
        3 => (FORM_FACTOR_SODIMM, TYPE_DETAIL_UNBUFFERED),
        4 => (
            FORM_FACTOR_DIMM,
            TYPE_DETAIL_REGISTERED | TYPE_DETAIL_LRDIMM,
        ),
        _ => return Err(Error::ModuleType),
    };
    let org = LocalRegisterCopy::<u8, Organization::Register>::new(base[12]);
    let bus = LocalRegisterCopy::<u8, BusWidth::Register>::new(base[13]);
    let package = LocalRegisterCopy::<u8, Package::Register>::new(base[6]);
    let density = LocalRegisterCopy::<u8, Density::Register>::new(base[4]).read(Density::CAPACITY);
    if org.is_set(Organization::ASYMMETRIC)
        || org.read(Organization::WIDTH) > 3
        || bus.read(BusWidth::PRIMARY) > 3
        || bus.read(BusWidth::EXTENSION) > 1
    {
        return Err(Error::Organization);
    }
    // Non-binary densities are JEDEC encodings 8/9, not shifts.
    let density_mib = match density {
        0..=7 => 32u32 << density,
        8 => 1536,
        9 => 3072,
        _ => return Err(Error::Organization),
    };
    let device_width = 4u32 << org.read(Organization::WIDTH);
    let data_width = 8u32 << bus.read(BusWidth::PRIMARY);
    if data_width < device_width {
        return Err(Error::Organization);
    }
    let package_ranks = u32::from(org.read(Organization::RANKS)) + 1;
    let dies = if package.is_set(Package::STACKED) {
        match package.read(Package::LOADING) {
            // Multi-load stacks expose their ranks separately.
            1 => 1,
            2 => u32::from(package.read(Package::DIES)) + 1,
            _ => return Err(Error::Organization),
        }
    } else {
        if package.read(Package::DIES) != 0 || package.read(Package::LOADING) != 0 {
            return Err(Error::Organization);
        }
        1
    };
    let logical_ranks = package_ranks * dies;
    // Attributes has a four-bit rank count; unknown is zero, never truncation.
    let ranks = if logical_ranks < 16 {
        logical_ranks as u8
    } else {
        0
    };
    let size_mib = density_mib
        .checked_mul(data_width / device_width)
        .and_then(|size| size.checked_mul(logical_ranks))
        .ok_or(Error::Organization)?;
    if base[17] != 0 {
        return Err(Error::Timebase);
    }
    let tck_ps = i32::from(base[18]) * 125 + i32::from(base[125] as i8);
    if tck_ps <= 0 {
        return Err(Error::Clock);
    }
    let max_mts = u16::try_from(2_000_000 / tck_ps).map_err(|_| Error::Clock)?;
    if max_mts == 0 {
        return Err(Error::Clock);
    }
    let mut dimm = MemoryDevice {
        size_mib: size_mib.into(),
        configured_mts: configured_mts.into(),
        max_mts: max_mts.into(),
        // SPD is a capability description, not a configured-voltage readback.
        voltage_mv: 0.into(),
        data_width: (data_width as u16).into(),
        total_width: ((data_width + u32::from(bus.read(BusWidth::EXTENSION)) * 8) as u16).into(),
        type_detail: (TYPE_DETAIL_SYNCHRONOUS | buffering).into(),
        memory_type: MEMORY_TYPE_DDR4,
        form_factor,
        ranks,
        channel,
        slot,
        ..MemoryDevice::default()
    };
    if let Some(identity) = spd[..spd.len().min(used_bytes)].get(320..349) {
        dimm.jedec_bank = identity[0] & 0x7f;
        dimm.jedec_id = identity[1];
        dimm.serial.copy_from_slice(&identity[5..9]);
        dimm.part_number.copy_from_slice(&identity[9..29]);
    }
    Ok(dimm)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seal(spd: &mut [u8]) {
        let crc = crc16(&spd[..126]);
        spd[126..128].copy_from_slice(&crc.to_le_bytes());
    }
    fn module() -> [u8; 512] {
        let mut spd = [0; 512];
        spd[0] = 0x23; // 384 meaningful bytes in a 512-byte EEPROM
        spd[1] = 0x11;
        spd[2] = 0x0c;
        spd[3] = 1; // registered DIMM
        spd[4] = 5; // 8 Gbit per die
        spd[12] = 1; // x8, one rank
        spd[13] = 0x0b; // 64 data + 8 ECC
        spd[18] = 5; // 625 ps => 3200 MT/s
        spd[320] = 0x80; // bank-zero parity bit is not a bank number
        spd[321] = 0xce;
        spd[325..329].copy_from_slice(&[0x12, 0x34, 0xab, 0xcd]);
        spd[329..349].copy_from_slice(b"TEST DDR4 MODULE    ");
        seal(&mut spd);
        spd
    }
    #[test]
    fn registered_ecc_module_and_independent_configured_rate() {
        let spd = module();
        let dimm = inventory(&spd, 2933, 7, 1).unwrap();
        assert_eq!(dimm.size_mib.get(), 8192);
        assert_eq!((dimm.data_width.get(), dimm.total_width.get()), (64, 72));
        assert_eq!(
            (dimm.max_mts.get(), dimm.configured_mts.get()),
            (3200, 2933)
        );
        assert_eq!((dimm.channel, dimm.slot, dimm.ranks), (7, 1, 1));
        assert_eq!(dimm.manufacturer(), Some("Samsung"));
        assert_eq!(dimm.serial_number().as_deref(), Some("1234abcd"));
        assert_eq!(dimm.part_number(), Some("TEST DDR4 MODULE"));
        assert_eq!(dimm.voltage_mv.get(), 0);
        let partial = inventory(&spd[..128], 0, 0, 0).unwrap();
        assert_eq!(partial.configured_mts.get(), 0);
        assert_eq!(partial.serial_number(), None);
        let mut unused_identity = spd;
        unused_identity[0] = 0x22; // only 256 bytes meaningful, despite the transport buffer
        seal(&mut unused_identity);
        assert_eq!(
            inventory(&unused_identity, 0, 0, 0)
                .unwrap()
                .serial_number(),
            None
        );
    }
    #[test]
    fn symmetric_3ds_and_nonbinary_density() {
        let mut spd = module();
        spd[3] = 4;
        spd[6] = 0xb2; // 4-high single-load stack
        seal(&mut spd);
        let dimm = inventory(&spd, 0, 0, 0).unwrap();
        assert_eq!(dimm.size_mib.get(), 32768);
        assert_eq!(dimm.ranks, 4);
        assert_ne!(dimm.type_detail.get() & TYPE_DETAIL_LRDIMM, 0);
        spd[6] = 0;
        for (density, size) in [(8, 12288), (9, 24576)] {
            spd[4] = density;
            seal(&mut spd);
            assert_eq!(inventory(&spd, 0, 0, 0).unwrap().size_mib.get(), size);
        }
    }
    #[test]
    fn rejects_corruption_reserved_and_asymmetric_encodings() {
        assert!(matches!(
            inventory(&[0; 127], 0, 0, 0),
            Err(Error::TooShort)
        ));
        let mut bad = module();
        bad[4] ^= 1;
        assert!(matches!(inventory(&bad, 0, 0, 0), Err(Error::Crc)));
        for (offset, value, error) in [
            (0, 0, Error::Header),
            (0, 0x13, Error::Header),
            (0, 0xa3, Error::Header),
            (2, 0, Error::NotDdr4),
            (1, 0x50, Error::Revision),
            (1, 0x20, Error::Revision),
            (3, 0xf, Error::ModuleType),
            (3, 0x81, Error::ModuleType),
            (12, 0x41, Error::Organization),
            (12, 7, Error::Organization),
            (13, 0x1b, Error::Organization),
            (6, 0x80, Error::Organization),
            (4, 0xf, Error::Organization),
            (17, 1, Error::Timebase),
            (18, 0, Error::Clock),
        ] {
            let mut bad = module();
            bad[offset] = value;
            seal(&mut bad);
            assert!(matches!(inventory(&bad, 0, 0, 0), Err(e) if e == error));
        }
    }
}
