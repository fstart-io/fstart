// SPDX-License-Identifier: MIT OR Apache-2.0
//! Firmware ABI lowering. Family models are Rust code, not parsed templates.
//! Binary bookkeeping belongs to amd-apcb; JSON adapts its private Serde API.
#[path = "apcb_profiles/mod.rs"]
mod profiles;
use super::{Result, apcb_source};
use amd_apcb::{BoolToken, ByteToken, DwordToken, TOKEN_ENTRY, WordToken, df, memory, psp};
use num_traits::ToPrimitive;
pub use profiles::Profile;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
#[path = "apcb_layout_rows.rs"]
pub(super) mod rows;
use rows::{CadRow, DataRow, MaxFrequencyRow, OdtRow};
#[path = "apcb_layout_parameters.rs"]
mod legacy;
use legacy::{ParameterLayer, Setting};

pub struct Definition {
    pub generation: Generation,
    pub blobs: Vec<Blob>,
    pub tables: BTreeMap<String, Table>,
    pub token_sets: BTreeMap<String, TokenSet>,
    pub spd_images: BTreeMap<String, String>,
}

/// Adding another ABI is an explicit, checked change.
#[derive(Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Generation {
    Rome,
    Milan,
    Picasso,
    Zeppelin,
}
impl Generation {
    pub(super) fn unsupported_speed(self) -> memory::DdrSpeed {
        match self {
            Self::Rome => memory::DdrSpeed::UnsupportedRome,
            Self::Milan => memory::DdrSpeed::UnsupportedMilan,
            Self::Picasso => memory::DdrSpeed::UnsupportedRome,
            Self::Zeppelin => memory::DdrSpeed::UnsupportedZeppelin,
        }
    }
    fn legacy(self) -> bool {
        matches!(self, Self::Picasso | Self::Zeppelin)
    }
}

#[derive(Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct Blob {
    pub name: String,
    pub revision: u32,
    pub donor_sha256: String,
    #[schemars(length(min = 1))]
    pub bindings: Vec<Binding>,
    /// Explicit group ordering also retains intentionally empty override groups.
    #[serde(default)]
    pub groups: Vec<Group>,
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

#[derive(Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub slot: Slot,
    #[serde(default)]
    pub instance: u8,
}

impl Binding {
    pub fn selector(&self, generation: Generation) -> apcb_source::Selector {
        use fstart_amdfw_format::directory::bios;
        apcb_source::Selector {
            kind: match self.slot {
                Slot::Active => bios::APCB,
                Slot::Backup => bios::APCB_BACKUP,
            },
            instance: self.instance,
            subprogram: match generation {
                Generation::Rome => 0,
                Generation::Milan => 1,
                Generation::Picasso | Generation::Zeppelin => 0,
            },
        }
    }
}

#[derive(Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Group {
    Psp,
    Ccx,
    Df,
    Memory,
    Gnb,
    Fch,
    Cbs,
    Token,
}
impl Group {
    fn id(self) -> u16 {
        use amd_apcb::GroupId;
        match self {
            Self::Psp => GroupId::Psp,
            Self::Ccx => GroupId::Ccx,
            Self::Df => GroupId::Df,
            Self::Memory => GroupId::Memory,
            Self::Gnb => GroupId::Gnb,
            Self::Fch => GroupId::Fch,
            Self::Cbs => GroupId::Cbs,
            Self::Token => GroupId::Token,
        }
        .to_u16()
        .unwrap()
    }
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

#[derive(Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct Reference {
    #[serde(rename = "use")]
    pub name: String,
    #[serde(default)]
    pub instance: u16,
    #[serde(default)]
    pub board_mask: Option<u16>,
    #[serde(default)]
    pub priority: Option<Vec<Priority>>,
}

impl Reference {
    fn header(&self, group: u16, id: u16, generation: Generation, tokens: bool) -> Result<Value> {
        let priority = self.priority.as_deref().unwrap_or(if generation.legacy() {
            &[]
        } else {
            &[Priority::Normal]
        });
        entry_header(
            group,
            id,
            self.instance,
            self.board_mask
                .unwrap_or(if generation.legacy() { 0 } else { u16::MAX }),
            priority,
            tokens,
            generation.legacy(),
        )
    }
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
            fn lower(&self,selection:&Reference,generation:Generation) -> Result<Value> {
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
                        Ok(json!({"header":selection.header(0x3000,id,generation,true)?,"tokens":tokens.into_values().collect::<Vec<_>>()}))
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
    Parameters {
        group: Group,
        layer: ParameterLayer,
        values: Vec<Setting>,
    },
    BoardIdGpio {
        pins: [memory::Gpio; 4],
        mapping: Vec<psp::IdApcbMapping>,
    },
    SpdInfo {
        slots: Vec<legacy::SpdSlot>,
    },
    UdimmOdt {
        rows: Vec<OdtRow>,
    },
    UdimmCad {
        rows: Vec<memory::UdimmDdr4CadBusElement>,
    },
    LegacyUdimmData {
        rows: Vec<memory::LegacyDdr4DataBusElement44>,
    },
    UdimmMaxFrequency {
        rows: Vec<MaxFrequencyRow>,
    },
    UdimmStretchFrequency {
        rows: Vec<MaxFrequencyRow>,
    },
    LegacyRdimmData {
        rows: Vec<memory::LegacyDdr4DataBusElement44>,
    },
    LegacyLrdimmData {
        rows: Vec<memory::LegacyLrdimmDdr4DataBusElement44>,
    },
    LegacyConsole {
        settings: memory::NaplesConsoleOutControl,
    },
    LegacyErrorOutput {
        settings: memory::ErrorOutControl108,
    },
    SodimmOdt {
        rows: Vec<OdtRow>,
    },
    SodimmCad {
        rows: Vec<memory::UdimmDdr4CadBusElement>,
    },
    SodimmData {
        rows: Vec<DataRow<memory::VrefDq>>,
    },
    SodimmMaxFrequency {
        rows: Vec<MaxFrequencyRow>,
    },
    SodimmStretchFrequency {
        rows: Vec<MaxFrequencyRow>,
    },
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
    /// Overrides explicitly apply to every socket/channel/DIMM.
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
    MilanErrorOutput {
        settings: memory::ErrorOutControl116,
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
        #[schemars(range(min = 0, max = 512))]
        slots: u16,
    },
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct SpdSlot {
    #[serde(default = "slot_present")]
    pub present: bool,
    pub socket: u8,
    pub channel: u8,
    pub dimm: u8,
    pub address: u8,
    pub mux: u8,
    pub mux_channel: u8,
    #[serde(default)]
    pub mux_control: u8,
}

fn slot_present() -> bool {
    true
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
    fn lower(
        &self,
        generation: Generation,
        spds: &BTreeMap<String, String>,
    ) -> Result<(u16, u16, &'static str, Value)> {
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
        let compatible = match self {
            Self::LegacyUdimmData { .. }
            | Self::LegacyRdimmData { .. }
            | Self::LegacyLrdimmData { .. }
            | Self::LegacyErrorOutput { .. } => matches!(generation, Generation::Zeppelin),
            Self::LegacyConsole { .. } => generation.legacy(),
            Self::SpdInfo { .. }
            | Self::BoardIdGpio { .. }
            | Self::SodimmOdt { .. }
            | Self::SodimmCad { .. }
            | Self::SodimmData { .. }
            | Self::SodimmMaxFrequency { .. }
            | Self::SodimmStretchFrequency { .. } => matches!(generation, Generation::Picasso),
            Self::ErrorOutput { .. } => {
                matches!(generation, Generation::Rome | Generation::Picasso)
            }
            Self::MilanErrorOutput { .. } => matches!(generation, Generation::Milan),
            Self::Console { .. }
            | Self::RdimmData { .. }
            | Self::LrdimmData { .. }
            | Self::ThreeDsData { .. }
            | Self::BoardIdEeprom { .. }
            | Self::PlatformTuningEnd {}
            | Self::Slink { .. } => !generation.legacy(),
            _ => true,
        };
        if !compatible {
            return Err("table layout is incompatible with this APCB generation".into());
        }
        match self {
            Self::Parameters {
                group,
                layer,
                values,
            } => {
                if !generation.legacy() {
                    return Err("parameter lists require APCB v2".into());
                }
                legacy::parameters(*group, *layer, values)
            }
            Self::BoardIdGpio { pins, mapping } => {
                let header: psp::BoardIdGettingMethodGpio =
                    serde_json::from_value(json!({"access_method":3,"bit_locations":pins}))
                        .map_err(|e| e.to_string())?;
                body!(0x1701, 0x60, "BoardIdGettingMethodGpio", (header, mapping))
            }
            Self::SpdInfo { slots } => {
                let header: memory::SpdInfo =
                    serde_json::from_value(json!({"total_dimms":slots.len()}))
                        .map_err(|e| e.to_string())?;
                let slots = slots
                    .iter()
                    .map(|slot| {
                        let data = match &slot.image {
                            Some(name) => super::apcb::parse_spd_hex(
                                spds.get(name)
                                    .ok_or_else(|| format!("unknown SPD image {name}"))?,
                            )?,
                            None => [0; 512],
                        };
                        Ok(memory::SpdDimm {
                            descriptor: slot.descriptor,
                            data: memory::SpdData(data),
                        })
                    })
                    .collect::<Result<Vec<_>>>()?;
                body!(0x1704, 0x30, "SpdInfo", (header, slots))
            }
            Self::UdimmOdt { rows } => rows!(0x1704, 0x41, "Ddr4OdtPatElement", rows, rdimm),
            Self::UdimmCad { rows } => body!(0x1704, 0x42, "UdimmDdr4CadBusElement", rows),
            Self::LegacyUdimmData { rows } => {
                body!(0x1704, 0x43, "LegacyDdr4DataBusElement44", rows)
            }
            Self::UdimmMaxFrequency { rows } => rows!(
                0x1704,
                0x44,
                "MaxFreqElement",
                rows,
                rdimm,
                generation.unsupported_speed()
            ),
            Self::UdimmStretchFrequency { rows } => rows!(
                0x1704,
                0x45,
                "MaxFreqElement",
                rows,
                rdimm,
                generation.unsupported_speed()
            ),
            Self::LegacyRdimmData { rows } => {
                body!(0x1704, 0x48, "LegacyDdr4DataBusElement44", rows)
            }
            Self::LegacyLrdimmData { rows } => {
                body!(0x1704, 0x56, "LegacyLrdimmDdr4DataBusElement44", rows)
            }
            Self::LegacyConsole { settings } => {
                body!(0x1704, 0x50, "NaplesConsoleOutControl", settings)
            }
            Self::LegacyErrorOutput { settings } => {
                body!(0x1704, 0x52, "ErrorOutControl108", settings)
            }
            Self::SodimmOdt { rows } => rows!(0x1704, 0x59, "Ddr4OdtPatElement", rows, rdimm),
            Self::SodimmCad { rows } => body!(0x1704, 0x5a, "UdimmDdr4CadBusElement", rows),
            Self::SodimmData { rows } => rows!(0x1704, 0x5b, "Ddr4DataBusElement", rows, rdimm),
            Self::SodimmMaxFrequency { rows } => rows!(
                0x1704,
                0x5c,
                "MaxFreqElement",
                rows,
                rdimm,
                generation.unsupported_speed()
            ),
            Self::SodimmStretchFrequency { rows } => rows!(
                0x1704,
                0x5d,
                "MaxFreqElement",
                rows,
                rdimm,
                generation.unsupported_speed()
            ),
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
                        "dimm_slot_present":slot.present,"socket_id":slot.socket,"channel_id":slot.channel,"dimm_id":slot.dimm,
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
                let mut operations = operations
                    .iter()
                    .map(Override::lower)
                    .collect::<Result<Vec<_>>>()?;
                if generation.legacy() {
                    // The legacy PSP_PSO_STRUCT reserves 108 bytes, including
                    // unused operation slots. Do not expose zero padding as ops.
                    use amd_apcb::ElementAsBytes;
                    let used: usize = operations
                        .iter()
                        .map(|op| op.element_as_bytes().len())
                        .sum();
                    if used > 108 {
                        return Err("legacy platform overrides exceed reserved capacity".into());
                    }
                    if used < 108 {
                        operations.push(memory::platform_specific_override::Element::Unknown(
                            vec![0; 108 - used],
                        ));
                    }
                }
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
            Self::MilanErrorOutput { settings } => {
                body!(0x1704, 0x52, "ErrorOutControl116", settings)
            }
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
                if *slots > 512 || (!generation.legacy() && *slots == 0) {
                    return Err("invalid PPR capacity for this APCB generation".into());
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
    legacy: bool,
) -> Result<Value> {
    if (!legacy && priority.is_empty())
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

impl Definition {
    #[cfg(test)]
    pub fn compile(self) -> Result<Vec<apcb_source::Replacement>> {
        apcb_source::compile_sources(self.lower()?, &apcb_source::Tables::new())
    }

    pub(crate) fn lower(self) -> Result<apcb_source::Sources> {
        if self.blobs.is_empty() {
            return Err("firmware model must define at least one APCB".into());
        }
        let mut blobs = Vec::new();
        let mut names = BTreeSet::new();
        for blob in self.blobs {
            if self.generation.legacy() && !blob.tokens.is_empty() {
                return Err("token entries require APCB v3; use v2 parameter lists".into());
            }
            if blob.name.is_empty() || !names.insert(blob.name.clone()) {
                return Err("APCB names must be nonempty and unique".into());
            }
            let mut entries = Vec::new();
            let mut groups = Vec::new();
            let mut group_ids = BTreeSet::new();
            for group in blob.groups {
                let id = group.id();
                if !group_ids.insert(id) {
                    return Err("duplicate APCB group".into());
                }
                groups.push(group_header(id)?);
            }
            for record in blob.records {
                let table = self
                    .tables
                    .get(&record.name)
                    .ok_or_else(|| format!("unknown table {}", record.name))?;
                let (group, id, key, body) = table
                    .lower(self.generation, &self.spd_images)
                    .map_err(|e| format!("{}: {e}", record.name))?;
                if group_ids.insert(group) {
                    groups.push(group_header(group)?);
                }
                entries.push(
                    json!({"header":record.header(group,id,self.generation,false)?,(key):body}),
                );
            }
            if self.generation.legacy() && !blob.tokens.is_empty() {
                return Err("APCB v2 uses parameter lists, not token records".into());
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
                        .lower(&selection, self.generation)
                        .map_err(|e| format!("{}: {e}", selection.name))?,
                );
            }
            // Stable wire defaults, including Rome's backwards end marker.
            // Container signatures/reserved bytes are not authoring inputs.
            let mut header = amd_apcb::V2_HEADER::default();
            header
                .header_size
                .set(if self.generation.legacy() { 32 } else { 128 });
            header
                .version
                .set(if self.generation.legacy() { 0x20 } else { 0x30 });
            header.set_unique_apcb_instance(blob.revision);
            let mut ext = amd_apcb::V3_HEADER_EXT::default();
            ext.signature_ending = *b"BCBA";
            let ext = if self.generation.legacy() {
                None
            } else {
                Some(ext)
            };
            let selectors: Vec<_> = blob
                .bindings
                .iter()
                .map(|b| b.selector(self.generation))
                .collect();
            blobs.push(json!({"name":blob.name,"selectors":selectors,"donor_sha256":blob.donor_sha256,
                "apcb":{"version":"0.5.5","header":header,"v3_header_ext":ext,"groups":groups,"entries":entries}}));
        }
        serde_json::from_value(json!({"schema":1,"blobs":blobs})).map_err(|e| e.to_string())
    }
}

fn group_header(group: u16) -> Result<Value> {
    let signature = match group {
        0x1701 => "PSPG",
        0x1702 => "CCXG",
        0x1703 => "DFG ",
        0x1704 => "MEMG",
        0x1705 => "GNBG",
        0x1706 => "FCHG",
        0x1707 => "CBSG",
        0x3000 => "TOKN",
        _ => return Err("unsupported group".into()),
    };
    Ok(
        json!({"header":{"signature":signature,"group_id":group,"header_size":16,"version":1,"_reserved_":0}}),
    )
}

#[cfg(test)]
#[path = "apcb_profile_tests.rs"]
mod tests;
