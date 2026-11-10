// SPDX-License-Identifier: MIT OR Apache-2.0
//! Genoa APOB 18h DDR5 records, not the SP3 214h-stride DDR4 schema.
//! Wire definitions: openSIL ApobCmn.h APOB_D5_SPD_STRUCT and Apob.h
//! APOB_MEM_DMI_PHYSICAL_DIMM: 40ch strides, SPD at +0ch, 12 described
//! positions in 24 allocated records, matching the 8/8 seeds.
//! Allocation capacity is not population. No EEPROM/controller accesses.
use super::Rs;
use crate::apob::{Apob, ApobError, group};
use tock_registers::{LocalRegisterCopy, register_bitfields};
use zerocopy::{
    FromBytes, Immutable, KnownLayout, Unaligned,
    little_endian::{U16, U32},
};
register_bitfields![u8,
    Location [ SOCKET OFFSET(0) NUMBITS(2) [], CHANNEL OFFSET(2) NUMBITS(4) [], DIMM OFFSET(6) NUMBITS(1) [], PRESENT OFFSET(7) NUMBITS(1) [] ]
];
pub const MAX_POSITIONS: usize = 24;
#[derive(FromBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
pub struct SpdRecord {
    dram_down: u8,
    present: u8,
    _padding: [u8; 2],
    _address: U32,
    pub socket: u8,
    pub channel: u8,
    pub dimm: u8,
    shadow: u8,
    pub data: [u8; 1024],
}
#[derive(FromBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
pub struct Position {
    location: u8,
    _spd_address: u8,
    _handle: U16,
    clock_mhz: U16,
    voltage_mv: U16,
}
impl Position {
    pub fn socket(&self) -> u8 {
        LocalRegisterCopy::<u8, Location::Register>::new(self.location).read(Location::SOCKET)
    }
    pub fn channel(&self) -> u8 {
        LocalRegisterCopy::<u8, Location::Register>::new(self.location).read(Location::CHANNEL)
    }
    pub fn dimm(&self) -> u8 {
        LocalRegisterCopy::<u8, Location::Register>::new(self.location).read(Location::DIMM)
    }
    pub fn present(&self) -> bool {
        LocalRegisterCopy::<u8, Location::Register>::new(self.location).is_set(Location::PRESENT)
    }
    pub fn configured_mts(&self) -> Option<u16> {
        self.clock_mhz.get().checked_mul(2)
    }
    pub fn voltage_mv(&self) -> u16 {
        self.voltage_mv.get()
    }
}
pub struct Memory<'a> {
    pub positions: &'a [Position],
    records: &'a [SpdRecord],
    /// Capability only; this does not prove active Chipkill/SECDED policy.
    pub ecc_capable: bool,
}
impl Memory<'_> {
    pub fn spd(&self, position: &Position) -> Option<&SpdRecord> {
        self.records.iter().find(|r| {
            (r.socket, r.channel, r.dimm)
                == (position.socket(), position.channel(), position.dimm())
        })
    }
}
/// Strict one-socket Genoa 18h profile. Do not silently accept Siena/other
/// versions, orphan SPDs, duplicate slots, stale shadow presence or truncation.
pub fn read<'a>(apob: &Apob<'a, Rs>) -> Result<Memory<'a>, ApobError> {
    if apob.version() != 0x18 {
        return Err(ApobError::Version(apob.version()));
    }
    let spd = apob.find(group::MEMORY, 17, 0)?;
    let dmi = apob.find(8, 8, 0)?;
    let bad = || ApobError::Value {
        group: 8,
        kind: 8,
        value: 0,
    };
    let dimms = spd.byte(0x30)?;
    let channels = spd.byte(0x31)?;
    let count = usize::from(dimms) * usize::from(channels);
    let kind = dmi.byte(0x30)?;
    if dimms == 0
        || dimms > 2
        || channels == 0
        || channels > 12
        || dmi.byte(0x31)? as usize != count
        || kind & 0x7f != 0x22
        || spd.bytes().len() < 0x34
        || !(spd.bytes().len() - 0x34).is_multiple_of(size_of::<SpdRecord>())
    {
        return Err(bad());
    }
    let capacity = (spd.bytes().len() - 0x34) / size_of::<SpdRecord>();
    if capacity < count || capacity > MAX_POSITIONS {
        return Err(bad());
    }
    let all = spd.slice::<SpdRecord>(0x34, capacity)?;
    let records = &all[..count];
    // Extra storage is not another socket or more installed DIMMs.
    if all[count..]
        .iter()
        .any(|r| r.present != 0 || r.dram_down != 0 || r.shadow != 0)
    {
        return Err(bad());
    }
    for (i, r) in records.iter().enumerate() {
        if r.socket != 0
            || r.channel >= channels
            || r.dimm >= dimms
            || r.present > 1
            || r.dram_down != 0
            || r.shadow != 0
            || records[..i]
                .iter()
                .any(|p| (p.socket, p.channel, p.dimm) == (r.socket, r.channel, r.dimm))
        {
            return Err(bad());
        }
    }
    let positions = dmi.slice::<Position>(0x34, count)?;
    for (i, p) in positions.iter().enumerate() {
        if p.socket() != 0
            || p.channel() >= channels
            || p.dimm() >= dimms
            || p.configured_mts().is_none()
            || positions[..i]
                .iter()
                .any(|q| (p.socket(), p.channel(), p.dimm()) == (q.socket(), q.channel(), q.dimm()))
        {
            return Err(bad());
        }
        let r = records
            .iter()
            .find(|r| (r.socket, r.channel, r.dimm) == (p.socket(), p.channel(), p.dimm()))
            .ok_or_else(bad)?;
        if p.present() != (r.present == 1) {
            return Err(bad());
        }
    }
    Ok(Memory {
        positions,
        records,
        ecc_capable: kind & 0x80 != 0,
    })
}
#[cfg(feature = "smbios")]
pub fn inventory(
    apob: &Apob<'_, Rs>,
) -> Result<heapless::Vec<crate::apob::smbios::Dimm, MAX_POSITIONS>, ApobError> {
    use fstart_core::memory_info::MemoryDevice;
    let memory = read(apob)?;
    let mut dimms = heapless::Vec::new();
    for p in memory.positions {
        let device = if p.present() {
            let mut device = fstart_driver_spd::ddr5::inventory(
                &memory
                    .spd(p)
                    .ok_or(ApobError::Value {
                        group: 1,
                        kind: 17,
                        value: 0,
                    })?
                    .data,
                p.configured_mts().unwrap(),
                p.channel(),
                p.dimm(),
            )
            .map_err(|_| ApobError::Value {
                group: 1,
                kind: 17,
                value: p.channel().into(),
            })?;
            device.voltage_mv = p.voltage_mv().into();
            device
        } else {
            MemoryDevice::empty(p.channel(), p.dimm())
        };
        dimms
            .push(crate::apob::smbios::Dimm {
                socket: p.socket(),
                die: 0,
                device,
            })
            .map_err(|_| ApobError::Value {
                group: 8,
                kind: 8,
                value: dimms.len() as u32,
            })?;
    }
    Ok(dimms)
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::apob::build::Builder;
    fn records() -> (Vec<u8>, Vec<u8>) {
        let mut s = vec![0; 4 + 24 * 0x40c];
        s[..2].copy_from_slice(&[1, 12]);
        let mut d = vec![0; 4 + 12 * 8];
        d[..3].copy_from_slice(&[0xa2, 12, 24]);
        for i in 0..12 {
            s[4 + i * 0x40c + 9] = i as u8;
            d[4 + i * 8] = (i as u8) << 2;
        }
        s[4 + 3 * 0x40c + 1] = 1;
        d[4 + 3 * 8] |= 0x80;
        d[4 + 3 * 8 + 4..4 + 3 * 8 + 6].copy_from_slice(&2400u16.to_le_bytes());
        d[4 + 3 * 8 + 6..4 + 3 * 8 + 8].copy_from_slice(&1100u16.to_le_bytes());
        (s, d)
    }
    fn blob(s: &[u8], d: &[u8]) -> Vec<u8> {
        Builder::new(0x18)
            .entry(1, 17, 0, s)
            .entry(8, 8, 0, d)
            .build()
    }
    #[test]
    fn allocation_capacity_is_not_population_and_seeds_match_by_coordinates() {
        assert_eq!(size_of::<SpdRecord>(), 0x40c);
        assert_eq!(size_of::<Position>(), 8);
        let (s, d) = records();
        let b = blob(&s, &d);
        let a = Apob::new(&b, Rs).unwrap();
        let m = read(&a).unwrap();
        assert_eq!(m.positions.len(), 12);
        assert_eq!(m.positions.iter().filter(|p| p.present()).count(), 1);
        let p = &m.positions[3];
        assert_eq!(
            (
                p.socket(),
                p.channel(),
                p.dimm(),
                p.configured_mts(),
                p.voltage_mv()
            ),
            (0, 3, 0, Some(4800), 1100)
        );
        assert!(m.ecc_capable);
        assert_eq!(m.spd(p).unwrap().channel, 3);
        let mut reordered = s;
        for j in 0..0x40c {
            reordered.swap(4 + j, 4 + 3 * 0x40c + j);
        }
        let b = blob(&reordered, &d);
        assert!(read(&Apob::new(&b, Rs).unwrap()).is_ok());
    }
    #[test]
    fn rejects_foreign_versions_mismatch_duplicate_and_orphan_records() {
        let (s, d) = records();
        for (offset, value) in [
            (0, 0),
            (1, 13),
            (4 + 1, 1),
            (4 + 0x40c + 9, 0),
            (4 + 3 * 0x40c + 8, 1),
            (4 + 3 * 0x40c + 1, 0),
            (4 + 3 * 0x40c + 11, 1),
            (4 + 12 * 0x40c + 1, 1),
        ] {
            let mut bad = s.clone();
            bad[offset] = value;
            let b = blob(&bad, &d);
            assert!(read(&Apob::new(&b, Rs).unwrap()).is_err(), "{offset}");
        }
        for offset in [0, 1, 4 + 8, 4 + 3 * 8] {
            let mut bad = d.clone();
            bad[offset] = 0;
            let b = blob(&s, &bad);
            assert!(read(&Apob::new(&b, Rs).unwrap()).is_err(), "{offset}");
        }
        for n in [0, 3, s.len() - 1] {
            let b = blob(&s[..n], &d);
            assert!(read(&Apob::new(&b, Rs).unwrap()).is_err());
        }
        let b = Builder::new(0x19)
            .entry(1, 17, 0, &s)
            .entry(8, 8, 0, &d)
            .build();
        assert!(read(&Apob::new(&b, Rs).unwrap()).is_err());
    }
}
