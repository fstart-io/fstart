// SPDX-License-Identifier: MIT OR Apache-2.0
//! Reusable silicon/firmware ABI construction. No mainboard identities/defaults.
pub use super::Generation as Abi;
use super::*;
use crate::BoardData;
include!("abi_catalog.rs");

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct ChannelWiring {
    #[serde(default)]
    clock_pins: Option<[u8; 8]>,
    #[serde(default)]
    cke_pins: Option<[u8; 4]>,
    #[serde(default)]
    odt_pins: Option<[u8; 4]>,
    #[serde(default)]
    chip_select_pins: Option<[u8; 8]>,
    dimms_per_channel: u8,
    channels_per_socket: u8,
}
fn wiring(input: &Value) -> Result<Table> {
    let data: ChannelWiring = serde_json::from_value(input.clone()).map_err(|e| e.to_string())?;
    if !(1..=4).contains(&data.dimms_per_channel) || !(1..=8).contains(&data.channels_per_socket) {
        return Err("invalid channel topology".into());
    }
    let mut operations = Vec::new();
    if let Some(connections) = data.clock_pins {
        operations.push(Override::MemclkMap { connections });
    }
    if let Some(connections) = data.cke_pins {
        operations.push(Override::CkeTristateMap { connections });
    }
    if let Some(connections) = data.odt_pins {
        operations.push(Override::OdtTristateMap { connections });
    }
    if let Some(connections) = data.chip_select_pins {
        operations.push(Override::CsTristateMap { connections });
    }
    operations.push(Override::MaxDimmsPerChannel {
        value: data.dimms_per_channel,
    });
    operations.push(Override::MaxChannelsPerSocket {
        value: data.channels_per_socket,
    });
    Ok(Table::PlatformOverride {
        scope: OverrideScope::All,
        operations,
    })
}

impl std::str::FromStr for Abi {
    type Err = String;
    fn from_str(name: &str) -> Result<Self> {
        Ok(match name {
            "rome" => Self::Rome,
            "milan" => Self::Milan,
            "picasso" => Self::Picasso,
            "zeppelin" => Self::Zeppelin,
            _ => return Err(format!("unsupported APCB ABI {name:?}")),
        })
    }
}
fn reference(name: &str) -> Reference {
    Reference {
        name: name.into(),
        instance: 0,
        board_mask: None,
        priority: None,
    }
}
fn image(data: &BoardData, name: &str, bindings: Vec<Binding>) -> Result<Blob> {
    let image = data
        .images
        .get(name)
        .ok_or_else(|| format!("missing image provenance {name:?}"))?;
    Ok(Blob {
        name: name.into(),
        revision: image.revision,
        donor_sha256: image.donor_sha256.clone(),
        bindings,
        groups: vec![],
        records: vec![],
        tokens: vec![],
    })
}
fn bind(slot: Slot, instance: u8) -> Binding {
    Binding { slot, instance }
}
fn token_sets(abi: Abi, bag: Option<&BTreeMap<String, Value>>) -> Result<[TokenSet; 4]> {
    let (mut b, mut y, mut w, mut d) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    if let Some(bag) = bag {
        for (name, value) in bag {
            if let Some(id) = name.strip_prefix("0x") {
                let id = u32::from_str_radix(id, 16).map_err(|_| "invalid numeric token ID")?;
                let value = value
                    .as_u64()
                    .and_then(|v| u32::try_from(v).ok())
                    .ok_or("numeric token value must fit u32")?;
                let raw = NumericToken { id, value };
                match numeric_class(abi, id)? {
                    0 => b.push(Token::Numeric(raw)),
                    1 => y.push(Token::Numeric(raw)),
                    2 => w.push(Token::Numeric(raw)),
                    3 => d.push(Token::Numeric(raw)),
                    _ => unreachable!(),
                }
            } else {
                let input = json!({(name):value});
                if let Ok(v) = serde_json::from_value::<BoolToken>(input.clone()) {
                    b.push(Token::Named(v));
                } else if let Ok(v) = serde_json::from_value::<ByteToken>(input.clone()) {
                    y.push(Token::Named(v));
                } else if let Ok(v) = serde_json::from_value::<WordToken>(input.clone()) {
                    w.push(Token::Named(v));
                } else if let Ok(v) = serde_json::from_value::<DwordToken>(input) {
                    d.push(Token::Named(v));
                } else {
                    return Err(format!("uncatalogued token or value {name:?} for {abi:?}"));
                }
            }
        }
    }
    Ok([
        TokenSet::Bool { values: b },
        TokenSet::Byte { values: y },
        TokenSet::Word { values: w },
        TokenSet::Dword { values: d },
    ])
}
fn nonempty(set: &TokenSet) -> bool {
    match set {
        TokenSet::Bool { values } => !values.is_empty(),
        TokenSet::Byte { values } => !values.is_empty(),
        TokenSet::Word { values } => !values.is_empty(),
        TokenSet::Dword { values } => !values.is_empty(),
    }
}
fn layer(
    model: &mut Definition,
    blob: &mut Blob,
    data: &BoardData,
    bag: &str,
    class: usize,
    instance: u16,
    priority: Option<Vec<Priority>>,
) -> Result<()> {
    let mut sets = token_sets(model.generation, data.tokens.get(bag))?.into_iter();
    let set = sets.nth(class).ok_or("invalid ABI token class")?;
    let name = format!("{bag}-{class}");
    model.token_sets.insert(name.clone(), set);
    blob.tokens.push(Reference {
        name,
        instance,
        board_mask: None,
        priority,
    });
    Ok(())
}
fn setup(model: &mut Definition, blob: &mut Blob, data: &BoardData) -> Result<()> {
    for class in 0..4 {
        for (id, bag) in &data.board_tokens {
            let set = token_sets(model.generation, Some(bag))?
                .into_iter()
                .nth(class)
                .unwrap();
            if nonempty(&set) {
                let id = id.parse::<u8>().map_err(|_| "invalid board ID")?;
                let name = format!("setup-board-{id}-{class}");
                model.token_sets.insert(name.clone(), set);
                blob.tokens.push(Reference {
                    name,
                    instance: 0,
                    board_mask: Some(1u16 << id),
                    priority: None,
                });
            }
        }
        layer(model, blob, data, "setup", class, 0, None)?;
    }
    Ok(())
}
fn cbs(
    model: &mut Definition,
    blob: &mut Blob,
    data: &BoardData,
    role: &str,
    medium_dword_first: bool,
) -> Result<()> {
    for class in 0..4 {
        if class == 3 && medium_dword_first {
            layer(
                model,
                blob,
                data,
                role,
                class,
                1,
                Some(vec![Priority::Medium]),
            )?;
            layer(
                model,
                blob,
                data,
                &format!("{role}-low"),
                class,
                0,
                Some(vec![Priority::Low]),
            )?;
        } else {
            layer(
                model,
                blob,
                data,
                &format!("{role}-low"),
                class,
                0,
                Some(vec![Priority::Low]),
            )?;
            layer(
                model,
                blob,
                data,
                role,
                class,
                1,
                Some(vec![Priority::Medium]),
            )?;
        }
    }
    Ok(())
}
impl Abi {
    pub(crate) fn definition(self, data: &BoardData) -> Result<Definition> {
        let mut model = Definition {
            generation: self,
            blobs: vec![],
            tables: BTreeMap::new(),
            token_sets: BTreeMap::new(),
            spd_images: data.spd_images.clone(),
        };
        for (key, values) in &data.defaults {
            model.tables.insert(key.clone(), table(self, key, values)?);
        }
        for (key, values) in &data.parameters {
            if model
                .tables
                .insert(key.clone(), parameters(self, key, values)?)
                .is_some()
            {
                return Err(format!("duplicate defaults {key}"));
            }
        }
        for (id, defaults) in &data.board_defaults {
            for (key, values) in defaults {
                model
                    .tables
                    .insert(format!("{key}@{id}"), table(self, key, values)?);
            }
        }
        for role in data.tokens.keys() {
            if ![
                "setup",
                "cbs-backup",
                "cbs-backup-low",
                "cbs-active",
                "cbs-active-low",
            ]
            .contains(&role.as_str())
            {
                return Err(format!("unsupported token defaults {role}"));
            }
        }
        if !self.legacy() {
            model
                .tables
                .insert("platform-tuning-end".into(), Table::PlatformTuningEnd {});
        }
        if matches!(self, Self::Picasso) {
            let mut ram: Vec<_> = data
                .images
                .keys()
                .map(|name| {
                    name.strip_prefix("ram-id-")
                        .ok_or("Picasso images must identify a RAM ID")
                        .and_then(|id| id.parse::<u8>().map_err(|_| "invalid RAM ID"))
                })
                .collect::<std::result::Result<_, _>>()
                .map_err(str::to_string)?;
            ram.sort_unstable();
            for id in ram {
                let name = format!("ram-id-{id}");
                let metadata = &data.images[&name];
                let mut blob = image(
                    data,
                    &name,
                    vec![bind(Slot::Backup, id), bind(Slot::Active, id)],
                )?;
                let mut spd = table(
                    self,
                    "spd-info",
                    data.defaults
                        .get("spd-info")
                        .ok_or("missing SPD routing defaults")?,
                )?;
                let Table::SpdInfo { slots } = &mut spd else {
                    return Err("invalid SPD ABI catalog".into());
                };
                for slot in slots {
                    if slot
                        .descriptor
                        .dram_down_spd_valid()
                        .map_err(|e| format!("{e:?}"))?
                        && slot
                            .descriptor
                            .dimm_present()
                            .map_err(|e| format!("{e:?}"))?
                    {
                        slot.image = metadata.spd_image.clone();
                    }
                }
                let spd_name = format!("spd-info@{id}");
                model.tables.insert(spd_name.clone(), spd);
                for &key in record_order(self) {
                    let actual = if key == "spd-info" { &spd_name } else { key };
                    if model.tables.contains_key(actual) {
                        blob.records.push(reference(actual));
                    }
                }
                model.blobs.push(blob);
            }
        } else {
            let bindings = if self.legacy() {
                vec![bind(Slot::Active, 0), bind(Slot::Backup, 0)]
            } else {
                vec![bind(Slot::Backup, 0)]
            };
            let mut blob = image(data, "board", bindings)?;
            for &key in record_order(self) {
                for id in data.board_defaults.keys() {
                    let name = format!("{key}@{id}");
                    if model.tables.contains_key(&name) {
                        let mut r = reference(&name);
                        r.board_mask =
                            Some(1u16 << id.parse::<u8>().map_err(|_| "invalid board ID")?);
                        blob.records.push(r);
                    }
                }
                if model.tables.contains_key(key) {
                    let mut r = reference(key);
                    if key == "board-id-eeprom" {
                        r.priority = Some(vec![Priority::HardForce, Priority::Normal]);
                    }
                    blob.records.push(r);
                }
            }
            if !self.legacy() {
                setup(&mut model, &mut blob, data)?;
            }
            model.blobs.push(blob);
        }
        if !self.legacy() {
            let mut backup = image(
                data,
                "cbs-backup",
                if matches!(self, Self::Rome) {
                    vec![bind(Slot::Backup, 8), bind(Slot::Active, 0)]
                } else {
                    vec![bind(Slot::Backup, 8)]
                },
            )?;
            if matches!(self, Self::Rome) && model.tables.contains_key("slink") {
                let mut r = reference("slink");
                r.priority = Some(vec![Priority::Low]);
                backup.records.push(r);
            }
            cbs(
                &mut model,
                &mut backup,
                data,
                "cbs-backup",
                matches!(self, Self::Rome),
            )?;
            model.blobs.push(backup);
            let mut ppr = image(
                data,
                "post-package-repair",
                vec![bind(Slot::Backup, 9), bind(Slot::Active, 1)],
            )?;
            let mut r = reference("empty-ppr");
            r.priority = Some(vec![Priority::EventLogging]);
            ppr.records.push(r);
            model.blobs.push(ppr);
            if matches!(self, Self::Milan) {
                let mut active = image(data, "cbs-active", vec![bind(Slot::Active, 0)])?;
                if data.images["cbs-active"].empty_memory_group {
                    active.groups = vec![Group::Df, Group::Memory, Group::Token];
                }
                if model.tables.contains_key("slink") {
                    let mut r = reference("slink");
                    r.priority = Some(vec![Priority::Low]);
                    active.records.push(r);
                }
                cbs(&mut model, &mut active, data, "cbs-active", true)?;
                model.blobs.push(active);
            }
        }
        Ok(model)
    }
}
