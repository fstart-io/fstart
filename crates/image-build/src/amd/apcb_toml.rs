// SPDX-License-Identifier: MIT OR Apache-2.0
//! Board policy. Framework firmware-family models own token storage,
//! record placement, defaults, selectors, ordering and donor guards.
pub use super::apcb_layout::Profile;
use super::{Result, apcb, apcb_layout, apcb_source};
use amd_apcb::memory;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Default, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct Memory {
    pub ecc: Option<bool>,
    pub power_down: Option<bool>,
    pub parity: Option<bool>,
    pub restore_context: Option<bool>,
    pub ignore_spd_checksum: Option<bool>,
    pub speed_limits: Vec<SpeedLimit>,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Module {
    Udimm,
    Rdimm,
    Sodimm,
    ThreeDs,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Training {
    Normal,
    Stretch,
}
fn normal() -> Training {
    Training::Normal
}

/// Population policy in physical units, not a packed frequency-table row.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct SpeedLimit {
    pub module: Module,
    /// Optional hardware board ID; the firmware profile translates selection.
    #[serde(default)]
    pub board_id: Option<u8>,
    #[serde(default = "normal")]
    pub training: Training,
    pub slots_per_channel: Vec<u8>,
    #[serde(default)]
    pub single_rank_dimms: u16,
    #[serde(default)]
    pub dual_rank_dimms: u16,
    #[serde(default)]
    pub quad_rank_dimms: u16,
    #[schemars(with = "apcb_layout::rows::Rate")]
    pub maximum_mt_s: u16,
}
impl SpeedLimit {
    fn identity(&self) -> Result<(u16, &'static str)> {
        use Module::*;
        use Training::*;
        Ok(match (self.module, self.training) {
            (Udimm, Normal) => (0x44, "MaxFreqElement"),
            (Udimm, Stretch) => (0x45, "MaxFreqElement"),
            (Rdimm, Normal) => (0x49, "MaxFreqElement"),
            (Rdimm, Stretch) => (0x4a, "MaxFreqElement"),
            (ThreeDs, Normal) => (0x4b, "MaxFreqElement"),
            (ThreeDs, Stretch) => (0x4c, "MaxFreqElement"),
            (Sodimm, Normal) => (0x5c, "MaxFreqElement"),
            (Sodimm, Stretch) => (0x5d, "MaxFreqElement"),
        })
    }
    fn lower(&self, generation: apcb_layout::Generation) -> Result<Value> {
        apcb_layout::rows::Rate::try_from(self.maximum_mt_s)?;
        let total = u32::from(self.single_rank_dimms)
            + u32::from(self.dual_rank_dimms)
            + u32::from(self.quad_rank_dimms);
        if total == 0
            || total > 4
            || self
                .slots_per_channel
                .iter()
                .any(|&slots| u32::from(slots) < total)
        {
            return Err("rank population must fit the channel's DIMM slots".into());
        }
        let row = apcb_layout::rows::MaxFrequencyRow {
            slots: self.slots_per_channel.clone(),
            conditions: [
                total as u16,
                self.single_rank_dimms,
                self.dual_rank_dimms,
                self.quad_rank_dimms,
            ],
            speed: serde_json::from_value(Value::String(format!("Ddr{}", self.maximum_mt_s)))
                .map_err(|e| e.to_string())?,
        };
        serde_json::to_value(row.rdimm(generation.unsupported_speed())?).map_err(|e| e.to_string())
    }
}

/// Select a JEDEC SPD image for a physical RAM-ID configuration. Descriptor
/// offsets, validity flags and training scratch allocations belong to the model.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Ram {
    pub id: u8,
    pub spd: String,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct Configuration {
    #[schemars(range(min = 1, max = 1))]
    pub schema: u8,
    #[serde(default)]
    pub memory: Memory,
    #[serde(default)]
    pub ram: Vec<Ram>,
    #[serde(default)]
    pub spd_images: BTreeMap<String, String>,
}
impl Configuration {
    pub fn parse(source: &str) -> Result<Self> {
        let policy: Self = toml::from_str(source).map_err(|e| e.to_string())?;
        if policy.schema != 1 {
            return Err("expected APCB policy schema 1".into());
        }
        Ok(policy)
    }

    fn apply(
        self,
        sources: &mut apcb_source::Sources,
        generation: apcb_layout::Generation,
    ) -> Result<()> {
        // These names are catalogued for SP3 and Zeppelin, not guessed from
        // Naples IDs on Picasso. Unknown family-specific settings stay defaults.
        for (setting, name, value) in [
            ("memory.ecc", "MemEnableEccFeature", self.memory.ecc),
            (
                "memory.power-down",
                "MemEnablePowerDown",
                self.memory.power_down,
            ),
            ("memory.parity", "MemEnableParity", self.memory.parity),
            (
                "memory.restore-context",
                "MemRestoreControl",
                self.memory.restore_context,
            ),
            (
                "memory.ignore-spd-checksum",
                "MemIgnoreSpdChecksum",
                self.memory.ignore_spd_checksum,
            ),
        ] {
            if let Some(value) = value {
                if matches!(generation, apcb_layout::Generation::Picasso) {
                    return Err(format!(
                        "{setting}: Picasso parameter semantics are not yet catalogued"
                    ));
                }
                let mut found = false;
                for blob in &mut sources.blobs {
                    for entry in blob.apcb["entries"]
                        .as_array_mut()
                        .ok_or("model entries missing")?
                    {
                        if let Some(tokens) = entry.get_mut("tokens").and_then(Value::as_array_mut)
                        {
                            for token in tokens {
                                if let Some(v) = token.get_mut("Bool").and_then(|t| t.get_mut(name))
                                {
                                    *v = Value::Bool(value);
                                    found = true;
                                }
                            }
                        }
                        if let Some(parameters) =
                            entry.get_mut("parameters").and_then(Value::as_array_mut)
                        {
                            for parameter in parameters {
                                if parameter["token"].as_str() == Some(name) {
                                    parameter["value"] = Value::from(u64::from(value));
                                    found = true;
                                }
                            }
                        }
                    }
                }
                if !found {
                    return Err(format!(
                        "setting {setting} is unsupported by this firmware profile"
                    ));
                }
            }
        }
        let mut limits: BTreeMap<(u16, u16, &str), Vec<Value>> = BTreeMap::new();
        for rule in self.memory.speed_limits {
            let mask = match rule.board_id {
                Some(id) if id < 16 => 1u16 << id,
                Some(_) => return Err("board ID must be below 16".into()),
                None => {
                    if matches!(
                        generation,
                        apcb_layout::Generation::Rome | apcb_layout::Generation::Milan
                    ) {
                        0xffff
                    } else {
                        0
                    }
                }
            };
            let (id, key) = rule.identity()?;
            limits
                .entry((id, mask, key))
                .or_default()
                .push(rule.lower(generation)?);
        }
        for ((id, mask, key), rows) in limits {
            let mut found = false;
            for blob in &mut sources.blobs {
                for entry in blob.apcb["entries"]
                    .as_array_mut()
                    .ok_or("model entries missing")?
                {
                    if entry["header"]["group_id"].as_u64() == Some(0x1704)
                        && entry["header"]["entry_id"].as_u64() == Some(u64::from(id))
                        && entry["header"]["board_instance_mask"].as_u64() == Some(u64::from(mask))
                    {
                        if entry.get(key).is_none() {
                            return Err("speed policy does not match the firmware profile".into());
                        }
                        entry[key] = Value::Array(rows.clone());
                        found = true;
                    }
                }
            }
            if !found {
                return Err(
                    "module/training speed policy is unsupported by this firmware profile".into(),
                );
            }
        }
        let mut ids = BTreeSet::new();
        for ram in self.ram {
            if !ids.insert(ram.id) {
                return Err("duplicate RAM-ID policy".into());
            }
            let image = apcb::parse_spd_hex(
                self.spd_images
                    .get(&ram.spd)
                    .ok_or_else(|| format!("unknown SPD image {}", ram.spd))?,
            )?;
            let mut found = false;
            for blob in &mut sources.blobs {
                if !blob.selectors.iter().any(|s| s.instance == ram.id) {
                    continue;
                }
                for entry in blob.apcb["entries"]
                    .as_array_mut()
                    .ok_or("model entries missing")?
                {
                    if let Some(body) = entry.get_mut("SpdInfo") {
                        let (header, mut dimms): (memory::SpdInfo, Vec<memory::SpdDimm>) =
                            serde_json::from_value(body.clone()).map_err(|e| e.to_string())?;
                        for dimm in &mut dimms {
                            if dimm
                                .descriptor
                                .dram_down_spd_valid()
                                .map_err(|e| format!("{e:?}"))?
                                && dimm
                                    .descriptor
                                    .dimm_present()
                                    .map_err(|e| format!("{e:?}"))?
                            {
                                dimm.data = memory::SpdData(image);
                                found = true;
                            }
                        }
                        *body = serde_json::to_value((header, dimms)).map_err(|e| e.to_string())?;
                    }
                }
            }
            if !found {
                return Err(format!(
                    "RAM ID {} has no soldered-memory SPD in this profile",
                    ram.id
                ));
            }
        }
        Ok(())
    }
}

pub(crate) fn lower(policy: &str, profile: Profile) -> Result<apcb_source::Sources> {
    let policy = Configuration::parse(policy)?;
    let model = profile.definition()?;
    let generation = model.generation;
    let mut sources = model.lower()?;
    policy.apply(&mut sources, generation)?;
    Ok(sources)
}

pub fn compile(policy: &str, profile: Profile) -> Result<Vec<apcb_source::Replacement>> {
    apcb_source::compile_sources(lower(policy, profile)?, &apcb_source::Tables::new())
}

pub fn build_blob(policy: &str, profile: Profile, name: &str) -> Result<Vec<u8>> {
    let sources = lower(policy, profile)?;
    let selector = *sources
        .blobs
        .iter()
        .find(|b| b.name == name)
        .ok_or_else(|| format!("no APCB named {name}"))?
        .selectors
        .first()
        .ok_or("profile has no bindings")?;
    let built = apcb_source::compile_sources(sources, &apcb_source::Tables::new())?;
    Ok(built
        .into_iter()
        .find(|r| r.selector == selector)
        .ok_or("profile binding missing")?
        .bytes)
}

pub fn schema() -> schemars::schema::RootSchema {
    schemars::schema_for!(Configuration)
}

#[cfg(test)]
#[path = "apcb_policy_tests.rs"]
mod tests;
