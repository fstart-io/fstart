// SPDX-License-Identifier: MIT OR Apache-2.0
//! Typed adapters for firmware-family DDR4 models. No wire encoding.
use super::Result;
use amd_apcb::memory;
use num_traits::ToPrimitive;
use schemars::{JsonSchema, r#gen::SchemaGenerator, schema::Schema};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeSet;

const RATES: &[u16] = &[
    400, 533, 667, 800, 1066, 1333, 1600, 1866, 2133, 2400, 2667, 2933, 3200,
];
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(try_from = "u16")]
pub struct Rate(u16);
impl TryFrom<u16> for Rate {
    type Error = String;
    fn try_from(value: u16) -> Result<Self> {
        if RATES.contains(&value) {
            Ok(Self(value))
        } else {
            Err(format!("unsupported DDR transfer rate {value}"))
        }
    }
}
impl JsonSchema for Rate {
    fn schema_name() -> String {
        "Ddr4Rate".into()
    }
    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        let mut schema = u16::json_schema(generator).into_object();
        schema.enum_values = Some(RATES.iter().map(|rate| json!(rate)).collect());
        Schema::Object(schema)
    }
}
fn rates(input: &[Rate]) -> Result<memory::DdrRates> {
    if input.is_empty() || input.iter().copied().collect::<BTreeSet<_>>().len() != input.len() {
        return Err("rates must be nonempty and unique".into());
    }
    let mut value = memory::DdrRates::new();
    for rate in input {
        match rate.0 {
            400 => value.set_ddr400(true),
            533 => value.set_ddr533(true),
            667 => value.set_ddr667(true),
            800 => value.set_ddr800(true),
            1066 => value.set_ddr1066(true),
            1333 => value.set_ddr1333(true),
            1600 => value.set_ddr1600(true),
            1866 => value.set_ddr1866(true),
            2133 => value.set_ddr2133(true),
            2400 => value.set_ddr2400(true),
            2667 => value.set_ddr2667(true),
            2933 => value.set_ddr2933(true),
            3200 => value.set_ddr3200(true),
            _ => unreachable!(),
        }
    }
    Ok(value)
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Rank {
    Empty,
    Single,
    Dual,
    Quad,
    Lr,
}
fn ranks(input: &[Rank], lrdimm: bool) -> Result<Value> {
    if input.iter().copied().collect::<BTreeSet<_>>().len() != input.len() {
        return Err("duplicate rank flag".into());
    }
    if lrdimm {
        let mut value = memory::LrdimmDdr4DimmRanks::new();
        for rank in input {
            match rank {
                Rank::Empty => value.set_unpopulated(true),
                Rank::Lr => value.set_lr(true),
                _ => return Err("LRDIMM rows accept only empty/lr ranks".into()),
            }
        }
        serde_json::to_value(value).map_err(|e| e.to_string())
    } else {
        let mut value = memory::Ddr4DimmRanks::new();
        for rank in input {
            match rank {
                Rank::Empty => value.set_unpopulated(true),
                Rank::Single => value.set_single_rank(true),
                Rank::Dual => value.set_dual_rank(true),
                Rank::Quad => value.set_quad_rank(true),
                Rank::Lr => return Err("RDIMM rows do not accept lr ranks".into()),
            }
        }
        serde_json::to_value(value).map_err(|e| e.to_string())
    }
}

fn slots(input: &[u8]) -> Result<memory::DimmsPerChannelSelector> {
    if input.is_empty() || input.iter().copied().collect::<BTreeSet<_>>().len() != input.len() {
        return Err("slot selectors must be nonempty and unique".into());
    }
    let mut flags = memory::DimmsPerChannelSelector::new();
    for slot in input {
        match slot {
            1 => flags.set_one_dimm(true),
            2 => flags.set_two_dimms(true),
            3 => flags.set_three_dimms(true),
            4 => flags.set_four_dimms(true),
            _ => return Err("slot selectors must be in 1..=4".into()),
        }
    }
    Ok(flags)
}

pub(super) fn native<T: serde::de::DeserializeOwned>(value: Value) -> Result<T> {
    serde_json::from_value(value).map_err(|e| e.to_string())
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct CadRow {
    pub slots: Vec<u8>,
    pub rates: Vec<Rate>,
    pub rank0: Vec<Rank>,
    pub rank1: Vec<Rank>,
    #[serde(default)]
    pub gear_down: bool,
    #[serde(default)]
    pub slow: bool,
    pub addr_cmd: u32,
    /// CKE, CS/ODT, address/command, clock drive strengths, respectively.
    pub drive: [memory::CadBusClkDriveStrength; 4],
}
impl CadRow {
    fn value(&self, lr: bool) -> Result<Value> {
        let selected_slots = slots(&self.slots)?
            .to_u32()
            .ok_or("invalid slot selector")?;
        Ok(
            json!({"dimm_slots_per_channel":selected_slots,"ddr_rates":rates(&self.rates)?,
            "vdd_io":{"1.2 V":true},"dimm0_ranks":ranks(&self.rank0,lr)?,"dimm1_ranks":ranks(&self.rank1,lr)?,
            "gear_down_mode":if lr{json!(u16::from(self.gear_down))}else{json!(self.gear_down)},
            "slow_mode":if lr{json!(u16::from(self.slow))}else{json!(self.slow)},
            "address_command_control":self.addr_cmd,"cke_drive_strength":self.drive[0],"cs_odt_drive_strength":self.drive[1],
            "address_command_drive_strength":self.drive[2],"clk_drive_strength":self.drive[3]}),
        )
    }
    pub fn rdimm(&self) -> Result<memory::RdimmDdr4CadBusElement> {
        native(self.value(false)?)
    }
    pub fn lrdimm(&self) -> Result<memory::LrdimmDdr4CadBusElement> {
        native(self.value(true)?)
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct DataRow<V> {
    pub slots: Vec<u8>,
    pub rates: Vec<Rate>,
    pub rank0: Vec<Rank>,
    pub rank1: Vec<Rank>,
    pub rtt_nom: memory::RttNom,
    pub rtt_wr: memory::RttWr,
    pub rtt_park: memory::RttPark,
    /// DQ, DQS, ODT drive strengths, respectively.
    pub drive: [u32; 3],
    pub pmu_vref: u32,
    pub dram_vref: V,
}
impl<V: Serialize> DataRow<V> {
    fn value(&self, lr: bool) -> Result<Value> {
        let selected_slots = slots(&self.slots)?
            .to_u32()
            .ok_or("invalid slot selector")?;
        Ok(
            json!({"dimm_slots_per_channel":selected_slots,"ddr_rates":rates(&self.rates)?,"vdd_io":{"1.2 V":true},
            "dimm0_ranks":ranks(&self.rank0,lr)?,"dimm1_ranks":ranks(&self.rank1,lr)?,
            "rtt_nom":self.rtt_nom,"rtt_wr":self.rtt_wr,"rtt_park":self.rtt_park,
            "dq_drive_strength":self.drive[0],"dqs_drive_strength":self.drive[1],"odt_drive_strength":self.drive[2],
            "pmu_phy_vref":self.pmu_vref,"vref_dq":self.dram_vref}),
        )
    }
}
impl DataRow<memory::VrefDq> {
    pub fn rdimm(&self) -> Result<memory::Ddr4DataBusElement> {
        native(self.value(false)?)
    }
}
impl DataRow<u32> {
    pub fn lrdimm(&self) -> Result<memory::LrdimmDdr4DataBusElement> {
        native(self.value(true)?)
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OdtRow {
    pub ranks: [Vec<Rank>; 3],
    /// Per chip select: read pattern, write pattern (each a four-bit bitmap).
    pub patterns: [[u8; 2]; 4],
}
impl OdtRow {
    fn value(&self, lr: bool) -> Result<Value> {
        if self.patterns.iter().flatten().any(|&v| v > 15) {
            return Err("ODT patterns are four-bit bitmaps".into());
        }
        let patterns: Vec<_> = self
            .patterns
            .iter()
            .map(|p| json!({"reading_pattern":p[0],"writing_pattern":p[1]}))
            .collect();
        Ok(
            json!({"dimm_rank_bitmaps":{"dimm0":ranks(&self.ranks[0],lr)?,"dimm1":ranks(&self.ranks[1],lr)?,"dimm2":ranks(&self.ranks[2],lr)?},
            "cs0_odt_patterns":patterns[0],"cs1_odt_patterns":patterns[1],"cs2_odt_patterns":patterns[2],"cs3_odt_patterns":patterns[3]}),
        )
    }
    pub fn rdimm(&self) -> Result<memory::Ddr4OdtPatElement> {
        native(self.value(false)?)
    }
    pub fn lrdimm(&self) -> Result<memory::LrdimmDdr4OdtPatElement> {
        native(self.value(true)?)
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MaxFrequencyRow {
    pub slots: Vec<u8>,
    pub conditions: [u16; 4],
    pub speed: memory::DdrSpeed,
}
impl MaxFrequencyRow {
    pub fn rdimm(&self, unsupported: memory::DdrSpeed) -> Result<memory::MaxFreqElement> {
        Ok(memory::MaxFreqElement::new(
            unsupported,
            memory::DimmsPerChannel::Specific(slots(&self.slots)?),
            self.conditions[0],
            self.conditions[1],
            self.conditions[2],
            self.conditions[3],
            self.speed,
        ))
    }
    pub fn lrdimm(&self, unsupported: memory::DdrSpeed) -> Result<memory::LrMaxFreqElement> {
        Ok(memory::LrMaxFreqElement::new(
            unsupported,
            memory::DimmsPerChannel::Specific(slots(&self.slots)?),
            self.conditions[0],
            self.conditions[1],
            self.conditions[2],
            self.conditions[3],
            self.speed,
        ))
    }
}
