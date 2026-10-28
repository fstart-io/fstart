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
    present: u8,
    /// Copied into the AGESA memory-info PPI; meaning not traced.
    pub info: [u8; 3],
    /// As `info`.
    pub dword4: U32,
    /// As `info`.
    pub dword8: U32,
    _c: U32,
    /// The DIMM's SPD bytes.
    pub spd: [u8; 512],
    _tail: U32,
}

impl SpdRecord {
    pub fn present(&self) -> bool {
        self.present != 0
    }
}

/// 1/0x11: SPD copies of one socket/die.
///
/// Layout from the consumers (ZP MemAm4Pei `ffdd46ce`; Rome
/// AmdMemSspSp3Pei `ffdee1cb`..`ffdee25f`, instruction-checked; Milan the
/// same function, decompiler-only): DIMMs per channel `+0x30`, channels
/// `+0x31`, records `+0x38`, stride 0x214, index `channel * dimms + dimm`.
/// The SSP/GN `ApobSspPei` getters compute `+0x34` (and stride 0x215 on
/// Rome) but nothing reads through them for SMBIOS.
pub struct SpdTable<'a> {
    dimms_per_channel: u8,
    records: &'a [SpdRecord],
}

impl<'a> SpdTable<'a> {
    /// `(channel, dimm, record)` for every present DIMM.
    pub fn dimms(&self) -> impl Iterator<Item = (u8, u8, &'a SpdRecord)> + 'a {
        let per = self.dimms_per_channel.max(1);
        let records = self.records;
        records
            .iter()
            .enumerate()
            .filter(|(_, r)| r.present())
            .map(move |(i, r)| {
                (
                    (i / usize::from(per)) as u8,
                    (i % usize::from(per)) as u8,
                    r,
                )
            })
    }

    pub fn dimms_per_channel(&self) -> u8 {
        self.dimms_per_channel
    }
}

pub fn spd<'a>(apob: &Apob<'a>, socket: u8, die: u8) -> Result<SpdTable<'a>, ApobError> {
    let entry = apob.find(group::MEMORY, SPD, instance(socket, die))?;
    let dimms_per_channel = entry.byte(0x30)?;
    let channels = entry.byte(0x31)?;
    Ok(SpdTable {
        dimms_per_channel,
        records: entry.slice(0x38, usize::from(channels) * usize::from(dimms_per_channel))?,
    })
}

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

impl Type17Seed {
    /// Skipped by AGESA when set.
    pub fn skip(&self) -> bool {
        self.location & 0x80 != 0
    }
    pub fn socket(&self) -> u8 {
        self.location & 1
    }
    pub fn channel(&self) -> u8 {
        self.location >> 1 & 7
    }
    pub fn dimm(&self) -> u8 {
        self.location >> 4 & 3
    }
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
    /// AGESA uses the record only when set.
    pub fn valid(&self) -> bool {
        self.location & 0x80 != 0
    }
    pub fn socket(&self) -> u8 {
        self.location & 3
    }
    pub fn channel(&self) -> u8 {
        self.location >> 2 & 7
    }
    pub fn dimm(&self) -> u8 {
        self.location >> 5 & 3
    }
    /// Bit 0 of the second byte: AGESA fills the interleave fields with 0xff.
    pub fn interleaved(&self) -> bool {
        self.flags & 1 != 0
    }
}

/// 8/8 instance 0 (Rome MemSmbiosV2SspPei, decompiler-only).
pub struct SmbiosInfo<'a> {
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
}

pub fn smbios_info<'a>(apob: &Apob<'a>) -> Result<SmbiosInfo<'a>, ApobError> {
    let entry = apob.find(group::SMBIOS, SMBIOS, 0)?;
    let t17 = usize::from(entry.byte(0x31)?);
    let t20 = usize::from(entry.byte(0x32)?);
    Ok(SmbiosInfo {
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
    fn record_sizes() {
        assert_eq!(size_of::<SpdRecord>(), 0x214);
        assert_eq!(size_of::<Type17Seed>(), 8);
        assert_eq!(size_of::<Type20Seed>(), 0x20);
    }

    #[test]
    fn spd_table() {
        // 2 DIMMs per channel, 8 channels; DIMMs on (ch 0, dimm 0) and (ch 5, dimm 1).
        let mut records = vec![0u8; 16 * 0x214];
        for (i, tag) in [(0usize, 0x11u8), (11, 0x22)] {
            records[i * 0x214] = 1;
            records[i * 0x214 + 0x10] = tag;
        }
        let body = Body::default().at(0x30, &[2, 8]).at(0x38, &records);
        let data = Builder::new(0x18).entry(1, 0x11, 0x100, &body.0).build();
        let apob = Apob::new(&data, Variant::Gn).unwrap();
        let table = spd(&apob, 1, 0).unwrap();
        let dimms: std::vec::Vec<_> = table.dimms().map(|(c, d, r)| (c, d, r.spd[0])).collect();
        assert_eq!(dimms, [(0, 0, 0x11), (5, 1, 0x22)]);
        // Counts larger than the entry fail.
        let body = Body::default().at(0x30, &[2, 9]).at(0x38, &records);
        let data = Builder::new(0x18).entry(1, 0x11, 0, &body.0).build();
        assert!(spd(&Apob::new(&data, Variant::Ssp).unwrap(), 0, 0).is_err());
    }

    #[test]
    fn smbios_seeds() {
        let t17 = [0b0000_0101u8, 0, 0x10, 0, 0, 0, 0, 0]; // socket 1, channel 2
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
            (s.skip(), s.socket(), s.channel(), s.dimm(), s.handle.get()),
            (false, 1, 2, 0, 0x10)
        );
        let s = &info.type20[0];
        assert_eq!(
            (s.valid(), s.socket(), s.channel(), s.handle.get()),
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
