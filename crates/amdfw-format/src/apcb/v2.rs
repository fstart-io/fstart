// SPDX-License-Identifier: MIT OR Apache-2.0
//! APCB v2 structured entry editors (Picasso, Zeppelin):
//! - `PSP / 0x60`: GPIO board-ID strap selection (`method = 3`, four
//!   `(gpio, iomux, bank)` triples starting at data offset `+2`);
//! - `MEMORY / 0x30`: per-slot DDR4 SPD table (`u32 count` followed by
//!   `count` `SpdSlot` records of 28-byte [`SpdDescriptor`] + 512-byte SPD).
//!
//! Because this walks the APCB v2 container directly rather than scanning for
//! magic sentinels, it works identically on both sentinel templates
//! (`APCB_magic.bin`) and real donor APCBs (such as Vilboz's extracted
//! instances).

use zerocopy::little_endian::U32;
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned};

use super::{Error, Version, find, group, version};

/// PSP group entry type for board-ID selection (`BoardIdGettingMethod`).
pub const BOARD_ID_METHOD: u16 = 0x60;
/// `BoardIdGettingMethod::Gpio` (16-bit method discriminator at `+0`).
pub const BOARD_ID_GPIO: u16 = 3;
/// Memory group entry type for embedded SPD tables (`DimmInfoSmbus` / `SpdData`).
pub const MEMORY_SPD: u16 = 0x30;

/// 28-byte descriptor preceding each 512-byte SPD payload in `MEMORY / 0x30`.
#[derive(FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned, Clone, Copy, Debug)]
#[repr(C)]
pub struct SpdDescriptor {
    pub valid: u8,
    pub present: u8,
    pub page: u8,
    pub nvdimm: u8,
    pub manufacturer: [u8; 4],
    pub address: [u8; 4],
    pub mux_present: u8,
    pub mux_address: u8,
    pub mux_channel: u8,
    pub padding0: u8,
    pub technology: [u8; 4],
    pub package: [u8; 4],
    pub socket: u8,
    pub channel: u8,
    pub dimm: u8,
    pub padding1: u8,
}

pub const SPD_DESCRIPTOR_SIZE: usize = size_of::<SpdDescriptor>();
pub const SPD_DATA_SIZE: usize = 512;
pub const SPD_SLOT_SIZE: usize = SPD_DESCRIPTOR_SIZE + SPD_DATA_SIZE;
const _: () = assert!(SPD_DESCRIPTOR_SIZE == 28 && SPD_SLOT_SIZE == 540);

/// Read the four `(gpio, iomux, bank)` RAM-ID triples from a v2 APCB's
/// `PSP / 0x60` entry, if it uses GPIO board-ID (`method == 3`).
pub fn read_gpio_id(apcb: &[u8]) -> Result<Option<[[u8; 3]; 4]>, Error> {
    if version(apcb)? != Version::V2 {
        return Ok(None);
    }
    let Some(entry) = find(apcb, group::PSP, BOARD_ID_METHOD, 0)? else {
        return Ok(None);
    };
    let data = &apcb[entry.data];
    if data.len() < 14 {
        return Err(Error::EntrySize {
            expected: 14,
            actual: data.len(),
        });
    }
    let method = u16::from_le_bytes([data[0], data[1]]);
    if method != BOARD_ID_GPIO {
        return Ok(None);
    }
    let mut out = [[0u8; 3]; 4];
    for (i, triple) in out.iter_mut().enumerate() {
        triple.copy_from_slice(&data[2 + i * 3..2 + (i + 1) * 3]);
    }
    Ok(Some(out))
}

/// Write the four `(gpio, iomux, bank)` RAM-ID triples into a v2 APCB's
/// `PSP / 0x60` entry (without fixing checksum). Returns `Ok(true)` if written.
pub fn write_gpio_id(apcb: &mut [u8], gpio: [[u8; 3]; 4]) -> Result<bool, Error> {
    if version(apcb)? != Version::V2 {
        return Ok(false);
    }
    let Some(entry) = find(apcb, group::PSP, BOARD_ID_METHOD, 0)? else {
        return Ok(false);
    };
    if entry.data.len() < 14 {
        return Err(Error::EntrySize {
            expected: 14,
            actual: entry.data.len(),
        });
    }
    let data = &mut apcb[entry.data];
    let method = u16::from_le_bytes([data[0], data[1]]);
    if method != BOARD_ID_GPIO {
        return Ok(false);
    }
    for (i, triple) in gpio.iter().enumerate() {
        data[2 + i * 3..2 + (i + 1) * 3].copy_from_slice(triple);
    }
    Ok(true)
}

/// Write per-(channel, dimm) 512-byte SPD payloads into a v2 APCB's
/// `MEMORY / 0x30` entry (without fixing checksum). Returns the number of slots
/// updated, or `Ok(0)` if no `MEMORY / 0x30` entry is present.
pub fn write_spds(
    apcb: &mut [u8],
    spds: [[Option<&[u8; SPD_DATA_SIZE]>; 2]; 2],
    strip_manufacturer: bool,
) -> Result<usize, Error> {
    if version(apcb)? != Version::V2 {
        return Ok(0);
    }
    let Some(entry) = find(apcb, group::MEMORY, MEMORY_SPD, 0)? else {
        return Ok(0);
    };
    let data = &mut apcb[entry.data];
    let (count_raw, rest) = U32::ref_from_prefix(data).map_err(|_| Error::EntrySize {
        expected: 4,
        actual: data.len(),
    })?;
    let count = count_raw.get() as usize;
    let expected = 4usize
        .checked_add(
            count
                .checked_mul(SPD_SLOT_SIZE)
                .ok_or(Error::TooMany(count))?,
        )
        .ok_or(Error::TooMany(count))?;
    if rest.len() + 4 < expected {
        return Err(Error::EntrySize {
            expected,
            actual: data.len(),
        });
    }
    let mut updated = 0;
    for i in 0..count {
        let slot_start = 4 + i * SPD_SLOT_SIZE;
        let slot = &mut data[slot_start..slot_start + SPD_SLOT_SIZE];
        let (desc_bytes, spd_bytes) = slot.split_at_mut(SPD_DESCRIPTOR_SIZE);
        let mut desc = SpdDescriptor::read_from_bytes(desc_bytes).map_err(|_| Error::Header)?;
        if desc.channel > 1 || desc.dimm > 1 {
            return Err(Error::TooMany(usize::from(desc.channel.max(desc.dimm))));
        }
        let spd = spds[usize::from(desc.channel)][usize::from(desc.dimm)];
        desc.valid = u8::from(spd.is_some());
        desc.present = desc.valid;
        desc_bytes.copy_from_slice(desc.as_bytes());
        if let Some(payload) = spd {
            spd_bytes.copy_from_slice(payload);
            if strip_manufacturer {
                spd_bytes[320..384].fill(0);
            }
        } else {
            spd_bytes.fill(0);
        }
        updated += 1;
    }
    Ok(updated)
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::super::{checksum_ok, fix_checksum};
    use super::*;
    use std::vec;
    use std::vec::Vec;

    fn v2_apcb(gpio: [[u8; 3]; 4]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(b"APCB");
        out.extend_from_slice(&0x20u16.to_le_bytes());
        out.extend_from_slice(&0x20u16.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&1u32.to_le_bytes());
        out.resize(0x20, 0);

        // PSPG group with entry 0x60 (GPIO board ID)
        let psp_at = out.len();
        out.extend_from_slice(b"PSPG");
        out.extend_from_slice(&group::PSP.to_le_bytes());
        out.extend_from_slice(&16u16.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&[0; 6]);
        let mut psp_data = vec![0u8; 16];
        psp_data[0..2].copy_from_slice(&BOARD_ID_GPIO.to_le_bytes());
        for (i, t) in gpio.iter().enumerate() {
            psp_data[2 + i * 3..2 + (i + 1) * 3].copy_from_slice(t);
        }
        out.extend_from_slice(&group::PSP.to_le_bytes());
        out.extend_from_slice(&BOARD_ID_METHOD.to_le_bytes());
        out.extend_from_slice(&((16 + psp_data.len()) as u16).to_le_bytes());
        out.extend_from_slice(&[0u8; 10]);
        out.extend_from_slice(&psp_data);
        let psp_size = (out.len() - psp_at) as u32;
        out[psp_at + 12..psp_at + 16].copy_from_slice(&psp_size.to_le_bytes());

        // MEMG group with entry 0x30 (1 SPD slot: ch 1, dimm 1)
        let mem_at = out.len();
        out.extend_from_slice(b"MEMG");
        out.extend_from_slice(&group::MEMORY.to_le_bytes());
        out.extend_from_slice(&16u16.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&[0; 6]);
        let mut mem_data = vec![0u8; 4 + SPD_SLOT_SIZE];
        mem_data[0..4].copy_from_slice(&1u32.to_le_bytes());
        mem_data[4 + 25] = 1; // channel 1
        mem_data[4 + 26] = 1; // dimm 1
        out.extend_from_slice(&group::MEMORY.to_le_bytes());
        out.extend_from_slice(&MEMORY_SPD.to_le_bytes());
        out.extend_from_slice(&((16 + mem_data.len()) as u16).to_le_bytes());
        out.extend_from_slice(&[0u8; 10]);
        out.extend_from_slice(&mem_data);
        let mem_size = (out.len() - mem_at) as u32;
        out[mem_at + 12..mem_at + 16].copy_from_slice(&mem_size.to_le_bytes());

        let total = out.len() as u32;
        out[8..12].copy_from_slice(&total.to_le_bytes());
        fix_checksum(&mut out).unwrap();
        out
    }

    #[test]
    fn edits_v2_gpio_and_spd_without_sentinels() {
        let mut apcb = v2_apcb([[0xfa, 0xde, 0xdd]; 4]);
        let want_gpio = [[132, 1, 0], [90, 1, 0], [86, 3, 0], [69, 1, 0]];
        assert!(write_gpio_id(&mut apcb, want_gpio).unwrap());
        let spd = [0xbb; 512];
        assert_eq!(
            write_spds(&mut apcb, [[None, None], [None, Some(&spd)]], true),
            Ok(1)
        );
        fix_checksum(&mut apcb).unwrap();
        assert!(checksum_ok(&apcb).unwrap());
        assert_eq!(read_gpio_id(&apcb), Ok(Some(want_gpio)));
    }
}
