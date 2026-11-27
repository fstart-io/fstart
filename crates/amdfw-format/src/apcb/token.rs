//! The token entries of an APCB v3: the settings the ABL reads by a 32-bit
//! token ID (the `TOKN` group, `0x3000`).
//!
//! A token entry is a typed array of `{u32 id, u32 value}` sorted by ID
//! (not always unique: GIGABYTE's F01 `0x68/0` has `0x2854b676` twice,
//! with one value), one entry type per value width: 0 bool, 1 byte, 2 word, 4 dword
//! (amd-apcb `TokenEntryId`; the ABL asks for a token by ID and type).
//! The same token can be in several entries: per instance, per board mask
//! and per priority. The ABL takes the occurrence of the highest priority:
//! the priority mask's lowest set bit (HardForce `0x01`, High, Medium,
//! EventLogging, Low, Normal `0x20`).
//!
//! What the MD25-CG0 F01 APCBs carry (reports/research/VALUES.md §2.5;
//! both Venice donors have only dword entries): `0x68/0` the defaults
//! (instance 0, Normal, 337 tokens), `0x60/0` and its backup `0x68/8` the
//! setup's (instance 1, Medium, 110 tokens; instance 0, Low, empty).
//!
//! Adding a token keeps the entry sorted and grows the entry, its group
//! and the APCB by [`TOKEN_SIZE`] into erased (0xFF) room after the APCB
//! and its HMAC tail, which moves with it. Editing does not seal the APCB:
//! [`super::commit`] does (unique instance, checksum, HMAC tail).

use zerocopy::FromBytes;

use super::{Entry, EntryHeader, Error, GroupHeader, HMAC_SIZE, Header, extent, group, walk};

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

/// The rank of a priority mask: 0 for HardForce, 5 for Normal; lower
/// ranks win. An entry without a priority bit ranks last.
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

/// Call `f` with every occurrence of token `id` in `apcb`: the entry, its
/// kind and the value.
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

/// The value of token `id` of `kind` that `apcb` gives the ABL: the
/// occurrence in the entry of the highest priority, the first of equals.
/// Board masks are not told apart.
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

/// The `n`-th token entry of `kind`.
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

/// The entry of `kind` a new token goes into: the one the ABL ranks first
/// for every board (all-boards entries before board-masked ones, then the
/// highest priority, then the first).
fn target_entry(apcb: &[u8], kind: TokenKind) -> Result<Option<Entry>, Error> {
    let mut best: Option<((bool, u32), Entry)> = None;
    token_entries(apcb, |entry, k| {
        let key = (entry.board_mask != 0xffff, rank(entry.priority_mask));
        if k == kind && best.as_ref().is_none_or(|(b, _)| key < *b) {
            best = Some((key, entry.clone()));
        }
    })?;
    Ok(best.map(|(_, entry)| entry))
}

/// The offset of the group header that holds offset `at`.
fn group_of(apcb: &[u8], at: usize) -> Result<usize, Error> {
    let (mut group_at, size) = extent(apcb)?;
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

/// Add `token` to the entry of its kind the ABL ranks first for every
/// board ([`target_entry`]), sorted by ID; if that entry has the token
/// already, its value is written. `Ok(false)` if `apcb` has no token
/// entry of the kind.
///
/// The APCB grows by [`TOKEN_SIZE`]: the buffer needs that many erased
/// bytes after the APCB and its HMAC tail (if it holds one), else
/// [`Error::NoRoom`] says how long it must be (a caller with a growable
/// buffer extends it with 0xFF).
pub fn add_token(apcb: &mut [u8], token: Token) -> Result<bool, Error> {
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

    let (_, size) = extent(apcb)?;
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
    let header_at = entry.data.start - size_of::<EntryHeader>();
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

/// Set token `token.id` of `apcb` to `token.value`: every occurrence in
/// an entry of its kind, or, if there is none, a new one ([`add_token`]).
/// The edit is not sealed ([`super::commit`]).
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

    /// An entry: type, instance, priority mask, board mask, data.
    type RawEntry<'a> = (u16, u16, u8, u16, &'a [u8]);

    fn tokens(pairs: &[(u32, u32)]) -> Vec<u8> {
        pairs
            .iter()
            .flat_map(|(id, value)| id.to_le_bytes().into_iter().chain(value.to_le_bytes()))
            .collect()
    }

    /// An APCB v3 of `groups` (signature, ID, entries) in a `len`-byte
    /// erased buffer, checksummed.
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

    const DWORD: u16 = 4;

    /// Shaped like the MD25-CG0's `0x60/0`: the DF CXL array (here 8
    /// bytes), then the token group's empty Low entry and its Medium entry.
    fn setup_apcb(len: usize) -> Vec<u8> {
        let medium = tokens(&[(1, 10), (5, 50), (9, 90)]);
        apcb(
            &[
                (b"DFG ", group::DF, &[(0xcc, 0, 0x10, 0xffff, &[0; 8])]),
                (
                    b"TOKN",
                    group::TOKEN,
                    &[
                        (DWORD, 0, 0x10, 0xffff, &[]),
                        (DWORD, 1, 0x04, 0xffff, &medium),
                    ],
                ),
            ],
            len,
        )
    }

    fn all_tokens(apcb: &[u8]) -> Vec<(u16, u32, u32)> {
        let mut out = Vec::new();
        token_entries(apcb, |entry, _| {
            out.extend(entry_tokens(apcb, entry).map(|(id, v)| (entry.instance, id, v)));
        })
        .unwrap();
        out
    }

    fn dword(id: u32, value: u32) -> Token {
        Token::new(TokenKind::Dword, id, value).unwrap()
    }

    #[test]
    fn kinds_and_values() {
        assert_eq!(
            TokenKind::ALL.map(TokenKind::entry_type),
            [0, 1, 2, 4],
            "amd-apcb TokenEntryId"
        );
        for kind in TokenKind::ALL {
            assert_eq!(TokenKind::of_entry(kind.entry_type()), Some(kind));
        }
        assert_eq!(TokenKind::of_entry(3), None);
        assert_eq!(TokenKind::of_size(2), Some(TokenKind::Word));
        assert_eq!(TokenKind::of_size(8), None);
        assert_eq!(
            Token::new(TokenKind::Bool, 7, 2),
            Err(Error::TokenValue { id: 7, value: 2 })
        );
        assert!(Token::new(TokenKind::Byte, 7, 0xff).is_ok());
        assert!(Token::new(TokenKind::Word, 7, 0x1_0000).is_err());
        assert_eq!(rank(0x01), 0);
        assert!(rank(0x04) < rank(0x10) && rank(0x10) < rank(0x20) && rank(0x20) < rank(0));
    }

    #[test]
    fn reads_the_highest_priority_occurrence() {
        let normal = tokens(&[(5, 1), (6, 2)]);
        let medium = tokens(&[(5, 3)]);
        let apcb = apcb(
            &[(
                b"TOKN",
                group::TOKEN,
                &[
                    (DWORD, 0, 0x20, 0xffff, &normal),
                    (DWORD, 1, 0x04, 0xffff, &medium),
                ],
            )],
            0x400,
        );
        assert_eq!(token(&apcb, TokenKind::Dword, 5), Ok(Some(3)));
        assert_eq!(token(&apcb, TokenKind::Dword, 6), Ok(Some(2)));
        assert_eq!(token(&apcb, TokenKind::Dword, 7), Ok(None));
        assert_eq!(token(&apcb, TokenKind::Byte, 5), Ok(None));
        let mut kinds = Vec::new();
        token_occurrences(&apcb, 5, |e, k, v| kinds.push((e.instance, k, v))).unwrap();
        assert_eq!(kinds, [(0, TokenKind::Dword, 1), (1, TokenKind::Dword, 3)]);
    }

    #[test]
    fn updates_every_occurrence_in_place() {
        let normal = tokens(&[(5, 1), (6, 2)]);
        let medium = tokens(&[(5, 3)]);
        let mut apcb = apcb(
            &[(
                b"TOKN",
                group::TOKEN,
                &[
                    (DWORD, 0, 0x20, 0xffff, &normal),
                    (DWORD, 1, 0x04, 0xffff, &medium),
                ],
            )],
            0x400,
        );
        let before = apcb.clone();
        assert_eq!(set_token(&mut apcb, dword(5, 3)), Ok(Set::Updated));
        assert_eq!(all_tokens(&apcb), [(0, 5, 3), (0, 6, 2), (1, 5, 3)]);
        assert_eq!(apcb.len(), before.len());
        assert_eq!(apcb[8..12], before[8..12]);
        let edited = apcb.clone();
        assert_eq!(set_token(&mut apcb, dword(5, 3)), Ok(Set::Unchanged));
        assert_eq!(apcb, edited);
        assert_eq!(
            update_token(&mut apcb, dword(6, 9)),
            Ok(Occurrences {
                found: 1,
                changed: 1
            })
        );
        // Another kind's entries are not this token's.
        assert_eq!(
            set_token(&mut apcb, Token::new(TokenKind::Bool, 5, 1).unwrap()),
            Ok(Set::NoEntry)
        );
    }

    #[test]
    fn adds_a_token_sorted_into_the_first_ranked_entry() {
        let mut apcb = setup_apcb(0x1000);
        let size = |a: &[u8]| u32::from_le_bytes(a[8..12].try_into().unwrap()) as usize;
        let old_size = size(&apcb);
        // A signed tail moves with the APCB.
        apcb[old_size..old_size + HMAC_SIZE].fill(0x5a);
        let before = all_tokens(&apcb);
        assert_eq!(set_token(&mut apcb, dword(7, 70)), Ok(Set::Added));
        assert_eq!(size(&apcb), old_size + TOKEN_SIZE);
        assert_eq!(apcb.len(), 0x1000);
        // Into the Medium entry (instance 1), between 5 and 9.
        assert_eq!(
            all_tokens(&apcb),
            [(1, 1, 10), (1, 5, 50), (1, 7, 70), (1, 9, 90)]
        );
        assert_eq!(before.len() + 1, all_tokens(&apcb).len());
        let mut entries = Vec::new();
        walk(&apcb, |e| {
            entries.push((e.group, e.kind, e.instance, e.data.len()));
            false
        })
        .unwrap();
        assert_eq!(
            entries,
            [(0x1703, 0xcc, 0, 8), (0x3000, 4, 0, 0), (0x3000, 4, 1, 32)]
        );
        let new_size = size(&apcb);
        assert!(
            apcb[new_size..new_size + HMAC_SIZE]
                .iter()
                .all(|&b| b == 0x5a)
        );
        assert!(apcb[new_size + HMAC_SIZE..].iter().all(|&b| b == 0xff));
        commit(&mut apcb).unwrap();
        assert!(checksum_ok(&apcb).unwrap());
        assert!(apcb[new_size..].iter().all(|&b| b == 0xff));
        assert_eq!(token(&apcb, TokenKind::Dword, 7), Ok(Some(70)));
        // At either end.
        assert_eq!(set_token(&mut apcb, dword(0, 1)), Ok(Set::Added));
        assert_eq!(set_token(&mut apcb, dword(u32::MAX, 2)), Ok(Set::Added));
        let ids: Vec<u32> = all_tokens(&apcb).iter().map(|t| t.1).collect();
        assert_eq!(ids, [0, 1, 5, 7, 9, u32::MAX]);
    }

    #[test]
    fn adding_needs_erased_room() {
        // A buffer that ends at the APCB (MD25's `0x68/8`) must grow.
        let full = setup_apcb(0);
        let size = full.len();
        let mut apcb = full.clone();
        assert_eq!(
            set_token(&mut apcb, dword(7, 70)),
            Err(Error::NoRoom {
                needed: size + TOKEN_SIZE
            })
        );
        assert_eq!(apcb, full);
        apcb.resize(size + TOKEN_SIZE, 0xff);
        assert_eq!(set_token(&mut apcb, dword(7, 70)), Ok(Set::Added));
        // A buffer with the tail keeps it after the APCB.
        let mut apcb = setup_apcb(size + HMAC_SIZE);
        assert_eq!(
            add_token(&mut apcb, dword(7, 70)),
            Err(Error::NoRoom {
                needed: size + HMAC_SIZE + TOKEN_SIZE
            })
        );
        // Data after the APCB is not overwritten.
        let mut apcb = setup_apcb(size + HMAC_SIZE + 0x10);
        apcb[size + HMAC_SIZE + 2] = 0;
        assert_eq!(
            add_token(&mut apcb, dword(7, 70)),
            Err(Error::Trailing(size + HMAC_SIZE))
        );
    }

    #[test]
    fn prefers_all_board_entries_and_refuses_unsorted_ones() {
        let masked = tokens(&[(1, 1)]);
        let mut apcb = apcb(
            &[(
                b"TOKN",
                group::TOKEN,
                &[
                    (DWORD, 0, 0x01, 0x0002, &masked),
                    (DWORD, 0, 0x20, 0xffff, &[]),
                ],
            )],
            0x400,
        );
        assert_eq!(set_token(&mut apcb, dword(2, 2)), Ok(Set::Added));
        assert_eq!(all_tokens(&apcb), [(0, 1, 1), (0, 2, 2)]);
        let mut found = Vec::new();
        token_occurrences(&apcb, 2, |e, _, _| found.push(e.board_mask)).unwrap();
        assert_eq!(found, [0xffff]);

        let duplicate = tokens(&[(3, 1), (3, 1), (9, 1)]);
        let mut apcb = apcb_unsorted(&duplicate);
        assert_eq!(add_token(&mut apcb, dword(3, 4)), Ok(true));
        assert_eq!(all_tokens(&apcb), [(0, 3, 4), (0, 3, 4), (0, 9, 1)]);
        let unsorted = tokens(&[(9, 1), (3, 1)]);
        let mut apcb = apcb_unsorted(&unsorted);
        assert!(matches!(
            add_token(&mut apcb, dword(5, 0)),
            Err(Error::Unsorted(_))
        ));
    }

    fn apcb_unsorted(data: &[u8]) -> Vec<u8> {
        apcb(
            &[(b"TOKN", group::TOKEN, &[(DWORD, 0, 0x20, 0xffff, data)])],
            0x400,
        )
    }

    /// Every APCB v3 in the images `FSTART_APCB_DONORS` names
    /// (`:`-separated; the venice donors `firmware/gigabyte-md25-cg0/image.bin`
    /// and `firmware/dell-pr9xj/dell-rbu.bin`): its offset and bytes,
    /// with the HMAC tail.
    fn donor_apcbs() -> Vec<(std::string::String, usize, Vec<u8>)> {
        let Ok(paths) = std::env::var("FSTART_APCB_DONORS") else {
            std::eprintln!("FSTART_APCB_DONORS unset: donor APCBs not checked");
            return Vec::new();
        };
        let mut out = Vec::new();
        for path in paths.split(':').filter(|p| !p.is_empty()) {
            let image = std::fs::read(path).unwrap();
            let mut at = 0;
            while let Some(found) = image[at..].windows(4).position(|w| w == b"APCB") {
                let start = at + found;
                at = start + 4;
                let Ok((_, size)) = extent(&image[start..]) else {
                    continue;
                };
                let end = (start + size + HMAC_SIZE).min(image.len());
                out.push((path.into(), start, image[start..end].to_vec()));
            }
        }
        out
    }

    /// The donors' APCBs decode as VALUES.md §2.5 has them, and adding a
    /// token to each keeps every other token.
    #[test]
    fn donor_apcbs_take_tokens() {
        let apcbs = donor_apcbs();
        let mut gigabyte_ecam = Vec::new();
        // Ten per donor: five per ISH slot (VALUES.md §2.5).
        let mut paths: Vec<&str> = apcbs.iter().map(|(p, ..)| p.as_str()).collect();
        paths.dedup();
        for path in paths {
            assert_eq!(apcbs.iter().filter(|a| a.0 == path).count(), 10, "{path}");
        }
        for (path, at, original) in &apcbs {
            let what = std::format!("{path}@{at:#x}");
            assert!(checksum_ok(original).unwrap(), "{what}");
            let before = all_tokens(original);
            let mut has_entry = false;
            token_entries(original, |e, kind| {
                has_entry |= kind == TokenKind::Dword;
                let ids: Vec<u32> = entry_tokens(original, e).map(|t| t.0).collect();
                // Sorted, not unique: GIGABYTE's `0x68/0` has
                // `0x2854b676` twice (both 1).
                assert!(ids.windows(2).all(|w| w[0] <= w[1]), "{what} sorted");
                assert_eq!(
                    kind,
                    TokenKind::Dword,
                    "{what}: Venice has dword entries only"
                );
            })
            .unwrap();
            if path.contains("gigabyte")
                && let Ok(Some(lo)) = token(original, TokenKind::Dword, 0x7a4f_a9ed)
            {
                gigabyte_ecam.push((
                    lo,
                    token(original, TokenKind::Dword, 0xf6b9_15bf).unwrap(),
                    token(original, TokenKind::Dword, 0x3d9b_7d7b).unwrap(),
                ));
            }
            // The donor's own values (where its occurrences agree; board
            // masks and priorities can differ): nothing changes.
            let mut apcb = original.clone();
            for &(_, id, value) in &before {
                let mut agree = true;
                token_occurrences(original, id, |_, _, v| agree &= v == value).unwrap();
                if agree {
                    assert_eq!(set_token(&mut apcb, dword(id, value)), Ok(Set::Unchanged));
                }
            }
            assert_eq!(&apcb, original, "{what}");
            // A new token: added once, the others kept.
            let id = 0x0bad_f00d;
            assert!(before.iter().all(|t| t.1 != id));
            apcb.resize(apcb.len() + TOKEN_SIZE, 0xff);
            let set = set_token(&mut apcb, dword(id, 0x1234)).unwrap();
            assert_eq!(
                set,
                if has_entry { Set::Added } else { Set::NoEntry },
                "{what}"
            );
            if has_entry {
                commit(&mut apcb).unwrap();
                assert!(checksum_ok(&apcb).unwrap());
                let mut after = all_tokens(&apcb);
                let added = after.iter().position(|t| t.1 == id).unwrap();
                after.remove(added);
                assert_eq!(after, before, "{what}");
                assert_eq!(token(&apcb, TokenKind::Dword, id), Ok(Some(0x1234)));
            }
        }
        if apcbs.iter().any(|(path, ..)| path.contains("gigabyte")) {
            // 2P_COMPLETE §2.2: `0x2FFC_8000_0000`, one segment, in slot A
            // and B.
            assert_eq!(
                gigabyte_ecam,
                [(0x8000_0000, Some(0x2ffc), Some(0x1000_0000)); 2]
            );
        }
    }
}
