//! APOB memory records: inputs for SMBIOS types 16/17/19/20 and the APCB
//! recovery flag (PSP_APOB §3.5, §3.8).
//!
//! | G/T | Instance | Use |
//! |---|---|---|
//! | 1/2 | `s << 8 \| d` | memory clock, populated channels |
//! | 1/0xf | any | PIE power flag (ZP) |
//! | 1/0x10 | 0 | APCB boot info (recovery flag) |
//! | 1/0x11 | `s << 8 \| d` | per-DIMM SPD copies |
//! | 8/8 | 0 | SMBIOS 17/20 seeds |
//!
//! Field names beyond those the report gives are left raw.

use tock_registers::{LocalRegisterCopy, register_bitfields};
use zerocopy::little_endian::{U16, U32};
use zerocopy::{FromBytes, Immutable, KnownLayout, Unaligned};

use super::{Apob, ApobError, Entry, group, instance};

const MEM_CONFIG: u32 = 2;
const PIE_POWER: u32 = 0xf;
const APCB_BOOT_INFO: u32 = 0x10;
const SPD: u32 = 0x11;
const SMBIOS: u32 = 8;

/// 1/2: memory configuration of one socket/die.
pub struct MemConfig<'a> {
    entry: Entry<'a>,
}

impl MemConfig<'_> {
    /// MEMCLK in MHz (`+0x34`, instruction-checked on ZP).
    pub fn memclk(&self) -> Result<u16, ApobError> {
        self.entry.at::<U16>(0x34).map(|v| v.get())
    }

    /// Per-channel byte at `+0x38 + channel` (ZP MemSmbiosV2Pei: non-zero
    /// sets bit 7 of the SMBIOS flags).
    pub fn channel_byte(&self, channel: u8) -> Result<u8, ApobError> {
        self.entry.byte(0x38 + usize::from(channel))
    }
}

pub fn mem_config<'a>(apob: &Apob<'a>, socket: u8, die: u8) -> Result<MemConfig<'a>, ApobError> {
    Ok(MemConfig {
        entry: apob.find(group::MEMORY, MEM_CONFIG, instance(socket, die))?,
    })
}

/// 1/0xf: the ABL's PIE power settings (ZP), first instance.
pub struct PiePower<'a> {
    entry: Entry<'a>,
}

impl PiePower<'_> {
    /// The byte at `+0x130` that AmdFabricZpDxe tests from APOB version 9 on
    /// (meaning unknown).
    pub fn flag(&self) -> Result<bool, ApobError> {
        self.entry.byte(0x130).map(|b| b != 0)
    }
}

pub fn pie_power<'a>(apob: &Apob<'a>) -> Result<PiePower<'a>, ApobError> {
    Ok(PiePower {
        entry: apob.first(group::MEMORY, PIE_POWER)?,
    })
}

/// 1/0x10 instance 0.
pub struct ApcbBootInfo<'a> {
    entry: Entry<'a>,
}

impl ApcbBootInfo<'_> {
    /// The ABL booted the backup APCB (`+0x1b5`). AGESA then restores or
    /// ignores it per policy; fstart only logs it.
    pub fn recovery(&self) -> Result<bool, ApobError> {
        self.entry.byte(0x1b5).map(|b| b != 0)
    }

    /// DIMM configuration changed since the last boot (`+0x1b4`).
    pub fn dimm_config_updated(&self) -> Result<bool, ApobError> {
        self.entry.byte(0x1b4).map(|b| b != 0)
    }
}

pub fn apcb_boot_info<'a>(apob: &Apob<'a>) -> Result<ApcbBootInfo<'a>, ApobError> {
    Ok(ApcbBootInfo {
        entry: apob.find(group::MEMORY, APCB_BOOT_INFO, 0)?,
    })
}

/// One DIMM record of 1/0x11 (0x214 bytes).
#[derive(FromBytes, KnownLayout, Immutable, Unaligned)]
#[repr(C)]
pub struct SpdRecord {
    /// Socket/die/channel/DIMM metadata precedes the consumer's validity
    /// byte. Do not start a whole-record slice at the validity byte: its
    /// final element would extend four bytes past the APOB entry.
    _location: [u8; 4],
    present: u8,
    /// Copied into the AGESA memory-info PPI; meaning not traced.
    pub info: [u8; 3],
    /// As `info`.
    pub dword4: U32,
    /// As `info`.
    pub dword8: U32,
    _c: U32,
    /// The DIMM's SPD bytes as the ABL keeps them, see [`Self::jedec_spd`].
    pub spd: [u8; 512],
}

/// DDR4 base-block bytes JEDEC reserves (coded 0x00, inside the CRC).
const SPD_RESERVED: core::ops::Range<usize> = 78..117;

impl SpdRecord {
    /// The SPD as the module has it. The Zeppelin ABL keeps the module's
    /// serial number, part number and manufacturer IDs in the reserved
    /// base-block bytes of its copy (MJ11-EC1 with four SK hynix
    /// HMA42GR7MFR4N-TF: bytes 80-107), which breaks the base CRC; the
    /// module's are zero.
    pub fn jedec_spd(&self) -> [u8; 512] {
        let mut spd = self.spd;
        spd[SPD_RESERVED].fill(0);
        spd
    }

    pub fn present(&self) -> bool {
        self.present == 1
    }
}

/// 1/0x11: SPD copies of one socket/die.
///
/// DIMMs per channel `+0x30`, channels `+0x31`, whole records `+0x34`,
/// stride 0x214, index `channel * dimms + dimm`. The memory-info consumer
/// view starts at the validity byte (`+0x38`), not the whole record.
/// RXM100CB original-byte witnesses: `ffd57076..ffd57122` (counts,
/// stride, validity at record+4, SPD at record+0x14, 512-byte copy).
/// The Rome ApobSsp getter's 0x215 stride is not the memory-info consumer
/// layout; do not apply it to this record.
pub struct SpdTable<'a> {
    dimms_per_channel: u8,
    records: &'a [SpdRecord],
}

impl<'a> SpdTable<'a> {
    /// `(channel, dimm, record)` for every exposed position, including empty.
    pub fn slots(&self) -> impl Iterator<Item = (u8, u8, &'a SpdRecord)> + 'a {
        let per = self.dimms_per_channel.max(1);
        let records = self.records;
        records.iter().enumerate().map(move |(i, r)| {
            (
                (i / usize::from(per)) as u8,
                (i % usize::from(per)) as u8,
                r,
            )
        })
    }

    /// `(channel, dimm, record)` for every valid SPD copy.
    pub fn dimms(&self) -> impl Iterator<Item = (u8, u8, &'a SpdRecord)> + 'a {
        self.slots().filter(|(_, _, record)| record.present())
    }

    pub fn dimms_per_channel(&self) -> u8 {
        self.dimms_per_channel
    }
}

pub fn spd<'a>(apob: &Apob<'a>, socket: u8, die: u8) -> Result<SpdTable<'a>, ApobError> {
    let entry = apob.find(group::MEMORY, SPD, instance(socket, die))?;
    let dimms_per_channel = entry.byte(0x30)?;
    let channels = entry.byte(0x31)?;
    use crate::psp::Variant;
    // These topology limits are established for SP3, not Raven/Picasso.
    let invalid = match apob.variant() {
        Variant::Zp => channels > 2,
        Variant::Ssp | Variant::Gn => channels > 8,
        Variant::Rv => false,
    } || (apob.variant() != Variant::Rv
        && (dimms_per_channel > 2 || (channels == 0) != (dimms_per_channel == 0)));
    if invalid {
        return Err(ApobError::Value {
            group: group::MEMORY,
            kind: SPD,
            value: u32::from(channels) << 8 | u32::from(dimms_per_channel),
        });
    }
    Ok(SpdTable {
        dimms_per_channel,
        records: entry.slice(0x34, usize::from(channels) * usize::from(dimms_per_channel))?,
    })
}

register_bitfields![u8,
    /// The position byte of the type-20 seeds, and of ZP's type-17 seeds
    /// (MJ11-EC1 APOB: 0x00, 0xa0, 0x04, 0xa4 for DIMMs in slot 1 of
    /// channels 0 and 1, as its SPD copies say).
    pub Location [
        SOCKET OFFSET(0) NUMBITS(2) [],
        CHANNEL OFFSET(2) NUMBITS(3) [],
        DIMM OFFSET(5) NUMBITS(2) [],
        /// Populated (type 17), valid (type 20).
        VALID OFFSET(7) NUMBITS(1) []
    ],
    /// The position byte of SSP/GN's type-17 seeds (Rome
    /// MemSmbiosV2SspPei; RXM100CB `ffd56fad`).
    pub SspLocation [
        SOCKET OFFSET(0) NUMBITS(1) [],
        CHANNEL OFFSET(1) NUMBITS(3) [],
        DIMM OFFSET(4) NUMBITS(2) [],
        PRESENT OFFSET(6) NUMBITS(1) [],
        /// AGESA skips the seed.
        SKIP OFFSET(7) NUMBITS(1) []
    ],
    pub Type20Flags [
        /// AGESA fills the interleave fields with 0xff.
        INTERLEAVED OFFSET(0) NUMBITS(1) []
    ]
];

/// An 8/8 type-17 seed (8 bytes).
#[derive(FromBytes, KnownLayout, Immutable, Unaligned)]
#[repr(C)]
pub struct Type17Seed {
    location: u8,
    _b1: u8,
    /// SMBIOS handle.
    pub handle: U16,
    /// Copied into the type-17 record (Rome MemSmbiosV2SspPei); meaning not
    /// traced.
    pub words: [U16; 2],
}

/// An 8/8 type-20 seed (0x20 bytes).
#[derive(FromBytes, KnownLayout, Immutable, Unaligned)]
#[repr(C)]
pub struct Type20Seed {
    location: u8,
    flags: u8,
    /// SMBIOS handle.
    pub handle: U16,
    /// Copied into the type-20 record; meaning not traced.
    pub dwords: [U32; 2],
    _c: U32,
    /// As `dwords`.
    pub dwords_hi: [U32; 4],
}

impl Type20Seed {
    pub fn location(&self) -> LocalRegisterCopy<u8, Location::Register> {
        LocalRegisterCopy::new(self.location)
    }
    pub fn flags(&self) -> LocalRegisterCopy<u8, Type20Flags::Register> {
        LocalRegisterCopy::new(self.flags)
    }
}

/// A DIMM position of the type-17 seeds, decoded for the APOB's
/// generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DimmPosition {
    pub socket: u8,
    /// The channel of the socket.
    pub channel: u8,
    pub dimm: u8,
    pub present: bool,
}

impl Type17Seed {
    /// The position for an APOB of `variant`: ZP (Zeppelin, Naples) lists
    /// every position in [`Location`], SSP/GN in [`SspLocation`]. `None`
    /// for a position AGESA skips, and on RV, whose seeds no APOB has shown.
    pub fn position(&self, variant: crate::psp::Variant) -> Option<DimmPosition> {
        use crate::psp::Variant;
        match variant {
            Variant::Zp => {
                let l = LocalRegisterCopy::<u8, Location::Register>::new(self.location);
                Some(DimmPosition {
                    socket: l.read(Location::SOCKET),
                    channel: l.read(Location::CHANNEL),
                    dimm: l.read(Location::DIMM),
                    present: l.is_set(Location::VALID),
                })
            }
            Variant::Ssp | Variant::Gn => {
                let l = LocalRegisterCopy::<u8, SspLocation::Register>::new(self.location);
                (!l.is_set(SspLocation::SKIP)).then(|| DimmPosition {
                    socket: l.read(SspLocation::SOCKET),
                    channel: l.read(SspLocation::CHANNEL),
                    dimm: l.read(SspLocation::DIMM),
                    present: l.is_set(SspLocation::PRESENT),
                })
            }
            Variant::Rv => None,
        }
    }
}

/// 8/8 instance 0 (Rome MemSmbiosV2SspPei, decompiler-only).
pub struct SmbiosInfo<'a> {
    variant: crate::psp::Variant,
    flags: u8,
    pub type17: &'a [Type17Seed],
    pub type20: &'a [Type20Seed],
}

impl SmbiosInfo<'_> {
    /// Bit 7 of `+0x30`: AGESA reports multi-bit ECC (SMBIOS 16 error
    /// correction 6) instead of none (3).
    pub fn ecc(&self) -> bool {
        self.flags & 0x80 != 0
    }

    /// The DIMM positions the seeds list.
    pub fn positions(&self) -> impl Iterator<Item = DimmPosition> + '_ {
        self.type17
            .iter()
            .filter_map(|seed| seed.position(self.variant))
    }
}

pub fn smbios_info<'a>(apob: &Apob<'a>) -> Result<SmbiosInfo<'a>, ApobError> {
    let entry = apob.find(group::SMBIOS, SMBIOS, 0)?;
    let t17 = usize::from(entry.byte(0x31)?);
    let t20 = usize::from(entry.byte(0x32)?);
    Ok(SmbiosInfo {
        variant: apob.variant(),
        flags: entry.byte(0x30)?,
        type17: entry.slice(0x34, t17)?,
        type20: entry.slice(0x34 + t17 * size_of::<Type17Seed>(), t20)?,
    })
}

#[cfg(test)]
mod tests {
    use super::super::build::{Body, Builder};
    use super::*;
    use crate::psp::Variant;

    #[test]
    fn zp_seeds_list_every_position() {
        let seed =
            |location: u8| Type17Seed::read_from_bytes(&[location, 0, 0, 0, 0, 0, 0, 0]).unwrap();
        // The MJ11-EC1's: DIMMs in slot 1 of channels 0 and 1.
        let positions: std::vec::Vec<_> = [0x00, 0xa0, 0x04, 0xa4]
            .map(|l| seed(l).position(Variant::Zp).unwrap())
            .iter()
            .map(|p| (p.channel, p.dimm, p.present))
            .collect();
        assert_eq!(
            positions,
            [(0, 0, false), (0, 1, true), (1, 0, false), (1, 1, true)]
        );
        // Rome's skip flag; Rome's layout.
        assert_eq!(seed(0x80).position(Variant::Ssp), None);
        assert_eq!(
            seed(0x5b).position(Variant::Ssp),
            Some(DimmPosition {
                socket: 1,
                channel: 5,
                dimm: 1,
                present: true
            })
        );
        // Picasso's are unknown.
        assert_eq!(seed(0xa0).position(Variant::Rv), None);
    }

    #[test]
    fn the_abls_reserved_bytes_are_cleared() {
        let mut record = SpdRecord::read_from_bytes(&[0; 0x214]).unwrap();
        record.spd[77] = 0x0b;
        record.spd[80..84].copy_from_slice(&[0x23, 0xec, 0xb9, 0xe8]);
        record.spd[117] = 0x86;
        let spd = record.jedec_spd();
        assert_eq!((spd[77], spd[80], spd[116], spd[117]), (0x0b, 0, 0, 0x86));
    }

    #[test]
    fn record_sizes() {
        assert_eq!(size_of::<SpdRecord>(), 0x214);
        assert_eq!(size_of::<Type17Seed>(), 8);
        assert_eq!(size_of::<Type20Seed>(), 0x20);
    }

    #[test]
    fn spd_table() {
        // 2 DIMMs per channel, 8 channels; DIMMs on (ch 0, dimm 0) and (ch 5, dimm 1).
        let mut records = vec![0u8; 16 * 0x214];
        for (i, tag) in [(0usize, 0x11u8), (11, 0x22), (15, 0x33)] {
            records[i * 0x214 + 4] = 1;
            records[i * 0x214 + 0x14] = tag;
            // The last SPD byte must belong to this record, not a tail.
            records[i * 0x214 + 0x213] = tag;
        }
        let body = Body::default().at(0x30, &[2, 8]).at(0x34, &records);
        let data = Builder::new(0x18).entry(1, 0x11, 0x100, &body.0).build();
        let apob = Apob::new(&data, Variant::Gn).unwrap();
        let table = spd(&apob, 1, 0).unwrap();
        let dimms: std::vec::Vec<_> = table.dimms().map(|(c, d, r)| (c, d, r.spd[0])).collect();
        assert_eq!(dimms, [(0, 0, 0x11), (5, 1, 0x22), (7, 1, 0x33)]);
        assert_eq!(table.slots().count(), 16);
        assert!(table.dimms().all(|(_, _, r)| r.spd[0] == r.spd[511]));
        // Counts larger than the entry fail.
        let body = Body::default().at(0x30, &[2, 9]).at(0x34, &records);
        let data = Builder::new(0x18).entry(1, 0x11, 0, &body.0).build();
        assert!(spd(&Apob::new(&data, Variant::Ssp).unwrap(), 0, 0).is_err());
    }

    #[test]
    fn exact_spd_entry_needs_no_trailing_record_bytes() {
        for variant in [Variant::Zp, Variant::Ssp, Variant::Gn, Variant::Rv] {
            let mut record = [0u8; 0x214];
            record[4] = 1;
            record[0x14] = 0x23;
            record[0x213] = 0x45;
            let body = Body::default().at(0x30, &[1, 1]).at(0x34, &record);
            let data = Builder::new(0x18).entry(1, 0x11, 0, &body.0).build();
            let apob = Apob::new(&data, variant).unwrap();
            let table = spd(&apob, 0, 0).unwrap();
            let (_, _, record) = table.dimms().next().unwrap();
            assert_eq!((record.spd[0], record.spd[511]), (0x23, 0x45));
        }
    }

    #[test]
    fn spd_validity_and_bounds() {
        let mut records = [0u8; 2 * 0x214];
        records[0] = 1; // Metadata is not the validity byte.
        records[4] = 2; // Only exactly 1 denotes a valid SPD copy.
        records[0x214 + 4] = 1;
        let body = Body::default().at(0x30, &[2, 1]).at(0x34, &records);
        let data = Builder::new(0x18).entry(1, SPD, 0, &body.0).build();
        let apob = Apob::new(&data, Variant::Gn).unwrap();
        let table = spd(&apob, 0, 0).unwrap();
        assert_eq!(table.slots().count(), 2);
        assert_eq!(
            table
                .dimms()
                .map(|(c, d, _)| (c, d))
                .collect::<std::vec::Vec<_>>(),
            [(0, 1)]
        );

        let body = Body::default()
            .at(0x30, &[2, 1])
            .at(0x34, &records[..records.len() - 1]);
        let data = Builder::new(0x18).entry(1, SPD, 0, &body.0).build();
        assert!(matches!(
            spd(&Apob::new(&data, Variant::Gn).unwrap(), 0, 0),
            Err(ApobError::TooShort { .. })
        ));

        for variant in [Variant::Zp, Variant::Ssp, Variant::Gn] {
            for (dimms, channels) in [(3, 1), (1, 9), (0, 1), (1, 0)] {
                let body = Body::default()
                    .at(0x30, &[dimms, channels])
                    .at(0x34, &[0; 27 * 0x214]);
                let data = Builder::new(0x18).entry(1, SPD, 0, &body.0).build();
                let apob = Apob::new(&data, variant).unwrap();
                assert!(matches!(spd(&apob, 0, 0), Err(ApobError::Value { .. })));
                // Do not impose SP3-only topology limits on existing Raven consumers.
                let apob = Apob::new(&data, Variant::Rv).unwrap();
                assert!(spd(&apob, 0, 0).is_ok());
            }
        }
        let body = Body::default().at(0x30, &[1, 3]).at(0x34, &[0; 3 * 0x214]);
        let data = Builder::new(0x18).entry(1, SPD, 0, &body.0).build();
        assert!(matches!(
            spd(&Apob::new(&data, Variant::Zp).unwrap(), 0, 0),
            Err(ApobError::Value { .. })
        ));
        assert!(spd(&Apob::new(&data, Variant::Ssp).unwrap(), 0, 0).is_ok());
    }

    #[test]
    fn seed_presence_and_bounds() {
        for (location, skip, present) in [
            (0, false, false),
            (0x40, false, true),
            (0x80, true, false),
            (0xc0, true, true),
        ] {
            let seed = Type17Seed {
                location,
                _b1: 0,
                handle: 0.into(),
                words: [0.into(); 2],
            };
            assert_eq!(
                seed.position(Variant::Ssp).map(|p| p.present),
                (!skip).then_some(present)
            );
        }
        for (t17, t20, size) in [(1, 0, 0x3b), (0, 1, 0x53), (1, 1, 0x5b)] {
            let body = Body::default().at(0x30, &[0, t17, t20]).len(size);
            let data = Builder::new(0x18).entry(8, SMBIOS, 0, &body.0).build();
            assert!(matches!(
                smbios_info(&Apob::new(&data, Variant::Ssp).unwrap()),
                Err(ApobError::TooShort { .. })
            ));
        }
    }

    #[test]
    fn smbios_seeds() {
        let t17 = [0b0100_0101u8, 0, 0x10, 0, 0, 0, 0, 0]; // present, socket 1, channel 2
        let mut t20 = vec![0u8; 0x20];
        t20[0] = 0x80 | 2 << 2 | 1; // valid, socket 1, channel 2
        t20[2] = 0x20;
        let body = Body::default()
            .at(0x30, &[0x80, 1, 1])
            .at(0x34, &t17)
            .at(0x3c, &t20);
        let data = Builder::new(0x18).entry(8, 8, 0, &body.0).build();
        let apob = Apob::new(&data, Variant::Ssp).unwrap();
        let info = smbios_info(&apob).unwrap();
        assert!(info.ecc());
        let s = &info.type17[0];
        assert_eq!(
            (s.position(Variant::Ssp), s.handle.get()),
            (
                Some(DimmPosition {
                    socket: 1,
                    channel: 2,
                    dimm: 0,
                    present: true
                }),
                0x10
            )
        );
        let s = &info.type20[0];
        let l = s.location();
        assert_eq!(
            (
                l.is_set(Location::VALID),
                l.read(Location::SOCKET),
                l.read(Location::CHANNEL),
                s.handle.get()
            ),
            (true, 1, 2, 0x20)
        );
    }

    #[test]
    fn small_records() {
        let mem = Body::default()
            .at(0x34, &1600u16.to_le_bytes())
            .at(0x38, &[0, 1]);
        let boot = Body::default().at(0x1b4, &[0, 1]);
        let data = Builder::new(0x18)
            .entry(1, 2, 0, &mem.0)
            .entry(1, 0x10, 0, &boot.0)
            .build();
        let apob = Apob::new(&data, Variant::Zp).unwrap();
        let mem = mem_config(&apob, 0, 0).unwrap();
        assert_eq!((mem.memclk(), mem.channel_byte(1)), (Ok(1600), Ok(1)));
        let boot = apcb_boot_info(&apob).unwrap();
        assert_eq!(
            (boot.recovery(), boot.dimm_config_updated()),
            (Ok(true), Ok(false))
        );
    }
}
