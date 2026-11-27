// SPDX-License-Identifier: MIT OR Apache-2.0
//! Unified APCB v2 and v3 overlay and patching for all AMD platforms.
//!
//! - **v2 (Picasso, Zeppelin)**: walker-based GPIO RAM-ID (`PSP / 0x60`) and
//!   SPD (`MEMORY / 0x30`) editing ([`inject`], [`ApcbOverlay::v2_gpio_id`]).
//!   Works on both sentinel templates and real donor APCBs.
//! - **v3 (Rome, Milan, Genoa, Siena, Venice)**: token overlays
//!   ([`ApcbOverlay::tokens`]) and DF CXL region descriptors
//!   ([`ApcbOverlay::cxl_regions`]).
//!
//! Safety and reproducibility rules:
//! - An empty overlay (or one whose values match the base APCBs) leaves every
//!   APCB byte-identical.
//! - A token present in the base APCBs is updated in every occurrence across
//!   all instances, board masks, and priorities (so no board-masked or
//!   higher-priority entry shadows the edit).
//! - A token absent from every base APCB requires an explicit `kind` (never
//!   silently assumed to be `Dword`, which would be ignored on Rome/Milan/
//!   Genoa/Siena where bool/byte/word entries exist) and is added to the
//!   override layer (`0x60/0` and its backup `0x68/8`) when present, else any
//!   APCB carrying a token entry of that kind.
//! - Fusing tokens ([`fstart_amdfw_format::apcb::names::IN_PLACE_ONLY`], such
//!   as `PspPsbAutoFuse` and `PspOpnPlatformComptFusing`) can only be updated
//!   in place to `0`, never added and never set nonzero by an overlay.
//! - Every APCB whose bytes changed is sealed via
//!   [`fstart_amdfw_format::apcb::commit`] (`unique_instance + 1`, checksum,
//!   and v3 HMAC tail erased to `0xFF`) and verified.

use fstart_amdfw_format::apcb::{
    self, CxlRegion,
    token::{self, Token, TokenKind},
    v2,
};
use fstart_amdfw_format::directory::bios;
use serde::{Deserialize, Serialize};

use super::{Key, Result};

/// One token as a board's `Cargo.toml` metadata writes it:
/// `{ id = 0x..., value = ... }` or `{ name = "FchConsoleOutMode", value = ... }`,
/// with an optional `size` (`1`, `2`, `4`, or `"bool"`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct BoardToken {
    #[serde(default)]
    pub id: Option<u32>,
    #[serde(default)]
    pub name: Option<String>,
    pub value: TokenValue,
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
    pub const fn get(self) -> u32 {
        match self {
            Self::Bool(b) => b as u32,
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
    pub const fn kind(self) -> Option<TokenKind> {
        match self {
            Self::Bytes(n) => TokenKind::of_size(n),
            Self::Bool(_) => Some(TokenKind::Bool),
        }
    }
}

/// A resolved APCB v3 token override carried in the build plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ApcbToken {
    pub id: u32,
    pub value: u32,
    #[serde(default)]
    pub kind: Option<TokenKind>,
}

impl ApcbToken {
    /// `Name (0x...)` or `0x...` for diagnostics.
    pub fn label(&self) -> String {
        match apcb::names::name(self.id) {
            Some(name) => format!("{name} ({:#010x})", self.id),
            None => format!("{:#010x}", self.id),
        }
    }
}

/// Unified APCB overlay carried in [`super::assembly::PspFiles`] and applied
/// to every APCB (`0x60`/`0x68`) of a packed image.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ApcbOverlay {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tokens: Vec<ApcbToken>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cxl_regions: Vec<CxlRegion>,
    /// Optional v2 GPIO RAM-ID triples (`PSP / 0x60`, four `(gpio, iomux, bank)`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub v2_gpio_id: Option<[[u8; 3]; 4]>,
}

impl ApcbOverlay {
    pub const fn new() -> Self {
        Self {
            tokens: Vec::new(),
            cxl_regions: Vec::new(),
            v2_gpio_id: None,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.tokens.is_empty() && self.cxl_regions.is_empty() && self.v2_gpio_id.is_none()
    }

    /// Add or replace a raw token override by ID.
    pub fn with_token(mut self, id: u32, value: u32, kind: Option<TokenKind>) -> Self {
        self.set_token(ApcbToken { id, value, kind });
        self
    }

    /// Add or replace a token override by canonical name or alias.
    pub fn with_named_token(
        mut self,
        name: &str,
        value: u32,
        kind: Option<TokenKind>,
    ) -> Result<Self> {
        let id = apcb::names::id(name)
            .ok_or_else(|| format!("APCB token {name:?}: no such token name"))?;
        self.set_token(ApcbToken { id, value, kind });
        Ok(self)
    }

    /// Disable `PspPsbAutoFuse` in place where present.
    pub fn disable_psb_auto_fuse(self) -> Self {
        self.with_token(0x2fcd_70c9, 0, None)
    }

    /// Enable or disable ABL console output (`FchConsoleOutMode`).
    pub fn abl_console_out(self, enable: bool) -> Self {
        self.with_token(0xddb7_59da, u32::from(enable), None)
    }

    /// Override v2 GPIO RAM-ID triples (`PSP / 0x60`).
    pub fn with_v2_gpio_id(mut self, gpio: [[u8; 3]; 4]) -> Self {
        self.v2_gpio_id = Some(gpio);
        self
    }

    /// Merge `other` on top of `self` (later values win per token ID).
    pub fn merge(&mut self, other: Self) {
        for token in other.tokens {
            self.set_token(token);
        }
        if !other.cxl_regions.is_empty() {
            self.cxl_regions = other.cxl_regions;
        }
        if other.v2_gpio_id.is_some() {
            self.v2_gpio_id = other.v2_gpio_id;
        }
    }

    fn set_token(&mut self, token: ApcbToken) {
        if let Some(existing) = self.tokens.iter_mut().find(|t| t.id == token.id) {
            *existing = token;
        } else {
            self.tokens.push(token);
        }
    }
}

/// Resolve board metadata tokens (`BoardToken`) into plan tokens (`ApcbToken`).
pub fn resolve_tokens(tokens: &[BoardToken]) -> Result<Vec<ApcbToken>> {
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

/// Whether `key` is an APCB (BIOS entry `0x60` or its backup `0x68`).
pub const fn is_apcb(key: Key) -> bool {
    matches!(key.kind, bios::APCB | bios::APCB_BACKUP)
}

/// Whether `key` is a v3 override-layer slot (`0x60/0` or its backup `0x68/8`).
const fn is_override_slot(key: Key) -> bool {
    (key.kind == bios::APCB && key.instance == 0)
        || (key.kind == bios::APCB_BACKUP && key.instance == 8)
}

/// Apply `overlay` to every APCB (`0x60`/`0x68`) of an image.
pub fn apply(apcbs: &mut [(Key, &mut Vec<u8>)], overlay: &ApcbOverlay) -> Result<()> {
    if overlay.is_empty() {
        return Ok(());
    }
    let originals: Vec<Vec<u8>> = apcbs.iter().map(|(_, bytes)| bytes.to_vec()).collect();

    if let Some(gpio) = overlay.v2_gpio_id {
        let mut written = 0usize;
        for (key, bytes) in apcbs.iter_mut() {
            if v2::write_gpio_id(bytes, gpio)
                .map_err(|e| format!("APCB {key:?}: v2 GPIO ID: {e:?}"))?
            {
                written += 1;
            }
        }
        if written == 0 {
            return Err(
                "v2 GPIO RAM-ID overlay requested, but no v2 APCB has PSP/0x60 GPIO".into(),
            );
        }
    }

    if !overlay.cxl_regions.is_empty() {
        let array =
            apcb::cxl_array_of(&overlay.cxl_regions).map_err(|e| format!("CXL regions: {e:?}"))?;
        let mut written = 0usize;
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

    for wanted in &overlay.tokens {
        set_token(apcbs, wanted)?;
    }

    for ((key, bytes), original) in apcbs.iter_mut().zip(&originals) {
        if **bytes != *original {
            apcb::commit(bytes).map_err(|e| format!("APCB {key:?}: {e:?}"))?;
            if !apcb::checksum_ok(bytes).map_err(|e| format!("APCB {key:?}: {e:?}"))? {
                return Err(format!("APCB {key:?}: checksum failed after commit"));
            }
        }
    }
    Ok(())
}

fn set_token(apcbs: &mut [(Key, &mut Vec<u8>)], wanted: &ApcbToken) -> Result<()> {
    let label = wanted.label();
    if apcb::names::in_place_only(wanted.id) && wanted.value != 0 {
        return Err(format!(
            "APCB token {label}: fusing tokens cannot be set nonzero ({:#x}) by an overlay",
            wanted.value
        ));
    }

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
        (None, []) => {
            return Err(format!(
                "APCB token {label}: no APCB carries it; give its explicit size (1, 2, 4, or \"bool\")"
            ));
        }
        (None, found) => {
            return Err(format!(
                "APCB token {label}: the APCBs carry it as {found:?}; give its size"
            ));
        }
    };

    let tok = Token::new(kind, wanted.id, wanted.value)
        .map_err(|_| format!("APCB token {label}: {:#x} is not a {kind:?}", wanted.value))?;
    let present = kinds.contains(&kind);
    if !present && apcb::names::in_place_only(wanted.id) {
        return Err(format!(
            "APCB token {label}: no APCB carries it, and a fusing token is only updated in place, never added"
        ));
    }

    let mut applied = 0usize;
    if present {
        for (key, bytes) in apcbs.iter_mut() {
            let found = token::update_token(bytes, tok)
                .map_err(|e| format!("APCB {key:?}: token {label}: {e:?}"))?
                .found;
            applied += usize::from(found > 0);
        }
    } else {
        // Prefer adding new tokens to override-layer slots (0x60/0 and backup
        // 0x68/8) if any of them has an entry of `kind`; otherwise fall back to
        // any APCB with an entry of `kind`.
        let has_override_target = apcbs.iter().any(|(key, bytes)| {
            is_override_slot(*key) && {
                let mut ok = false;
                let _ = token::token_entries(bytes, |_, k| ok |= k == kind);
                ok
            }
        });
        for (key, bytes) in apcbs.iter_mut() {
            if has_override_target && !is_override_slot(*key) {
                continue;
            }
            let error = |e| format!("APCB {key:?}: token {label}: {e:?}");
            let added = loop {
                match token::add_token(bytes, tok) {
                    Err(apcb::Error::NoRoom { needed }) if needed > bytes.len() => {
                        bytes.resize(needed, 0xff);
                    }
                    result => break result.map_err(error)?,
                }
            };
            applied += usize::from(added);
        }
    }

    if applied == 0 {
        return Err(format!(
            "APCB token {label}: no APCB has a {kind:?} token entry"
        ));
    }

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

/// Picasso APCB v2 GPIO RAM-ID + SPD injector.
///
/// Uses the native APCB v2 walker (`PSP / 0x60` and `MEMORY / 0x30`), so it
/// works identically on sentinel templates (`APCB_magic.bin`) and real donor
/// APCBs (no magic sentinel scanning required).
pub fn inject(
    template: &[u8],
    gpio: [[u8; 3]; 4],
    spds: [[Option<&[u8; 512]>; 2]; 2],
    strip_manufacturer: bool,
) -> Result<Vec<u8>> {
    let mut output = template.to_vec();
    if !v2::write_gpio_id(&mut output, gpio).map_err(|e| format!("APCB v2 GPIO board ID: {e:?}"))? {
        return Err("APCB v2 template has no PSP/0x60 GPIO board-ID entry".into());
    }
    let slots = v2::write_spds(&mut output, spds, strip_manufacturer)
        .map_err(|e| format!("APCB v2 SPD table: {e:?}"))?;
    if slots == 0 {
        return Err("APCB v2 template has no MEMORY/0x30 SPD table".into());
    }
    apcb::fix_checksum(&mut output).map_err(|e| format!("APCB checksum: {e:?}"))?;
    Ok(output)
}

pub fn parse_spd_hex(text: &str) -> Result<[u8; 512]> {
    let digits: String = text
        .lines()
        .map(|line| line.split('#').next().unwrap_or(""))
        .flat_map(str::chars)
        .filter(|c| !c.is_whitespace())
        .collect();
    if digits.len() != 1024 || !digits.is_ascii() {
        return Err("SPD must contain exactly 512 hex bytes".into());
    }
    let mut result = [0; 512];
    for (index, byte) in result.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&digits[index * 2..index * 2 + 2], 16)
            .map_err(|_| "non-hex SPD byte")?;
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v2_template() -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(b"APCB");
        out.extend_from_slice(&0x20u16.to_le_bytes());
        out.extend_from_slice(&0x20u16.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&1u32.to_le_bytes());
        out.resize(0x20, 0);

        let psp_at = out.len();
        out.extend_from_slice(b"PSPG");
        out.extend_from_slice(&apcb::group::PSP.to_le_bytes());
        out.extend_from_slice(&16u16.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&[0; 6]);
        let mut psp_data = vec![0u8; 16];
        psp_data[0..2].copy_from_slice(&v2::BOARD_ID_GPIO.to_le_bytes());
        out.extend_from_slice(&apcb::group::PSP.to_le_bytes());
        out.extend_from_slice(&v2::BOARD_ID_METHOD.to_le_bytes());
        out.extend_from_slice(&((16 + psp_data.len()) as u16).to_le_bytes());
        out.extend_from_slice(&[0u8; 10]);
        out.extend_from_slice(&psp_data);
        let psp_size = (out.len() - psp_at) as u32;
        out[psp_at + 12..psp_at + 16].copy_from_slice(&psp_size.to_le_bytes());

        let mem_at = out.len();
        out.extend_from_slice(b"MEMG");
        out.extend_from_slice(&apcb::group::MEMORY.to_le_bytes());
        out.extend_from_slice(&16u16.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&[0; 6]);
        let mut mem_data = vec![0u8; 4 + v2::SPD_SLOT_SIZE];
        mem_data[0..4].copy_from_slice(&1u32.to_le_bytes());
        mem_data[4 + 25] = 1; // channel 1
        mem_data[4 + 26] = 1; // dimm 1
        out.extend_from_slice(&apcb::group::MEMORY.to_le_bytes());
        out.extend_from_slice(&v2::MEMORY_SPD.to_le_bytes());
        out.extend_from_slice(&((16 + mem_data.len()) as u16).to_le_bytes());
        out.extend_from_slice(&[0u8; 10]);
        out.extend_from_slice(&mem_data);
        let mem_size = (out.len() - mem_at) as u32;
        out[mem_at + 12..mem_at + 16].copy_from_slice(&mem_size.to_le_bytes());

        let total = out.len() as u32;
        out[8..12].copy_from_slice(&total.to_le_bytes());
        apcb::fix_checksum(&mut out).unwrap();
        out
    }

    type RawTokenEntry<'a> = (u16, u8, u16, &'a [(u32, u32)]);

    fn v3_token_apcb(entries: &[RawTokenEntry<'_>], room: usize) -> Vec<u8> {
        let mut blob = b"APCB".to_vec();
        blob.extend_from_slice(&0x80u16.to_le_bytes());
        blob.extend_from_slice(&0x30u16.to_le_bytes());
        blob.resize(0x80, 0);
        blob[0x20..0x24].copy_from_slice(b"ECB2");
        let group = blob.len();
        blob.extend_from_slice(b"TOKN");
        blob.extend_from_slice(&0x3000u16.to_le_bytes());
        blob.extend_from_slice(&16u16.to_le_bytes());
        blob.extend_from_slice(&1u16.to_le_bytes());
        blob.extend_from_slice(&[0; 6]);
        for (instance, &(kind, priority, board_mask, tokens)) in entries.iter().enumerate() {
            blob.extend_from_slice(&0x3000u16.to_le_bytes());
            blob.extend_from_slice(&kind.to_le_bytes());
            blob.extend_from_slice(&(16 + 8 * tokens.len() as u16).to_le_bytes());
            blob.extend_from_slice(&(instance as u16).to_le_bytes());
            blob.extend_from_slice(&[2, 1, 8, priority, 4, 0]);
            blob.extend_from_slice(&board_mask.to_le_bytes());
            for (id, value) in tokens {
                blob.extend_from_slice(&id.to_le_bytes());
                blob.extend_from_slice(&value.to_le_bytes());
            }
        }
        let gsize = (blob.len() - group) as u32;
        blob[group + 12..group + 16].copy_from_slice(&gsize.to_le_bytes());
        let size = blob.len() as u32;
        blob[8..12].copy_from_slice(&size.to_le_bytes());
        blob.resize(blob.len() + room, 0xff);
        apcb::fix_checksum(&mut blob).unwrap();
        blob
    }

    #[test]
    fn inject_v2_uses_walker_and_checksums() {
        let template = v2_template();
        let spd0 = [0xaa; 512];
        let spd1 = [0xbb; 512];
        let gpio = [[132, 1, 0], [90, 1, 0], [86, 3, 0], [69, 1, 0]];
        let output = inject(
            &template,
            gpio,
            [[None, None], [Some(&spd0), Some(&spd1)]],
            true,
        )
        .unwrap();
        assert!(apcb::checksum_ok(&output).unwrap());
        assert_eq!(v2::read_gpio_id(&output), Ok(Some(gpio)));
        let mem = apcb::find(&output, apcb::group::MEMORY, v2::MEMORY_SPD, 0)
            .unwrap()
            .unwrap();
        let spd_slice = &output[mem.data.start + 4 + v2::SPD_DESCRIPTOR_SIZE..];
        assert_eq!(spd_slice[0], 0xbb);
        assert_eq!(&spd_slice[320..384], &[0; 64]);
        assert!(inject(b"APCBshort", [[0; 3]; 4], [[None; 2]; 2], false).is_err());
    }

    #[test]
    fn overlay_preserves_unchanged_and_guards_fusing_tokens() {
        let mut defaults = v3_token_apcb(
            &[
                (0, 0x20, 0xffff, &[(0x2fcd_70c9, 1)]), // PspPsbAutoFuse (bool)
                (1, 0x20, 0x0002, &[(0xddb7_59da, 1)]), // board-masked FchConsoleOutMode
                (1, 0x20, 0xffff, &[(0xddb7_59da, 0)]), // all-boards FchConsoleOutMode
            ],
            0x100,
        );
        let mut overrides = v3_token_apcb(&[(1, 0x04, 0xffff, &[])], 0x100);
        let mut backup = v3_token_apcb(&[(1, 0x04, 0xffff, &[])], 0);
        let before = (defaults.clone(), overrides.clone(), backup.clone());

        // Empty overlay: byte-identical.
        {
            let mut set = [
                (Key::new(bios::APCB_BACKUP, 0, 0), &mut defaults),
                (Key::new(bios::APCB, 0, 0), &mut overrides),
                (Key::new(bios::APCB_BACKUP, 0, 8), &mut backup),
            ];
            apply(&mut set, &ApcbOverlay::new()).unwrap();
        }
        assert_eq!(
            (&defaults, &overrides, &backup),
            (&before.0, &before.1, &before.2)
        );

        // Refuses setting fusing token nonzero.
        {
            let mut set = [(Key::new(bios::APCB_BACKUP, 0, 0), &mut defaults)];
            let err = apply(
                &mut set,
                &ApcbOverlay::new().with_token(0x2fcd_70c9, 1, None),
            )
            .unwrap_err();
            assert!(err.contains("nonzero"), "{err}");
        }

        // Updates PspPsbAutoFuse -> 0 in place, updates FchConsoleOutMode across
        // both masked and all-boards entries, and adds a new byte token ONLY to
        // the override slots (0x60/0 and 0x68/8).
        let overlay = ApcbOverlay::new()
            .disable_psb_auto_fuse()
            .abl_console_out(true)
            .with_token(0x1122_3344, 0x42, Some(TokenKind::Byte));
        {
            let mut set = [
                (Key::new(bios::APCB_BACKUP, 0, 0), &mut defaults),
                (Key::new(bios::APCB, 0, 0), &mut overrides),
                (Key::new(bios::APCB_BACKUP, 0, 8), &mut backup),
            ];
            apply(&mut set, &overlay).unwrap();
        }
        assert_eq!(
            token::token(&defaults, TokenKind::Bool, 0x2fcd_70c9),
            Ok(Some(0))
        );
        assert_eq!(
            token::token(&defaults, TokenKind::Byte, 0xddb7_59da),
            Ok(Some(1))
        );
        assert_eq!(
            token::token(&defaults, TokenKind::Byte, 0x1122_3344),
            Ok(None)
        );
        assert_eq!(
            token::token(&overrides, TokenKind::Byte, 0x1122_3344),
            Ok(Some(0x42))
        );
        assert_eq!(
            token::token(&backup, TokenKind::Byte, 0x1122_3344),
            Ok(Some(0x42))
        );
        assert!(apcb::checksum_ok(&defaults).unwrap());
        assert!(apcb::checksum_ok(&overrides).unwrap());
        assert!(apcb::checksum_ok(&backup).unwrap());
    }

    #[test]
    fn absent_token_requires_explicit_kind() {
        let mut overrides = v3_token_apcb(&[(4, 0x04, 0xffff, &[])], 0x100);
        let mut set = [(Key::new(bios::APCB, 0, 0), &mut overrides)];
        let err = apply(
            &mut set,
            &ApcbOverlay::new().with_token(0x07a5_db75, 0, None),
        )
        .unwrap_err();
        assert!(err.contains("explicit size"), "{err}");
    }
}
