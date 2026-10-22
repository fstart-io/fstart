// SPDX-License-Identifier: MIT OR Apache-2.0
//! Strict reader for the Picasso subset of the `fw.cfg` firmware list.
//! Paths are resolved relative to an explicit source root,
//! not relative to the config file. An optional blob directory replaces
//! FIRMWARE_LOCATION for independently supplied, licensed AMD binaries.
use super::{Blob, Key, Result};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

#[derive(Debug, Default, Clone, Copy)]
pub struct Options {
    pub secure_os: bool,
    pub mp2: bool,
    pub secure_debug: bool,
    pub s0i3: bool,
}
#[derive(Debug)]
pub struct Files {
    pub psp: Vec<(Key, PathBuf)>,
    pub bios: Vec<(Key, PathBuf)>,
}
impl Files {
    pub fn read(&self) -> Result<(Vec<Blob>, Vec<Blob>)> {
        let read = |files: &[(Key, PathBuf)]| {
            files
                .iter()
                .map(|(key, path)| {
                    std::fs::read(path)
                        .map(|bytes| Blob { key: *key, bytes })
                        .map_err(|e| format!("{}: {e}", path.display()))
                })
                .collect::<Result<Vec<Blob>>>()
        };
        Ok((read(&self.psp)?, read(&self.bios)?))
    }
}

pub fn parse(
    text: &str,
    source_root: &Path,
    blob_directory: Option<&Path>,
    options: Options,
) -> Result<Files> {
    let lines: Vec<_> = text
        .lines()
        .enumerate()
        .filter_map(|(i, line)| {
            let fields: Vec<_> = line
                .split('#')
                .next()
                .unwrap_or("")
                .split_whitespace()
                .collect();
            (!fields.is_empty()).then_some((i + 1, fields))
        })
        .collect();
    let mut location = None;
    let mut soc = None;
    for (line, fields) in &lines {
        if fields.len() != 2 {
            return Err(format!(
                "fw.cfg:{line}: expected key and value; level/hash options are unsupported on Picasso"
            ));
        }
        match fields[0] {
            "SOC_NAME" if soc.replace(fields[1]).is_some() => {
                return Err("duplicate SOC_NAME".into());
            }
            "FIRMWARE_LOCATION" if location.replace(fields[1]).is_some() => {
                return Err("duplicate FIRMWARE_LOCATION".into());
            }
            _ => {}
        }
    }
    if !soc.is_some_and(|s| s.eq_ignore_ascii_case("Picasso")) {
        return Err("fw.cfg must select SOC_NAME Picasso".into());
    }
    let location = location.ok_or("missing FIRMWARE_LOCATION")?;
    let directory = blob_directory
        .map(Path::to_path_buf)
        .unwrap_or_else(|| source_root.join(location));
    let mut result = Files {
        psp: Vec::new(),
        bios: Vec::new(),
    };
    let mut seen = BTreeSet::new();
    for (line, fields) in lines {
        let name = fields[0];
        if matches!(name, "SOC_NAME" | "FIRMWARE_LOCATION") {
            continue;
        }
        let (bios, key, include) = identify(name, options)
            .ok_or_else(|| format!("fw.cfg:{line}: unsupported Picasso key {name}"))?;
        if !include {
            continue;
        }
        if !seen.insert((bios, key)) {
            return Err(format!("fw.cfg:{line}: duplicate identity {key:?}"));
        }
        let path = if fields[1].contains('/') {
            source_root.join(fields[1])
        } else {
            directory.join(fields[1])
        };
        if bios {
            result.bios.push((key, path));
        } else {
            result.psp.push((key, path));
        }
    }
    Ok(result)
}

fn identify(name: &str, o: Options) -> Option<(bool, Key, bool)> {
    for (prefix, kind) in [("PSP_PMUI_FILE_SUB", 0x64), ("PSP_PMUD_FILE_SUB", 0x65)] {
        if let Some(tail) = name.strip_prefix(prefix) {
            let (sub, instance) = tail.split_once("_INS")?;
            let (sub, instance) = (
                u8::from_str_radix(sub, 16).ok()?,
                u8::from_str_radix(instance, 16).ok()?,
            );
            // The PMU selectors of the Picasso BIOS directory.
            if sub > 1 || !(1..=5).contains(&instance) {
                return None;
            }
            return Some((true, Key::new(kind, sub, instance), true));
        }
    }
    let (kind, sub, include) = match name {
        "AMD_PUBKEY_FILE" => (0, 0, true),
        "PSPBTLDR_FILE" => (1, 0, true),
        // Whitelist bootloader is a different type (0x73), not an alternative 0x01.
        // Deliberately disabled: the CLI has no secure-debug unlock/whitelist policy.
        "PSPBTLDR_WL_FILE" => (0x73, 0, false),
        "PSPSECUREOS_FILE" => (2, 0, o.secure_os),
        "PSPRCVR_FILE" => (3, 0, true),
        "PSPNVRAM_FILE" => (4, 0, true),
        "PUBSIGNEDKEY_FILE" => (5, 0, true),
        "PSP_SMUFW1_SUB0_FILE" => (8, 0, true),
        "PSP_SMUFW1_SUB1_FILE" => (8, 1, true),
        "PSP_SMUFW1_SUB2_FILE" => (8, 2, true),
        "PSP_SEC_DBG_KEY_FILE" => (9, 0, o.secure_debug),
        "PSP_SEC_DEBUG_FILE" => (0x13, 0, o.secure_debug),
        "PSP_SMUFW2_SUB0_FILE" => (0x12, 0, true),
        "PSP_SMUFW2_SUB1_FILE" => (0x12, 1, true),
        "PSP_SMUFW2_SUB2_FILE" => (0x12, 2, true),
        "PSP_IKEK_FILE" => (0x21, 0, true),
        "PSP_SECG0_FILE" => (0x24, 0, true),
        "PSP_SECG1_FILE" => (0x24, 1, true),
        "PSP_SECG2_FILE" => (0x24, 2, true),
        "PSP_MP2FW0_FILE" => (0x25, 0, o.mp2),
        "PSP_MP2FW1_FILE" => (0x25, 1, o.mp2),
        "PSP_MP2FW2_FILE" => (0x25, 2, o.mp2),
        "PSP_DRIVERS_FILE" => (0x28, 0, true),
        "PSP_S0I3_FILE" => (0x2d, 0, o.s0i3),
        "RTM_PUBKEY_FILE" => return Some((true, Key::new(5, 0, 0), true)),
        "PSP_MP2CFG_FILE" => return Some((true, Key::new(0x6a, 0, 0), o.mp2)),
        _ => {
            if let Some(index) = name
                .strip_prefix("PSP_ABL")
                .and_then(|s| s.strip_suffix("_FILE"))
            {
                let index: u8 = index.parse().ok()?;
                if index > 7 {
                    return None;
                }
                (0x30 + index, 0, true)
            } else {
                return None;
            }
        }
    };
    Some((false, Key::new(kind, sub, 0), include))
}
