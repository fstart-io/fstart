// SPDX-License-Identifier: MIT OR Apache-2.0
//! Source-built APCB v2/v3 configuration for the PSP's AGESA boot loader.
//!
//! Oxide's `amd-apcb` owns the binary layouts, typed tables and token encoding.
//! We retain source entry order: its normal builder sorts entries without
//! considering priority, which changes vendor CBS override ordering. No binary
//! template is needed. Unused directory-slot padding is not configuration.
use super::Result;
use amd_apcb::{Apcb, ApcbIoOptions};
use fstart_amdfw_format::directory::{BiosEntry, Entry, Levels, bios};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    borrow::Cow,
    collections::{BTreeMap, VecDeque},
};

pub type Tables = BTreeMap<String, Value>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Selector {
    pub kind: u8,
    pub instance: u8,
    pub subprogram: u8,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Sources {
    pub schema: u8,
    pub blobs: Vec<Source>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    pub name: String,
    pub selectors: Vec<Selector>,
    /// SHA-256 of the original donor's declared APCB contents, not slot padding.
    /// Editing the source changes the output, not this compatibility guard.
    pub donor_sha256: String,
    pub apcb: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Replacement {
    pub selector: Selector,
    pub donor_sha256: String,
    pub bytes: Vec<u8>,
}

fn number(value: &Value, field: &str) -> Result<usize> {
    value[field]
        .as_u64()
        .and_then(|v| usize::try_from(v).ok())
        .ok_or_else(|| format!("APCB: missing or invalid {field}"))
}

fn array<'a>(value: &'a Value, field: &str) -> Result<&'a Vec<Value>> {
    value[field]
        .as_array()
        .ok_or_else(|| format!("APCB: missing {field} array"))
}

fn load(bytes: &[u8]) -> Result<Apcb<'static>> {
    if bytes.len() > Apcb::MAX_SIZE {
        return Err("APCB exceeds 64 KiB".into());
    }
    let apcb = Apcb::load(Cow::Owned(bytes.to_vec()), &ApcbIoOptions::default())
        .map_err(|e| format!("APCB: {e:?}"))?;
    let header = serde_json::to_value(*apcb.header().map_err(|e| format!("{e:?}"))?)
        .map_err(|e| e.to_string())?;
    let header_size = number(&header, "header_size")?;
    if !matches!(
        (number(&header, "version")?, header_size),
        (0x20, 32) | (0x30, 128)
    ) {
        return Err("source generation supports APCB v2 and v3 headers".into());
    }
    let size = number(&header, "apcb_size")?;
    if size > bytes.len() || size < header_size {
        return Err("invalid declared APCB size".into());
    }
    Ok(apcb)
}

/// Export typed fields. The result is accepted by [`build`], with no template.
pub fn decode(bytes: &[u8]) -> Result<Value> {
    let apcb = load(bytes)?;
    let mut value = serde_json::to_value(&apcb).map_err(|e| e.to_string())?;
    // Upstream GroupItem emits context, but SerdeGroupItem rejects it.
    for group in value["groups"]
        .as_array_mut()
        .ok_or("missing APCB groups")?
    {
        group
            .as_object_mut()
            .ok_or("invalid APCB group")?
            .remove("context");
    }
    if array(&value, "entries")?
        .iter()
        .any(|e| e.get("struct_body").is_some())
    {
        return Err("APCB contains a structure without typed source support".into());
    }
    let rebuilt = build(&value, &Tables::new())?;
    if bytes.get(..rebuilt.len()) != Some(rebuilt.as_slice()) {
        return Err("APCB typed export is not lossless; refusing to discard unknown data".into());
    }
    Ok(value)
}

fn entry_key(header: &Value) -> Result<String> {
    let mut key = header.clone();
    // Lengths are recomputed when a table changes, not part of entry identity.
    key.as_object_mut()
        .ok_or("invalid APCB entry header")?
        .remove("entry_size");
    // PriorityLevels' omitted fields default to zero in amd-apcb's schema.
    if let Some(priority) = key.get_mut("priority_mask").and_then(Value::as_object_mut) {
        priority.retain(|_, value| value != &Value::Bool(false) && value != &Value::from(0));
    }
    serde_json::to_string(&key).map_err(|e| e.to_string())
}

/// Compile typed source, optionally referring to named electrical tables through
/// `entries[].table`. Table values are the same typed bodies `amd-apcb` exports.
pub fn build(source: &Value, tables: &Tables) -> Result<Vec<u8>> {
    if !source.is_object() {
        return Err("APCB source must be an object".into());
    }
    let mut source = source.clone();
    for entry in source["entries"]
        .as_array_mut()
        .ok_or("missing APCB entries")?
    {
        let object = entry.as_object_mut().ok_or("invalid APCB entry")?;
        if let Some(table) = object.remove("table") {
            if object.len() != 1 || !object.contains_key("header") {
                return Err("a table reference must have only header and table".into());
            }
            let name = table.as_str().ok_or("APCB table name must be a string")?;
            let body = tables
                .get(name)
                .ok_or_else(|| format!("unknown APCB table {name}"))?;
            let body = body.as_object().ok_or("APCB table must be a typed body")?;
            if body.len() != 1 || body.contains_key("header") || body.contains_key("table") {
                return Err(format!("invalid APCB table {name}"));
            }
            object.extend(body.clone());
        }
    }
    if !matches!(
        (
            number(&source["header"], "version")?,
            number(&source["header"], "header_size")?
        ),
        (0x20, 32) | (0x30, 128)
    ) {
        return Err("source generation supports APCB v2 and v3 headers".into());
    }
    let apcb: Apcb<'static> =
        serde_json::from_value(source.clone()).map_err(|e| format!("APCB source: {e}"))?;
    let canonical = serde_json::to_value(&apcb).map_err(|e| e.to_string())?;
    let saved = apcb
        .save_no_inc()
        .map_err(|e| format!("APCB build: {e:?}"))?;
    let size = number(&canonical["header"], "apcb_size")?;
    let header_size = number(&canonical["header"], "header_size")?;
    let mut output = saved[..header_size].to_vec();
    let mut offset = header_size;
    let mut groups = BTreeMap::new();
    // Use the library's decoded lengths and headers, not a second APCB parser.
    for group in array(&canonical, "groups")? {
        let header = &group["header"];
        let id = number(header, "group_id")?;
        let original = array(&source, "groups")?
            .iter()
            .find(|g| g["header"]["group_id"] == header["group_id"])
            .ok_or("missing APCB group in source")?;
        let mut original_header = original["header"].clone();
        let mut generated_header = header.clone();
        original_header
            .as_object_mut()
            .ok_or("invalid group header")?
            .remove("group_size");
        generated_header
            .as_object_mut()
            .ok_or("invalid group header")?
            .remove("group_size");
        if original_header != generated_header {
            return Err("APCB builder cannot preserve this group header".into());
        }
        let end = offset
            .checked_add(number(header, "group_size")?)
            .ok_or("APCB size overflow")?;
        let header_end = offset + number(header, "header_size")?;
        let group_header = saved
            .get(offset..header_end)
            .ok_or("invalid APCB group size")?
            .to_vec();
        offset = header_end;
        let mut entries = BTreeMap::new();
        for entry in array(&canonical, "entries")?
            .iter()
            .filter(|e| e["header"]["group_id"] == header["group_id"])
        {
            let length = number(&entry["header"], "entry_size")?.next_multiple_of(4);
            let data = saved
                .get(offset..offset + length)
                .ok_or("invalid APCB entry size")?
                .to_vec();
            entries
                .entry(entry_key(&entry["header"])?)
                .or_insert_with(VecDeque::new)
                .push_back(data);
            offset += length;
        }
        if offset != end || groups.insert(id, (group_header, entries)).is_some() {
            return Err("invalid or duplicate APCB group".into());
        }
    }
    for group in array(&source, "groups")? {
        let id = number(&group["header"], "group_id")?;
        let (header, mut entries) = groups
            .remove(&id)
            .ok_or("missing or duplicate APCB group")?;
        output.extend_from_slice(&header);
        for entry in array(&source, "entries")?
            .iter()
            .filter(|e| e["header"]["group_id"] == group["header"]["group_id"])
        {
            output.extend(
                entries
                    .get_mut(&entry_key(&entry["header"])?)
                    .and_then(VecDeque::pop_front)
                    .ok_or("APCB builder changed an entry header or source has extra duplicates")?,
            );
        }
        if entries.values().any(|queue| !queue.is_empty()) {
            return Err("APCB source omitted entries".into());
        }
    }
    if !groups.is_empty() || offset != size || output.len() != size {
        return Err("APCB source omitted groups or has invalid lengths".into());
    }
    // Reordering preserves the additive checksum; validate before returning.
    load(&output)?;
    Ok(output)
}

pub fn compile(source: &str, tables: &Tables) -> Result<Vec<Replacement>> {
    let sources: Sources = serde_json::from_str(source).map_err(|e| e.to_string())?;
    compile_sources(sources, tables)
}

pub(crate) fn compile_sources(sources: Sources, tables: &Tables) -> Result<Vec<Replacement>> {
    if sources.schema != 1 || sources.blobs.is_empty() {
        return Err("expected APCB source schema 1 with at least one blob".into());
    }
    let mut replacements = BTreeMap::new();
    for source in sources.blobs {
        if source.selectors.is_empty()
            || source.donor_sha256.len() != 64
            || !source.donor_sha256.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err(format!(
                "invalid APCB selectors/donor SHA-256 for {}",
                source.name
            ));
        }
        let bytes = build(&source.apcb, tables).map_err(|e| format!("{}: {e}", source.name))?;
        for selector in source.selectors {
            if !matches!(selector.kind, bios::APCB | bios::APCB_BACKUP)
                || selector.instance > 15
                || selector.subprogram > 7
            {
                return Err(format!("invalid APCB selector {selector:?}"));
            }
            let replacement = Replacement {
                selector,
                donor_sha256: source.donor_sha256.to_ascii_lowercase(),
                bytes: bytes.clone(),
            };
            if replacements.insert(selector, replacement).is_some() {
                return Err(format!("duplicate APCB selector {selector:?}"));
            }
        }
    }
    Ok(replacements.into_values().collect())
}

pub fn sha256(bytes: &[u8]) -> String {
    fstart_crypto::digest::hash_sha256(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Replace every APCB directory slot or fail. Only the declared configuration
/// is overwritten: unrelated firmware and unused slot padding remain intact.
/// Verify all mappings and donor pins before making any writes.
pub fn replace(
    bytes: &mut [u8],
    directories: &Levels<'_, BiosEntry>,
    replacements: &[Replacement],
) -> Result<()> {
    for (index, replacement) in replacements.iter().enumerate() {
        if replacements[..index]
            .iter()
            .any(|other| other.selector == replacement.selector)
        {
            return Err("duplicate generated APCB selector".into());
        }
    }
    let mut used = vec![false; replacements.len()];
    let mut writes: BTreeMap<usize, (usize, usize)> = BTreeMap::new();
    for directory in directories.iter() {
        for entry in directory
            .entries
            .iter()
            .filter(|e| matches!(e.kind, bios::APCB | bios::APCB_BACKUP))
        {
            let selector = Selector {
                kind: entry.kind,
                instance: entry.instance(),
                subprogram: entry.subprogram(),
            };
            let (index, replacement) = replacements
                .iter()
                .enumerate()
                .find(|(_, r)| r.selector == selector)
                .ok_or_else(|| format!("no generated APCB for {selector:?}"))?;
            let range = directory.range(entry).map_err(|e| format!("{e:?}"))?;
            let original = bytes.get(range.clone()).ok_or("APCB slot outside image")?;
            let decoded = load(original)?;
            let header = serde_json::to_value(*decoded.header().map_err(|e| format!("{e:?}"))?)
                .map_err(|e| e.to_string())?;
            let size = number(&header, "apcb_size")?;
            if sha256(&original[..size]) != replacement.donor_sha256 {
                return Err(format!(
                    "donor APCB differs from the reviewed source for {selector:?}; refusing to replace it"
                ));
            }
            load(&replacement.bytes)?;
            if replacement.bytes.len() > range.len() {
                return Err(format!(
                    "generated APCB exceeds slot capacity for {selector:?}"
                ));
            }
            let end = range.start + replacement.bytes.len();
            for (&start, &(other_end, other_index)) in &writes {
                if range.start < other_end
                    && start < end
                    && (start != range.start
                        || replacements[other_index].bytes != replacement.bytes)
                {
                    return Err("generated APCB slots overlap with different contents".into());
                }
            }
            writes.insert(range.start, (end, index));
            used[index] = true;
        }
    }
    if used.iter().any(|seen| !seen) {
        return Err("generated APCB selector is absent from donor directories".into());
    }
    for (start, (end, index)) in writes {
        bytes[start..end].copy_from_slice(&replacements[index].bytes);
    }
    Ok(())
}

#[cfg(test)]
#[path = "apcb_source_tests.rs"]
mod tests;
