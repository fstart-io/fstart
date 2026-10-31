// SPDX-License-Identifier: MIT OR Apache-2.0
//! Legacy parameter authoring; attributes and termination belong to amd-apcb.
use super::{Group, Result};
use amd_apcb::{EntryId, Parameter, ParameterAttributes, ParameterTimePoint, ParameterTokenConfig};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

#[derive(Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum ParameterLayer {
    Default,
    Override,
    Debug,
    DebugDummy,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SpdSlot {
    pub descriptor: amd_apcb::memory::SpdDescriptor,
    /// A named JEDEC SPD image; omission reserves a zeroed training scratch area.
    #[serde(default)]
    pub image: Option<String>,
}
#[derive(Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Width {
    U8,
    U16,
    U32,
    U64,
}
impl Width {
    fn bytes(self) -> u8 {
        match self {
            Self::U8 => 1,
            Self::U16 => 2,
            Self::U32 => 4,
            Self::U64 => 8,
        }
    }
}
fn any_time() -> ParameterTimePoint {
    ParameterTimePoint::Any
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Setting {
    pub token: ParameterTokenConfig,
    pub width: Width,
    pub value: u64,
    #[serde(default = "any_time")]
    pub time: ParameterTimePoint,
}
impl Setting {
    fn lower(&self) -> Result<Parameter> {
        if self.token == ParameterTokenConfig::Limit {
            return Err("parameter termination is generated, not authored".into());
        }
        let width = self.width.bytes();
        if width < 8 && self.value.checked_shr(u32::from(width) * 8).unwrap_or(0) != 0 {
            return Err("parameter exceeds declared storage width".into());
        }
        let attrs = ParameterAttributes::new()
            .with_time_point(self.time)
            .with_token(self.token)
            .with_size_minus_one(width - 1);
        Parameter::new(&attrs, self.value).map_err(|e| format!("{e:?}"))
    }
}

pub fn parameters(
    group: Group,
    layer: ParameterLayer,
    values: &[Setting],
) -> Result<(u16, u16, &'static str, Value)> {
    use ParameterLayer::{Debug, DebugDummy, Default as Def, Override as Ovr};
    use amd_apcb::{
        CbsEntryId as B, CcxEntryId as C, DfEntryId as D, FchEntryId as F, GnbEntryId as G,
        MemoryEntryId as M, PspEntryId as P,
    };
    let id = match (group, layer) {
        (Group::Psp, Def) => EntryId::Psp(P::DefaultParameters),
        (Group::Psp, Ovr) => EntryId::Psp(P::Parameters),
        (Group::Ccx, Def) => EntryId::Ccx(C::DefaultParameters),
        (Group::Ccx, Ovr) => EntryId::Ccx(C::Parameters),
        (Group::Df, Def) => EntryId::Df(D::DefaultParameters),
        (Group::Df, Ovr) => EntryId::Df(D::Parameters),
        (Group::Memory, Def) => EntryId::Memory(M::DefaultParameters),
        (Group::Memory, Ovr) => EntryId::Memory(M::Parameters),
        (Group::Gnb, Def) => EntryId::Gnb(G::DefaultParameters),
        (Group::Gnb, Ovr) => EntryId::Gnb(G::Parameters),
        (Group::Fch, Def) => EntryId::Fch(F::DefaultParameters),
        (Group::Fch, Ovr) => EntryId::Fch(F::Parameters),
        (Group::Cbs, Def) => EntryId::Cbs(B::DefaultParameters),
        (Group::Cbs, Ovr) => EntryId::Cbs(B::Parameters),
        (Group::Cbs, Debug) => EntryId::Cbs(B::DebugParameters),
        (Group::Cbs, DebugDummy) => EntryId::Cbs(B::DebugDummyParameters),
        _ => return Err("invalid legacy parameter group/layer".into()),
    };
    let values = values
        .iter()
        .map(Setting::lower)
        .collect::<Result<Vec<_>>>()?;
    Ok((
        group.id(),
        id.type_id(),
        "parameters",
        serde_json::to_value(values).map_err(|e| e.to_string())?,
    ))
}
