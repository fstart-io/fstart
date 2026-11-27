// SPDX-License-Identifier: MIT OR Apache-2.0
//! The token entries of an APCB v3: the settings the ABL reads by a 32-bit
//! token ID (the `TOKN` group, `0x3000`).
//!
//! A token entry is a typed array of `{u32 id, u32 value}` sorted by ID, one
//! entry type per value width: 0 bool, 1 byte, 2 word, 4 dword. The same token
//! can appear in several entries (per instance, per board mask, and per
//! priority); the ABL takes the occurrence of the highest priority (the lowest
//! set bit in `priority_mask`: HardForce `0x01`, High `0x02`, Medium `0x04`,
//! EventLogging `0x08`, Low `0x10`, Normal `0x20`).
//!
//! Adding a token keeps the entry sorted and grows the entry, its group, and
//! the APCB by [`TOKEN_SIZE`] into erased (`0xFF`) room after the APCB and its
//! HMAC tail. Editing does not seal the APCB: [`super::commit`] does.

use zerocopy::FromBytes;

use super::{
    Entry, EntryHeader, Error, GroupHeader, HMAC_SIZE, Header, Version, extent, group, walk,
};

/// Bytes of one token, whatever its kind: `{u32 id, u32 value}`.
pub const TOKEN_SIZE: usize = 8;

/// The kind of a token entry: the width of its values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "kebab-case"))]
pub enum TokenKind {
    Bool,
    Byte,
    Word,
    Dword,
}

impl TokenKind {
    pub const ALL: [Self; 4] = [Self::Bool, Self::Byte, Self::Word, Self::Dword];

    /// The token group's entry type of this kind.
    pub const fn entry_type(self) -> u16 {
        match self {
            Self::Bool => 0,
            Self::Byte => 1,
            Self::Word => 2,
            Self::Dword => 4,
        }
    }

    /// The kind of token group entry type `kind`.
    pub const fn of_entry(kind: u16) -> Option<Self> {
        match kind {
            0 => Some(Self::Bool),
            1 => Some(Self::Byte),
            2 => Some(Self::Word),
            4 => Some(Self::Dword),
            _ => None,
        }
    }

    /// The integer kind of `bytes` (1, 2 or 4).
    pub const fn of_size(bytes: u8) -> Option<Self> {
        match bytes {
            1 => Some(Self::Byte),
            2 => Some(Self::Word),
            4 => Some(Self::Dword),
            _ => None,
        }
    }

    /// The largest value of this kind.
    pub const fn max(self) -> u32 {
        match self {
            Self::Bool => 1,
            Self::Byte => 0xff,
            Self::Word => 0xffff,
            Self::Dword => u32::MAX,
        }
    }
}

/// A token's ID and value in an entry of its kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Token {
    pub kind: TokenKind,
    pub id: u32,
    pub value: u32,
}

impl Token {
    /// The token, if `value` fits `kind`.
    pub const fn new(kind: TokenKind, id: u32, value: u32) -> Result<Self, Error> {
        if value > kind.max() {
            return Err(Error::TokenValue { id, value });
        }
        Ok(Self { kind, id, value })
    }
}

/// What [`set_token`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Set {
    /// The APCB has no token entry of the token's kind.
    NoEntry,
    /// Every occurrence already had the value.
    Unchanged,
    /// Occurrences were rewritten.
    Updated,
    /// The token was added ([`add_token`]).
    Added,
}

/// Occurrences of a token in an APCB ([`update_token`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Occurrences {
    pub found: usize,
    pub changed: usize,
}

/// The kind of `entry` if it is a token entry.
pub fn kind_of(entry: &Entry) -> Option<TokenKind> {
    if entry.group != group::TOKEN {
        return None;
    }
    TokenKind::of_entry(entry.kind)
}

/// The rank of a priority mask: 0 for HardForce, 5 for Normal; lower ranks win.
pub const fn rank(priority_mask: u8) -> u32 {
    if priority_mask == 0 {
        u8::BITS
    } else {
        priority_mask.trailing_zeros()
    }
}

/// Call `f` with every token entry of `apcb` and its kind.
pub fn token_entries(apcb: &[u8], mut f: impl FnMut(&Entry, TokenKind)) -> Result<(), Error> {
    walk(apcb, |entry| {
        if let Some(kind) = kind_of(entry) {
            f(entry, kind);
        }
        false
    })?;
    Ok(())
}

/// The `(id, value)` pairs of token entry `entry` of `apcb`, in order.
pub fn entry_tokens<'a>(apcb: &'a [u8], entry: &Entry) -> impl Iterator<Item = (u32, u32)> + 'a {
    apcb[entry.data.clone()]
        .as_chunks::<TOKEN_SIZE>()
        .0
        .iter()
        .map(|t| {
            let (id, value) = t.split_at(4);
            (
                u32::from_le_bytes(id.try_into().unwrap_or_default()),
                u32::from_le_bytes(value.try_into().unwrap_or_default()),
            )
        })
}

/// Call `f` with every occurrence of token `id` in `apcb`.
pub fn token_occurrences(
    apcb: &[u8],
    id: u32,
    mut f: impl FnMut(&Entry, TokenKind, u32),
) -> Result<(), Error> {
    token_entries(apcb, |entry, kind| {
        for (_, value) in entry_tokens(apcb, entry).filter(|&(t, _)| t == id) {
            f(entry, kind, value);
        }
    })
}

/// The value of token `id` of `kind` that `apcb` gives the ABL: the occurrence
/// in the entry of the highest priority, the first of equals.
pub fn token(apcb: &[u8], kind: TokenKind, id: u32) -> Result<Option<u32>, Error> {
    let mut best: Option<(u32, u32)> = None;
    token_occurrences(apcb, id, |entry, k, value| {
        let r = rank(entry.priority_mask);
        if k == kind && best.is_none_or(|(b, _)| r < b) {
            best = Some((r, value));
        }
    })?;
    Ok(best.map(|(_, value)| value))
}

fn nth_entry(apcb: &[u8], kind: TokenKind, n: usize) -> Result<Option<Entry>, Error> {
    let mut seen = 0;
    walk(apcb, |entry| {
        if kind_of(entry) != Some(kind) {
            return false;
        }
        seen += 1;
        seen > n
    })
}

/// Write `token.value` into every occurrence of `token.id` in the entries
/// of `token.kind`, whatever their instance, board mask and priority.
pub fn update_token(apcb: &mut [u8], token: Token) -> Result<Occurrences, Error> {
    let mut occurrences = Occurrences::default();
    let mut n = 0;
    while let Some(entry) = nth_entry(apcb, token.kind, n)? {
        n += 1;
        for at in entry.data.clone().step_by(TOKEN_SIZE) {
            if at + TOKEN_SIZE > entry.data.end {
                break;
            }
            let pair = &mut apcb[at..at + TOKEN_SIZE];
            let (id, value) = pair.split_at_mut(4);
            if *id != token.id.to_le_bytes() {
                continue;
            }
            occurrences.found += 1;
            if *value != token.value.to_le_bytes() {
                occurrences.changed += 1;
                value.copy_from_slice(&token.value.to_le_bytes());
            }
        }
    }
    Ok(occurrences)
}

/// The all-boards (`0xffff`) entry of `kind` a new token goes into: highest
/// priority first, then first in container order. Board-masked-only entries
/// are never selected so a new token is never silently added for only one
/// board mask.
fn target_entry(apcb: &[u8], kind: TokenKind) -> Result<Option<Entry>, Error> {
    let mut best: Option<(u32, Entry)> = None;
    token_entries(apcb, |entry, k| {
        if k == kind && entry.board_mask == 0xffff {
            let key = rank(entry.priority_mask);
            if best.as_ref().is_none_or(|(b, _)| key < *b) {
                best = Some((key, entry.clone()));
            }
        }
    })?;
    Ok(best.map(|(_, entry)| entry))
}

fn group_of(apcb: &[u8], at: usize) -> Result<usize, Error> {
    let (_, mut group_at, size) = extent(apcb)?;
    while group_at + size_of::<GroupHeader>() <= size {
        let (group, _) = GroupHeader::ref_from_prefix(&apcb[group_at..size])
            .map_err(|_| Error::Truncated(group_at))?;
        let end = group_at + group.group_size.get() as usize;
        if end <= group_at {
            break;
        }
        if (group_at..end).contains(&at) {
            return Ok(group_at);
        }
        group_at = end;
    }
    Err(Error::Truncated(at))
}

/// Add `token` to the entry of its kind the ABL ranks first for every board
/// ([`target_entry`]), sorted by ID; if that entry has the token already, its
/// value is written. `Ok(false)` if `apcb` is v2 or has no token entry of the
/// kind.
pub fn add_token(apcb: &mut [u8], token: Token) -> Result<bool, Error> {
    let (ver, _, size) = extent(apcb)?;
    if ver != Version::V3 {
        return Ok(false);
    }
    let Some(entry) = target_entry(apcb, token.kind)? else {
        return Ok(false);
    };
    if entry.data.len() % TOKEN_SIZE != 0 {
        return Err(Error::EntrySize {
            expected: entry.data.len().next_multiple_of(TOKEN_SIZE),
            actual: entry.data.len(),
        });
    }
    let mut below = 0;
    let mut last = None;
    let mut present = false;
    for (id, _) in entry_tokens(apcb, &entry) {
        if last.is_some_and(|last| id < last) {
            return Err(Error::Unsorted(entry.data.start));
        }
        last = Some(id);
        present |= id == token.id;
        if id < token.id {
            below += 1;
        }
    }
    if present {
        for at in entry.data.clone().step_by(TOKEN_SIZE) {
            if apcb[at..at + 4] == token.id.to_le_bytes() {
                apcb[at + 4..at + TOKEN_SIZE].copy_from_slice(&token.value.to_le_bytes());
            }
        }
        return Ok(true);
    }

    let end = if apcb.len() >= size + HMAC_SIZE {
        size + HMAC_SIZE
    } else {
        size
    };
    let needed = end + TOKEN_SIZE;
    if apcb.len() < needed {
        return Err(Error::NoRoom { needed });
    }
    if apcb[end..needed].iter().any(|&b| b != 0xff) {
        return Err(Error::Trailing(end));
    }
    let header_at = entry.header_offset;
    let group_at = group_of(apcb, header_at)?;
    let grow = |value: usize| value.checked_add(TOKEN_SIZE).ok_or(Error::Header);
    let entry_size = u16::try_from(grow(entry.data.len() + size_of::<EntryHeader>())?)
        .map_err(|_| Error::TooMany(entry.data.len() / TOKEN_SIZE + 1))?;
    let (group, _) = GroupHeader::ref_from_prefix(&apcb[group_at..]).map_err(|_| Error::Header)?;
    let group_size = u32::try_from(grow(group.group_size.get() as usize)?)
        .map_err(|_| Error::Truncated(group_at))?;
    let apcb_size = u32::try_from(grow(size)?).map_err(|_| Error::Header)?;

    let at = entry.data.start + below * TOKEN_SIZE;
    apcb.copy_within(at..end, at + TOKEN_SIZE);
    apcb[at..at + 4].copy_from_slice(&token.id.to_le_bytes());
    apcb[at + 4..at + TOKEN_SIZE].copy_from_slice(&token.value.to_le_bytes());
    let (header, _) =
        EntryHeader::mut_from_prefix(&mut apcb[header_at..]).map_err(|_| Error::Header)?;
    header.entry_size.set(entry_size);
    let (group, _) =
        GroupHeader::mut_from_prefix(&mut apcb[group_at..]).map_err(|_| Error::Header)?;
    group.group_size.set(group_size);
    let (header, _) = Header::mut_from_prefix(apcb).map_err(|_| Error::Header)?;
    header.apcb_size.set(apcb_size);
    Ok(true)
}

/// Set token `token.id` of `apcb` to `token.value`: every occurrence in an
/// entry of its kind, or, if there is none, a new one ([`add_token`]).
pub fn set_token(apcb: &mut [u8], token: Token) -> Result<Set, Error> {
    let occurrences = update_token(apcb, token)?;
    Ok(if occurrences.changed > 0 {
        Set::Updated
    } else if occurrences.found > 0 {
        Set::Unchanged
    } else if add_token(apcb, token)? {
        Set::Added
    } else {
        Set::NoEntry
    })
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::super::{checksum_ok, commit, fix_checksum};
    use super::*;
    use std::vec::Vec;

    type RawEntry<'a> = (u16, u16, u8, u16, &'a [u8]);

    fn tokens(pairs: &[(u32, u32)]) -> Vec<u8> {
        pairs
            .iter()
            .flat_map(|(id, value)| id.to_le_bytes().into_iter().chain(value.to_le_bytes()))
            .collect()
    }

    fn apcb(groups: &[(&[u8; 4], u16, &[RawEntry])], len: usize) -> Vec<u8> {
        let mut out = b"APCB".to_vec();
        out.extend_from_slice(&0x80u16.to_le_bytes());
        out.extend_from_slice(&0x30u16.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&0x1000u32.to_le_bytes());
        out.resize(0x20, 0);
        out.extend_from_slice(b"ECB2");
        out.resize(0x80, 0);
        for (signature, id, entries) in groups {
            let at = out.len();
            out.extend_from_slice(*signature);
            out.extend_from_slice(&id.to_le_bytes());
            out.extend_from_slice(&16u16.to_le_bytes());
            out.extend_from_slice(&1u16.to_le_bytes());
            out.extend_from_slice(&[0; 6]);
            for &(kind, instance, priority, board_mask, data) in *entries {
                out.extend_from_slice(&id.to_le_bytes());
                out.extend_from_slice(&kind.to_le_bytes());
                out.extend_from_slice(&(16 + data.len() as u16).to_le_bytes());
                out.extend_from_slice(&instance.to_le_bytes());
                let token = *id == group::TOKEN;
                out.extend_from_slice(&[
                    if token { 2 } else { 0 },
                    u8::from(token),
                    if token { 8 } else { 0 },
                    priority,
                    if token { 4 } else { 0 },
                    0,
                ]);
                out.extend_from_slice(&board_mask.to_le_bytes());
                out.extend_from_slice(data);
                out.resize(out.len().next_multiple_of(4), 0);
            }
            let size = (out.len() - at) as u32;
            out[at + 12..at + 16].copy_from_slice(&size.to_le_bytes());
        }
        let size = out.len() as u32;
        out[8..12].copy_from_slice(&size.to_le_bytes());
        out.resize(len.max(out.len()), 0xff);
        fix_checksum(&mut out).unwrap();
        out
    }

    #[test]
    fn updates_and_adds_tokens_across_kinds() {
        let normal_b = tokens(&[(5, 1), (9, 0)]);
        let medium_d = tokens(&[(1, 10), (9, 90)]);
        let mut blob = apcb(
            &[(
                b"TOKN",
                group::TOKEN,
                &[
                    (0, 0, 0x20, 0xffff, &normal_b),
                    (4, 1, 0x04, 0xffff, &medium_d),
                ],
            )],
            0x400,
        );
        assert_eq!(token(&blob, TokenKind::Bool, 5), Ok(Some(1)));
        assert_eq!(
            set_token(&mut blob, Token::new(TokenKind::Bool, 5, 0).unwrap()),
            Ok(Set::Updated)
        );
        assert_eq!(token(&blob, TokenKind::Bool, 5), Ok(Some(0)));
        assert_eq!(
            set_token(&mut blob, Token::new(TokenKind::Dword, 7, 70).unwrap()),
            Ok(Set::Added)
        );
        commit(&mut blob).unwrap();
        assert!(checksum_ok(&blob).unwrap());
        assert_eq!(token(&blob, TokenKind::Dword, 7), Ok(Some(70)));
    }
}
