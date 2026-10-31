// SPDX-License-Identifier: MIT OR Apache-2.0
//! Experimental authoring format, separate from APCB's wire representation.
//!
//! TOML is validated against Rust types before lowering to amd-apcb's existing
//! typed builder. No binary headers, lengths, checksums or raw structure bodies
//! are accepted as configuration. JSON is only an internal adapter to upstream's
//! private SerdeApcb API, not a template or an authored input to this path.
use super::{Result, apcb_v3};
use amd_apcb::{BoolToken, ByteToken, DwordToken, TOKEN_ENTRY, WordToken, df, memory, psp};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
#[path = "apcb_toml_rows.rs"]
mod rows;
use rows::{CadRow, DataRow, MaxFrequencyRow, OdtRow};

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Configuration {
    #[schemars(range(min = 1, max = 1))]
    pub schema: u8,
    pub generation: Generation,
    #[schemars(length(min = 1))]
    pub blobs: Vec<Blob>,
    #[serde(default)]
    pub tables: BTreeMap<String, Table>,
    #[serde(default, rename = "token-sets")]
    pub token_sets: BTreeMap<String, TokenSet>,
}

/// Deliberately bounded experiment: adding another ABI is an explicit change.
#[derive(Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Generation {
    Rome,
}
impl Generation {
    fn unsupported_speed(self) -> memory::DdrSpeed {
        match self {
            Self::Rome => memory::DdrSpeed::UnsupportedRome,
        }
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SharedTables {
    #[schemars(range(min = 1, max = 1))]
    pub schema: u8,
    pub tables: BTreeMap<String, Table>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct Blob {
    pub name: String,
    pub revision: u32,
    pub donor_sha256: String,
    #[schemars(length(min = 1))]
    pub bindings: Vec<Binding>,
    #[serde(default)]
    pub records: Vec<Reference>,
    #[serde(default)]
    pub tokens: Vec<Reference>,
}

#[derive(Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Slot {
    Active,
    Backup,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub slot: Slot,
    #[serde(default)]
    pub instance: u8,
}

impl Binding {
    pub fn selector(&self, generation: Generation) -> apcb_v3::Selector {
        use fstart_amdfw_format::directory::bios;
        apcb_v3::Selector {
            kind: match self.slot {
                Slot::Active => bios::APCB,
                Slot::Backup => bios::APCB_BACKUP,
            },
            instance: self.instance,
            subprogram: match generation {
                Generation::Rome => 0,
            },
        }
    }
}

fn all_boards() -> u16 {
    u16::MAX
}
fn normal_priority() -> Vec<Priority> {
    vec![Priority::Normal]
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Priority {
    HardForce,
    High,
    Medium,
    EventLogging,
    Low,
    Normal,
}
impl Priority {
    fn key(self) -> &'static str {
        match self {
            Self::HardForce => "hard_force",
            Self::High => "high",
            Self::Medium => "medium",
            Self::EventLogging => "event_logging",
            Self::Low => "low",
            Self::Normal => "normal",
        }
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct Reference {
    #[serde(rename = "use")]
    pub name: String,
    #[serde(default)]
    pub instance: u16,
    #[serde(default = "all_boards")]
    pub board_mask: u16,
    #[serde(default = "normal_priority")]
    pub priority: Vec<Priority>,
}

/// Explicit numerical identities are needed for unrecognized enum values too,
/// not only unrecognized IDs. They never bypass the token storage-width check.
#[derive(Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum Token<T> {
    Named(T),
    Numeric(NumericToken),
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NumericToken {
    pub id: u32,
    pub value: u32,
}

macro_rules! token_sets {
    ($($variant:ident($ty:ty)),* $(,)?) => {
        #[derive(Deserialize, JsonSchema)]
        #[serde(tag="width", rename_all="lowercase", deny_unknown_fields)]
        pub enum TokenSet {
            $( $variant { values: Vec<Token<$ty>> }, )*
        }
        impl TokenSet {
            fn lower(&self,selection:&Reference) -> Result<Value> {
                match self {
                    $(Self::$variant { values } => {
                        let class = stringify!($variant);
                        let (id,max) = match class {
                            "Bool"=>(0,1), "Byte"=>(1,0xff), "Word"=>(2,0xffff), "Dword"=>(4,u32::MAX), _=>unreachable!(),
                        };
                        let mut tokens = BTreeMap::new();
                        for token in values {
                            let (key,value) = match token {
                                Token::Named(value) => {
                                    let raw = TOKEN_ENTRY::try_from(value).map_err(|e|format!("{e:?}"))?;
                                    (raw.key.get(),json!({(class):serde_json::to_value(value).map_err(|e|e.to_string())?}))
                                }
                                Token::Numeric(raw) => {
                                    if raw.value>max { return Err(format!("numeric {class} token {:#x} exceeds storage width",raw.id)); }
                                    (raw.id,json!({"Unknown":{"entry_id":class,"tag":raw.id,"value":raw.value}}))
                                }
                            };
                            if tokens.insert(key,value).is_some() {return Err(format!("duplicate {class} token {key:#x}"));}
                        }
                        Ok(json!({"header":entry_header(0x3000,id,selection.instance,selection.board_mask,&selection.priority,true)?,"tokens":tokens.into_values().collect::<Vec<_>>()}))
                    },)*
                }
            }
        }
    }
}
token_sets!(
    Bool(BoolToken),
    Byte(ByteToken),
    Word(WordToken),
    Dword(DwordToken)
);

/// Body layouts remain upstream Rust types. Semantic table kinds determine
/// their wire group/type IDs, so configuration cannot retag incompatible data.
#[derive(Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Table {
    BoardIdEeprom {
        controller: u16,
        address: u16,
        #[serde(rename = "id-offset")]
        id_offset: u16,
        #[serde(rename = "revision-offset")]
        revision_offset: u16,
        mapping: Vec<psp::IdRevApcbMapping>,
    },
    SpdSmbus {
        slots: Vec<SpdSlot>,
    },
    /// This prototype's overrides explicitly apply to every socket/channel/DIMM.
    PlatformOverride {
        scope: OverrideScope,
        operations: Vec<Override>,
    },
    RdimmOdt {
        rows: Vec<OdtRow>,
    },
    RdimmCad {
        rows: Vec<CadRow>,
    },
    RdimmData {
        rows: Vec<DataRow<memory::VrefDq>>,
    },
    RdimmMaxFrequency {
        rows: Vec<MaxFrequencyRow>,
    },
    RdimmStretchFrequency {
        rows: Vec<MaxFrequencyRow>,
    },
    ThreeDsMaxFrequency {
        rows: Vec<MaxFrequencyRow>,
    },
    ThreeDsStretchFrequency {
        rows: Vec<MaxFrequencyRow>,
    },
    ThreeDsData {
        rows: Vec<DataRow<memory::VrefDq>>,
    },
    Console {
        settings: memory::ConsoleOutControl,
    },
    ErrorOutput {
        settings: memory::ErrorOutControl112,
    },
    VoltageControl {
        settings: memory::ExtVoltageControl,
    },
    LrdimmOdt {
        rows: Vec<OdtRow>,
    },
    LrdimmCad {
        rows: Vec<CadRow>,
    },
    LrdimmData {
        rows: Vec<DataRow<u32>>,
    },
    LrdimmMaxFrequency {
        rows: Vec<MaxFrequencyRow>,
    },
    /// Required end marker; padding is emitted by the existing binary builder.
    // A tagged unit variant silently accepts extra fields, even with deny_unknown_fields.
    PlatformTuningEnd {},
    Slink {
        settings: df::SlinkConfig,
    },
    /// Initial empty repair-event capacity, not fabricated raw records.
    EmptyPpr {
        #[schemars(range(min = 1, max = 512))]
        slots: u16,
    },
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct SpdSlot {
    pub socket: u8,
    pub channel: u8,
    pub dimm: u8,
    pub address: u8,
    pub mux: u8,
    pub mux_channel: u8,
    #[serde(default)]
    pub mux_control: u8,
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum OverrideScope {
    All,
}

#[derive(Deserialize, JsonSchema)]
#[serde(tag = "operation", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Override {
    MemclkMap { connections: [u8; 8] },
    CkeTristateMap { connections: [u8; 4] },
    OdtTristateMap { connections: [u8; 4] },
    CsTristateMap { connections: [u8; 8] },
    MaxDimmsPerChannel { value: u8 },
    MaxChannelsPerSocket { value: u8 },
}
impl Override {
    fn lower(&self) -> Result<memory::platform_specific_override::Element> {
        let (name, field, value) = match self {
            Self::MemclkMap { connections } => ("MemclkMap", "connections", json!(connections)),
            Self::CkeTristateMap { connections } => {
                ("CkeTristateMap", "connections", json!(connections))
            }
            Self::OdtTristateMap { connections } => {
                ("OdtTristateMap", "connections", json!(connections))
            }
            Self::CsTristateMap { connections } => {
                ("CsTristateMap", "connections", json!(connections))
            }
            Self::MaxDimmsPerChannel { value } => ("MaxDimmsPerChannel", "value", json!(value)),
            Self::MaxChannelsPerSocket { value } => ("MaxChannelsPerSocket", "value", json!(value)),
        };
        serde_json::from_value(json!({(name):{
            "sockets":{"socket_0":true,"socket_1":true,"socket_2":true,"socket_3":true,"socket_4":true,"socket_5":true,"socket_6":true,"socket_7":true},
            "channels":"Any","dimms":"Any",(field):value,
        }})).map_err(|e|e.to_string())
    }
}

impl Table {
    fn lower(&self, generation: Generation) -> Result<(u16, u16, &'static str, Value)> {
        macro_rules! body {
            ($group:expr,$id:expr,$key:literal,$value:expr) => {
                Ok((
                    $group,
                    $id,
                    $key,
                    serde_json::to_value($value).map_err(|e| e.to_string())?,
                ))
            };
        }
        macro_rules! rows {
            ($group:expr,$id:expr,$key:literal,$rows:expr,$method:ident $(,$arg:expr)*) => {{
                let values = $rows
                    .iter()
                    .map(|row| row.$method($($arg),*))
                    .collect::<Result<Vec<_>>>()?;
                body!($group, $id, $key, values)
            }};
        }
        match self {
            Self::BoardIdEeprom {
                controller,
                address,
                id_offset,
                revision_offset,
                mapping,
            } => {
                let header: psp::BoardIdGettingMethodEeprom = serde_json::from_value(json!({
                    "access_method":2,"i2c_controller_index":controller,"device_address":address,
                    "board_id_offset":id_offset,"board_rev_offset":revision_offset,
                }))
                .map_err(|e| e.to_string())?;
                body!(
                    0x1701,
                    0x60,
                    "BoardIdGettingMethodEeprom",
                    (header, mapping)
                )
            }
            Self::SpdSmbus { slots } => {
                let mut seen = BTreeSet::new();
                let mut rows = Vec::new();
                for slot in slots {
                    if !seen.insert((slot.socket, slot.channel, slot.dimm)) {
                        return Err("duplicate SPD slot".into());
                    }
                    // Deserialize into the existing layout, not a second SPD encoder.
                    rows.push(serde_json::from_value::<memory::DimmInfoSmbusElement>(json!({
                        "dimm_slot_present":true,"socket_id":slot.socket,"channel_id":slot.channel,"dimm_id":slot.dimm,
                        "dimm_smbus_address":slot.address,"i2c_mux_address":slot.mux,
                        "mux_control_address":slot.mux_control,"mux_channel":slot.mux_channel,
                    })).map_err(|e|e.to_string())?);
                }
                body!(0x1704, 0x31, "DimmInfoSmbusElement", rows)
            }
            Self::PlatformOverride {
                scope: OverrideScope::All,
                operations,
            } => {
                let operations = operations
                    .iter()
                    .map(Override::lower)
                    .collect::<Result<Vec<_>>>()?;
                body!(0x1704, 0x40, "platform_specific_overrides", operations)
            }
            Self::RdimmOdt { rows } => rows!(0x1704, 0x46, "Ddr4OdtPatElement", rows, rdimm),
            Self::RdimmCad { rows } => rows!(0x1704, 0x47, "RdimmDdr4CadBusElement", rows, rdimm),
            Self::RdimmData { rows } => rows!(0x1704, 0x48, "Ddr4DataBusElement", rows, rdimm),
            Self::RdimmMaxFrequency { rows } => rows!(
                0x1704,
                0x49,
                "MaxFreqElement",
                rows,
                rdimm,
                generation.unsupported_speed()
            ),
            Self::RdimmStretchFrequency { rows } => {
                rows!(
                    0x1704,
                    0x4a,
                    "MaxFreqElement",
                    rows,
                    rdimm,
                    generation.unsupported_speed()
                )
            }
            Self::ThreeDsMaxFrequency { rows } => {
                rows!(
                    0x1704,
                    0x4b,
                    "MaxFreqElement",
                    rows,
                    rdimm,
                    generation.unsupported_speed()
                )
            }
            Self::ThreeDsStretchFrequency { rows } => {
                rows!(
                    0x1704,
                    0x4c,
                    "MaxFreqElement",
                    rows,
                    rdimm,
                    generation.unsupported_speed()
                )
            }
            Self::ThreeDsData { rows } => rows!(0x1704, 0x4d, "Ddr4DataBusElement", rows, rdimm),
            Self::Console { settings } => body!(0x1704, 0x50, "ConsoleOutControl", settings),
            Self::ErrorOutput { settings } => body!(0x1704, 0x52, "ErrorOutControl112", settings),
            Self::VoltageControl { settings } => body!(0x1704, 0x53, "ExtVoltageControl", settings),
            Self::LrdimmOdt { rows } => {
                rows!(0x1704, 0x54, "LrdimmDdr4OdtPatElement", rows, lrdimm)
            }
            Self::LrdimmCad { rows } => {
                rows!(0x1704, 0x55, "LrdimmDdr4CadBusElement", rows, lrdimm)
            }
            Self::LrdimmData { rows } => {
                rows!(0x1704, 0x56, "LrdimmDdr4DataBusElement", rows, lrdimm)
            }
            Self::LrdimmMaxFrequency { rows } => rows!(
                0x1704,
                0x57,
                "LrMaxFreqElement",
                rows,
                lrdimm,
                generation.unsupported_speed()
            ),
            Self::PlatformTuningEnd {} => {
                let end: memory::platform_tuning::Element =
                    serde_json::from_value(json!({"Terminator":{}})).map_err(|e| e.to_string())?;
                body!(0x1704, 0x75, "platform_tuning", vec![end])
            }
            Self::Slink { settings } => body!(0x1703, 0xcc, "SlinkConfig", settings),
            Self::EmptyPpr { slots } => {
                if !(1..=512).contains(slots) {
                    return Err("PPR slots must be in 1..=512".into());
                }
                body!(
                    0x1704,
                    0x5e,
                    "DdrPostPackageRepairElement",
                    vec![memory::DdrPostPackageRepairElement::default(); usize::from(*slots)]
                )
            }
        }
    }
}

fn entry_header(
    group: u16,
    id: u16,
    instance: u16,
    boards: u16,
    priority: &[Priority],
    tokens: bool,
) -> Result<Value> {
    if priority.is_empty()
        || priority.iter().copied().collect::<BTreeSet<_>>().len() != priority.len()
    {
        return Err("priority must be nonempty and contain no duplicates".into());
    }
    let priority: BTreeMap<_, _> = priority.iter().map(|p| (p.key(), true)).collect();
    Ok(
        json!({"group_id":group,"entry_id":id,"instance_id":instance,
        "context_type":if tokens{"Tokens"}else{"Struct"},"context_format":if tokens{"SortAscending"}else{"Raw"},
        "unit_size":if tokens{8}else{0},"key_size":if tokens{4}else{0},"key_pos":0,
        "priority_mask":priority,"board_instance_mask":boards}),
    )
}

impl Configuration {
    fn validate_schema(&self) -> Result<()> {
        if self.schema != 1 || self.blobs.is_empty() {
            return Err("expected authoring schema 1 and at least one APCB".into());
        }
        Ok(())
    }

    pub fn parse(source: &str) -> Result<Self> {
        let config: Self = toml::from_str(source).map_err(|e| e.to_string())?;
        config.validate_schema()?;
        Ok(config)
    }

    pub fn compile(mut self, shared: SharedTables) -> Result<Vec<apcb_v3::Replacement>> {
        self.validate_schema()?;
        if shared.schema != 1 {
            return Err("expected shared-table schema 1".into());
        }
        for (name, table) in shared.tables {
            if self.tables.insert(name.clone(), table).is_some() {
                return Err(format!("duplicate table {name}"));
            }
        }
        let mut blobs = Vec::new();
        let mut names = BTreeSet::new();
        for blob in self.blobs {
            if blob.name.is_empty() || !names.insert(blob.name.clone()) {
                return Err("APCB names must be nonempty and unique".into());
            }
            let mut entries = Vec::new();
            let mut groups = Vec::new();
            let mut group_ids = BTreeSet::new();
            for record in blob.records {
                let table = self
                    .tables
                    .get(&record.name)
                    .ok_or_else(|| format!("unknown table {}", record.name))?;
                let (group, id, key, body) = table
                    .lower(self.generation)
                    .map_err(|e| format!("{}: {e}", record.name))?;
                if group_ids.insert(group) {
                    groups.push(group_header(group)?);
                }
                entries.push(json!({"header":entry_header(group,id,record.instance,record.board_mask,&record.priority,false)?,(key):body}));
            }
            for selection in blob.tokens {
                let tokens = self
                    .token_sets
                    .get(&selection.name)
                    .ok_or_else(|| format!("unknown token set {}", selection.name))?;
                if group_ids.insert(0x3000) {
                    groups.push(group_header(0x3000)?);
                }
                entries.push(
                    tokens
                        .lower(&selection)
                        .map_err(|e| format!("{}: {e}", selection.name))?,
                );
            }
            // Stable wire defaults, including Rome's backwards end marker.
            // Container signatures/reserved bytes are not authoring inputs.
            let mut header = amd_apcb::V2_HEADER::default();
            header.header_size.set(128);
            header.set_unique_apcb_instance(blob.revision);
            let mut ext = amd_apcb::V3_HEADER_EXT::default();
            ext.signature_ending = *b"BCBA";
            let selectors: Vec<_> = blob
                .bindings
                .iter()
                .map(|b| b.selector(self.generation))
                .collect();
            blobs.push(json!({"name":blob.name,"selectors":selectors,"donor_sha256":blob.donor_sha256,
                "apcb":{"version":"0.5.5","header":header,"v3_header_ext":ext,"groups":groups,"entries":entries}}));
        }
        // Reuse the validated, lossless builder and existing replacement guards.
        apcb_v3::compile(
            &json!({"schema":1,"blobs":blobs}).to_string(),
            &apcb_v3::Tables::new(),
        )
    }
}

fn group_header(group: u16) -> Result<Value> {
    let signature = match group {
        0x1701 => "PSPG",
        0x1703 => "DFG ",
        0x1704 => "MEMG",
        0x3000 => "TOKN",
        _ => return Err("unsupported group".into()),
    };
    Ok(
        json!({"header":{"signature":signature,"group_id":group,"header_size":16,"version":1,"_reserved_":0}}),
    )
}

pub fn compile(source: &str, shared: &str) -> Result<Vec<apcb_v3::Replacement>> {
    Configuration::parse(source)?.compile(toml::from_str(shared).map_err(|e| e.to_string())?)
}

pub fn schema() -> schemars::schema::RootSchema {
    schemars::schema_for!(Configuration)
}
pub fn tables_schema() -> schemars::schema::RootSchema {
    schemars::schema_for!(SharedTables)
}

#[cfg(test)]
#[path = "apcb_toml_tests.rs"]
mod tests;
