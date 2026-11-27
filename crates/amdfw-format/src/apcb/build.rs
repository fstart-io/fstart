// SPDX-License-Identifier: MIT OR Apache-2.0
//! Native `no_std`, allocation-free APCB v3 builder for the override
//! (`0x60/0`, backup `0x68/8`) and event/PPR store (`0x60/1`, backup `0x68/9`)
//! layers across SP3 (Rome/Milan), RS (Genoa/Siena), and Venice.
//!
//! These four APCB files have a uniform, minimal structure across every v3
//! generation:
//! - Override (`0x60/0` padded to 4 KiB; `0x68/8` exact size): 128-byte v3
//!   header (`ECB2`..`BCBA`/`BCPA`), `DFG ` (`0x1703`) entry `0x00CC`, and
//!   `TOKN` (`0x3000`) entries for all four [`TokenKind`]s (`inst=0, prio=0x10`
//!   empty Low + `inst=1, prio=0x04` sorted Medium carrying override tokens).
//! - Event/PPR store (`0x60/1` padded to 4 KiB; `0x68/9` exact size): 128-byte
//!   v3 header + `MEMG` (`0x1704`) entry `0x005E` (`inst=0, prio=0x08`).

use super::{
    CHECKSUM, CXL_REGIONS, Error, SIGNATURE, VERSION_V3, fix_checksum, group,
    token::{TOKEN_SIZE, Token, TokenKind},
};

/// Generation-specific layout parameters for synthesized APCB v3 override and
/// event-store payloads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(
    feature = "serde",
    serde(rename_all = "kebab-case", deny_unknown_fields)
)]
pub struct V3Layout {
    /// Four-byte trailer at header offset `0x7c..0x80` (`BCBA` on SP3, `BCPA`
    /// on RS/Venice).
    pub ext_ending: [u8; 4],
    /// Byte length of `DFG / 0x00CC` in `0x60/0` & `0x68/8`.
    pub df_cc_len: u16,
    /// Byte length of `MEMG / 0x005E` in `0x60/1` & `0x68/9`.
    pub event_store_len: u16,
    /// SP3 PPR store marks unused 8-byte slots with `0xFF` at byte 7; RS uses
    /// all-zero bytes.
    pub ppr_ff_marker: bool,
}

impl V3Layout {
    /// SP3 (Rome and Milan): `BCBA`, 64-byte `DF/0xCC`, 512-byte PPR store.
    pub const SP3: Self = Self {
        ext_ending: *b"BCBA",
        df_cc_len: 64,
        event_store_len: 512,
        ppr_ff_marker: true,
    };

    /// SP5 (Genoa): `BCPA`, 512-byte `DF/0xCC`, 1024-byte event store.
    pub const GENOA: Self = Self {
        ext_ending: *b"BCPA",
        df_cc_len: 512,
        event_store_len: 1024,
        ppr_ff_marker: false,
    };

    /// SP6 (Siena): `BCPA`, 2112-byte `DF/0xCC`, 1024-byte event store.
    pub const SIENA: Self = Self {
        ext_ending: *b"BCPA",
        df_cc_len: 2112,
        event_store_len: 1024,
        ppr_ff_marker: false,
    };

    /// SP7 (Venice): `BCPA`, 1344-byte `DF/0xCC`, 1024-byte event store.
    pub const VENICE: Self = Self {
        ext_ending: *b"BCPA",
        df_cc_len: 1344,
        event_store_len: 1024,
        ppr_ff_marker: false,
    };
}

/// Memory group entry type for the event / post-package-repair store.
pub const MEMORY_EVENT_STORE: u16 = 0x5e;

const HEADER_SIZE: usize = 0x80;
const GROUP_HEADER_SIZE: usize = 16;
const ENTRY_HEADER_SIZE: usize = 16;

fn write_v3_header(buf: &mut [u8], layout: V3Layout, unique_instance: u32) -> Result<(), Error> {
    if buf.len() < HEADER_SIZE {
        return Err(Error::NoRoom {
            needed: HEADER_SIZE,
        });
    }
    buf[..HEADER_SIZE].fill(0);
    buf[0..4].copy_from_slice(&SIGNATURE);
    buf[4..6].copy_from_slice(&(HEADER_SIZE as u16).to_le_bytes());
    buf[6..8].copy_from_slice(&VERSION_V3.to_le_bytes());
    buf[12..16].copy_from_slice(&unique_instance.to_le_bytes());
    // Standard v3 ECB2 extension fields observed identically across Rome, Milan,
    // Genoa, and Siena.
    buf[0x20..0x24].copy_from_slice(b"ECB2");
    buf[0x26..0x28].copy_from_slice(&0x0010u16.to_le_bytes());
    buf[0x28..0x2a].copy_from_slice(&0x0012u16.to_le_bytes());
    buf[0x2b] = 0x01;
    buf[0x2c] = 0x60;
    buf[0x32..0x34].copy_from_slice(&0xffffu16.to_le_bytes());
    buf[0x34] = 0x40;
    buf[0x40] = 0x58;
    buf[0x7c..0x80].copy_from_slice(&layout.ext_ending);
    Ok(())
}

fn write_group_header(buf: &mut [u8], at: usize, sig: &[u8; 4], gid: u16, size: u32) {
    buf[at..at + 4].copy_from_slice(sig);
    buf[at + 4..at + 6].copy_from_slice(&gid.to_le_bytes());
    buf[at + 6..at + 8].copy_from_slice(&(GROUP_HEADER_SIZE as u16).to_le_bytes());
    buf[at + 8..at + 10].copy_from_slice(&1u16.to_le_bytes());
    buf[at + 10..at + 12].copy_from_slice(&0u16.to_le_bytes());
    buf[at + 12..at + 16].copy_from_slice(&size.to_le_bytes());
}

#[allow(clippy::too_many_arguments)]
fn write_entry_header(
    buf: &mut [u8],
    at: usize,
    gid: u16,
    kind: u16,
    entry_size: u16,
    instance: u16,
    is_token: bool,
    priority: u8,
    board_mask: u16,
) {
    buf[at..at + 2].copy_from_slice(&gid.to_le_bytes());
    buf[at + 2..at + 4].copy_from_slice(&kind.to_le_bytes());
    buf[at + 4..at + 6].copy_from_slice(&entry_size.to_le_bytes());
    buf[at + 6..at + 8].copy_from_slice(&instance.to_le_bytes());
    buf[at + 8] = if is_token { 2 } else { 0 };
    buf[at + 9] = u8::from(is_token);
    buf[at + 10] = if is_token { TOKEN_SIZE as u8 } else { 0 };
    buf[at + 11] = priority;
    buf[at + 12] = if is_token { 4 } else { 0 };
    buf[at + 13] = 0;
    buf[at + 14..at + 16].copy_from_slice(&board_mask.to_le_bytes());
}

/// Synthesize a v3 override APCB (`0x60/0` or backup `0x68/8`) into `buf`
/// (remaining bytes of `buf` are filled with `0xFF`). Returns `apcb_size`.
pub fn write_override_apcb(
    buf: &mut [u8],
    layout: V3Layout,
    unique_instance: u32,
    df_cc: Option<&[u8]>,
    tokens: &[Token],
) -> Result<usize, Error> {
    let df_len = usize::from(layout.df_cc_len);
    if let Some(data) = df_cc
        && data.len() != df_len
    {
        return Err(Error::EntrySize {
            expected: df_len,
            actual: data.len(),
        });
    }
    let df_padded = df_len.next_multiple_of(4);
    let df_group_size = GROUP_HEADER_SIZE + ENTRY_HEADER_SIZE + df_padded;

    let mut tokn_entries_size = 0usize;
    for kind in TokenKind::ALL {
        let count = tokens.iter().filter(|t| t.kind == kind).count();
        tokn_entries_size += ENTRY_HEADER_SIZE; // inst=0 Low (empty)
        tokn_entries_size += ENTRY_HEADER_SIZE + count * TOKEN_SIZE; // inst=1 Medium
    }
    let tokn_group_size = GROUP_HEADER_SIZE + tokn_entries_size;
    let total = HEADER_SIZE + df_group_size + tokn_group_size;
    if buf.len() < total {
        return Err(Error::NoRoom { needed: total });
    }

    write_v3_header(buf, layout, unique_instance)?;
    let mut at = HEADER_SIZE;

    // DFG group with 0x00CC entry
    write_group_header(buf, at, b"DFG ", group::DF, df_group_size as u32);
    at += GROUP_HEADER_SIZE;
    write_entry_header(
        buf,
        at,
        group::DF,
        CXL_REGIONS,
        (ENTRY_HEADER_SIZE + df_len) as u16,
        0,
        false,
        0x10,
        0xffff,
    );
    at += ENTRY_HEADER_SIZE;
    buf[at..at + df_padded].fill(0);
    if let Some(data) = df_cc {
        buf[at..at + df_len].copy_from_slice(data);
    }
    at += df_padded;

    // TOKN group: Low (inst=0, empty) + Medium (inst=1, sorted by ID) per kind
    write_group_header(buf, at, b"TOKN", group::TOKEN, tokn_group_size as u32);
    at += GROUP_HEADER_SIZE;
    for kind in TokenKind::ALL {
        write_entry_header(
            buf,
            at,
            group::TOKEN,
            kind.entry_type(),
            ENTRY_HEADER_SIZE as u16,
            0,
            true,
            0x10,
            0xffff,
        );
        at += ENTRY_HEADER_SIZE;

        let count = tokens.iter().filter(|t| t.kind == kind).count();
        let entry_size = u16::try_from(ENTRY_HEADER_SIZE + count * TOKEN_SIZE)
            .map_err(|_| Error::TooMany(count))?;
        write_entry_header(
            buf,
            at,
            group::TOKEN,
            kind.entry_type(),
            entry_size,
            1,
            true,
            0x04,
            0xffff,
        );
        at += ENTRY_HEADER_SIZE;

        // Insertion-sort matching tokens by ID directly into `buf` (no_std).
        let start = at;
        let mut written = 0usize;
        for tok in tokens.iter().filter(|t| t.kind == kind) {
            let mut pos = written;
            while pos > 0 {
                let prev_at = start + (pos - 1) * TOKEN_SIZE;
                let prev_id =
                    u32::from_le_bytes(buf[prev_at..prev_at + 4].try_into().unwrap_or_default());
                if prev_id <= tok.id {
                    break;
                }
                buf.copy_within(prev_at..prev_at + TOKEN_SIZE, prev_at + TOKEN_SIZE);
                pos -= 1;
            }
            let dst = start + pos * TOKEN_SIZE;
            buf[dst..dst + 4].copy_from_slice(&tok.id.to_le_bytes());
            buf[dst + 4..dst + TOKEN_SIZE].copy_from_slice(&tok.value.to_le_bytes());
            written += 1;
        }
        at += written * TOKEN_SIZE;
    }

    debug_assert_eq!(at, total);
    buf[8..12].copy_from_slice(&(total as u32).to_le_bytes());
    buf[total..].fill(0xff);
    buf[CHECKSUM] = 0;
    fix_checksum(buf)?;
    Ok(total)
}

/// Synthesize a clean v3 event / PPR store APCB (`0x60/1` or backup `0x68/9`)
/// into `buf` (remaining bytes filled with `0xFF`). Returns `apcb_size`.
pub fn write_event_store_apcb(
    buf: &mut [u8],
    layout: V3Layout,
    unique_instance: u32,
) -> Result<usize, Error> {
    let data_len = usize::from(layout.event_store_len);
    let data_padded = data_len.next_multiple_of(4);
    let mem_group_size = GROUP_HEADER_SIZE + ENTRY_HEADER_SIZE + data_padded;
    let total = HEADER_SIZE + mem_group_size;
    if buf.len() < total {
        return Err(Error::NoRoom { needed: total });
    }

    write_v3_header(buf, layout, unique_instance)?;
    let mut at = HEADER_SIZE;
    write_group_header(buf, at, b"MEMG", group::MEMORY, mem_group_size as u32);
    at += GROUP_HEADER_SIZE;
    write_entry_header(
        buf,
        at,
        group::MEMORY,
        MEMORY_EVENT_STORE,
        (ENTRY_HEADER_SIZE + data_len) as u16,
        0,
        false,
        0x08,
        0xffff,
    );
    at += ENTRY_HEADER_SIZE;
    buf[at..at + data_padded].fill(0);
    if layout.ppr_ff_marker {
        for slot in buf[at..at + data_len].as_chunks_mut::<8>().0 {
            slot[7] = 0xff;
        }
    }
    at += data_padded;

    debug_assert_eq!(at, total);
    buf[8..12].copy_from_slice(&(total as u32).to_le_bytes());
    buf[total..].fill(0xff);
    buf[CHECKSUM] = 0;
    fix_checksum(buf)?;
    Ok(total)
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::super::{Version, checksum_ok, find, token, version};
    use super::*;
    use std::vec;

    #[test]
    fn synthesizes_valid_override_and_event_store_for_every_layout() {
        let toks = [
            Token::new(TokenKind::Byte, 0xddb7_59da, 1).unwrap(),
            Token::new(TokenKind::Bool, 0xbbb1_85a2, 1).unwrap(),
            Token::new(TokenKind::Byte, 0x2cf3_dac9, 4).unwrap(),
            Token::new(TokenKind::Dword, 0x3d9b_7d7b, 0x1000_0000).unwrap(),
        ];
        for layout in [
            V3Layout::SP3,
            V3Layout::GENOA,
            V3Layout::SIENA,
            V3Layout::VENICE,
        ] {
            let mut ov = vec![0u8; 0x1000];
            let sz = write_override_apcb(&mut ov, layout, 1, None, &toks).unwrap();
            assert_eq!(version(&ov), Ok(Version::V3));
            assert!(checksum_ok(&ov).unwrap());
            assert!(ov[sz..].iter().all(|&b| b == 0xff));
            assert_eq!(token::token(&ov, TokenKind::Byte, 0xddb7_59da), Ok(Some(1)));
            assert_eq!(token::token(&ov, TokenKind::Byte, 0x2cf3_dac9), Ok(Some(4)));
            assert_eq!(
                token::token(&ov, TokenKind::Dword, 0x3d9b_7d7b),
                Ok(Some(0x1000_0000))
            );
            let df = find(&ov, group::DF, CXL_REGIONS, 0).unwrap().unwrap();
            assert_eq!(df.data.len(), usize::from(layout.df_cc_len));

            let mut ev = vec![0u8; 0x1000];
            let ev_sz = write_event_store_apcb(&mut ev, layout, 1).unwrap();
            assert_eq!(version(&ev), Ok(Version::V3));
            assert!(checksum_ok(&ev).unwrap());
            let mem = find(&ev, group::MEMORY, MEMORY_EVENT_STORE, 0)
                .unwrap()
                .unwrap();
            assert_eq!(mem.data.len(), usize::from(layout.event_store_len));
            if layout.ppr_ff_marker {
                assert_eq!(ev[mem.data.start + 7], 0xff);
            } else {
                assert!(ev[mem.data].iter().all(|&b| b == 0));
            }
            assert!(ev[ev_sz..].iter().all(|&b| b == 0xff));
        }
    }
}
