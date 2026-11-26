// SPDX-License-Identifier: MIT OR Apache-2.0
//! A board's APCB settings, written into the APCBs of its image at build
//! time: `[package.metadata.fstart.amd] apcb-tokens` (ABL tokens) and
//! `cxl-regions` (the DF CXL region array). See docs/amd-apcb.md.
//!
//! A token is written into every APCB v3 (BIOS entries 0x60/0x68) that
//! carries it, in every entry of its kind (all instances, board masks and
//! priorities, so no copy the ABL might prefer keeps another value). A
//! token no APCB carries is added to every APCB with a token entry of its
//! kind, in the entry the ABL ranks first
//! ([`fstart_amdfw_format::apcb::token::add_token`]); an APCB file that
//! has no room left grows. A token that lands nowhere fails the build, and
//! so does a fusing token ([`apcb::names::IN_PLACE_ONLY`], such as
//! `PspPsbAutoFuse`) that no APCB carries: those are only updated in
//! place, never added with a kind the ABL may not read. After writing,
//! every APCB that carries a token reads the board's value. An
//! APCB whose bytes changed is sealed (unique instance + 1, checksum, HMAC
//! tail erased); one whose bytes did not is left as the board's file has
//! it, so a board that names its donor's values packs the donor's APCBs.
use fstart_amdfw_format::apcb::{
    self, CxlRegion,
    token::{self, Token, TokenKind},
};
use serde::{Deserialize, Serialize};

use super::Key;

/// One token as a board's table writes it: `{ id = 0x..., value = ... }`
/// or `{ name = "DfPciMmioSize", value = ... }`, with an optional `size`
/// (1, 2, 4 or `"bool"`) when the APCBs do not already say.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct BoardToken {
    /// The token ID.
    #[serde(default)]
    pub id: Option<u32>,
    /// A name of [`apcb::names::NAMES`] (or an alias); with `id`, both
    /// must name the same token.
    #[serde(default)]
    pub name: Option<String>,
    pub value: TokenValue,
    /// The kind of entry the token is in: its value's width in bytes, or
    /// `"bool"`. Without it, the kind of the entries that carry the token,
    /// else (a named token no APCB carries) dword, as both Venice donors
    /// carry every named token.
    #[serde(default)]
    pub size: Option<TokenSize>,
}

/// A token's value: an integer or a boolean (1/0).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum TokenValue {
    Bool(bool),
    Int(u32),
}

impl TokenValue {
    pub fn get(self) -> u32 {
        match self {
            Self::Bool(b) => u32::from(b),
            Self::Int(v) => v,
        }
    }
}

/// `size = 1 | 2 | 4 | "bool"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum TokenSize {
    Bytes(u8),
    Bool(BoolSize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BoolSize {
    #[serde(rename = "bool")]
    Bool,
}

impl TokenSize {
    fn kind(self) -> Option<TokenKind> {
        match self {
            Self::Bytes(n) => TokenKind::of_size(n),
            Self::Bool(_) => Some(TokenKind::Bool),
        }
    }
}

/// A token of the build plan: its ID, value and, if the board gave it,
/// kind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ApcbToken {
    pub id: u32,
    pub value: u32,
    #[serde(default)]
    pub kind: Option<TokenKind>,
}

impl ApcbToken {
    /// `Name (0x...)` or `0x...` for messages.
    pub fn label(&self) -> String {
        match apcb::names::name(self.id) {
            Some(name) => format!("{name} ({:#010x})", self.id),
            None => format!("{:#010x}", self.id),
        }
    }
}

/// The board's tokens resolved: names to IDs, sizes to kinds, booleans
/// to 1/0. A token named twice is an error.
pub fn resolve(tokens: &[BoardToken]) -> Result<Vec<ApcbToken>, String> {
    let mut out: Vec<ApcbToken> = Vec::new();
    for token in tokens {
        let by_name = match &token.name {
            Some(name) => Some(
                apcb::names::id(name)
                    .ok_or_else(|| format!("APCB token {name:?}: no such token name"))?,
            ),
            None => None,
        };
        let id = match (token.id, by_name) {
            (Some(id), Some(named)) if id != named => {
                return Err(format!(
                    "APCB token {:?} is {named:#010x}, not {id:#010x}",
                    token.name.as_deref().unwrap_or_default()
                ));
            }
            (Some(id), _) | (None, Some(id)) => id,
            (None, None) => return Err("APCB token without an id or a name".into()),
        };
        let kind = match token.size {
            Some(size) => Some(
                size.kind()
                    .ok_or_else(|| format!("APCB token {id:#010x}: size {size:?}"))?,
            ),
            None => None,
        };
        let resolved = ApcbToken {
            id,
            value: token.value.get(),
            kind,
        };
        if let Some(kind) = kind {
            Token::new(kind, id, resolved.value)
                .map_err(|_| format!("APCB token {}: value too wide", resolved.label()))?;
        }
        if out.iter().any(|t| t.id == id) {
            return Err(format!("APCB token {} set twice", resolved.label()));
        }
        out.push(resolved);
    }
    Ok(out)
}

/// Whether `key` is an APCB (BIOS entry 0x60 or its backup 0x68).
pub fn is_apcb(key: Key) -> bool {
    use fstart_amdfw_format::directory::bios;
    matches!(key.kind, bios::APCB | bios::APCB_BACKUP)
}

/// Write `regions` and `tokens` into `apcbs`, every APCB of an image (each
/// a file's bytes, which may grow); see the module documentation.
pub fn apply(
    apcbs: &mut [(Key, &mut Vec<u8>)],
    regions: &[CxlRegion],
    tokens: &[ApcbToken],
) -> Result<(), String> {
    if regions.is_empty() && tokens.is_empty() {
        return Ok(());
    }
    let originals: Vec<Vec<u8>> = apcbs.iter().map(|(_, bytes)| bytes.to_vec()).collect();
    if !regions.is_empty() {
        let array = apcb::cxl_array_of(regions).map_err(|e| format!("CXL regions: {e:?}"))?;
        let mut written = 0;
        for (key, bytes) in apcbs.iter_mut() {
            written += usize::from(
                apcb::write_cxl_array(bytes, &array)
                    .map_err(|e| format!("APCB {key:?}: CXL regions: {e:?}"))?,
            );
        }
        if written == 0 {
            return Err(
                "CXL regions requested, but no APCB has a DF CXL region array (0xCC)".into(),
            );
        }
    }
    for wanted in tokens {
        set(apcbs, wanted)?;
    }
    for ((key, bytes), original) in apcbs.iter_mut().zip(&originals) {
        if **bytes != *original {
            apcb::commit(bytes).map_err(|e| format!("APCB {key:?}: {e:?}"))?;
        }
    }
    Ok(())
}

/// Set one token in `apcbs`.
fn set(apcbs: &mut [(Key, &mut Vec<u8>)], wanted: &ApcbToken) -> Result<(), String> {
    let label = wanted.label();
    let mut kinds = Vec::new();
    for (key, bytes) in apcbs.iter() {
        token::token_occurrences(bytes, wanted.id, |_, kind, _| {
            if !kinds.contains(&kind) {
                kinds.push(kind);
            }
        })
        .map_err(|e| format!("APCB {key:?}: {e:?}"))?;
    }
    let kind = match (wanted.kind, kinds.as_slice()) {
        (Some(kind), []) => kind,
        (Some(kind), found) if found.contains(&kind) => kind,
        (Some(kind), found) => {
            return Err(format!(
                "APCB token {label}: the APCBs carry it as {found:?}, not {kind:?}"
            ));
        }
        (None, [kind]) => *kind,
        (None, []) if apcb::names::name(wanted.id).is_some() => TokenKind::Dword,
        (None, []) => {
            return Err(format!(
                "APCB token {label}: no APCB carries it; give its size"
            ));
        }
        (None, found) => {
            return Err(format!(
                "APCB token {label}: the APCBs carry it as {found:?}; give its size"
            ));
        }
    };
    let token = Token::new(kind, wanted.id, wanted.value)
        .map_err(|_| format!("APCB token {label}: {:#x} is not a {kind:?}", wanted.value))?;
    let present = kinds.contains(&kind);
    if !present && apcb::names::in_place_only(wanted.id) {
        return Err(format!(
            "APCB token {label}: no APCB carries it, and a fusing token is only updated in \
             place, never added"
        ));
    }
    let mut applied = 0;
    for (key, bytes) in apcbs.iter_mut() {
        let error = |e| format!("APCB {key:?}: token {label}: {e:?}");
        if present {
            let found = token::update_token(bytes, token).map_err(error)?.found;
            applied += usize::from(found > 0);
            continue;
        }
        let added = loop {
            match token::add_token(bytes, token) {
                Err(apcb::Error::NoRoom { needed }) if needed > bytes.len() => {
                    bytes.resize(needed, 0xff);
                }
                result => break result.map_err(error)?,
            }
        };
        applied += usize::from(added);
    }
    if applied == 0 {
        return Err(format!(
            "APCB token {label}: no APCB has a {kind:?} token entry"
        ));
    }
    // What the ABL will read: the board's value wherever the token is.
    for (key, bytes) in apcbs.iter() {
        let read = token::token(bytes, kind, wanted.id)
            .map_err(|e| format!("APCB {key:?}: token {label}: {e:?}"))?;
        if read.is_some_and(|value| value != wanted.value) {
            return Err(format!(
                "APCB {key:?}: token {label} reads {read:#x?} after writing {:#x}",
                wanted.value
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use fstart_amdfw_format::directory::bios;

    fn board(value: serde_json::Value) -> Result<Vec<ApcbToken>, String> {
        let tokens: Vec<BoardToken> = serde_json::from_value(value).map_err(|e| e.to_string())?;
        resolve(&tokens)
    }

    #[test]
    fn board_tokens_resolve_by_id_or_name() {
        let tokens = board(serde_json::json!([
            { "name": "DfPciMmioBaseLo", "value": 0x8000_0000u32 },
            { "name": "CbsDfDbgNumPciSegments", "id": 0x3d9b_7d7b, "value": 0x1000_0000 },
            { "id": 0x1234_5678, "value": true, "size": "bool" },
            { "id": 0x1234_5679, "value": 0xff, "size": 1 },
        ]))
        .unwrap();
        assert_eq!(
            tokens,
            [
                ApcbToken {
                    id: 0x7a4f_a9ed,
                    value: 0x8000_0000,
                    kind: None
                },
                ApcbToken {
                    id: 0x3d9b_7d7b,
                    value: 0x1000_0000,
                    kind: None
                },
                ApcbToken {
                    id: 0x1234_5678,
                    value: 1,
                    kind: Some(TokenKind::Bool)
                },
                ApcbToken {
                    id: 0x1234_5679,
                    value: 0xff,
                    kind: Some(TokenKind::Byte)
                },
            ]
        );
        assert_eq!(tokens[1].label(), "DfPciMmioSize (0x3d9b7d7b)");
        for bad in [
            serde_json::json!([{ "name": "Nope", "value": 1 }]),
            serde_json::json!([{ "name": "DfPciMmioSize", "id": 1, "value": 1 }]),
            serde_json::json!([{ "value": 1 }]),
            serde_json::json!([{ "id": 1, "value": 2, "size": "bool" }]),
            serde_json::json!([{ "id": 1, "value": 0x100, "size": 1 }]),
            serde_json::json!([{ "id": 1, "value": 1, "size": 3 }]),
            serde_json::json!([{ "id": 1, "value": -1 }]),
            serde_json::json!([{ "id": 1, "value": 1, "width": 4 }]),
            serde_json::json!([
                { "id": 0x3d9b_7d7b, "value": 1 },
                { "name": "DfPciMmioSize", "value": 1 }
            ]),
        ] {
            assert!(board(bad.clone()).is_err(), "{bad}");
        }
    }

    /// A token entry: type, priority mask, `(id, value)`s.
    type RawEntry<'a> = (u16, u8, &'a [(u32, u32)]);

    /// An APCB v3 with a token group of `entries`; `room` erased bytes
    /// after it.
    fn token_apcb(entries: &[RawEntry], room: usize) -> Vec<u8> {
        let mut apcb = b"APCB".to_vec();
        apcb.extend_from_slice(&0x80u16.to_le_bytes());
        apcb.extend_from_slice(&0x30u16.to_le_bytes());
        apcb.resize(0x80, 0);
        let group = apcb.len();
        apcb.extend_from_slice(b"TOKN");
        apcb.extend_from_slice(&0x3000u16.to_le_bytes());
        apcb.extend_from_slice(&16u16.to_le_bytes());
        apcb.extend_from_slice(&1u16.to_le_bytes());
        apcb.extend_from_slice(&[0; 6]);
        for (instance, &(kind, priority, tokens)) in entries.iter().enumerate() {
            apcb.extend_from_slice(&0x3000u16.to_le_bytes());
            apcb.extend_from_slice(&kind.to_le_bytes());
            apcb.extend_from_slice(&(16 + 8 * tokens.len() as u16).to_le_bytes());
            apcb.extend_from_slice(&(instance as u16).to_le_bytes());
            apcb.extend_from_slice(&[2, 1, 8, priority, 4, 0, 0xff, 0xff]);
            for (id, value) in tokens {
                apcb.extend_from_slice(&id.to_le_bytes());
                apcb.extend_from_slice(&value.to_le_bytes());
            }
        }
        let size = (apcb.len() - group) as u32;
        apcb[group + 12..group + 16].copy_from_slice(&size.to_le_bytes());
        let size = apcb.len() as u32;
        apcb[8..12].copy_from_slice(&size.to_le_bytes());
        apcb.resize(apcb.len() + room, 0xff);
        apcb::fix_checksum(&mut apcb).unwrap();
        apcb
    }

    fn apply_to(files: &mut [Vec<u8>], tokens: &[ApcbToken]) -> Result<(), String> {
        let mut apcbs: Vec<(Key, &mut Vec<u8>)> = files
            .iter_mut()
            .enumerate()
            .map(|(i, bytes)| (Key::new(bios::APCB_BACKUP, 0, i as u8), bytes))
            .collect();
        apply(&mut apcbs, &[], tokens)
    }

    fn dword(apcb: &[u8], id: u32) -> Option<u32> {
        token::token(apcb, TokenKind::Dword, id).unwrap()
    }

    /// MD25-CG0's set: the defaults (`0x68/0`, Normal), the setup's
    /// (`0x60/0` in a 4 KiB entry, Medium), its backup (`0x68/8`, no
    /// room), and an APCB without tokens.
    fn md25_like() -> Vec<Vec<u8>> {
        let defaults: &[(u32, u32)] = &[(0x3d9b_7d7b, 0x1000_0000), (0x7a4f_a9ed, 0x8000_0000)];
        let setup: &[(u32, u32)] = &[(0x0000_0010, 1)];
        let mut memg = token_apcb(&[], 0);
        memg[0x80..0x84].copy_from_slice(b"MEMG");
        memg[0x84..0x86].copy_from_slice(&0x1704u16.to_le_bytes());
        apcb::fix_checksum(&mut memg).unwrap();
        vec![
            token_apcb(&[(4, 0x20, defaults)], 0x100),
            token_apcb(&[(4, 0x10, &[]), (4, 0x04, setup)], 0x400),
            token_apcb(&[(4, 0x10, &[]), (4, 0x04, setup)], 0),
            memg,
        ]
    }

    #[test]
    fn the_donors_values_leave_the_apcbs_alone() {
        let mut files = md25_like();
        let before = files.clone();
        let tokens = [
            ApcbToken {
                id: 0x3d9b_7d7b,
                value: 0x1000_0000,
                kind: None,
            },
            ApcbToken {
                id: 0x7a4f_a9ed,
                value: 0x8000_0000,
                kind: Some(TokenKind::Dword),
            },
        ];
        apply_to(&mut files, &tokens).unwrap();
        assert_eq!(files, before);
    }

    #[test]
    fn tokens_are_rewritten_where_they_are_and_added_where_none_is() {
        let mut files = md25_like();
        let before = files.clone();
        // Present in the defaults only: rewritten there, not added to the
        // setup's.
        let size = ApcbToken {
            id: 0x3d9b_7d7b,
            value: 0x2000_0000,
            kind: None,
        };
        // In no APCB: added to each with a dword entry, the setup's into its
        // Medium entry; the backup file grows (MemSmeMkEnable, named).
        let added = ApcbToken {
            id: 0x07a5_db75,
            value: 0,
            kind: None,
        };
        apply_to(&mut files, &[size.clone(), added.clone()]).unwrap();
        assert_eq!(dword(&files[0], size.id), Some(0x2000_0000));
        assert_eq!(dword(&files[1], size.id), None);
        for file in &files[..3] {
            assert_eq!(dword(file, added.id), Some(0));
            assert!(apcb::checksum_ok(file).unwrap());
        }
        let mut medium = Vec::new();
        token::token_occurrences(&files[1], added.id, |e, _, _| medium.push(e.priority_mask))
            .unwrap();
        assert_eq!(medium, [0x04]);
        assert_eq!(files[1].len(), before[1].len());
        assert_eq!(files[2].len(), before[2].len() + token::TOKEN_SIZE);
        assert_eq!(files[3], before[3]);
        // Sealed: unique instance + 1.
        assert_eq!(files[0][0x0c], before[0][0x0c] + 1);

        // A token of another kind than the APCBs carry it as, one that
        // lands nowhere, and an unnamed token without a size fail.
        let mut files = md25_like();
        for wrong in [
            ApcbToken {
                id: 0x3d9b_7d7b,
                value: 1,
                kind: Some(TokenKind::Byte),
            },
            ApcbToken {
                id: 0x1111_1111,
                value: 1,
                kind: Some(TokenKind::Word),
            },
            ApcbToken {
                id: 0x1111_1111,
                value: 1,
                kind: None,
            },
        ] {
            assert!(apply_to(&mut files, &[wrong]).is_err());
        }
        // Without a token group at all.
        let mut memg = vec![md25_like().remove(3)];
        assert!(apply_to(&mut memg, &[added]).is_err());
    }

    #[test]
    fn fusing_tokens_are_only_updated_in_place() {
        let psb = |value| ApcbToken {
            id: 0x2fcd_70c9,
            value,
            kind: None,
        };
        // Absent from every APCB: not added (with any kind), the build fails.
        let mut files = md25_like();
        let before = files.clone();
        for kind in [None, Some(TokenKind::Dword), Some(TokenKind::Bool)] {
            let token = ApcbToken { kind, ..psb(0) };
            let error = apply_to(&mut files, &[token]).unwrap_err();
            assert!(error.contains("only updated in place"), "{error}");
        }
        assert_eq!(files, before);
        // Present in the defaults (as F01 has it): rewritten there only.
        let defaults: &[(u32, u32)] = &[(0x2fcd_70c9, 1)];
        let mut files = vec![
            token_apcb(&[(4, 0x20, defaults)], 0),
            token_apcb(&[(4, 0x04, &[(0x10, 1)])], 0x100),
        ];
        let setup = files[1].clone();
        apply_to(&mut files, &[psb(0)]).unwrap();
        assert_eq!(dword(&files[0], 0x2fcd_70c9), Some(0));
        assert_eq!(files[1], setup);
    }

    /// `[package.metadata.fstart]` of a workspace Cargo.toml.
    /// `None` when the tree has no such manifest (a board not yet added).
    fn fstart_table(manifest: &str) -> Option<serde_json::Value> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(manifest);
        let text = std::fs::read_to_string(&path).ok()?;
        let value: toml::Value = toml::from_str(&text).unwrap();
        Some(serde_json::to_value(&value["package"]["metadata"]["fstart"]).unwrap())
    }

    /// MD25-CG0's tokens resolve; on its F01 APCBs (`FSTART_APCB_DONORS`
    /// naming `gigabyte-md25-cg0/image.bin`, as extracted for `--blobs`)
    /// they change nothing, and another ECAM and a new token change what
    /// they should.
    #[test]
    fn md25_tokens_on_its_donor_apcbs() {
        use super::super::blobs::{self, BoardBlobs, SocBlobs};
        let Some(manifest) = fstart_table("../../boards/gigabyte/md25-cg0/Cargo.toml") else {
            return;
        };
        let board: BoardBlobs = serde_json::from_value(manifest["amd"].clone()).unwrap();
        let tokens = resolve(&board.apcb_tokens).unwrap();
        assert_eq!(tokens.len(), 11);
        assert!(tokens.iter().all(|t| t.kind.is_none()));
        assert_eq!(
            tokens.iter().find(|t| t.id == 0x3d9b_7d7b).map(|t| t.value),
            Some(0x1000_0000)
        );

        let Some(image) = std::env::var("FSTART_APCB_DONORS").ok().and_then(|v| {
            v.split(':')
                .find(|p| p.contains("gigabyte-md25-cg0"))
                .map(String::from)
        }) else {
            eprintln!("FSTART_APCB_DONORS names no MD25-CG0 image: donor APCBs not checked");
            return;
        };
        let soc: SocBlobs = serde_json::from_value(
            fstart_table("../platform-amd/Cargo.toml").unwrap()["amd"]["venice"].clone(),
        )
        .unwrap();
        let rom = std::fs::read(image).unwrap();
        let (found, _) = blobs::extract(&soc, &board, "gigabyte-md25-cg0", &rom).unwrap();
        let mut files: Vec<(Key, Vec<u8>)> = found
            .into_iter()
            .filter_map(|f| {
                let name = f
                    .file
                    .strip_prefix("md25-cg0/apcb-")?
                    .strip_suffix(".bin")?;
                let (kind, instance) = name.split_once('-')?;
                let kind = u8::from_str_radix(kind, 16).ok()?;
                Some((Key::new(kind, 0, instance.parse().ok()?), f.bytes))
            })
            .collect();
        assert_eq!(files.len(), 5);
        let originals = files.clone();
        let run = |files: &mut Vec<(Key, Vec<u8>)>, tokens: &[ApcbToken]| {
            let mut apcbs: Vec<(Key, &mut Vec<u8>)> =
                files.iter_mut().map(|(k, b)| (*k, b)).collect();
            apply(&mut apcbs, &[], tokens)
        };
        // Every token but PspPsbAutoFuse keeps F01's value; that one the
        // board turns off, in the APCBs that carry it.
        const PSB_AUTO_FUSE: u32 = 0x2fcd_70c9;
        let donor: Vec<ApcbToken> = tokens
            .iter()
            .filter(|t| t.id != PSB_AUTO_FUSE)
            .cloned()
            .collect();
        run(&mut files, &donor).unwrap();
        assert_eq!(files, originals);
        run(&mut files, &tokens).unwrap();
        let changed: Vec<Key> = files
            .iter()
            .zip(&originals)
            .filter(|(a, b)| a.1 != b.1)
            .map(|(a, _)| a.0)
            .collect();
        assert_eq!(changed, [Key::new(0x68, 0, 0)]);
        files = originals.clone();

        // Dell's ECAM (`0x1000_0000_0000`, two segments) and PSB auto-fuse
        // off (GIGABYTE 1, in `0x68/0` only), and a token none carries.
        let dell = [
            (0x7a4f_a9ed, 0),
            (0xf6b9_15bf, 0x1000),
            (0x3d9b_7d7b, 0x2000_0000),
        ];
        let mut other: Vec<ApcbToken> = dell
            .iter()
            .map(|&(id, value)| ApcbToken {
                id,
                value,
                kind: None,
            })
            .collect();
        other.push(ApcbToken {
            id: 0x2fcd_70c9,
            value: 0,
            kind: None,
        });
        other.push(ApcbToken {
            id: 0x0bad_f00d,
            value: 7,
            kind: Some(TokenKind::Dword),
        });
        run(&mut files, &other).unwrap();
        for ((key, bytes), (_, original)) in files.iter().zip(&originals) {
            let changed = bytes != original;
            let has_tokens = {
                let mut any = false;
                token::token_entries(original, |_, _| any = true).unwrap();
                any
            };
            assert_eq!(changed, has_tokens, "{key:?}");
            if !has_tokens {
                continue;
            }
            assert!(apcb::checksum_ok(bytes).unwrap());
            assert_eq!(dword(bytes, 0x0bad_f00d), Some(7), "{key:?}");
            let defaults =
                key.kind == fstart_amdfw_format::directory::bios::APCB_BACKUP && key.instance == 0;
            for &(id, value) in &dell {
                // Only the defaults carry them: not added elsewhere.
                assert_eq!(dword(bytes, id), defaults.then_some(value), "{key:?}");
            }
            assert_eq!(dword(bytes, 0x2fcd_70c9), defaults.then_some(0), "{key:?}");
        }
    }
}
