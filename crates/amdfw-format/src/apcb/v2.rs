// SPDX-License-Identifier: MIT OR Apache-2.0
//! APCB v2 structured entry editors (Picasso, Zeppelin):
//! - `PSP / 0x60`: GPIO board-ID strap selection (`method = 3`, four
//!   `(gpio, iomux, bank)` triples starting at data offset `+2`);
//! - `MEMORY / 0x30`: per-slot DDR4 SPD table (`u32 count` followed by
//!   `count` `SpdSlot` records of 28-byte [`SpdDescriptor`] + 512-byte SPD);
//! - Parameter-list entries (`PSPG/0x02`, `CCXG/0x04`, `DFG/0x06`,
//!   `MEMG/0x08`, `GNBG/0x0a`, `FCHG/0x0c`, `CBSG/0x0d`, `CBSG/0x0f`):
//!   packed `APCB_PARAM_ATTRIBUTE_ENTRY` (`u32`) table terminated by
//!   `(time_point = 0, token = 0x1fff)` followed by contiguous little-endian
//!   values of `1`, `2`, `4`, or `8` bytes each ([`for_each_entry_param`],
//!   [`read_param`], [`update_param`]).
//!
//! Because this walks the APCB v2 container directly rather than scanning for
//! magic sentinels, it works identically on both sentinel templates
//! (`APCB_magic.bin`) and real donor APCBs (such as Vilboz's and MJ11's
//! extracted instances).

use zerocopy::little_endian::U32;
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned};

use super::{Error, Version, find, group, version, walk};

/// PSP group entry type for board-ID selection (`BoardIdGettingMethod`).
pub const BOARD_ID_METHOD: u16 = 0x60;
/// `BoardIdGettingMethod::Gpio` (16-bit method discriminator at `+0`).
pub const BOARD_ID_GPIO: u16 = 3;
/// Memory group entry type for embedded SPD tables (`DimmInfoSmbus` / `SpdData`).
pub const MEMORY_SPD: u16 = 0x30;

/// Entry types of APCB v2 packed parameter-list entries.
pub mod param_type {
    use super::group;

    pub const PSP_PARAMS: u16 = 0x02;
    pub const CCX_PARAMS: u16 = 0x04;
    pub const DF_PARAMS: u16 = 0x06;
    pub const MEMORY_PARAMS: u16 = 0x08;
    pub const GNB_PARAMS: u16 = 0x0a;
    pub const FCH_PARAMS: u16 = 0x0c;
    pub const CBS_COMMON_PARAMS: u16 = 0x0d;
    pub const CBS_DEBUG_PARAMS: u16 = 0x0f;

    /// Whether `(group_id, entry_type)` is an APCB v2 packed parameter-list entry.
    pub const fn is_param_entry(group_id: u16, entry_type: u16) -> bool {
        matches!(
            (group_id, entry_type),
            (group::PSP, PSP_PARAMS)
                | (group::CCX, CCX_PARAMS)
                | (group::DF, DF_PARAMS)
                | (group::MEMORY, MEMORY_PARAMS)
                | (group::GNB, GNB_PARAMS)
                | (group::FCH, FCH_PARAMS)
                | (group::CBS, CBS_COMMON_PARAMS | CBS_DEBUG_PARAMS)
        )
    }
}

/// Token ID marking the end of an APCB v2 `APCB_PARAM_ATTRIBUTE_ENTRY` table.
const PARAM_END_TOKEN: u16 = 0x1fff;

/// One decoded parameter from an APCB v2 packed parameter-list entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct V2Param {
    pub time_point: u8,
    pub token: u16,
    /// Width in bytes (`1`, `2`, `4`, or `8`).
    pub size: u8,
    pub value: u64,
}

#[derive(Clone, Copy)]
struct Attr {
    time_point: u8,
    token: u16,
    size: u8,
}

fn decode_attr(word: u32) -> Attr {
    Attr {
        time_point: (word & 0xff) as u8,
        token: ((word >> 8) & 0x1fff) as u16,
        size: (((word >> 21) & 0x7) as u8) + 1,
    }
}

fn valid_param_size(size: u8) -> bool {
    matches!(size, 1 | 2 | 4 | 8)
}

fn value_fits(size: u8, value: u64) -> bool {
    match size {
        1 => value <= u64::from(u8::MAX),
        2 => value <= u64::from(u16::MAX),
        4 => value <= u64::from(u32::MAX),
        8 => true,
        _ => false,
    }
}

/// Locate the byte offset where the value block begins (immediately after the
/// `(time_point = 0, token = 0x1fff)` terminator attribute) and count entries.
fn param_value_start(payload: &[u8]) -> Result<(usize, usize), Error> {
    let mut off = 0usize;
    let mut count = 0usize;
    while off + 4 <= payload.len() {
        let word = u32::from_le_bytes([
            payload[off],
            payload[off + 1],
            payload[off + 2],
            payload[off + 3],
        ]);
        off += 4;
        let attr = decode_attr(word);
        if attr.time_point == 0 && attr.token == PARAM_END_TOKEN {
            return Ok((count, off));
        }
        if !valid_param_size(attr.size) {
            return Err(Error::EntrySize {
                expected: 1,
                actual: usize::from(attr.size),
            });
        }
        count += 1;
    }
    Err(Error::Truncated(payload.len()))
}

/// Walk every parameter in a single APCB v2 parameter-list entry's `payload`.
/// Stops early if `f` returns `true`.
pub fn for_each_entry_param(
    payload: &[u8],
    mut f: impl FnMut(V2Param) -> bool,
) -> Result<(), Error> {
    let (count, mut val_off) = param_value_start(payload)?;
    for i in 0..count {
        let off = i * 4;
        let attr = decode_attr(u32::from_le_bytes([
            payload[off],
            payload[off + 1],
            payload[off + 2],
            payload[off + 3],
        ]));
        let sz = usize::from(attr.size);
        let end = val_off.checked_add(sz).ok_or(Error::Truncated(val_off))?;
        let slice = payload.get(val_off..end).ok_or(Error::Truncated(end))?;
        let mut buf = [0u8; 8];
        buf[..sz].copy_from_slice(slice);
        let value = u64::from_le_bytes(buf);
        val_off = end;
        if f(V2Param {
            time_point: attr.time_point,
            token: attr.token,
            size: attr.size,
            value,
        }) {
            break;
        }
    }
    Ok(())
}

/// Read the first occurrence of `(group_id, entry_type, token)` from a v2 APCB.
pub fn read_param(
    apcb: &[u8],
    group_id: u16,
    entry_type: u16,
    token: u16,
) -> Result<Option<V2Param>, Error> {
    if version(apcb)? != Version::V2 {
        return Ok(None);
    }
    let mut found = None;
    let mut err = None;
    walk(apcb, |entry| {
        if entry.group != group_id || entry.kind != entry_type {
            return false;
        }
        if let Err(e) = for_each_entry_param(&apcb[entry.data.clone()], |p| {
            if p.token == token {
                found = Some(p);
                true
            } else {
                false
            }
        }) {
            err = Some(e);
            return true;
        }
        found.is_some()
    })?;
    if let Some(e) = err {
        return Err(e);
    }
    Ok(found)
}

/// Update every occurrence of `(group_id, entry_type, token)` in place across
/// all matching entries of a v2 APCB (without fixing checksum). Returns the
/// number of occurrences updated.
///
/// Updating all matching entries ensures duplicate entries (such as MJ11's
/// duplicated `CBSG / 0x0d` and `CBSG / 0x0f` blocks) stay consistent.
pub fn update_param(
    apcb: &mut [u8],
    group_id: u16,
    entry_type: u16,
    token: u16,
    value: u64,
) -> Result<usize, Error> {
    if version(apcb)? != Version::V2 {
        return Ok(0);
    }
    // Collect matching entry data ranges first without allocating.
    let mut updated = 0usize;
    let mut cursor = 0usize;
    loop {
        let mut next_range = None;
        walk(apcb, |entry| {
            if entry.header_offset >= cursor && entry.group == group_id && entry.kind == entry_type
            {
                cursor = entry.header_offset + 1;
                next_range = Some(entry.data.clone());
                true
            } else {
                false
            }
        })?;
        let Some(range) = next_range else {
            break;
        };
        let payload = &mut apcb[range];
        let (count, mut val_off) = param_value_start(payload)?;
        for i in 0..count {
            let off = i * 4;
            let attr = decode_attr(u32::from_le_bytes([
                payload[off],
                payload[off + 1],
                payload[off + 2],
                payload[off + 3],
            ]));
            let sz = usize::from(attr.size);
            let end = val_off.checked_add(sz).ok_or(Error::Truncated(val_off))?;
            if end > payload.len() {
                return Err(Error::Truncated(end));
            }
            if attr.token == token {
                if !value_fits(attr.size, value) {
                    return Err(Error::TokenValue {
                        id: u32::from(token),
                        value: value as u32,
                    });
                }
                payload[val_off..end].copy_from_slice(&value.to_le_bytes()[..sz]);
                updated += 1;
            }
            val_off = end;
        }
    }
    Ok(updated)
}

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

    fn v2_param_apcb() -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(b"APCB");
        out.extend_from_slice(&0x20u16.to_le_bytes());
        out.extend_from_slice(&0x20u16.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&1u32.to_le_bytes());
        out.resize(0x20, 0);

        let attr = |tp: u8, token: u16, size: u8| -> [u8; 4] {
            let word = u32::from(tp) | (u32::from(token) << 8) | (u32::from(size - 1) << 21);
            word.to_le_bytes()
        };
        let end_attr = || -> [u8; 4] { (u32::from(PARAM_END_TOKEN) << 8).to_le_bytes() };

        // CBSG with two duplicate 0x0d entries (mirroring MJ11).
        let cbs_at = out.len();
        out.extend_from_slice(b"CBSG");
        out.extend_from_slice(&group::CBS.to_le_bytes());
        out.extend_from_slice(&16u16.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&[0; 6]);
        let mut payload = Vec::new();
        payload.extend_from_slice(&attr(1, 0x001c, 4));
        payload.extend_from_slice(&attr(1, 0x005b, 1));
        payload.extend_from_slice(&attr(1, 0x002c, 2));
        payload.extend_from_slice(&end_attr());
        payload.extend_from_slice(&0x0000_0001u32.to_le_bytes());
        payload.push(0x00);
        payload.extend_from_slice(&0x1234u16.to_le_bytes());
        payload.push(0xff);
        for _ in 0..2 {
            out.extend_from_slice(&group::CBS.to_le_bytes());
            out.extend_from_slice(&param_type::CBS_COMMON_PARAMS.to_le_bytes());
            out.extend_from_slice(&((16 + payload.len()) as u16).to_le_bytes());
            out.extend_from_slice(&[0u8; 10]);
            out.extend_from_slice(&payload);
        }
        let cbs_size = (out.len() - cbs_at) as u32;
        out[cbs_at + 12..cbs_at + 16].copy_from_slice(&cbs_size.to_le_bytes());

        let total = out.len() as u32;
        out[8..12].copy_from_slice(&total.to_le_bytes());
        fix_checksum(&mut out).unwrap();
        out
    }

    #[test]
    fn reads_and_updates_v2_params_across_duplicate_entries() {
        let mut apcb = v2_param_apcb();
        assert_eq!(
            read_param(&apcb, group::CBS, param_type::CBS_COMMON_PARAMS, 0x002c),
            Ok(Some(V2Param {
                time_point: 1,
                token: 0x002c,
                size: 2,
                value: 0x1234,
            }))
        );
        // Rejects value wider than 1-byte param 0x005b.
        assert!(matches!(
            update_param(
                &mut apcb,
                group::CBS,
                param_type::CBS_COMMON_PARAMS,
                0x005b,
                0x100
            ),
            Err(Error::TokenValue { id: 0x005b, .. })
        ));
        // Updates both duplicate 0x0d entries in place.
        assert_eq!(
            update_param(
                &mut apcb,
                group::CBS,
                param_type::CBS_COMMON_PARAMS,
                0x002c,
                0xabcd
            ),
            Ok(2)
        );
        fix_checksum(&mut apcb).unwrap();
        assert!(checksum_ok(&apcb).unwrap());
        let mut seen = Vec::new();
        walk(&apcb, |e| {
            if e.group == group::CBS && e.kind == param_type::CBS_COMMON_PARAMS {
                for_each_entry_param(&apcb[e.data.clone()], |p| {
                    if p.token == 0x002c {
                        seen.push(p.value);
                    }
                    false
                })
                .unwrap();
            }
            false
        })
        .unwrap();
        assert_eq!(seen, vec![0xabcd, 0xabcd]);
    }
}
