// SPDX-License-Identifier: MIT OR Apache-2.0
//! DDR5 inventory (JESD400-5, encoding 1.x), independent of transport/CPU.
//! The CRC covers bytes 0..509, not the manufacturing data at 512..551.
//! Only symmetric, monolithic RDIMM/UDIMM/SODIMM organizations are decoded;
//! unsupported stacks/hybrids must not become guessed volatile capacities.
use crate::crc16;
use fstart_core::memory_info::{
    FORM_FACTOR_DIMM, FORM_FACTOR_SODIMM, MEMORY_TYPE_DDR5, MemoryDevice, TYPE_DETAIL_REGISTERED,
    TYPE_DETAIL_SYNCHRONOUS, TYPE_DETAIL_UNBUFFERED,
};
use tock_registers::{LocalRegisterCopy, register_bitfields};
register_bitfields![u8,
    Header [
        BETA OFFSET(0) NUMBITS(4) [],
        TOTAL OFFSET(4) NUMBITS(3) [],
        BETA_HI OFFSET(7) NUMBITS(1) []
    ],
    Revision [ MAJOR OFFSET(4) NUMBITS(4) [] ],
    Module [
        KIND OFFSET(0) NUMBITS(4) [],
        HYBRID OFFSET(4) NUMBITS(4) []
    ],
    Density [
        GBITS OFFSET(0) NUMBITS(5) [],
        PACKAGE OFFSET(5) NUMBITS(3) []
    ],
    IoWidth [ WIDTH OFFSET(5) NUMBITS(3) [] ],
    Organization [
        RANKS OFFSET(3) NUMBITS(3) [],
        ASYMMETRIC OFFSET(6) NUMBITS(1) []
    ],
    BusWidth [
        PRIMARY OFFSET(0) NUMBITS(3) [],
        EXTENSION OFFSET(3) NUMBITS(2) [],
        SUBCHANNELS OFFSET(5) NUMBITS(3) []
    ]
];
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    TooShort,
    NotDdr5,
    Header,
    Revision,
    Crc,
    ModuleType,
    Organization,
    Clock,
}
/// Configured MT/s comes from the controller, not the SPD. Zero is unknown.
/// Manufacturing identity is optional and is not authenticated by the CRC.
pub fn inventory(
    spd: &[u8],
    configured_mts: u16,
    channel: u8,
    slot: u8,
) -> Result<MemoryDevice, Error> {
    let base = spd.get(..512).ok_or(Error::TooShort)?;
    if base[2] != 0x12 {
        return Err(Error::NotDdr5);
    }
    let h = LocalRegisterCopy::<u8, Header::Register>::new(base[0]);
    if !matches!(h.read(Header::TOTAL), 2..=4)
        || h.read(Header::BETA) != 0
        || h.is_set(Header::BETA_HI)
    {
        return Err(Error::Header);
    }
    for offset in [1, 192] {
        if LocalRegisterCopy::<u8, Revision::Register>::new(base[offset]).read(Revision::MAJOR) != 1
        {
            return Err(Error::Revision);
        }
    }
    if crc16(&base[..510]) != u16::from_le_bytes([base[510], base[511]]) {
        return Err(Error::Crc);
    }
    let module = LocalRegisterCopy::<u8, Module::Register>::new(base[3]);
    if module.read(Module::HYBRID) != 0 {
        return Err(Error::ModuleType);
    }
    let (form_factor, detail) = match module.read(Module::KIND) {
        1 => (FORM_FACTOR_DIMM, TYPE_DETAIL_REGISTERED),
        2 => (FORM_FACTOR_DIMM, TYPE_DETAIL_UNBUFFERED),
        3 => (FORM_FACTOR_SODIMM, TYPE_DETAIL_UNBUFFERED),
        _ => return Err(Error::ModuleType),
    };
    let density = LocalRegisterCopy::<u8, Density::Register>::new(base[4]);
    let org = LocalRegisterCopy::<u8, Organization::Register>::new(base[234]);
    let bus = LocalRegisterCopy::<u8, BusWidth::Register>::new(base[235]);
    let width = LocalRegisterCopy::<u8, IoWidth::Register>::new(base[6]).read(IoWidth::WIDTH);
    if density.read(Density::PACKAGE) != 0
        || org.is_set(Organization::ASYMMETRIC)
        || base[234] & 0x87 != 0
        || base[6] & 0x1f != 0
        || width > 3
        || bus.read(BusWidth::PRIMARY) > 3
        || bus.read(BusWidth::EXTENSION) > 2
        || bus.read(BusWidth::SUBCHANNELS) > 2
    {
        return Err(Error::Organization);
    }
    let density_mib = match density.read(Density::GBITS) {
        1 => 512u32,
        2 => 1024,
        3 => 1536,
        4 => 2048,
        5 => 3072,
        6 => 4096,
        7 => 6144,
        8 => 8192,
        _ => return Err(Error::Organization),
    };
    let device_width = 4u32 << width;
    let subchannels = 1u32 << bus.read(BusWidth::SUBCHANNELS);
    let primary = 8u32 << bus.read(BusWidth::PRIMARY);
    if primary < device_width {
        return Err(Error::Organization);
    }
    let ranks = org.read(Organization::RANKS) + 1;
    let data_width = primary * subchannels;
    let total_width = (primary + 4 * u32::from(bus.read(BusWidth::EXTENSION))) * subchannels;
    let size_mib = density_mib
        .checked_mul(data_width / device_width)
        .and_then(|size| size.checked_mul(ranks.into()))
        .ok_or(Error::Organization)?;
    // JEDEC bins are quantized to whole picoseconds (4800 MT/s: 416 ps).
    // Accept only a standard 400-MT/s bin within one ps, not custom/XMP timings.
    let tck = u32::from(u16::from_le_bytes([base[20], base[21]]));
    let max_mts = (3200u32..=8800)
        .step_by(400)
        .find(|rate| (2_000_000 / rate).abs_diff(tck) <= 1)
        .ok_or(Error::Clock)? as u16;
    if base[19] != 0 || configured_mts > max_mts {
        return Err(Error::Clock);
    }
    let mut dimm = MemoryDevice {
        size_mib: size_mib.into(),
        max_mts: max_mts.into(),
        configured_mts: configured_mts.into(),
        data_width: (data_width as u16).into(),
        total_width: (total_width as u16).into(),
        memory_type: MEMORY_TYPE_DDR5,
        form_factor,
        ranks,
        channel,
        slot,
        type_detail: (TYPE_DETAIL_SYNCHRONOUS | detail).into(),
        ..MemoryDevice::default()
    };
    let declared_bytes = 128usize << h.read(Header::TOTAL);
    if let Some(identity) = spd[..spd.len().min(declared_bytes)].get(512..551) {
        dimm.jedec_bank = identity[0] & 0x7f;
        dimm.jedec_id = identity[1];
        dimm.serial.copy_from_slice(&identity[5..9]);
        dimm.part_number.copy_from_slice(&identity[9..39]);
    }
    Ok(dimm)
}
#[cfg(test)]
mod tests {
    use super::*;
    fn seal(spd: &mut [u8]) {
        let crc = crc16(&spd[..510]);
        spd[510..512].copy_from_slice(&crc.to_le_bytes());
    }
    fn module() -> [u8; 1024] {
        let mut spd = [0; 1024];
        spd[..8].copy_from_slice(&[0x30, 0x10, 0x12, 1, 4, 0x20, 0, 0x62]);
        spd[192] = 0x10;
        spd[235] = 0x32;
        spd[20..22].copy_from_slice(&416u16.to_le_bytes());
        spd[512] = 0x80;
        spd[513] = 0xad;
        spd[517..521].copy_from_slice(&[1, 2, 3, 4]);
        spd[521..551].copy_from_slice(b"FULL THIRTY BYTE PART NUMBER!!");
        seal(&mut spd);
        spd
    }
    #[test]
    fn two_subchannels_are_one_32_gib_module_not_two_dimms() {
        let spd = module();
        let d = inventory(&spd, 4800, 3, 0).unwrap();
        assert_eq!(
            (
                d.size_mib.get(),
                d.data_width.get(),
                d.total_width.get(),
                d.ranks
            ),
            (32768, 64, 80, 1)
        );
        assert_eq!(
            (d.max_mts.get(), d.configured_mts.get(), d.voltage_mv.get()),
            (4800, 4800, 0)
        );
        assert_eq!(d.memory_type, 0x22);
        assert_eq!(d.part_number(), Some("FULL THIRTY BYTE PART NUMBER!!"));
        assert_eq!(d.manufacturer(), Some("Hynix"));
        assert_eq!(d.serial_number().as_deref(), Some("01020304"));
        let d = inventory(&spd[..512], 0, 0, 0).unwrap();
        assert_eq!(d.configured_mts.get(), 0);
        assert_eq!(d.serial_number(), None);
        let mut base_only = spd;
        base_only[0] = 0x20;
        seal(&mut base_only);
        assert_eq!(
            inventory(&base_only, 0, 0, 0).unwrap().serial_number(),
            None
        );
    }
    #[test]
    fn nonbinary_density_and_ranks_do_not_use_ddr4_encodings() {
        let mut s = module();
        s[4] = 5;
        s[234] = 8;
        seal(&mut s);
        assert_eq!(inventory(&s, 4400, 0, 0).unwrap().size_mib.get(), 98304);
    }
    #[test]
    fn rejects_bad_crc_stacks_asymmetry_and_custom_timings() {
        let s = module();
        assert_eq!(inventory(&s[..511], 0, 0, 0).unwrap_err(), Error::TooShort);
        let mut bad = s;
        bad[400] ^= 1;
        assert_eq!(inventory(&bad, 0, 0, 0).unwrap_err(), Error::Crc);
        for (offset, value) in [
            (0, 0x31),
            (1, 0x20),
            (2, 0x0c),
            (3, 0x81),
            (3, 7),
            (4, 0x44),
            (4, 0),
            (6, 0xe0),
            (234, 0x40),
            (235, 0x3a),
            (235, 0xe2),
            (19, 1),
            (20, 0),
        ] {
            let mut bad = s;
            bad[offset] = value;
            seal(&mut bad);
            assert!(inventory(&bad, 0, 0, 0).is_err(), "{offset}");
        }
        assert_eq!(inventory(&s, 5200, 0, 0).unwrap_err(), Error::Clock);
        let mut identity = s;
        identity[521] = b'X';
        assert!(inventory(&identity, 0, 0, 0).is_ok()); // outside CRC
    }
}
