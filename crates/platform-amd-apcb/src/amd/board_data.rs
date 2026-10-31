// SPDX-License-Identifier: MIT OR Apache-2.0
//! Board-owned decoded compatibility values, not a layout/programming language.
//! This transitional data preserves settings whose semantics are not verified.
use crate::Result;
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct Image {
    pub revision: u32,
    pub donor_sha256: String,
    /// Representation-only quirk retained for byte-exact compatibility.
    #[serde(default)]
    pub empty_memory_group: bool,
    #[serde(default)]
    pub spd_image: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Variant {
    pub images: BTreeMap<String, Image>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct BoardData {
    pub schema: u8,
    /// Audit information only. Never used for dispatch or ABI selection.
    pub provenance: String,
    pub images: BTreeMap<String, Image>,
    #[serde(default)]
    pub defaults: BTreeMap<String, Value>,
    #[serde(default)]
    pub board_defaults: BTreeMap<String, BTreeMap<String, Value>>,
    /// Numeric parameter IDs and values; width, order and time point are ABI data.
    #[serde(default)]
    pub parameters: BTreeMap<String, BTreeMap<String, u64>>,
    #[serde(default)]
    pub tokens: BTreeMap<String, BTreeMap<String, Value>>,
    #[serde(default)]
    pub board_tokens: BTreeMap<String, BTreeMap<String, Value>>,
    #[serde(default)]
    pub spd_images: BTreeMap<String, String>,
    /// Reuse one board's defaults for firmware/assembly variants.
    #[serde(default)]
    pub variants: BTreeMap<String, Variant>,
}
impl BoardData {
    pub fn parse(text: &str, variant: &str) -> Result<Self> {
        let mut data: Self = toml::from_str(text).map_err(|e| e.to_string())?;
        if data.schema != 1 || data.images.is_empty() {
            return Err("expected board compatibility schema 1 and image provenance".into());
        }
        if !variant.is_empty() {
            data.images = data
                .variants
                .remove(variant)
                .ok_or_else(|| format!("unknown board-data variant {variant:?}"))?
                .images;
        }
        for key in data.board_defaults.keys().chain(data.board_tokens.keys()) {
            if key.parse::<u8>().ok().is_none_or(|id| id >= 16) {
                return Err("board ID must be below 16".into());
            }
        }
        Ok(data)
    }
}
