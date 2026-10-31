// SPDX-License-Identifier: MIT OR Apache-2.0
// Reviewed firmware defaults. Rust owns the ABI; these are not user templates.
use super::*;
fn electrical_rdimm_odt() -> Result<Table> {
    Ok(Table::RdimmOdt {
        rows: native(
            json!([{"ranks":[["empty"],["single"],[]],"patterns":[[0,0],[0,0],[0,0],[0,0]]},{"ranks":[["empty"],["dual"],[]],"patterns":[[0,0],[0,0],[0,2],[0,8]]},{"ranks":[["single"],["empty"],[]],"patterns":[[0,0],[0,0],[0,0],[0,0]]},{"ranks":[["dual"],["empty"],[]],"patterns":[[0,1],[0,4],[0,0],[0,0]]},{"ranks":[["single"],["single"],[]],"patterns":[[0,0],[0,0],[0,0],[0,0]]},{"ranks":[["single"],["dual"],[]],"patterns":[[2,2],[0,0],[1,1],[1,1]]},{"ranks":[["dual"],["single"],[]],"patterns":[[2,2],[2,2],[1,1],[0,0]]},{"ranks":[["dual"],["dual"],[]],"patterns":[[10,10],[10,10],[5,5],[5,5]]}]),
        )?,
    })
}
fn electrical_rdimm_cad() -> Result<Table> {
    Ok(Table::RdimmCad {
        rows: native(
            json!([{"slots":[1],"rates":[1600],"rank0":["single","dual"],"rank1":["empty"],"addr-cmd":3750201,"drive":["30 Ω","30 Ω","30 Ω","30 Ω"]},{"slots":[1],"rates":[1866],"rank0":["single","dual"],"rank1":["empty"],"addr-cmd":3618615,"drive":["30 Ω","30 Ω","30 Ω","30 Ω"]},{"slots":[1],"rates":[2133],"rank0":["single","dual"],"rank1":["empty"],"addr-cmd":3487029,"drive":["30 Ω","30 Ω","30 Ω","30 Ω"]},{"slots":[1],"rates":[2400],"rank0":["single","dual"],"rank1":["empty"],"addr-cmd":3355443,"drive":["30 Ω","30 Ω","30 Ω","30 Ω"]},{"slots":[1],"rates":[2667],"rank0":["single","dual"],"rank1":["empty"],"addr-cmd":3223857,"drive":["30 Ω","30 Ω","30 Ω","30 Ω"]},{"slots":[1],"rates":[2933],"rank0":["single","dual"],"rank1":["empty"],"addr-cmd":3092271,"drive":["30 Ω","30 Ω","30 Ω","30 Ω"]},{"slots":[1],"rates":[3200],"rank0":["single","dual"],"rank1":["empty"],"addr-cmd":2960685,"drive":["30 Ω","30 Ω","30 Ω","30 Ω"]},{"slots":[2],"rates":[1600],"rank0":["empty"],"rank1":["single","dual"],"addr-cmd":3750201,"drive":["30 Ω","30 Ω","30 Ω","30 Ω"]},{"slots":[2],"rates":[1600],"rank0":["single","dual"],"rank1":["empty"],"addr-cmd":3750201,"drive":["30 Ω","30 Ω","30 Ω","30 Ω"]},{"slots":[2],"rates":[1600],"rank0":["single","dual"],"rank1":["single","dual"],"addr-cmd":3488057,"drive":["30 Ω","30 Ω","30 Ω","30 Ω"]},{"slots":[2],"rates":[1866],"rank0":["empty"],"rank1":["single","dual"],"addr-cmd":3618615,"drive":["30 Ω","30 Ω","30 Ω","30 Ω"]},{"slots":[2],"rates":[1866],"rank0":["single","dual"],"rank1":["empty"],"addr-cmd":3618615,"drive":["30 Ω","30 Ω","30 Ω","30 Ω"]},{"slots":[2],"rates":[1866],"rank0":["single"],"rank1":["single"],"addr-cmd":3356985,"drive":["30 Ω","30 Ω","30 Ω","30 Ω"]},{"slots":[2],"rates":[1866],"rank0":["single"],"rank1":["dual"],"addr-cmd":3356471,"drive":["30 Ω","30 Ω","30 Ω","30 Ω"]},{"slots":[2],"rates":[1866],"rank0":["dual"],"rank1":["single","dual"],"addr-cmd":3356471,"drive":["30 Ω","30 Ω","30 Ω","30 Ω"]},{"slots":[2],"rates":[2133],"rank0":["empty"],"rank1":["single","dual"],"addr-cmd":3487029,"drive":["30 Ω","30 Ω","30 Ω","30 Ω"]},{"slots":[2],"rates":[2133],"rank0":["single","dual"],"rank1":["empty"],"addr-cmd":3487029,"drive":["30 Ω","30 Ω","30 Ω","30 Ω"]},{"slots":[2],"rates":[2133],"rank0":["single","dual"],"rank1":["single","dual"],"addr-cmd":3224885,"drive":["30 Ω","30 Ω","30 Ω","30 Ω"]},{"slots":[2],"rates":[2400],"rank0":["empty"],"rank1":["single","dual"],"addr-cmd":3355443,"drive":["30 Ω","30 Ω","30 Ω","30 Ω"]},{"slots":[2],"rates":[2400],"rank0":["single","dual"],"rank1":["empty"],"addr-cmd":3355443,"drive":["30 Ω","30 Ω","30 Ω","30 Ω"]},{"slots":[2],"rates":[2400],"rank0":["single","dual"],"rank1":["single","dual"],"addr-cmd":3093299,"drive":["30 Ω","30 Ω","30 Ω","30 Ω"]},{"slots":[2],"rates":[2667],"rank0":["empty"],"rank1":["single","dual"],"addr-cmd":3223857,"drive":["30 Ω","30 Ω","30 Ω","30 Ω"]},{"slots":[2],"rates":[2667],"rank0":["single","dual"],"rank1":["empty"],"addr-cmd":3223857,"drive":["30 Ω","30 Ω","30 Ω","30 Ω"]},{"slots":[2],"rates":[2667],"rank0":["single","dual"],"rank1":["single","dual"],"addr-cmd":2961713,"drive":["30 Ω","30 Ω","30 Ω","30 Ω"]},{"slots":[2],"rates":[2933],"rank0":["empty"],"rank1":["single","dual"],"addr-cmd":3092271,"drive":["30 Ω","30 Ω","30 Ω","30 Ω"]},{"slots":[2],"rates":[2933],"rank0":["single","dual"],"rank1":["empty"],"addr-cmd":3092271,"drive":["30 Ω","30 Ω","30 Ω","30 Ω"]},{"slots":[2],"rates":[2933],"rank0":["single","dual"],"rank1":["single","dual"],"addr-cmd":2895663,"drive":["30 Ω","30 Ω","30 Ω","30 Ω"]},{"slots":[2],"rates":[3200],"rank0":["empty"],"rank1":["single","dual"],"addr-cmd":2960685,"drive":["30 Ω","30 Ω","30 Ω","30 Ω"]},{"slots":[2],"rates":[3200],"rank0":["single","dual"],"rank1":["empty"],"addr-cmd":2960685,"drive":["30 Ω","30 Ω","30 Ω","30 Ω"]},{"slots":[2],"rates":[3200],"rank0":["single","dual"],"rank1":["single","dual"],"addr-cmd":2764077,"drive":["30 Ω","30 Ω","30 Ω","30 Ω"]}]),
        )?,
    })
}
fn electrical_rdimm_data() -> Result<Table> {
    Ok(Table::RdimmData {
        rows: native(
            json!([{"slots":[1],"rates":[1600,1866,2133,2400,2667,2933,3200],"rank0":["single"],"rank1":["empty"],"rtt-nom":"Off","rtt-wr":"Off","rtt-park":"48 Ω","drive":[62,62,24],"pmu-vref":91,"dram-vref":{"Range1":"74.95%"}},{"slots":[1],"rates":[1600,1866,2133,2400,2667,2933,3200],"rank0":["dual"],"rank1":["empty"],"rtt-nom":"60 Ω","rtt-wr":"Off","rtt-park":"240 Ω","drive":[62,62,24],"pmu-vref":93,"dram-vref":{"Range1":"74.95%"}},{"slots":[2],"rates":[1600,1866,2133,2400,2667,2933,3200],"rank0":["empty"],"rank1":["single"],"rtt-nom":"Off","rtt-wr":"Off","rtt-park":"48 Ω","drive":[62,62,24],"pmu-vref":91,"dram-vref":{"Range1":"74.95%"}},{"slots":[2],"rates":[1600,1866,2133,2400,2667,2933,3200],"rank0":["empty"],"rank1":["dual"],"rtt-nom":"60 Ω","rtt-wr":"Off","rtt-park":"240 Ω","drive":[62,62,24],"pmu-vref":93,"dram-vref":{"Range1":"74.95%"}},{"slots":[2],"rates":[1600,1866,2133,2400,2667,2933,3200],"rank0":["single"],"rank1":["empty"],"rtt-nom":"Off","rtt-wr":"Off","rtt-park":"48 Ω","drive":[62,62,24],"pmu-vref":91,"dram-vref":{"Range1":"74.95%"}},{"slots":[2],"rates":[1600,1866,2133,2400,2667,2933,3200],"rank0":["single"],"rank1":["single"],"rtt-nom":"Off","rtt-wr":"80 Ω","rtt-park":"34 Ω","drive":[62,62,24],"pmu-vref":104,"dram-vref":{"Range1":"78.85%"}},{"slots":[2],"rates":[1600,1866,2133,2400,2667,2933,3200],"rank0":["single"],"rank1":["dual"],"rtt-nom":"34 Ω","rtt-wr":"120 Ω","rtt-park":"240 Ω","drive":[62,62,24],"pmu-vref":103,"dram-vref":{"Range1":"80.80%"}},{"slots":[2],"rates":[1600,1866,2133,2400,2667,2933,3200],"rank0":["dual"],"rank1":["empty"],"rtt-nom":"60 Ω","rtt-wr":"Off","rtt-park":"240 Ω","drive":[62,62,24],"pmu-vref":93,"dram-vref":{"Range1":"74.95%"}},{"slots":[2],"rates":[1600,1866,2133,2400,2667,2933,3200],"rank0":["dual"],"rank1":["single"],"rtt-nom":"34 Ω","rtt-wr":"120 Ω","rtt-park":"240 Ω","drive":[62,62,24],"pmu-vref":103,"dram-vref":{"Range1":"80.80%"}},{"slots":[2],"rates":[1600,1866,2133,2400,2667,2933,3200],"rank0":["dual"],"rank1":["dual"],"rtt-nom":"60 Ω","rtt-wr":"120 Ω","rtt-park":"240 Ω","drive":[62,62,24],"pmu-vref":106,"dram-vref":{"Range1":"79.50%"}}]),
        )?,
    })
}
fn electrical_three_ds_data() -> Result<Table> {
    Ok(Table::ThreeDsData {
        rows: native(
            json!([{"slots":[1],"rates":[1600,1866,2133,2400,2667,2933,3200],"rank0":["dual"],"rank1":["empty"],"rtt-nom":"60 Ω","rtt-wr":"Off","rtt-park":"240 Ω","drive":[62,62,24],"pmu-vref":91,"dram-vref":{"Range1":"71.70%"}},{"slots":[2],"rates":[1600,1866,2133,2400,2667,2933,3200],"rank0":["empty"],"rank1":["dual"],"rtt-nom":"60 Ω","rtt-wr":"Off","rtt-park":"240 Ω","drive":[62,62,24],"pmu-vref":91,"dram-vref":{"Range1":"71.70%"}},{"slots":[2],"rates":[1600,1866,2133,2400,2667,2933,3200],"rank0":["dual"],"rank1":["empty"],"rtt-nom":"60 Ω","rtt-wr":"Off","rtt-park":"240 Ω","drive":[62,62,24],"pmu-vref":91,"dram-vref":{"Range1":"71.70%"}},{"slots":[2],"rates":[1600,1866,2133,2400,2667,2933,3200],"rank0":["dual"],"rank1":["dual"],"rtt-nom":"60 Ω","rtt-wr":"120 Ω","rtt-park":"240 Ω","drive":[62,62,24],"pmu-vref":104,"dram-vref":{"Range1":"77.55%"}}]),
        )?,
    })
}
fn electrical_lrdimm_odt() -> Result<Table> {
    Ok(Table::LrdimmOdt {
        rows: native(
            json!([{"ranks":[["empty"],["lr"],[]],"patterns":[[0,0],[0,0],[0,0],[0,0]]},{"ranks":[["lr"],["empty"],[]],"patterns":[[0,0],[0,0],[0,0],[0,0]]},{"ranks":[["lr"],["lr"],[]],"patterns":[[0,0],[0,0],[0,0],[0,0]]}]),
        )?,
    })
}
fn electrical_lrdimm_cad() -> Result<Table> {
    Ok(Table::LrdimmCad {
        rows: native(
            json!([{"slots":[1],"rates":[1600],"rank0":["lr"],"rank1":["empty"],"addr-cmd":3750201,"drive":["30 Ω","30 Ω","30 Ω","30 Ω"]},{"slots":[1],"rates":[1866],"rank0":["lr"],"rank1":["empty"],"addr-cmd":3618615,"drive":["30 Ω","30 Ω","30 Ω","30 Ω"]},{"slots":[1],"rates":[2133],"rank0":["lr"],"rank1":["empty"],"addr-cmd":3487029,"drive":["30 Ω","30 Ω","30 Ω","30 Ω"]},{"slots":[1],"rates":[2400],"rank0":["lr"],"rank1":["empty"],"addr-cmd":3355443,"drive":["30 Ω","30 Ω","30 Ω","30 Ω"]},{"slots":[1],"rates":[2667],"rank0":["lr"],"rank1":["empty"],"addr-cmd":3223857,"drive":["30 Ω","30 Ω","30 Ω","30 Ω"]},{"slots":[1],"rates":[2933],"rank0":["lr"],"rank1":["empty"],"addr-cmd":3092271,"drive":["30 Ω","30 Ω","30 Ω","30 Ω"]},{"slots":[1],"rates":[3200],"rank0":["lr"],"rank1":["empty"],"addr-cmd":2960685,"drive":["30 Ω","30 Ω","30 Ω","30 Ω"]},{"slots":[2],"rates":[1600],"rank0":["empty"],"rank1":["lr"],"addr-cmd":3750201,"drive":["30 Ω","30 Ω","30 Ω","30 Ω"]},{"slots":[2],"rates":[1600],"rank0":["lr"],"rank1":["lr"],"addr-cmd":3488057,"drive":["30 Ω","30 Ω","30 Ω","30 Ω"]},{"slots":[2],"rates":[1866],"rank0":["empty"],"rank1":["lr"],"addr-cmd":3618615,"drive":["30 Ω","30 Ω","30 Ω","30 Ω"]},{"slots":[2],"rates":[1866],"rank0":["lr"],"rank1":["lr"],"addr-cmd":3356985,"drive":["30 Ω","30 Ω","30 Ω","30 Ω"]},{"slots":[2],"rates":[2133],"rank0":["empty"],"rank1":["lr"],"addr-cmd":3487029,"drive":["30 Ω","30 Ω","30 Ω","30 Ω"]},{"slots":[2],"rates":[2133],"rank0":["lr"],"rank1":["lr"],"addr-cmd":3224885,"drive":["30 Ω","30 Ω","30 Ω","30 Ω"]},{"slots":[2],"rates":[2400],"rank0":["empty"],"rank1":["lr"],"addr-cmd":3355443,"drive":["30 Ω","30 Ω","30 Ω","30 Ω"]},{"slots":[2],"rates":[2400],"rank0":["lr"],"rank1":["lr"],"addr-cmd":3093299,"drive":["30 Ω","30 Ω","30 Ω","30 Ω"]},{"slots":[2],"rates":[2667],"rank0":["empty"],"rank1":["lr"],"addr-cmd":3223857,"drive":["30 Ω","30 Ω","30 Ω","30 Ω"]},{"slots":[2],"rates":[2667],"rank0":["lr"],"rank1":["lr"],"addr-cmd":2961713,"drive":["30 Ω","30 Ω","30 Ω","30 Ω"]},{"slots":[2],"rates":[2933],"rank0":["empty"],"rank1":["lr"],"addr-cmd":3092271,"drive":["30 Ω","30 Ω","30 Ω","30 Ω"]},{"slots":[2],"rates":[2933],"rank0":["lr"],"rank1":["lr"],"addr-cmd":2895663,"drive":["30 Ω","30 Ω","30 Ω","30 Ω"]},{"slots":[2],"rates":[3200],"rank0":["empty"],"rank1":["lr"],"addr-cmd":2960685,"drive":["30 Ω","30 Ω","30 Ω","30 Ω"]},{"slots":[2],"rates":[3200],"rank0":["lr"],"rank1":["lr"],"addr-cmd":2764077,"drive":["30 Ω","30 Ω","30 Ω","30 Ω"]}]),
        )?,
    })
}
fn electrical_lrdimm_data() -> Result<Table> {
    Ok(Table::LrdimmData {
        rows: native(
            json!([{"slots":[1],"rates":[1600,1866,2133,2400,2667,2933,3200],"rank0":["lr"],"rank1":["empty"],"rtt-nom":"Off","rtt-wr":"Off","rtt-park":"48 Ω","drive":[62,62,24],"pmu-vref":93,"dram-vref":23},{"slots":[2],"rates":[1600,1866,2133,2400,2667,2933,3200],"rank0":["empty"],"rank1":["lr"],"rtt-nom":"Off","rtt-wr":"Off","rtt-park":"48 Ω","drive":[62,62,24],"pmu-vref":93,"dram-vref":23},{"slots":[2],"rates":[1600,1866,2133,2400,2667,2933,3200],"rank0":["lr"],"rank1":["empty"],"rtt-nom":"Off","rtt-wr":"Off","rtt-park":"48 Ω","drive":[62,62,24],"pmu-vref":93,"dram-vref":23},{"slots":[2],"rates":[1600,1866,2133,2400,2667,2933,3200],"rank0":["lr"],"rank1":["lr"],"rtt-nom":"Off","rtt-wr":"80 Ω","rtt-park":"34 Ω","drive":[62,62,24],"pmu-vref":106,"dram-vref":34}]),
        )?,
    })
}
fn electrical_defaults() -> Result<BTreeMap<String, Table>> {
    Ok(BTreeMap::from([
        ("rdimm-odt".into(), electrical_rdimm_odt()?),
        ("rdimm-cad".into(), electrical_rdimm_cad()?),
        ("rdimm-data".into(), electrical_rdimm_data()?),
        ("three-ds-data".into(), electrical_three_ds_data()?),
        ("lrdimm-odt".into(), electrical_lrdimm_odt()?),
        ("lrdimm-cad".into(), electrical_lrdimm_cad()?),
        ("lrdimm-data".into(), electrical_lrdimm_data()?),
    ]))
}
fn s8030_rome_rdimm_max_frequency_board_1() -> Result<Table> {
    Ok(Table::RdimmMaxFrequency {
        rows: native(
            json!([{"slots":[1],"conditions":[1,1,0,0],"speed":"Ddr2667"},{"slots":[1],"conditions":[1,0,1,0],"speed":"Ddr2667"},{"slots":[2],"conditions":[1,1,0,0],"speed":"Ddr2667"},{"slots":[2],"conditions":[1,0,1,0],"speed":"Ddr2400"},{"slots":[2],"conditions":[2,2,0,0],"speed":"Ddr2133"},{"slots":[2],"conditions":[2,1,1,0],"speed":"Ddr2133"},{"slots":[2],"conditions":[2,0,2,0],"speed":"Ddr2133"}]),
        )?,
    })
}
fn s8030_rome_rdimm_max_frequency() -> Result<Table> {
    Ok(Table::RdimmMaxFrequency {
        rows: native(
            json!([{"slots":[1],"conditions":[1,1,0,0],"speed":"Ddr3200"},{"slots":[1],"conditions":[1,0,1,0],"speed":"Ddr3200"},{"slots":[2],"conditions":[1,1,0,0],"speed":"Ddr3200"},{"slots":[2],"conditions":[1,0,1,0],"speed":"Ddr3200"},{"slots":[2],"conditions":[2,2,0,0],"speed":"Ddr2933"},{"slots":[2],"conditions":[2,1,1,0],"speed":"Ddr2933"},{"slots":[2],"conditions":[2,0,2,0],"speed":"Ddr2933"}]),
        )?,
    })
}
fn s8030_rome_rdimm_stretch_frequency() -> Result<Table> {
    Ok(Table::RdimmStretchFrequency {
        rows: native(
            json!([{"slots":[1],"conditions":[1,1,0,0],"speed":"Ddr3200"},{"slots":[1],"conditions":[1,0,1,0],"speed":"Ddr3200"},{"slots":[2],"conditions":[1,1,0,0],"speed":"Ddr3200"},{"slots":[2],"conditions":[1,0,1,0],"speed":"Ddr3200"},{"slots":[2],"conditions":[2,2,0,0],"speed":"Ddr3200"},{"slots":[2],"conditions":[2,1,1,0],"speed":"Ddr3200"},{"slots":[2],"conditions":[2,0,2,0],"speed":"Ddr3200"}]),
        )?,
    })
}
fn s8030_rome_three_ds_max_frequency_board_1() -> Result<Table> {
    Ok(Table::ThreeDsMaxFrequency {
        rows: native(
            json!([{"slots":[1],"conditions":[1,0,1,0],"speed":"Ddr2667"},{"slots":[2],"conditions":[1,0,1,0],"speed":"Ddr2400"},{"slots":[2],"conditions":[2,0,2,0],"speed":"Ddr1866"}]),
        )?,
    })
}
fn s8030_rome_three_ds_max_frequency() -> Result<Table> {
    Ok(Table::ThreeDsMaxFrequency {
        rows: native(
            json!([{"slots":[1],"conditions":[1,0,1,0],"speed":"Ddr3200"},{"slots":[2],"conditions":[1,0,1,0],"speed":"Ddr2933"},{"slots":[2],"conditions":[2,0,2,0],"speed":"Ddr2667"}]),
        )?,
    })
}
fn s8030_rome_three_ds_stretch_frequency() -> Result<Table> {
    Ok(Table::ThreeDsStretchFrequency {
        rows: native(
            json!([{"slots":[1],"conditions":[1,0,1,0],"speed":"Ddr3200"},{"slots":[2],"conditions":[1,0,1,0],"speed":"Ddr3200"},{"slots":[2],"conditions":[2,0,2,0],"speed":"Ddr3200"}]),
        )?,
    })
}
fn s8030_rome_lrdimm_max_frequency_board_1() -> Result<Table> {
    Ok(Table::LrdimmMaxFrequency {
        rows: native(
            json!([{"slots":[1],"conditions":[1,0,1,0],"speed":"Ddr2667"},{"slots":[1],"conditions":[1,0,0,1],"speed":"Ddr2667"},{"slots":[2],"conditions":[1,0,1,0],"speed":"Ddr2667"},{"slots":[2],"conditions":[1,0,0,1],"speed":"Ddr2667"},{"slots":[2],"conditions":[2,0,2,0],"speed":"Ddr2133"},{"slots":[2],"conditions":[2,0,0,2],"speed":"Ddr2133"}]),
        )?,
    })
}
fn s8030_rome_lrdimm_max_frequency() -> Result<Table> {
    Ok(Table::LrdimmMaxFrequency {
        rows: native(
            json!([{"slots":[1],"conditions":[1,0,1,0],"speed":"Ddr3200"},{"slots":[1],"conditions":[1,0,0,1],"speed":"Ddr3200"},{"slots":[2],"conditions":[1,0,1,0],"speed":"Ddr3200"},{"slots":[2],"conditions":[1,0,0,1],"speed":"Ddr3200"},{"slots":[2],"conditions":[2,0,2,0],"speed":"Ddr2933"},{"slots":[2],"conditions":[2,0,0,2],"speed":"Ddr2933"}]),
        )?,
    })
}
fn s8030_rome_empty_ppr() -> Result<Table> {
    Ok(Table::EmptyPpr { slots: 64 })
}
fn s8030_rome_board_id_eeprom() -> Result<Table> {
    Ok(Table::BoardIdEeprom {
        controller: 5,
        address: 80,
        id_offset: 16,
        revision_offset: 18,
        mapping: native(
            json!([{"id_and_rev_and_feature_mask":255,"id_and_feature_value":12,"rev_and_feature_value":"NotApplicable","board_instance_index":1},{"id_and_rev_and_feature_mask":255,"id_and_feature_value":55,"rev_and_feature_value":"NotApplicable","board_instance_index":2},{"id_and_rev_and_feature_mask":255,"id_and_feature_value":13,"rev_and_feature_value":"NotApplicable","board_instance_index":0},{"id_and_rev_and_feature_mask":255,"id_and_feature_value":49,"rev_and_feature_value":"NotApplicable","board_instance_index":0},{"id_and_rev_and_feature_mask":255,"id_and_feature_value":57,"rev_and_feature_value":"NotApplicable","board_instance_index":1},{"id_and_rev_and_feature_mask":255,"id_and_feature_value":48,"rev_and_feature_value":"NotApplicable","board_instance_index":0},{"id_and_rev_and_feature_mask":255,"id_and_feature_value":52,"rev_and_feature_value":{"Value":5},"board_instance_index":1},{"id_and_rev_and_feature_mask":255,"id_and_feature_value":52,"rev_and_feature_value":"NotApplicable","board_instance_index":0},{"id_and_rev_and_feature_mask":255,"id_and_feature_value":14,"rev_and_feature_value":"NotApplicable","board_instance_index":0}]),
        )?,
    })
}
fn s8030_rome_spd_smbus() -> Result<Table> {
    Ok(Table::SpdSmbus {
        slots: native(
            json!([{"socket":0,"channel":0,"dimm":0,"address":160,"mux":224,"mux-channel":4},{"socket":0,"channel":0,"dimm":1,"address":162,"mux":224,"mux-channel":4},{"socket":0,"channel":1,"dimm":0,"address":164,"mux":224,"mux-channel":4},{"socket":0,"channel":1,"dimm":1,"address":166,"mux":224,"mux-channel":4},{"socket":0,"channel":2,"dimm":0,"address":168,"mux":224,"mux-channel":4},{"socket":0,"channel":2,"dimm":1,"address":170,"mux":224,"mux-channel":4},{"socket":0,"channel":3,"dimm":0,"address":172,"mux":224,"mux-channel":4},{"socket":0,"channel":3,"dimm":1,"address":174,"mux":224,"mux-channel":4},{"socket":0,"channel":4,"dimm":0,"address":160,"mux":224,"mux-channel":5},{"socket":0,"channel":4,"dimm":1,"address":162,"mux":224,"mux-channel":5},{"socket":0,"channel":5,"dimm":0,"address":164,"mux":224,"mux-channel":5},{"socket":0,"channel":5,"dimm":1,"address":166,"mux":224,"mux-channel":5},{"socket":0,"channel":6,"dimm":0,"address":168,"mux":224,"mux-channel":5},{"socket":0,"channel":6,"dimm":1,"address":170,"mux":224,"mux-channel":5},{"socket":0,"channel":7,"dimm":0,"address":172,"mux":224,"mux-channel":5},{"socket":0,"channel":7,"dimm":1,"address":174,"mux":224,"mux-channel":5},{"socket":1,"channel":0,"dimm":0,"address":160,"mux":224,"mux-channel":6},{"socket":1,"channel":0,"dimm":1,"address":162,"mux":224,"mux-channel":6},{"socket":1,"channel":1,"dimm":0,"address":164,"mux":224,"mux-channel":6},{"socket":1,"channel":1,"dimm":1,"address":166,"mux":224,"mux-channel":6},{"socket":1,"channel":2,"dimm":0,"address":168,"mux":224,"mux-channel":6},{"socket":1,"channel":2,"dimm":1,"address":170,"mux":224,"mux-channel":6},{"socket":1,"channel":3,"dimm":0,"address":172,"mux":224,"mux-channel":6},{"socket":1,"channel":3,"dimm":1,"address":174,"mux":224,"mux-channel":6},{"socket":1,"channel":4,"dimm":0,"address":160,"mux":224,"mux-channel":7},{"socket":1,"channel":4,"dimm":1,"address":162,"mux":224,"mux-channel":7},{"socket":1,"channel":5,"dimm":0,"address":164,"mux":224,"mux-channel":7},{"socket":1,"channel":5,"dimm":1,"address":166,"mux":224,"mux-channel":7},{"socket":1,"channel":6,"dimm":0,"address":168,"mux":224,"mux-channel":7},{"socket":1,"channel":6,"dimm":1,"address":170,"mux":224,"mux-channel":7},{"socket":1,"channel":7,"dimm":0,"address":172,"mux":224,"mux-channel":7},{"socket":1,"channel":7,"dimm":1,"address":174,"mux":224,"mux-channel":7}]),
        )?,
    })
}
fn s8030_rome_platform_override() -> Result<Table> {
    Ok(Table::PlatformOverride {
        scope: OverrideScope::All,
        operations: vec![
            Override::MemclkMap {
                connections: [0, 1, 2, 3, 0, 0, 0, 0],
            },
            Override::CkeTristateMap {
                connections: [0, 1, 2, 3],
            },
            Override::OdtTristateMap {
                connections: [0, 1, 2, 3],
            },
            Override::CsTristateMap {
                connections: [0, 1, 2, 3, 0, 0, 0, 0],
            },
            Override::MaxDimmsPerChannel { value: 1 },
            Override::MaxChannelsPerSocket { value: 8 },
        ],
    })
}
fn s8030_rome_console() -> Result<Table> {
    Ok(Table::Console {
        settings: native(
            json!({"abl_console_out_control":{"enable_console_logging":true,"enable_mem_flow_logging":true,"enable_mem_setreg_logging":true,"enable_mem_getreg_logging":false,"enable_mem_status_logging":true,"enable_mem_pmu_logging":true,"enable_mem_pmu_sram_read_logging":false,"enable_mem_pmu_sram_write_logging":false,"enable_mem_test_verbose_logging":false,"enable_mem_basic_output_logging":true,"abl_console_port":128},"abl_breakpoint_control":{"enable_breakpoint":false,"break_on_all_dies":false}}),
        )?,
    })
}
fn s8030_rome_error_output() -> Result<Table> {
    Ok(Table::ErrorOutput {
        settings: native(
            json!({"enable_error_reporting":false,"enable_error_reporting_gpio":false,"enable_error_reporting_beep_codes":false,"enable_using_handshake":false,"input_port":132,"output_delay":15000,"output_port":128,"stop_on_first_fatal_error":false,"input_port_size":"32 Bit","output_port_size":"32 Bit","input_port_type":"FchHtIo","output_port_type":"FchHtIo","clear_acknowledgement":false,"error_reporting_gpio":{"pin":85,"iomux_control":1,"bank_control":192},"beep_code_table":[{"custom_error_type":"General","peak_map":1,"peak_attr":{"peak_count":8,"pulse_width":0,"repeat_count":0}},{"custom_error_type":"Memory","peak_map":2,"peak_attr":{"peak_count":20,"pulse_width":0,"repeat_count":0}},{"custom_error_type":"Df","peak_map":3,"peak_attr":{"peak_count":20,"pulse_width":0,"repeat_count":0}},{"custom_error_type":"Ccx","peak_map":4,"peak_attr":{"peak_count":20,"pulse_width":0,"repeat_count":0}},{"custom_error_type":"Gnb","peak_map":5,"peak_attr":{"peak_count":20,"pulse_width":0,"repeat_count":0}},{"custom_error_type":"Psp","peak_map":6,"peak_attr":{"peak_count":20,"pulse_width":0,"repeat_count":0}},{"custom_error_type":"Smu","peak_map":7,"peak_attr":{"peak_count":20,"pulse_width":0,"repeat_count":0}},{"custom_error_type":"Unknown","peak_map":2,"peak_attr":{"peak_count":4,"pulse_width":0,"repeat_count":0}}],"enable_heart_beat":false,"enable_power_good_gpio":false,"power_good_gpio":{"pin":0,"iomux_control":0,"bank_control":0}}),
        )?,
    })
}
fn s8030_rome_voltage_control() -> Result<Table> {
    Ok(Table::VoltageControl {
        settings: native(
            json!({"enabled":true,"input_port":132,"output_port":128,"input_port_size":"32 Bit","output_port_size":"32 Bit","input_port_type":"FchHtIo","output_port_type":"FchHtIo","clear_acknowledgement":false}),
        )?,
    })
}
fn s8030_rome_platform_tuning_end() -> Result<Table> {
    Ok(Table::PlatformTuningEnd {})
}
fn s8030_rome_slink() -> Result<Table> {
    Ok(Table::Slink {
        settings: native(
            json!({"regions":[{"size":0,"alignment":0,"socket":0,"phys_nbio_map":0,"interleaving":"256 B"},{"size":0,"alignment":0,"socket":0,"phys_nbio_map":0,"interleaving":"256 B"},{"size":0,"alignment":0,"socket":1,"phys_nbio_map":0,"interleaving":"256 B"},{"size":0,"alignment":0,"socket":1,"phys_nbio_map":0,"interleaving":"256 B"}]}),
        )?,
    })
}
pub(super) fn s8030_rome() -> Result<Definition> {
    let mut tables = BTreeMap::from([
        (
            "rdimm-max-frequency-board-1".into(),
            s8030_rome_rdimm_max_frequency_board_1()?,
        ),
        (
            "rdimm-max-frequency".into(),
            s8030_rome_rdimm_max_frequency()?,
        ),
        (
            "rdimm-stretch-frequency".into(),
            s8030_rome_rdimm_stretch_frequency()?,
        ),
        (
            "three-ds-max-frequency-board-1".into(),
            s8030_rome_three_ds_max_frequency_board_1()?,
        ),
        (
            "three-ds-max-frequency".into(),
            s8030_rome_three_ds_max_frequency()?,
        ),
        (
            "three-ds-stretch-frequency".into(),
            s8030_rome_three_ds_stretch_frequency()?,
        ),
        (
            "lrdimm-max-frequency-board-1".into(),
            s8030_rome_lrdimm_max_frequency_board_1()?,
        ),
        (
            "lrdimm-max-frequency".into(),
            s8030_rome_lrdimm_max_frequency()?,
        ),
        ("empty-ppr".into(), s8030_rome_empty_ppr()?),
        ("board-id-eeprom".into(), s8030_rome_board_id_eeprom()?),
        ("spd-smbus".into(), s8030_rome_spd_smbus()?),
        ("platform-override".into(), s8030_rome_platform_override()?),
        ("console".into(), s8030_rome_console()?),
        ("error-output".into(), s8030_rome_error_output()?),
        ("voltage-control".into(), s8030_rome_voltage_control()?),
        (
            "platform-tuning-end".into(),
            s8030_rome_platform_tuning_end()?,
        ),
        ("slink".into(), s8030_rome_slink()?),
    ]);
    for (name, table) in electrical_defaults()? {
        if tables.insert(name, table).is_some() {
            return Err("duplicate SP3 default table".into());
        }
    }
    Ok(Definition {
        generation: Generation::Rome,
        blobs: vec![
            Blob {
                name: "board".into(),
                revision: 7876,
                donor_sha256: "6f56aa5889f5b6eaa1be74864af3f962ca4c48ac599f8556caf03dcf8aea42ed"
                    .into(),
                bindings: vec![Binding {
                    slot: Slot::Backup,
                    instance: 0,
                }],
                groups: vec![],
                records: vec![
                    Reference {
                        name: "board-id-eeprom".into(),
                        instance: 0,
                        board_mask: None,
                        priority: Some(vec![Priority::HardForce, Priority::Normal]),
                    },
                    Reference {
                        name: "spd-smbus".into(),
                        instance: 0,
                        board_mask: None,
                        priority: None,
                    },
                    Reference {
                        name: "platform-override".into(),
                        instance: 0,
                        board_mask: None,
                        priority: None,
                    },
                    Reference {
                        name: "rdimm-odt".into(),
                        instance: 0,
                        board_mask: None,
                        priority: None,
                    },
                    Reference {
                        name: "rdimm-cad".into(),
                        instance: 0,
                        board_mask: None,
                        priority: None,
                    },
                    Reference {
                        name: "rdimm-data".into(),
                        instance: 0,
                        board_mask: None,
                        priority: None,
                    },
                    Reference {
                        name: "rdimm-max-frequency-board-1".into(),
                        instance: 0,
                        board_mask: Some(2),
                        priority: None,
                    },
                    Reference {
                        name: "rdimm-max-frequency".into(),
                        instance: 0,
                        board_mask: None,
                        priority: None,
                    },
                    Reference {
                        name: "rdimm-stretch-frequency".into(),
                        instance: 0,
                        board_mask: None,
                        priority: None,
                    },
                    Reference {
                        name: "three-ds-max-frequency-board-1".into(),
                        instance: 0,
                        board_mask: Some(2),
                        priority: None,
                    },
                    Reference {
                        name: "three-ds-max-frequency".into(),
                        instance: 0,
                        board_mask: None,
                        priority: None,
                    },
                    Reference {
                        name: "three-ds-stretch-frequency".into(),
                        instance: 0,
                        board_mask: None,
                        priority: None,
                    },
                    Reference {
                        name: "three-ds-data".into(),
                        instance: 0,
                        board_mask: None,
                        priority: None,
                    },
                    Reference {
                        name: "console".into(),
                        instance: 0,
                        board_mask: None,
                        priority: None,
                    },
                    Reference {
                        name: "error-output".into(),
                        instance: 0,
                        board_mask: None,
                        priority: None,
                    },
                    Reference {
                        name: "voltage-control".into(),
                        instance: 0,
                        board_mask: None,
                        priority: None,
                    },
                    Reference {
                        name: "lrdimm-odt".into(),
                        instance: 0,
                        board_mask: None,
                        priority: None,
                    },
                    Reference {
                        name: "lrdimm-cad".into(),
                        instance: 0,
                        board_mask: None,
                        priority: None,
                    },
                    Reference {
                        name: "lrdimm-data".into(),
                        instance: 0,
                        board_mask: None,
                        priority: None,
                    },
                    Reference {
                        name: "lrdimm-max-frequency-board-1".into(),
                        instance: 0,
                        board_mask: Some(2),
                        priority: None,
                    },
                    Reference {
                        name: "lrdimm-max-frequency".into(),
                        instance: 0,
                        board_mask: None,
                        priority: None,
                    },
                    Reference {
                        name: "platform-tuning-end".into(),
                        instance: 0,
                        board_mask: None,
                        priority: None,
                    },
                ],
                tokens: vec![
                    Reference {
                        name: "board-bool".into(),
                        instance: 0,
                        board_mask: Some(1),
                        priority: None,
                    },
                    Reference {
                        name: "board-bool".into(),
                        instance: 0,
                        board_mask: None,
                        priority: None,
                    },
                    Reference {
                        name: "board-byte-board-1".into(),
                        instance: 0,
                        board_mask: Some(2),
                        priority: None,
                    },
                    Reference {
                        name: "board-byte".into(),
                        instance: 0,
                        board_mask: None,
                        priority: None,
                    },
                    Reference {
                        name: "board-word".into(),
                        instance: 0,
                        board_mask: None,
                        priority: None,
                    },
                    Reference {
                        name: "board-dword".into(),
                        instance: 0,
                        board_mask: None,
                        priority: None,
                    },
                ],
            },
            Blob {
                name: "cbs-backup".into(),
                revision: 7431,
                donor_sha256: "27061c5113b34e7f8875e18303c8f2c797b2afa239e92d1effcbbf916b249afc"
                    .into(),
                bindings: vec![
                    Binding {
                        slot: Slot::Backup,
                        instance: 8,
                    },
                    Binding {
                        slot: Slot::Active,
                        instance: 0,
                    },
                ],
                groups: vec![],
                records: vec![Reference {
                    name: "slink".into(),
                    instance: 0,
                    board_mask: None,
                    priority: Some(vec![Priority::Low]),
                }],
                tokens: vec![
                    Reference {
                        name: "empty-bool".into(),
                        instance: 0,
                        board_mask: None,
                        priority: Some(vec![Priority::Low]),
                    },
                    Reference {
                        name: "cbs-backup-bool-medium".into(),
                        instance: 1,
                        board_mask: None,
                        priority: Some(vec![Priority::Medium]),
                    },
                    Reference {
                        name: "empty-byte".into(),
                        instance: 0,
                        board_mask: None,
                        priority: Some(vec![Priority::Low]),
                    },
                    Reference {
                        name: "cbs-backup-byte-medium".into(),
                        instance: 1,
                        board_mask: None,
                        priority: Some(vec![Priority::Medium]),
                    },
                    Reference {
                        name: "empty-word".into(),
                        instance: 0,
                        board_mask: None,
                        priority: Some(vec![Priority::Low]),
                    },
                    Reference {
                        name: "empty-word".into(),
                        instance: 1,
                        board_mask: None,
                        priority: Some(vec![Priority::Medium]),
                    },
                    Reference {
                        name: "cbs-backup-dword-medium".into(),
                        instance: 1,
                        board_mask: None,
                        priority: Some(vec![Priority::Medium]),
                    },
                    Reference {
                        name: "empty-dword".into(),
                        instance: 0,
                        board_mask: None,
                        priority: Some(vec![Priority::Low]),
                    },
                ],
            },
            Blob {
                name: "post-package-repair".into(),
                revision: 9371,
                donor_sha256: "20640126271299dc0083d9855eef4909e3743ad2c6474fdd6052e08c6fbcbe69"
                    .into(),
                bindings: vec![
                    Binding {
                        slot: Slot::Backup,
                        instance: 9,
                    },
                    Binding {
                        slot: Slot::Active,
                        instance: 1,
                    },
                ],
                groups: vec![],
                records: vec![Reference {
                    name: "empty-ppr".into(),
                    instance: 0,
                    board_mask: None,
                    priority: Some(vec![Priority::EventLogging]),
                }],
                tokens: vec![],
            },
        ],
        tables,
        token_sets: BTreeMap::from([
            (
                "board-bool".into(),
                TokenSet::Bool {
                    values: vec![
                        Token::Named(native(json!({"MemLrdimmCapable":true}))?),
                        Token::Named(native(json!({"PspTpPort":true}))?),
                        Token::Named(native(json!({"MotherBoardType0":false}))?),
                        Token::Named(native(json!({"MemForcePowerDownThrottleEnable":true}))?),
                        Token::Named(native(json!({"PspPsbAutoFuse":true}))?),
                        Token::Named(native(json!({"MemDqsTrainingControl":true}))?),
                        Token::Named(native(json!({"MemEnableParity":true}))?),
                        Token::Named(native(json!({"MemUdimmCapable":true}))?),
                        Token::Named(native(json!({"BmcVgaIoEnable":false}))?),
                        Token::Named(native(json!({"MemEnableBankGroupSwap":true}))?),
                        Token::Named(native(json!({"MemChannelInterleaving":false}))?),
                        Token::Named(native(json!({"MemPstate":true}))?),
                        Token::Named(native(json!({"MemLimitMemoryToBelow1TiB":true}))?),
                        Token::Named(native(json!({"MemEnableBankSwizzle":true}))?),
                        Token::Named(native(json!({"VgaProgram":true}))?),
                        Token::Named(native(json!({"MemSpdReadOptimizationDdr4":true}))?),
                        Token::Named(native(json!({"DfGroupDPlatform":true}))?),
                        Token::Named(native(json!({"MemHoleRemapping":true}))?),
                        Token::Named(native(json!({"CcxPpinOptIn":false}))?),
                        Token::Named(native(json!({"MemEnableChipSelectInterleaving":true}))?),
                        Token::Named(native(json!({"ConfigureSecondPcieLink":false}))?),
                        Token::Named(native(json!({"MemMbistAggressorStaticLaneControl":false}))?),
                        Token::Named(native(json!({"MctpRerouteEnable":false}))?),
                        Token::Named(native(json!({"MemSodimmCapable":true}))?),
                        Token::Named(native(json!({"MemIgnoreSpdChecksum":true}))?),
                        Token::Named(native(json!({"MemRdimmCapable":true}))?),
                        Token::Named(native(json!({"MemEccSyncFlood":false}))?),
                        Token::Named(native(
                            json!({"MemAutoRefreshsCountForThrottling":"Enabled"}),
                        )?),
                        Token::Named(native(json!({"MemNvdimmNDisable":false}))?),
                        Token::Named(native(json!({"GeneralCapsuleMode":true}))?),
                        Token::Named(native(json!({"MemDramDoubleRefreshRateMilan":false}))?),
                        Token::Named(native(json!({"DisplayPmuTrainingResults":false}))?),
                        Token::Named(native(json!({"MemSwCmdThrottleEnable":false}))?),
                        Token::Named(native(json!({"MemEnableBankGroupSwapAlt":true}))?),
                        Token::Named(native(json!({"MemOnDieThermalSensor":true}))?),
                        Token::Named(native(json!({"MemAllClocks":true}))?),
                        Token::Named(native(json!({"MemEnablePowerDown":false}))?),
                        Token::Named(native(json!({"MemUncorrectedEccRetryDdr4":true}))?),
                        Token::Named(native(json!({"MemOdtsCmdThrottleEnable":true}))?),
                        Token::Named(native(json!({"MemClear":false}))?),
                        Token::Named(native(json!({"MemPostPackageRepairEnable":true}))?),
                        Token::Named(native(json!({"PspErrorDisplay":true}))?),
                        Token::Named(native(json!({"MemEccRedirection":false}))?),
                        Token::Named(native(json!({"MemMbistTgtStaticLaneControl":false}))?),
                        Token::Named(native(json!({"MemDdrRouteBalancedTee":false}))?),
                        Token::Named(native(json!({"MemQuadRankCapable":true}))?),
                        Token::Named(native(json!({"PspStopOnError":false}))?),
                        Token::Named(native(json!({"IohcMixedRwWorkaround":false}))?),
                        Token::Named(native(json!({"MemTempControlledRefreshEnable":false}))?),
                        Token::Named(native(json!({"MemTempControlledExtendedRefresh":false}))?),
                        Token::Named(native(json!({"MemEnableEccFeature":true}))?),
                        Token::Named(native(json!({"BmcInitBeforeDram":false}))?),
                        Token::Named(native(json!({"MemRestoreControl":false}))?),
                    ],
                },
            ),
            (
                "board-byte-board-1".into(),
                TokenSet::Byte {
                    values: vec![
                        Token::Named(native(json!({"MemMbistAggressorOn":false}))?),
                        Token::Named(native(json!({"DfGmiEncrypt":"Auto"}))?),
                        Token::Named(native(json!({"BmcEndLane":255}))?),
                        Token::Named(native(json!({"MemAutoRefreshFineGranMode":"Fixed1Times"}))?),
                        Token::Named(native(json!({"BmcFunction":2}))?),
                        Token::Named(native(json!({"MemMbistWorseCasGranularity":0}))?),
                        Token::Named(native(json!({"FchSmbusSpeed":{"Value":42}}))?),
                        Token::Numeric(NumericToken {
                            id: 614336725,
                            value: 3,
                        }),
                        Token::Named(native(json!({"DfMemInterleavingSize":"Auto"}))?),
                        Token::Named(native(json!({"MemNvdimmPowerSource":"DeviceManaged"}))?),
                        Token::Numeric(NumericToken {
                            id: 686512045,
                            value: 30,
                        }),
                        Token::Named(native(json!({"BmcVgaIoBarToReplace":0}))?),
                        Token::Named(native(json!({"DfDramNumaPerSocket":"Four"}))?),
                        Token::Named(native(json!({"MemMbistReadDataEyeVoltageStep":2}))?),
                        Token::Named(native(json!({"DfRemapAt1TiB":"Auto"}))?),
                        Token::Named(native(json!({"MemDramAddressCommandParityRetryCount":1}))?),
                        Token::Named(native(json!({"Df4LinkMaxXgmiSpeed":"10.667 Gbit/s"}))?),
                        Token::Named(native(json!({"MemMbistAggressorStaticLaneVal":0}))?),
                        Token::Named(native(json!({"MemDataPoison":"Enabled"}))?),
                        Token::Named(native(json!({"MemMbistTgtStaticLaneVal":0}))?),
                        Token::Named(native(json!({"MemMbistDataEyeType":"1D Timing"}))?),
                        Token::Named(native(json!({"Df3LinkMaxXgmiSpeed":"Auto"}))?),
                        Token::Named(native(json!({"MemHealPprType":"SoftRepair"}))?),
                        Token::Numeric(NumericToken {
                            id: 1434937120,
                            value: 253,
                        }),
                        Token::Named(native(json!({"MemMbistTestMode":"PhysicalInterface"}))?),
                        Token::Named(native(json!({"MemMbistAggressorStaticLaneSelEcc":0}))?),
                        Token::Named(native(json!({"MemMbistReadDataEyeTimingStep":1}))?),
                        Token::Named(native(json!({"MemHealTestSelect":"Normal"}))?),
                        Token::Named(native(json!({"MemRollWindowDepth":{"Memclks":255}}))?),
                        Token::Named(native(json!({"FchConsoleOutSuperIoType":"Auto"}))?),
                        Token::Named(native(json!({"MemHealMaxBankFails":3}))?),
                        Token::Named(native(json!({"MemRcdParity":true}))?),
                        Token::Named(native(json!({"DfInvertDramMap":"Auto"}))?),
                        Token::Named(native(json!({"DfProbeFilter":"Auto"}))?),
                        Token::Named(native(json!({"OdtsCmdThrottleCycles":87}))?),
                        Token::Named(native(json!({"MemControllerWritingCrcMaxReplay":8}))?),
                        Token::Named(native(json!({"DfXgmiEncrypt":"Auto"}))?),
                        Token::Named(native(json!({"MemPsErrorHandling":0}))?),
                        Token::Named(native(json!({"DfSaveRestoreMemEncrypt":"Auto"}))?),
                        Token::Named(native(json!({"MemControllerWritingCrcMode":"Disabled"}))?),
                        Token::Named(native(json!({"DimmSensorResolution":1}))?),
                        Token::Named(native(json!({"BmcSocket":15}))?),
                        Token::Named(native(json!({"PcieResetPinSelect":0}))?),
                        Token::Named(native(json!({"MemMbistDataEyeExecutionRepeatCount":1}))?),
                        Token::Named(native(json!({"DfBottomIo":176}))?),
                        Token::Named(native(json!({"MemDataScramble":1}))?),
                        Token::Named(native(json!({"BmcLinkSpeed":"PcieGen1"}))?),
                        Token::Named(native(json!({"DfMemClear":"Auto"}))?),
                        Token::Named(native(json!({"FchConsoleOutBasicEnable":0}))?),
                        Token::Named(native(json!({"MemMbistTgtStaticLaneSelEcc":0}))?),
                        Token::Named(native(json!({"DfXgmiTxEqMode":"Auto"}))?),
                        Token::Named(native(json!({"AblSerialBaudRate":"115200 Baud"}))?),
                        Token::Named(native(json!({"MemMbistPatternLength":3}))?),
                        Token::Named(native(json!({"BmcRcbCheckingMode":"Auto"}))?),
                        Token::Named(native(
                            json!({"MemTrainingHdtControl":"AssertionMessages"}),
                        )?),
                        Token::Named(native(json!({"DfXgmiConfig":"Auto"}))?),
                        Token::Named(native(json!({"MemMbistHaltOnError":1}))?),
                        Token::Named(native(json!({"BmcStartLane":255}))?),
                        Token::Named(native(json!({"PmuTrainingMode":"1D,2D"}))?),
                        Token::Named(native(json!({"CbsMemUncorrectedEccRetryDdr4":true}))?),
                        Token::Named(native(json!({"MemControllerWritingCrcLimit":0}))?),
                        Token::Named(native(json!({"MemParityErrorMaxReplayDdr4":8}))?),
                        Token::Named(native(json!({"MemMbistWriteDataEyeVoltageStep":2}))?),
                        Token::Named(native(json!({"DfMemInterleaving":"Auto"}))?),
                        Token::Named(native(json!({"MemMbistPerBitSlaveDieReport":0}))?),
                        Token::Named(native(json!({"PspEnableDebugMode":"Disabled"}))?),
                        Token::Named(native(
                            json!({"MemRdimmTimingRcdF0Rc0FAdditionalLatency":"Auto"}),
                        )?),
                        Token::Named(native(json!({"MemTsmeModeRome":"Disabled"}))?),
                        Token::Named(native(json!({"BmcDevice":5}))?),
                        Token::Named(native(json!({"MemMbistWriteDataEyeTimingStep":1}))?),
                        Token::Named(native(json!({"MemMbistAggressorsChannels":"Disabled"}))?),
                        Token::Named(native(json!({"SwCmdThrotCycles":0}))?),
                        Token::Named(native(json!({"FchConsoleOutMode":"Enabled"}))?),
                        Token::Named(native(json!({"MemMbistTest":"Enabled"}))?),
                        Token::Named(native(json!({"BmcGen2TxDeemphasis":"Disabled"}))?),
                        Token::Named(native(json!({"MemMbistPatternSelect":"Prbs"}))?),
                        Token::Named(native(json!({"MemHealBistEnable":"Disabled"}))?),
                        Token::Named(native(json!({"BmcVgaIoPortSize":0}))?),
                        Token::Named(native(json!({"FchConsoleOutSerialPort":"SuperIo"}))?),
                    ],
                },
            ),
            (
                "board-byte".into(),
                TokenSet::Byte {
                    values: vec![
                        Token::Named(native(json!({"MemMbistAggressorOn":false}))?),
                        Token::Named(native(
                            json!({"MemOverrideDimmSpdMaxActivityCount":"Auto"}),
                        )?),
                        Token::Named(native(json!({"DfGmiEncrypt":"Auto"}))?),
                        Token::Named(native(json!({"MemUrgRefLimit":6}))?),
                        Token::Named(native(json!({"BmcEndLane":129}))?),
                        Token::Named(native(json!({"MemAutoRefreshFineGranMode":"Fixed1Times"}))?),
                        Token::Named(native(json!({"BmcFunction":2}))?),
                        Token::Named(native(json!({"MemMbistWorseCasGranularity":0}))?),
                        Token::Named(native(json!({"FchSmbusSpeed":{"Value":42}}))?),
                        Token::Numeric(NumericToken {
                            id: 614336725,
                            value: 3,
                        }),
                        Token::Named(native(json!({"DfMemInterleavingSize":"Auto"}))?),
                        Token::Named(native(json!({"MemNvdimmPowerSource":"DeviceManaged"}))?),
                        Token::Numeric(NumericToken {
                            id: 686512045,
                            value: 30,
                        }),
                        Token::Named(native(json!({"BmcVgaIoBarToReplace":0}))?),
                        Token::Named(native(json!({"DfDramNumaPerSocket":"Four"}))?),
                        Token::Named(native(json!({"MemMbistReadDataEyeVoltageStep":2}))?),
                        Token::Named(native(json!({"DfRemapAt1TiB":"Auto"}))?),
                        Token::Named(native(json!({"MemDramAddressCommandParityRetryCount":1}))?),
                        Token::Named(native(json!({"Df4LinkMaxXgmiSpeed":"Auto"}))?),
                        Token::Named(native(json!({"MemMbistAggressorStaticLaneVal":0}))?),
                        Token::Named(native(json!({"MemDramDoubleRefreshRate":0}))?),
                        Token::Named(native(json!({"MemDataPoison":"Enabled"}))?),
                        Token::Named(native(json!({"MemMbistTgtStaticLaneVal":0}))?),
                        Token::Named(native(json!({"MemMbistDataEyeType":"1D Timing"}))?),
                        Token::Named(native(json!({"Df3LinkMaxXgmiSpeed":"Auto"}))?),
                        Token::Named(native(json!({"MemHealPprType":"SoftRepair"}))?),
                        Token::Numeric(NumericToken {
                            id: 1434937120,
                            value: 253,
                        }),
                        Token::Named(native(json!({"MemMbistTestMode":"PhysicalInterface"}))?),
                        Token::Named(native(json!({"MemMbistAggressorStaticLaneSelEcc":0}))?),
                        Token::Named(native(json!({"MemMbistReadDataEyeTimingStep":1}))?),
                        Token::Named(native(json!({"MemHealTestSelect":"Normal"}))?),
                        Token::Named(native(json!({"MemRollWindowDepth":{"Memclks":255}}))?),
                        Token::Named(native(json!({"FchConsoleOutSuperIoType":"Auto"}))?),
                        Token::Named(native(json!({"MemHealMaxBankFails":3}))?),
                        Token::Named(native(json!({"MemRcdParity":true}))?),
                        Token::Named(native(json!({"DfInvertDramMap":"Auto"}))?),
                        Token::Named(native(json!({"DfProbeFilter":"Auto"}))?),
                        Token::Named(native(json!({"OdtsCmdThrottleCycles":87}))?),
                        Token::Named(native(json!({"MemControllerWritingCrcMaxReplay":8}))?),
                        Token::Named(native(json!({"DfXgmiEncrypt":"Auto"}))?),
                        Token::Named(native(json!({"MemPsErrorHandling":0}))?),
                        Token::Named(native(json!({"DfSaveRestoreMemEncrypt":"Auto"}))?),
                        Token::Named(native(json!({"MemControllerWritingCrcMode":"Disabled"}))?),
                        Token::Named(native(json!({"DimmSensorResolution":1}))?),
                        Token::Named(native(json!({"BmcSocket":0}))?),
                        Token::Named(native(json!({"SecondPcieLinkSpeed":"Gen2"}))?),
                        Token::Named(native(json!({"PcieResetPinSelect":0}))?),
                        Token::Named(native(json!({"MemMbistDataEyeExecutionRepeatCount":1}))?),
                        Token::Named(native(json!({"DfBottomIo":176}))?),
                        Token::Named(native(json!({"MemDataScramble":1}))?),
                        Token::Named(native(json!({"BmcLinkSpeed":"PcieGen1"}))?),
                        Token::Named(native(json!({"DfMemClear":"Auto"}))?),
                        Token::Named(native(json!({"FchConsoleOutBasicEnable":0}))?),
                        Token::Named(native(json!({"MemMbistTgtStaticLaneSelEcc":0}))?),
                        Token::Named(native(json!({"DfXgmiTxEqMode":"Auto"}))?),
                        Token::Named(native(json!({"AblSerialBaudRate":"115200 Baud"}))?),
                        Token::Named(native(json!({"MemMbistPatternLength":3}))?),
                        Token::Named(native(json!({"BmcRcbCheckingMode":"Auto"}))?),
                        Token::Named(native(
                            json!({"MemTrainingHdtControl":"AssertionMessages"}),
                        )?),
                        Token::Named(native(json!({"DfXgmiConfig":"Auto"}))?),
                        Token::Named(native(json!({"MemMbistHaltOnError":1}))?),
                        Token::Named(native(json!({"BmcStartLane":129}))?),
                        Token::Named(native(json!({"MemSelfRefreshExitStaggering":"Disabled"}))?),
                        Token::Named(native(json!({"PmuTrainingMode":"1D,2D"}))?),
                        Token::Named(native(json!({"CbsMemUncorrectedEccRetryDdr4":true}))?),
                        Token::Named(native(json!({"MemControllerWritingCrcLimit":0}))?),
                        Token::Named(native(json!({"MemParityErrorMaxReplayDdr4":8}))?),
                        Token::Named(native(json!({"MemMbistWriteDataEyeVoltageStep":2}))?),
                        Token::Named(native(json!({"DfMemInterleaving":"Auto"}))?),
                        Token::Named(native(json!({"MemMbistPerBitSlaveDieReport":0}))?),
                        Token::Named(native(json!({"PspEnableDebugMode":"Disabled"}))?),
                        Token::Named(native(
                            json!({"MemRdimmTimingRcdF0Rc0FAdditionalLatency":"Auto"}),
                        )?),
                        Token::Named(native(json!({"MemTsmeModeRome":"Disabled"}))?),
                        Token::Named(native(json!({"BmcDevice":5}))?),
                        Token::Named(native(json!({"MemMbistWriteDataEyeTimingStep":1}))?),
                        Token::Named(native(json!({"MemMbistAggressorsChannels":"Disabled"}))?),
                        Token::Named(native(json!({"SwCmdThrotCycles":0}))?),
                        Token::Named(native(json!({"FchConsoleOutMode":"Disabled"}))?),
                        Token::Named(native(json!({"MemMbistTest":"Enabled"}))?),
                        Token::Named(native(json!({"MemSubUrgRefLowerBound":4}))?),
                        Token::Named(native(json!({"BmcGen2TxDeemphasis":"Disabled"}))?),
                        Token::Named(native(json!({"MemMbistPatternSelect":"Prbs"}))?),
                        Token::Named(native(json!({"MemHealBistEnable":"Disabled"}))?),
                        Token::Named(native(json!({"BmcVgaIoPortSize":0}))?),
                        Token::Named(native(json!({"FchConsoleOutSerialPort":"SuperIo"}))?),
                    ],
                },
            ),
            (
                "board-word".into(),
                TokenSet::Word {
                    values: vec![
                        Token::Named(native(json!({"BmcVgaIoPort":0}))?),
                        Token::Named(native(json!({"Dimm3DsSensorCritical":80}))?),
                        Token::Named(native(json!({"Dimm3DsSensorUpper":75}))?),
                        Token::Named(native(json!({"EccSymbolSize":"x16"}))?),
                        Token::Named(native(json!({"DimmSensorCritical":95}))?),
                        Token::Named(native(json!({"DimmSensorConfig":1032}))?),
                        Token::Named(native(json!({"DimmSensorUpper":80}))?),
                        Token::Named(native(json!({"DimmSensorLower":10}))?),
                        Token::Named(native(json!({"FchGppClkMap":"Auto"}))?),
                        Token::Named(native(json!({"PspSyshubWatchdogTimerInterval":2600}))?),
                    ],
                },
            ),
            (
                "board-dword".into(),
                TokenSet::Dword {
                    values: vec![
                        Token::Named(native(json!({"MemPowerDownMode":0}))?),
                        Token::Named(native(json!({"MemBusFrequencyLimit":"Ddr3200"}))?),
                        Token::Named(native(json!({"DfPciMmioSize":268435456}))?),
                        Token::Named(native(json!({"FchRom3BaseHigh":2044}))?),
                        Token::Named(native(json!({"MemMbistAggressorStaticLaneSelLo":0}))?),
                        Token::Named(native(json!({"MemMbistTgtStaticLaneSelLo":0}))?),
                        Token::Named(native(json!({"DfCakeCrcThresholdBounds":{"Value":100}}))?),
                        Token::Named(native(json!({"CcxMinSevAsid":1}))?),
                        Token::Named(native(json!({"MemMbistTgtStaticLaneSelHi":0}))?),
                        Token::Named(native(json!({"MemSelfHealBistTimeout":10000}))?),
                        Token::Named(native(json!({"MemClockValue":"Ddr2400"}))?),
                        Token::Named(native(json!({"PspMeasureConfig":0}))?),
                        Token::Named(native(json!({"MemMbistAggressorStaticLaneSelHi":0}))?),
                        Token::Named(native(json!({"MemUserTimingMode":"Auto"}))?),
                    ],
                },
            ),
            ("empty-bool".into(), TokenSet::Bool { values: vec![] }),
            (
                "cbs-backup-bool-medium".into(),
                TokenSet::Bool {
                    values: vec![
                        Token::Named(native(json!({"MemMbistAggressorStaticLaneControl":false}))?),
                        Token::Named(native(json!({"MemPostPackageRepairEnable":false}))?),
                        Token::Named(native(json!({"MemMbistTgtStaticLaneControl":false}))?),
                    ],
                },
            ),
            ("empty-byte".into(), TokenSet::Byte { values: vec![] }),
            (
                "cbs-backup-byte-medium".into(),
                TokenSet::Byte {
                    values: vec![
                        Token::Named(native(json!({"CbsMemAddrCmdParityErrorMaxReplayDdr4":8}))?),
                        Token::Numeric(NumericToken {
                            id: 218818671,
                            value: 0,
                        }),
                        Token::Named(native(json!({"MemUrgRefLimit":6}))?),
                        Token::Named(native(json!({"MemMbistWorseCasGranularity":0}))?),
                        Token::Named(native(json!({"MemMbistReadDataEyeVoltageStep":2}))?),
                        Token::Named(native(json!({"MemMbistAggressorStaticLaneVal":0}))?),
                        Token::Named(native(json!({"MemDramDoubleRefreshRate":0}))?),
                        Token::Numeric(NumericToken {
                            id: 1286431756,
                            value: 0,
                        }),
                        Token::Named(native(json!({"MemMbistTgtStaticLaneVal":0}))?),
                        Token::Named(native(
                            json!({"MemMbistDataEyeType":"WorstCaseMarginOnly"}),
                        )?),
                        Token::Named(native(json!({"MemHealPprType":"SoftRepair"}))?),
                        Token::Named(native(json!({"MemMbistAggressorStaticLaneSelEcc":0}))?),
                        Token::Named(native(json!({"MemMbistReadDataEyeTimingStep":1}))?),
                        Token::Named(native(json!({"MemHealTestSelect":"Normal"}))?),
                        Token::Named(native(json!({"CbsMemWriteCrcErrorMaxReplayDdr4":8}))?),
                        Token::Named(native(json!({"MemMbistTgtStaticLaneSelEcc":0}))?),
                        Token::Named(native(json!({"MemMbistPatternLength":3}))?),
                        Token::Numeric(NumericToken {
                            id: 3022303216,
                            value: 0,
                        }),
                        Token::Named(native(json!({"MemSelfRefreshExitStaggering":"Disabled"}))?),
                        Token::Named(native(json!({"MemMbistWriteDataEyeVoltageStep":2}))?),
                        Token::Named(native(json!({"MemMbistWriteDataEyeTimingStep":1}))?),
                        Token::Named(native(
                            json!({"MemMbistAggressorsChannels":"1 aggressor/(2 ch)"}),
                        )?),
                        Token::Named(native(json!({"MemSubUrgRefLowerBound":4}))?),
                        Token::Named(native(json!({"MemMbistPatternSelect":"Prbs"}))?),
                        Token::Named(native(json!({"MemHealBistEnable":"Disabled"}))?),
                        Token::Numeric(NumericToken {
                            id: 4285882405,
                            value: 1,
                        }),
                    ],
                },
            ),
            ("empty-word".into(), TokenSet::Word { values: vec![] }),
            (
                "cbs-backup-dword-medium".into(),
                TokenSet::Dword {
                    values: vec![
                        Token::Named(native(json!({"FchRom3BaseHigh":2044}))?),
                        Token::Named(native(json!({"MemMbistAggressorStaticLaneSelLo":0}))?),
                        Token::Named(native(json!({"MemMbistTgtStaticLaneSelLo":0}))?),
                        Token::Named(native(json!({"MemMbistTgtStaticLaneSelHi":0}))?),
                        Token::Named(native(json!({"MemMbistAggressorStaticLaneSelHi":0}))?),
                    ],
                },
            ),
            ("empty-dword".into(), TokenSet::Dword { values: vec![] }),
        ]),
        spd_images: BTreeMap::from([]),
    })
}
fn s8030_milan_board_id_eeprom() -> Result<Table> {
    Ok(Table::BoardIdEeprom {
        controller: 5,
        address: 80,
        id_offset: 16,
        revision_offset: 18,
        mapping: native(
            json!([{"id_and_rev_and_feature_mask":255,"id_and_feature_value":13,"rev_and_feature_value":"NotApplicable","board_instance_index":0},{"id_and_rev_and_feature_mask":255,"id_and_feature_value":48,"rev_and_feature_value":"NotApplicable","board_instance_index":0},{"id_and_rev_and_feature_mask":255,"id_and_feature_value":52,"rev_and_feature_value":{"Value":5},"board_instance_index":1},{"id_and_rev_and_feature_mask":255,"id_and_feature_value":52,"rev_and_feature_value":{"Value":6},"board_instance_index":1},{"id_and_rev_and_feature_mask":255,"id_and_feature_value":52,"rev_and_feature_value":{"Value":7},"board_instance_index":1},{"id_and_rev_and_feature_mask":255,"id_and_feature_value":52,"rev_and_feature_value":"NotApplicable","board_instance_index":0}]),
        )?,
    })
}
fn s8030_milan_spd_smbus() -> Result<Table> {
    Ok(Table::SpdSmbus {
        slots: native(
            json!([{"socket":0,"channel":0,"dimm":0,"address":160,"mux":224,"mux-channel":4},{"socket":0,"channel":0,"dimm":1,"address":162,"mux":224,"mux-channel":4},{"socket":0,"channel":1,"dimm":0,"address":164,"mux":224,"mux-channel":4},{"socket":0,"channel":1,"dimm":1,"address":166,"mux":224,"mux-channel":4},{"socket":0,"channel":2,"dimm":0,"address":168,"mux":224,"mux-channel":4},{"socket":0,"channel":2,"dimm":1,"address":170,"mux":224,"mux-channel":4},{"socket":0,"channel":3,"dimm":0,"address":172,"mux":224,"mux-channel":4},{"socket":0,"channel":3,"dimm":1,"address":174,"mux":224,"mux-channel":4},{"socket":0,"channel":4,"dimm":0,"address":160,"mux":224,"mux-channel":5},{"socket":0,"channel":4,"dimm":1,"address":162,"mux":224,"mux-channel":5},{"socket":0,"channel":5,"dimm":0,"address":164,"mux":224,"mux-channel":5},{"socket":0,"channel":5,"dimm":1,"address":166,"mux":224,"mux-channel":5},{"socket":0,"channel":6,"dimm":0,"address":168,"mux":224,"mux-channel":5},{"socket":0,"channel":6,"dimm":1,"address":170,"mux":224,"mux-channel":5},{"socket":0,"channel":7,"dimm":0,"address":172,"mux":224,"mux-channel":5},{"socket":0,"channel":7,"dimm":1,"address":174,"mux":224,"mux-channel":5},{"socket":1,"channel":0,"dimm":0,"address":160,"mux":224,"mux-channel":6},{"socket":1,"channel":0,"dimm":1,"address":162,"mux":224,"mux-channel":6},{"socket":1,"channel":1,"dimm":0,"address":164,"mux":224,"mux-channel":6},{"socket":1,"channel":1,"dimm":1,"address":166,"mux":224,"mux-channel":6},{"socket":1,"channel":2,"dimm":0,"address":168,"mux":224,"mux-channel":6},{"socket":1,"channel":2,"dimm":1,"address":170,"mux":224,"mux-channel":6},{"socket":1,"channel":3,"dimm":0,"address":172,"mux":224,"mux-channel":6},{"socket":1,"channel":3,"dimm":1,"address":174,"mux":224,"mux-channel":6},{"socket":1,"channel":4,"dimm":0,"address":160,"mux":224,"mux-channel":7},{"socket":1,"channel":4,"dimm":1,"address":162,"mux":224,"mux-channel":7},{"socket":1,"channel":5,"dimm":0,"address":164,"mux":224,"mux-channel":7},{"socket":1,"channel":5,"dimm":1,"address":166,"mux":224,"mux-channel":7},{"socket":1,"channel":6,"dimm":0,"address":168,"mux":224,"mux-channel":7},{"socket":1,"channel":6,"dimm":1,"address":170,"mux":224,"mux-channel":7},{"socket":1,"channel":7,"dimm":0,"address":172,"mux":224,"mux-channel":7},{"socket":1,"channel":7,"dimm":1,"address":174,"mux":224,"mux-channel":7}]),
        )?,
    })
}
fn s8030_milan_platform_override() -> Result<Table> {
    Ok(Table::PlatformOverride {
        scope: OverrideScope::All,
        operations: vec![
            Override::MaxDimmsPerChannel { value: 1 },
            Override::MaxChannelsPerSocket { value: 8 },
        ],
    })
}
fn s8030_milan_board_rdimm_odt() -> Result<Table> {
    Ok(Table::RdimmOdt {
        rows: native(
            json!([{"ranks":[["empty"],["single"],[]],"patterns":[[0,0],[0,0],[0,0],[0,0]]},{"ranks":[["empty"],["dual"],[]],"patterns":[[0,0],[0,0],[0,4],[0,8]]},{"ranks":[["single"],["empty"],[]],"patterns":[[0,0],[0,0],[0,0],[0,0]]},{"ranks":[["dual"],["empty"],[]],"patterns":[[0,1],[0,2],[0,0],[0,0]]},{"ranks":[["single"],["single"],[]],"patterns":[[0,0],[0,0],[0,0],[0,0]]},{"ranks":[["single"],["dual"],[]],"patterns":[[4,4],[0,0],[1,1],[1,1]]},{"ranks":[["dual"],["single"],[]],"patterns":[[4,4],[4,4],[1,1],[0,0]]},{"ranks":[["dual"],["dual"],[]],"patterns":[[12,12],[12,12],[3,3],[3,3]]}]),
        )?,
    })
}
fn s8030_milan_rdimm_max_frequency() -> Result<Table> {
    Ok(Table::RdimmMaxFrequency {
        rows: native(
            json!([{"slots":[1],"conditions":[1,1,0,0],"speed":"Ddr3200"},{"slots":[1],"conditions":[1,0,1,0],"speed":"Ddr3200"},{"slots":[2],"conditions":[1,1,0,0],"speed":"Ddr3200"},{"slots":[2],"conditions":[1,0,1,0],"speed":"Ddr3200"},{"slots":[2],"conditions":[2,2,0,0],"speed":"Ddr2933"},{"slots":[2],"conditions":[2,1,1,0],"speed":"Ddr2933"},{"slots":[2],"conditions":[2,0,2,0],"speed":"Ddr2933"}]),
        )?,
    })
}
fn s8030_milan_rdimm_stretch_frequency() -> Result<Table> {
    Ok(Table::RdimmStretchFrequency {
        rows: native(
            json!([{"slots":[1],"conditions":[1,1,0,0],"speed":"Ddr3200"},{"slots":[1],"conditions":[1,0,1,0],"speed":"Ddr3200"},{"slots":[2],"conditions":[1,1,0,0],"speed":"Ddr3200"},{"slots":[2],"conditions":[1,0,1,0],"speed":"Ddr3200"},{"slots":[2],"conditions":[2,2,0,0],"speed":"Ddr3200"},{"slots":[2],"conditions":[2,1,1,0],"speed":"Ddr3200"},{"slots":[2],"conditions":[2,0,2,0],"speed":"Ddr3200"}]),
        )?,
    })
}
fn s8030_milan_three_ds_max_frequency() -> Result<Table> {
    Ok(Table::ThreeDsMaxFrequency {
        rows: native(
            json!([{"slots":[1],"conditions":[1,0,1,0],"speed":"Ddr3200"},{"slots":[2],"conditions":[1,0,1,0],"speed":"Ddr2933"},{"slots":[2],"conditions":[2,0,2,0],"speed":"Ddr2667"}]),
        )?,
    })
}
fn s8030_milan_three_ds_stretch_frequency() -> Result<Table> {
    Ok(Table::ThreeDsStretchFrequency {
        rows: native(
            json!([{"slots":[1],"conditions":[1,0,1,0],"speed":"Ddr3200"},{"slots":[2],"conditions":[1,0,1,0],"speed":"Ddr3200"},{"slots":[2],"conditions":[2,0,2,0],"speed":"Ddr3200"}]),
        )?,
    })
}
fn s8030_milan_console() -> Result<Table> {
    Ok(Table::Console {
        settings: native(
            json!({"abl_console_out_control":{"enable_console_logging":true,"enable_mem_flow_logging":true,"enable_mem_setreg_logging":true,"enable_mem_getreg_logging":false,"enable_mem_status_logging":true,"enable_mem_pmu_logging":true,"enable_mem_pmu_sram_read_logging":false,"enable_mem_pmu_sram_write_logging":false,"enable_mem_test_verbose_logging":false,"enable_mem_basic_output_logging":true,"abl_console_port":128},"abl_breakpoint_control":{"enable_breakpoint":false,"break_on_all_dies":false}}),
        )?,
    })
}
fn s8030_milan_milan_error_output() -> Result<Table> {
    Ok(Table::MilanErrorOutput {
        settings: native(
            json!({"enable_error_reporting":false,"enable_error_reporting_gpio":false,"enable_error_reporting_beep_codes":false,"enable_using_handshake":false,"input_port":132,"output_delay":15000,"output_port":128,"stop_on_first_fatal_error":false,"input_port_size":"32 Bit","output_port_size":"32 Bit","input_port_type":"FchHtIo","output_port_type":"FchHtIo","clear_acknowledgement":false,"error_reporting_gpio":{"pin":85,"iomux_control":1,"bank_control":192},"beep_code_table":[{"custom_error_type":"General","peak_map":1,"peak_attr":{"peak_count":8,"pulse_width":0,"repeat_count":0}},{"custom_error_type":"Memory","peak_map":2,"peak_attr":{"peak_count":20,"pulse_width":0,"repeat_count":0}},{"custom_error_type":"Df","peak_map":3,"peak_attr":{"peak_count":20,"pulse_width":0,"repeat_count":0}},{"custom_error_type":"Ccx","peak_map":4,"peak_attr":{"peak_count":20,"pulse_width":0,"repeat_count":0}},{"custom_error_type":"Gnb","peak_map":5,"peak_attr":{"peak_count":20,"pulse_width":0,"repeat_count":0}},{"custom_error_type":"Psp","peak_map":6,"peak_attr":{"peak_count":20,"pulse_width":0,"repeat_count":0}},{"custom_error_type":"Smu","peak_map":7,"peak_attr":{"peak_count":20,"pulse_width":0,"repeat_count":0}},{"custom_error_type":"Unknown","peak_map":2,"peak_attr":{"peak_count":4,"pulse_width":0,"repeat_count":0}}],"enable_heart_beat":false,"enable_power_good_gpio":false,"power_good_gpio":{"pin":0,"iomux_control":0,"bank_control":0}}),
        )?,
    })
}
fn s8030_milan_voltage_control() -> Result<Table> {
    Ok(Table::VoltageControl {
        settings: native(
            json!({"enabled":true,"input_port":132,"output_port":128,"input_port_size":"32 Bit","output_port_size":"32 Bit","input_port_type":"FchHtIo","output_port_type":"FchHtIo","clear_acknowledgement":false}),
        )?,
    })
}
fn s8030_milan_lrdimm_max_frequency() -> Result<Table> {
    Ok(Table::LrdimmMaxFrequency {
        rows: native(
            json!([{"slots":[1],"conditions":[1,0,1,0],"speed":"Ddr3200"},{"slots":[1],"conditions":[1,0,0,1],"speed":"Ddr3200"},{"slots":[2],"conditions":[1,0,1,0],"speed":"Ddr3200"},{"slots":[2],"conditions":[1,0,0,1],"speed":"Ddr3200"},{"slots":[2],"conditions":[2,0,2,0],"speed":"Ddr2933"},{"slots":[2],"conditions":[2,0,0,2],"speed":"Ddr2933"}]),
        )?,
    })
}
fn s8030_milan_platform_tuning_end() -> Result<Table> {
    Ok(Table::PlatformTuningEnd {})
}
fn s8030_milan_empty_ppr() -> Result<Table> {
    Ok(Table::EmptyPpr { slots: 64 })
}
fn s8030_milan_slink() -> Result<Table> {
    Ok(Table::Slink {
        settings: native(
            json!({"regions":[{"size":0,"alignment":0,"socket":0,"phys_nbio_map":0,"interleaving":"256 B"},{"size":0,"alignment":0,"socket":0,"phys_nbio_map":0,"interleaving":"256 B"},{"size":0,"alignment":0,"socket":1,"phys_nbio_map":0,"interleaving":"256 B"},{"size":0,"alignment":0,"socket":1,"phys_nbio_map":0,"interleaving":"256 B"}]}),
        )?,
    })
}
pub(super) fn s8030_milan() -> Result<Definition> {
    let mut tables = BTreeMap::from([
        ("board-id-eeprom".into(), s8030_milan_board_id_eeprom()?),
        ("spd-smbus".into(), s8030_milan_spd_smbus()?),
        ("platform-override".into(), s8030_milan_platform_override()?),
        ("board-rdimm-odt".into(), s8030_milan_board_rdimm_odt()?),
        (
            "rdimm-max-frequency".into(),
            s8030_milan_rdimm_max_frequency()?,
        ),
        (
            "rdimm-stretch-frequency".into(),
            s8030_milan_rdimm_stretch_frequency()?,
        ),
        (
            "three-ds-max-frequency".into(),
            s8030_milan_three_ds_max_frequency()?,
        ),
        (
            "three-ds-stretch-frequency".into(),
            s8030_milan_three_ds_stretch_frequency()?,
        ),
        ("console".into(), s8030_milan_console()?),
        (
            "milan-error-output".into(),
            s8030_milan_milan_error_output()?,
        ),
        ("voltage-control".into(), s8030_milan_voltage_control()?),
        (
            "lrdimm-max-frequency".into(),
            s8030_milan_lrdimm_max_frequency()?,
        ),
        (
            "platform-tuning-end".into(),
            s8030_milan_platform_tuning_end()?,
        ),
        ("empty-ppr".into(), s8030_milan_empty_ppr()?),
        ("slink".into(), s8030_milan_slink()?),
    ]);
    for (name, table) in electrical_defaults()? {
        if tables.insert(name, table).is_some() {
            return Err("duplicate SP3 default table".into());
        }
    }
    Ok(Definition {
        generation: Generation::Milan,
        blobs: vec![
            Blob {
                name: "board".into(),
                revision: 8447,
                donor_sha256: "59a7be1a1dc8457b610e266ba5298572f386e4e4bf10f00daaf2f2fb33147de5"
                    .into(),
                bindings: vec![Binding {
                    slot: Slot::Backup,
                    instance: 0,
                }],
                groups: vec![Group::Psp, Group::Memory, Group::Token],
                records: vec![
                    Reference {
                        name: "board-id-eeprom".into(),
                        instance: 0,
                        board_mask: None,
                        priority: Some(vec![Priority::HardForce, Priority::Normal]),
                    },
                    Reference {
                        name: "spd-smbus".into(),
                        instance: 0,
                        board_mask: None,
                        priority: None,
                    },
                    Reference {
                        name: "platform-override".into(),
                        instance: 0,
                        board_mask: None,
                        priority: None,
                    },
                    Reference {
                        name: "board-rdimm-odt".into(),
                        instance: 0,
                        board_mask: None,
                        priority: None,
                    },
                    Reference {
                        name: "rdimm-cad".into(),
                        instance: 0,
                        board_mask: None,
                        priority: None,
                    },
                    Reference {
                        name: "rdimm-data".into(),
                        instance: 0,
                        board_mask: None,
                        priority: None,
                    },
                    Reference {
                        name: "rdimm-max-frequency".into(),
                        instance: 0,
                        board_mask: None,
                        priority: None,
                    },
                    Reference {
                        name: "rdimm-stretch-frequency".into(),
                        instance: 0,
                        board_mask: None,
                        priority: None,
                    },
                    Reference {
                        name: "three-ds-max-frequency".into(),
                        instance: 0,
                        board_mask: None,
                        priority: None,
                    },
                    Reference {
                        name: "three-ds-stretch-frequency".into(),
                        instance: 0,
                        board_mask: None,
                        priority: None,
                    },
                    Reference {
                        name: "three-ds-data".into(),
                        instance: 0,
                        board_mask: None,
                        priority: None,
                    },
                    Reference {
                        name: "console".into(),
                        instance: 0,
                        board_mask: None,
                        priority: None,
                    },
                    Reference {
                        name: "milan-error-output".into(),
                        instance: 0,
                        board_mask: None,
                        priority: None,
                    },
                    Reference {
                        name: "voltage-control".into(),
                        instance: 0,
                        board_mask: None,
                        priority: None,
                    },
                    Reference {
                        name: "lrdimm-odt".into(),
                        instance: 0,
                        board_mask: None,
                        priority: None,
                    },
                    Reference {
                        name: "lrdimm-cad".into(),
                        instance: 0,
                        board_mask: None,
                        priority: None,
                    },
                    Reference {
                        name: "lrdimm-data".into(),
                        instance: 0,
                        board_mask: None,
                        priority: None,
                    },
                    Reference {
                        name: "lrdimm-max-frequency".into(),
                        instance: 0,
                        board_mask: None,
                        priority: None,
                    },
                    Reference {
                        name: "platform-tuning-end".into(),
                        instance: 0,
                        board_mask: None,
                        priority: None,
                    },
                ],
                tokens: vec![
                    Reference {
                        name: "board-bool".into(),
                        instance: 0,
                        board_mask: Some(1),
                        priority: None,
                    },
                    Reference {
                        name: "board-bool-boards-65535".into(),
                        instance: 0,
                        board_mask: None,
                        priority: None,
                    },
                    Reference {
                        name: "board-byte".into(),
                        instance: 0,
                        board_mask: None,
                        priority: None,
                    },
                    Reference {
                        name: "board-word".into(),
                        instance: 0,
                        board_mask: None,
                        priority: None,
                    },
                    Reference {
                        name: "board-dword".into(),
                        instance: 0,
                        board_mask: None,
                        priority: None,
                    },
                ],
            },
            Blob {
                name: "cbs-backup".into(),
                revision: 7784,
                donor_sha256: "32eec005935662bc68a358ea01e5fc086443c1b62efa6313a69a689df73cb226"
                    .into(),
                bindings: vec![Binding {
                    slot: Slot::Backup,
                    instance: 8,
                }],
                groups: vec![Group::Token],
                records: vec![],
                tokens: vec![
                    Reference {
                        name: "empty-bool".into(),
                        instance: 0,
                        board_mask: None,
                        priority: Some(vec![Priority::Low]),
                    },
                    Reference {
                        name: "cbs-backup-bool-medium".into(),
                        instance: 1,
                        board_mask: None,
                        priority: Some(vec![Priority::Medium]),
                    },
                    Reference {
                        name: "empty-byte".into(),
                        instance: 0,
                        board_mask: None,
                        priority: Some(vec![Priority::Low]),
                    },
                    Reference {
                        name: "cbs-backup-byte-medium".into(),
                        instance: 1,
                        board_mask: None,
                        priority: Some(vec![Priority::Medium]),
                    },
                    Reference {
                        name: "empty-word".into(),
                        instance: 0,
                        board_mask: None,
                        priority: Some(vec![Priority::Low]),
                    },
                    Reference {
                        name: "cbs-backup-word-medium".into(),
                        instance: 1,
                        board_mask: None,
                        priority: Some(vec![Priority::Medium]),
                    },
                    Reference {
                        name: "empty-dword".into(),
                        instance: 0,
                        board_mask: None,
                        priority: Some(vec![Priority::Low]),
                    },
                    Reference {
                        name: "cbs-backup-dword-medium".into(),
                        instance: 1,
                        board_mask: None,
                        priority: Some(vec![Priority::Medium]),
                    },
                ],
            },
            Blob {
                name: "post-package-repair".into(),
                revision: 8760,
                donor_sha256: "cb3278a0024f69ebb78a36c61ee470ddf9df326f1bd75b4fab438259d471f410"
                    .into(),
                bindings: vec![
                    Binding {
                        slot: Slot::Backup,
                        instance: 9,
                    },
                    Binding {
                        slot: Slot::Active,
                        instance: 1,
                    },
                ],
                groups: vec![Group::Memory],
                records: vec![Reference {
                    name: "empty-ppr".into(),
                    instance: 0,
                    board_mask: None,
                    priority: Some(vec![Priority::EventLogging]),
                }],
                tokens: vec![],
            },
            Blob {
                name: "cbs-active".into(),
                revision: 7536,
                donor_sha256: "e29b0902d900e3f0df2b8f4cf413cf9a25408985a4cc9d604fde166cc5a67ecb"
                    .into(),
                bindings: vec![Binding {
                    slot: Slot::Active,
                    instance: 0,
                }],
                groups: vec![Group::Df, Group::Token],
                records: vec![Reference {
                    name: "slink".into(),
                    instance: 0,
                    board_mask: None,
                    priority: Some(vec![Priority::Low]),
                }],
                tokens: vec![
                    Reference {
                        name: "empty-bool".into(),
                        instance: 0,
                        board_mask: None,
                        priority: Some(vec![Priority::Low]),
                    },
                    Reference {
                        name: "cbs-backup-bool-medium".into(),
                        instance: 1,
                        board_mask: None,
                        priority: Some(vec![Priority::Medium]),
                    },
                    Reference {
                        name: "empty-byte".into(),
                        instance: 0,
                        board_mask: None,
                        priority: Some(vec![Priority::Low]),
                    },
                    Reference {
                        name: "cbs-backup-byte-medium".into(),
                        instance: 1,
                        board_mask: None,
                        priority: Some(vec![Priority::Medium]),
                    },
                    Reference {
                        name: "empty-word".into(),
                        instance: 0,
                        board_mask: None,
                        priority: Some(vec![Priority::Low]),
                    },
                    Reference {
                        name: "cbs-backup-word-medium".into(),
                        instance: 1,
                        board_mask: None,
                        priority: Some(vec![Priority::Medium]),
                    },
                    Reference {
                        name: "cbs-active-dword-medium".into(),
                        instance: 1,
                        board_mask: None,
                        priority: Some(vec![Priority::Medium]),
                    },
                    Reference {
                        name: "empty-dword".into(),
                        instance: 0,
                        board_mask: None,
                        priority: Some(vec![Priority::Low]),
                    },
                ],
            },
        ],
        tables,
        token_sets: BTreeMap::from([
            (
                "board-bool".into(),
                TokenSet::Bool {
                    values: vec![
                        Token::Named(native(json!({"MemLrdimmCapable":true}))?),
                        Token::Named(native(json!({"MemModeUnganged":true}))?),
                        Token::Named(native(json!({"PspTpPort":true}))?),
                        Token::Named(native(json!({"MemDimmTypeDdr3Capable":false}))?),
                        Token::Named(native(json!({"DfPickerThrottleEnable":false}))?),
                        Token::Named(native(json!({"PspEventLogDisplay":true}))?),
                        Token::Named(native(json!({"GnbAdditionalFeatures":true}))?),
                        Token::Named(native(json!({"MemForcePowerDownThrottleEnable":false}))?),
                        Token::Named(native(json!({"PspPsbAutoFuse":true}))?),
                        Token::Named(native(json!({"MemDqsTrainingControl":true}))?),
                        Token::Named(native(json!({"MemEnableParity":true}))?),
                        Token::Named(native(json!({"MemUdimmCapable":true}))?),
                        Token::Named(native(json!({"MemEnableBankGroupSwap":true}))?),
                        Token::Named(native(json!({"MemChannelInterleaving":false}))?),
                        Token::Named(native(json!({"MemPstate":true}))?),
                        Token::Named(native(json!({"MemAmp":true}))?),
                        Token::Named(native(json!({"MemLimitMemoryToBelow1TiB":true}))?),
                        Token::Named(native(json!({"MemEnableBankSwizzle":false}))?),
                        Token::Named(native(json!({"MemSpdReadOptimizationDdr4":true}))?),
                        Token::Named(native(json!({"DfGroupDPlatform":true}))?),
                        Token::Named(native(json!({"MemHoleRemapping":true}))?),
                        Token::Named(native(json!({"CcxPpinOptIn":false}))?),
                        Token::Named(native(json!({"MemEnableChipSelectInterleaving":false}))?),
                        Token::Named(native(json!({"ConfigureSecondPcieLink":false}))?),
                        Token::Named(native(json!({"MemMbistAggressorStaticLaneControl":false}))?),
                        Token::Named(native(json!({"MemSodimmCapable":true}))?),
                        Token::Named(native(json!({"MemIgnoreSpdChecksum":true}))?),
                        Token::Named(native(json!({"MemRdimmCapable":true}))?),
                        Token::Named(native(json!({"MemEccSyncFlood":false}))?),
                        Token::Named(native(
                            json!({"MemAutoRefreshsCountForThrottling":"Enabled"}),
                        )?),
                        Token::Named(native(json!({"MemNvdimmNDisable":false}))?),
                        Token::Named(native(json!({"GeneralCapsuleMode":true}))?),
                        Token::Named(native(json!({"MemDramDoubleRefreshRateMilan":false}))?),
                        Token::Named(native(
                            json!({"DfNps1With4ChannelInterleavedRdimm4ChannelNonInterleavedNvdimm":false}),
                        )?),
                        Token::Named(native(json!({"DisplayPmuTrainingResults":false}))?),
                        Token::Named(native(
                            json!({"GnbAdditionalFeatureL3PerformanceBias":true}),
                        )?),
                        Token::Named(native(json!({"MemSwCmdThrottleEnable":false}))?),
                        Token::Named(native(json!({"MemEnableBankGroupSwapAlt":true}))?),
                        Token::Named(native(json!({"MemDimmTypeLpddr3Capable":false}))?),
                        Token::Named(native(json!({"MemOnDieThermalSensor":true}))?),
                        Token::Named(native(json!({"MemAllClocks":true}))?),
                        Token::Named(native(json!({"MemEnablePowerDown":false}))?),
                        Token::Named(native(json!({"DxioVgaApiEnable":false}))?),
                        Token::Named(native(json!({"MemUncorrectedEccRetryDdr4":true}))?),
                        Token::Named(native(json!({"MemOdtsCmdThrottleEnable":true}))?),
                        Token::Named(native(json!({"MemClear":false}))?),
                        Token::Named(native(json!({"MemPostPackageRepairEnable":true}))?),
                        Token::Named(native(json!({"MemTsmeModeMilan":false}))?),
                        Token::Named(native(json!({"MemDdr4ForceDataMaskDisable":false}))?),
                        Token::Named(native(json!({"PspErrorDisplay":true}))?),
                        Token::Named(native(json!({"MemEccRedirection":false}))?),
                        Token::Named(native(json!({"MemMbistTgtStaticLaneControl":false}))?),
                        Token::Named(native(json!({"MemDdrRouteBalancedTee":false}))?),
                        Token::Named(native(json!({"MemQuadRankCapable":true}))?),
                        Token::Named(native(json!({"PspStopOnError":false}))?),
                        Token::Named(native(json!({"MemTempControlledRefreshEnable":false}))?),
                        Token::Named(native(json!({"PerformanceTracing":false}))?),
                        Token::Named(native(json!({"MemTempControlledExtendedRefresh":false}))?),
                        Token::Named(native(json!({"PcieResetControl":true}))?),
                        Token::Named(native(json!({"MemEnableEccFeature":true}))?),
                        Token::Named(native(json!({"MemRestoreControl":false}))?),
                    ],
                },
            ),
            (
                "board-bool-boards-65535".into(),
                TokenSet::Bool {
                    values: vec![
                        Token::Named(native(json!({"MemLrdimmCapable":true}))?),
                        Token::Named(native(json!({"MemModeUnganged":true}))?),
                        Token::Named(native(json!({"PspTpPort":true}))?),
                        Token::Named(native(json!({"MemDimmTypeDdr3Capable":false}))?),
                        Token::Named(native(json!({"DfPickerThrottleEnable":false}))?),
                        Token::Named(native(json!({"PspEventLogDisplay":true}))?),
                        Token::Named(native(json!({"GnbAdditionalFeatures":true}))?),
                        Token::Named(native(json!({"MemForcePowerDownThrottleEnable":false}))?),
                        Token::Named(native(json!({"PspPsbAutoFuse":true}))?),
                        Token::Named(native(json!({"MemDqsTrainingControl":true}))?),
                        Token::Named(native(json!({"MemEnableParity":true}))?),
                        Token::Named(native(json!({"MemUdimmCapable":true}))?),
                        Token::Named(native(json!({"MemEnableBankGroupSwap":true}))?),
                        Token::Named(native(json!({"MemChannelInterleaving":false}))?),
                        Token::Named(native(json!({"MemPstate":true}))?),
                        Token::Named(native(json!({"MemAmp":true}))?),
                        Token::Named(native(json!({"MemLimitMemoryToBelow1TiB":true}))?),
                        Token::Named(native(json!({"MemEnableBankSwizzle":false}))?),
                        Token::Named(native(json!({"VgaProgram":true}))?),
                        Token::Named(native(json!({"MemSpdReadOptimizationDdr4":true}))?),
                        Token::Named(native(json!({"DfGroupDPlatform":true}))?),
                        Token::Named(native(json!({"MemHoleRemapping":true}))?),
                        Token::Named(native(json!({"CcxPpinOptIn":false}))?),
                        Token::Named(native(json!({"MemOcVddioControl":false}))?),
                        Token::Named(native(json!({"MemEnableChipSelectInterleaving":false}))?),
                        Token::Named(native(json!({"ConfigureSecondPcieLink":false}))?),
                        Token::Named(native(json!({"MemMbistAggressorStaticLaneControl":false}))?),
                        Token::Named(native(json!({"MemSodimmCapable":true}))?),
                        Token::Named(native(json!({"MemIgnoreSpdChecksum":true}))?),
                        Token::Named(native(json!({"MemRdimmCapable":true}))?),
                        Token::Named(native(json!({"MemEccSyncFlood":false}))?),
                        Token::Named(native(
                            json!({"MemAutoRefreshsCountForThrottling":"Enabled"}),
                        )?),
                        Token::Named(native(json!({"MemNvdimmNDisable":false}))?),
                        Token::Named(native(json!({"GeneralCapsuleMode":true}))?),
                        Token::Named(native(json!({"MemDramDoubleRefreshRateMilan":false}))?),
                        Token::Named(native(
                            json!({"DfNps1With4ChannelInterleavedRdimm4ChannelNonInterleavedNvdimm":false}),
                        )?),
                        Token::Named(native(json!({"DisplayPmuTrainingResults":false}))?),
                        Token::Named(native(
                            json!({"GnbAdditionalFeatureL3PerformanceBias":true}),
                        )?),
                        Token::Named(native(json!({"MemSwCmdThrottleEnable":false}))?),
                        Token::Named(native(json!({"MemEnableBankGroupSwapAlt":true}))?),
                        Token::Named(native(json!({"MemDimmTypeLpddr3Capable":false}))?),
                        Token::Named(native(json!({"MemOnDieThermalSensor":true}))?),
                        Token::Named(native(json!({"MemAllClocks":true}))?),
                        Token::Named(native(json!({"MemEnablePowerDown":false}))?),
                        Token::Named(native(json!({"DxioVgaApiEnable":false}))?),
                        Token::Named(native(json!({"MemUncorrectedEccRetryDdr4":true}))?),
                        Token::Named(native(json!({"MemOdtsCmdThrottleEnable":true}))?),
                        Token::Named(native(json!({"MemClear":false}))?),
                        Token::Named(native(json!({"MemPostPackageRepairEnable":true}))?),
                        Token::Named(native(json!({"MemTsmeModeMilan":false}))?),
                        Token::Named(native(json!({"MemDdr4ForceDataMaskDisable":false}))?),
                        Token::Named(native(json!({"PspErrorDisplay":true}))?),
                        Token::Named(native(json!({"MemEccRedirection":false}))?),
                        Token::Named(native(json!({"MemMbistTgtStaticLaneControl":false}))?),
                        Token::Named(native(json!({"MemDdrRouteBalancedTee":false}))?),
                        Token::Named(native(json!({"MemQuadRankCapable":true}))?),
                        Token::Named(native(json!({"PspStopOnError":false}))?),
                        Token::Named(native(json!({"MemTempControlledRefreshEnable":false}))?),
                        Token::Named(native(json!({"PerformanceTracing":false}))?),
                        Token::Named(native(json!({"MemTempControlledExtendedRefresh":false}))?),
                        Token::Named(native(json!({"PcieResetControl":true}))?),
                        Token::Named(native(json!({"MemEnableEccFeature":true}))?),
                        Token::Named(native(json!({"BmcInitBeforeDram":false}))?),
                        Token::Named(native(json!({"MemRestoreControl":false}))?),
                    ],
                },
            ),
            (
                "board-byte".into(),
                TokenSet::Byte {
                    values: vec![
                        Token::Named(native(json!({"MemMbistAggressorOn":false}))?),
                        Token::Named(native(
                            json!({"MemOverrideDimmSpdMaxActivityCount":"Auto"}),
                        )?),
                        Token::Named(native(json!({"DfGmiEncrypt":"Auto"}))?),
                        Token::Named(native(json!({"MemUrgRefLimit":6}))?),
                        Token::Named(native(json!({"BmcEndLane":129}))?),
                        Token::Named(native(json!({"MemAutoRefreshFineGranMode":"Fixed1Times"}))?),
                        Token::Named(native(json!({"BmcFunction":2}))?),
                        Token::Named(native(json!({"WorkloadProfile":"Disabled"}))?),
                        Token::Named(native(json!({"MemMbistWorseCasGranularity":0}))?),
                        Token::Named(native(json!({"FchSmbusSpeed":{"Value":42}}))?),
                        Token::Named(native(json!({"DfSysStorageAtTopOfMem":"Auto"}))?),
                        Token::Named(native(json!({"DfMemInterleavingSize":"Auto"}))?),
                        Token::Named(native(json!({"MemNvdimmPowerSource":"DeviceManaged"}))?),
                        Token::Named(native(json!({"DfDramNumaPerSocket":"Auto"}))?),
                        Token::Named(native(json!({"GnbAdditionalFeatureDsm2":"Auto"}))?),
                        Token::Named(native(json!({"MemMbistReadDataEyeVoltageStep":1}))?),
                        Token::Named(native(json!({"DfRemapAt1TiB":"Auto"}))?),
                        Token::Named(native(json!({"MemDramAddressCommandParityRetryCount":1}))?),
                        Token::Named(native(json!({"Df4LinkMaxXgmiSpeed":"Auto"}))?),
                        Token::Named(native(json!({"MemMbistDataEyeSilentExecution":false}))?),
                        Token::Named(native(json!({"MemMbistAggressorStaticLaneVal":0}))?),
                        Token::Named(native(json!({"MemDramDoubleRefreshRate":0}))?),
                        Token::Named(native(json!({"MemDataPoison":"Enabled"}))?),
                        Token::Named(native(json!({"MemMbistTgtStaticLaneVal":0}))?),
                        Token::Named(native(json!({"MemMbistDataEyeType":"1D Timing"}))?),
                        Token::Named(native(json!({"Df3LinkMaxXgmiSpeed":"Auto"}))?),
                        Token::Named(native(json!({"MemHealPprType":"SoftRepair"}))?),
                        Token::Named(native(json!({"CcxSevAsidCount":"509"}))?),
                        Token::Named(native(json!({"MemMbistTestMode":"PhysicalInterface"}))?),
                        Token::Named(native(json!({"MemMbistAggressorStaticLaneSelEcc":0}))?),
                        Token::Named(native(json!({"MemMbistReadDataEyeTimingStep":1}))?),
                        Token::Named(native(json!({"MemHealTestSelect":"Normal"}))?),
                        Token::Named(native(json!({"MemRollWindowDepth":{"Memclks":255}}))?),
                        Token::Named(native(json!({"FchConsoleOutSuperIoType":"Auto"}))?),
                        Token::Named(native(json!({"MemHealMaxBankFails":3}))?),
                        Token::Named(native(json!({"DvArbiterMin":1}))?),
                        Token::Named(native(json!({"MemRcdParity":true}))?),
                        Token::Named(native(json!({"DfInvertDramMap":"Auto"}))?),
                        Token::Named(native(json!({"DfProbeFilter":"Auto"}))?),
                        Token::Named(native(json!({"OdtsCmdThrottleCycles":87}))?),
                        Token::Named(native(json!({"MemControllerWritingCrcMaxReplay":8}))?),
                        Token::Named(native(json!({"DfXgmiEncrypt":"Auto"}))?),
                        Token::Named(native(json!({"MemPsErrorHandling":0}))?),
                        Token::Named(native(json!({"DvArbiterMax":10}))?),
                        Token::Named(native(
                            json!({"MemPmuBistTestSelect":{"algorithm_1":true,"algorithm_2":true,"algorithm_3":true,"algorithm_4":true,"algorithm_5":true}}),
                        )?),
                        Token::Named(native(json!({"MemCpuVrefRange":0}))?),
                        Token::Named(native(json!({"DfSaveRestoreMemEncrypt":"Auto"}))?),
                        Token::Named(native(json!({"MemControllerWritingCrcMode":"Disabled"}))?),
                        Token::Named(native(json!({"DimmSensorResolution":1}))?),
                        Token::Named(native(json!({"BmcSocket":0}))?),
                        Token::Named(native(json!({"SecondPcieLinkSpeed":"Gen2"}))?),
                        Token::Named(native(json!({"PcieResetPinSelect":0}))?),
                        Token::Named(native(json!({"MemMbistDataEyeExecutionRepeatCount":1}))?),
                        Token::Named(native(json!({"DfBottomIo":176}))?),
                        Token::Named(native(json!({"FchConsoleOutSerialPortIoBase":"3f8"}))?),
                        Token::Named(native(
                            json!({"MemTccd5ReadCommandSpacingMode":"Disabled"}),
                        )?),
                        Token::Named(native(json!({"MemDataScramble":1}))?),
                        Token::Named(native(json!({"BmcLinkSpeed":"PcieGen1"}))?),
                        Token::Named(native(json!({"DfMemClear":"Auto"}))?),
                        Token::Named(native(json!({"FchConsoleOutBasicEnable":0}))?),
                        Token::Named(native(json!({"MemRcwWeakDriveDisable":"Enabled"}))?),
                        Token::Named(native(json!({"MemMbistTgtStaticLaneSelEcc":0}))?),
                        Token::Named(native(json!({"MemDramVrefRange":0}))?),
                        Token::Named(native(json!({"PspSevMode2":"Enabled"}))?),
                        Token::Named(native(json!({"DfXgmiTxEqMode":"Auto"}))?),
                        Token::Named(native(json!({"AblSerialBaudRate":"115200 Baud"}))?),
                        Token::Named(native(json!({"MemMbistPatternLength":3}))?),
                        Token::Named(native(json!({"BmcRcbCheckingMode":"Auto"}))?),
                        Token::Named(native(json!({"DfPstateModeSelect":"Auto"}))?),
                        Token::Named(native(
                            json!({"MemTrainingHdtControl":"AssertionMessages"}),
                        )?),
                        Token::Named(native(json!({"ReservedDramModuleDrtmMode":"Disabled"}))?),
                        Token::Named(native(json!({"DfXgmiConfig":"Auto"}))?),
                        Token::Named(native(json!({"MemMbistHaltOnError":1}))?),
                        Token::Named(native(json!({"BmcStartLane":129}))?),
                        Token::Named(native(json!({"MemSelfRefreshExitStaggering":"Disabled"}))?),
                        Token::Named(native(json!({"PmuTrainingMode":"1D,2D"}))?),
                        Token::Named(native(json!({"CbsMemUncorrectedEccRetryDdr4":true}))?),
                        Token::Named(native(json!({"MemControllerWritingCrcLimit":0}))?),
                        Token::Named(native(json!({"MemParityErrorMaxReplayDdr4":8}))?),
                        Token::Named(native(json!({"MemActionOnBistFailure":"DoNothing"}))?),
                        Token::Named(native(json!({"MemMbistWriteDataEyeVoltageStep":1}))?),
                        Token::Named(native(json!({"DfMemInterleaving":"Auto"}))?),
                        Token::Named(native(json!({"MemMbistPerBitSlaveDieReport":0}))?),
                        Token::Named(native(json!({"PspEnableDebugMode":"Disabled"}))?),
                        Token::Named(native(
                            json!({"MemRdimmTimingRcdF0Rc0FAdditionalLatency":"Auto"}),
                        )?),
                        Token::Named(native(json!({"BmcDevice":5}))?),
                        Token::Named(native(json!({"MemMbistWriteDataEyeTimingStep":1}))?),
                        Token::Named(native(json!({"FchPreferredIo":"Auto"}))?),
                        Token::Named(native(json!({"MemMbistAggressorsChannels":"Disabled"}))?),
                        Token::Named(native(json!({"SwCmdThrotCycles":0}))?),
                        Token::Named(native(json!({"FchConsoleOutMode":"Disabled"}))?),
                        Token::Named(native(json!({"MemMbistTest":"Disabled"}))?),
                        Token::Named(native(
                            json!({"SecondPcieLinkMaxPayload":"HardwareDefault"}),
                        )?),
                        Token::Named(native(json!({"MemSubUrgRefLowerBound":4}))?),
                        Token::Named(native(json!({"GnbSmuDfPstateFclkLimit":"Auto"}))?),
                        Token::Named(native(json!({"MemMbistPatternSelect":"Prbs"}))?),
                        Token::Named(native(json!({"GnbAdditionalFeatureDsmDetector2":"Auto"}))?),
                        Token::Named(native(json!({"MemHealBistEnable":"Disabled"}))?),
                        Token::Named(native(json!({"FchConsoleOutSerialPort":"SuperIo"}))?),
                    ],
                },
            ),
            (
                "board-word".into(),
                TokenSet::Word {
                    values: vec![
                        Token::Named(native(json!({"Dimm3DsSensorCritical":80}))?),
                        Token::Named(native(json!({"LrdimmSensorCritical":80}))?),
                        Token::Named(native(json!({"ScrubL2Rate":0}))?),
                        Token::Named(native(json!({"Dimm3DsSensorUpper":75}))?),
                        Token::Named(native(json!({"EccSymbolSize":"x16"}))?),
                        Token::Named(native(json!({"DimmSensorCritical":95}))?),
                        Token::Named(native(json!({"DimmSensorConfig":1032}))?),
                        Token::Named(native(json!({"FchI2cSdaHoldOverrideMode":"IgnoreBoth"}))?),
                        Token::Named(native(json!({"FchI2cSdaTxHold":0}))?),
                        Token::Named(native(json!({"ScrubIcacheRate":0}))?),
                        Token::Named(native(json!({"ScrubDramRate":0}))?),
                        Token::Named(native(json!({"FchI2cSdaRxHold":0}))?),
                        Token::Named(native(json!({"ScrubDcacheRate":0}))?),
                        Token::Named(native(json!({"DimmSensorUpper":80}))?),
                        Token::Named(native(json!({"ScrubL3Rate":0}))?),
                        Token::Named(native(json!({"DimmSensorLower":10}))?),
                        Token::Named(native(json!({"FchGppClkMap":"Auto"}))?),
                        Token::Named(native(json!({"LrdimmSensorUpper":66}))?),
                        Token::Named(native(json!({"PspSyshubWatchdogTimerInterval":2600}))?),
                    ],
                },
            ),
            (
                "board-dword".into(),
                TokenSet::Dword {
                    values: vec![
                        Token::Named(native(json!({"DxioPhyParamDc":"Skip"}))?),
                        Token::Named(native(json!({"MemPowerDownMode":0}))?),
                        Token::Named(native(json!({"MemBusFrequencyLimit":"Ddr3200"}))?),
                        Token::Named(native(json!({"DfPciMmioSize":268435456}))?),
                        Token::Named(native(json!({"FchRom3BaseHigh":2044}))?),
                        Token::Named(native(json!({"PcieResetGpioPin":4294967295u64}))?),
                        Token::Named(native(json!({"MemMbistAggressorStaticLaneSelLo":0}))?),
                        Token::Named(native(json!({"DxioPhyParamIqofc":"Skip"}))?),
                        Token::Named(native(json!({"MemMbistTgtStaticLaneSelLo":0}))?),
                        Token::Named(native(json!({"GnbAdditionalFeatureOnRamp":70}))?),
                        Token::Named(native(json!({"DfCakeCrcThresholdBounds":{"Value":100}}))?),
                        Token::Named(native(json!({"CcxMinSevAsid":1}))?),
                        Token::Named(native(json!({"MemMbistTgtStaticLaneSelHi":0}))?),
                        Token::Named(native(json!({"DxioPhyParamPole":"Skip"}))?),
                        Token::Named(native(json!({"MemSelfHealBistTimeout":10000}))?),
                        Token::Named(native(json!({"MemClockValue":"Ddr2400"}))?),
                        Token::Named(native(json!({"CpuFetchFromSpiApBase":4293918720u64}))?),
                        Token::Named(native(json!({"PspMeasureConfig":0}))?),
                        Token::Named(native(json!({"DxioPhyParamVga":"Skip"}))?),
                        Token::Named(native(json!({"GnbAdditionalFeatureOffRamp":40}))?),
                        Token::Named(native(json!({"MemMbistAggressorStaticLaneSelHi":0}))?),
                        Token::Named(native(json!({"MemUserTimingMode":"Auto"}))?),
                    ],
                },
            ),
            ("empty-bool".into(), TokenSet::Bool { values: vec![] }),
            (
                "cbs-backup-bool-medium".into(),
                TokenSet::Bool {
                    values: vec![
                        Token::Numeric(NumericToken {
                            id: 218818671,
                            value: 0,
                        }),
                        Token::Named(native(json!({"MemMbistAggressorStaticLaneControl":false}))?),
                        Token::Named(native(json!({"MemPostPackageRepairEnable":false}))?),
                        Token::Named(native(json!({"MemMbistTgtStaticLaneControl":false}))?),
                    ],
                },
            ),
            ("empty-byte".into(), TokenSet::Byte { values: vec![] }),
            (
                "cbs-backup-byte-medium".into(),
                TokenSet::Byte {
                    values: vec![
                        Token::Named(native(json!({"CbsMemAddrCmdParityErrorMaxReplayDdr4":8}))?),
                        Token::Numeric(NumericToken {
                            id: 163953868,
                            value: 8,
                        }),
                        Token::Named(native(json!({"MemUrgRefLimit":6}))?),
                        Token::Named(native(json!({"MemMbistWorseCasGranularity":0}))?),
                        Token::Named(native(json!({"DfDramNumaPerSocket":"Four"}))?),
                        Token::Named(native(json!({"MemMbistReadDataEyeVoltageStep":1}))?),
                        Token::Named(native(json!({"MemMbistAggressorStaticLaneVal":0}))?),
                        Token::Named(native(json!({"MemDramDoubleRefreshRate":0}))?),
                        Token::Numeric(NumericToken {
                            id: 1286431756,
                            value: 0,
                        }),
                        Token::Named(native(json!({"MemMbistTgtStaticLaneVal":0}))?),
                        Token::Named(native(json!({"MemHealPprType":"SoftRepair"}))?),
                        Token::Named(native(json!({"MemMbistAggressorStaticLaneSelEcc":0}))?),
                        Token::Named(native(json!({"MemMbistReadDataEyeTimingStep":1}))?),
                        Token::Named(native(json!({"MemHealTestSelect":"Normal"}))?),
                        Token::Numeric(NumericToken {
                            id: 1698103689,
                            value: 255,
                        }),
                        Token::Numeric(NumericToken {
                            id: 1799957978,
                            value: 16,
                        }),
                        Token::Named(native(
                            json!({"MemPmuBistTestSelect":{"algorithm_1":true,"algorithm_2":true,"algorithm_3":true,"algorithm_4":true,"algorithm_5":true}}),
                        )?),
                        Token::Named(native(json!({"CbsMemWriteCrcErrorMaxReplayDdr4":8}))?),
                        Token::Named(native(
                            json!({"MemTccd5ReadCommandSpacingMode":"Disabled"}),
                        )?),
                        Token::Named(native(json!({"MemMbistTgtStaticLaneSelEcc":0}))?),
                        Token::Named(native(json!({"PspSevMode2":"Enabled"}))?),
                        Token::Named(native(json!({"MemMbistPatternLength":3}))?),
                        Token::Numeric(NumericToken {
                            id: 3022303216,
                            value: 0,
                        }),
                        Token::Named(native(json!({"MemSelfRefreshExitStaggering":"Disabled"}))?),
                        Token::Named(native(json!({"MemMbistWriteDataEyeVoltageStep":1}))?),
                        Token::Named(native(json!({"DfMemInterleaving":"Auto"}))?),
                        Token::Named(native(json!({"MemMbistWriteDataEyeTimingStep":1}))?),
                        Token::Numeric(NumericToken {
                            id: 3649799737,
                            value: 255,
                        }),
                        Token::Named(native(
                            json!({"MemMbistAggressorsChannels":"1 aggressor/(2 ch)"}),
                        )?),
                        Token::Named(native(json!({"MemSubUrgRefLowerBound":4}))?),
                        Token::Numeric(NumericToken {
                            id: 4103355309,
                            value: 255,
                        }),
                        Token::Named(native(json!({"MemMbistPatternSelect":"Prbs"}))?),
                        Token::Named(native(json!({"MemHealBistEnable":"Disabled"}))?),
                        Token::Numeric(NumericToken {
                            id: 4285882405,
                            value: 1,
                        }),
                    ],
                },
            ),
            ("empty-word".into(), TokenSet::Word { values: vec![] }),
            (
                "cbs-backup-word-medium".into(),
                TokenSet::Word {
                    values: vec![Token::Named(native(
                        json!({"CbsMemPowerDownDelay":{"Value":3000}}),
                    )?)],
                },
            ),
            ("empty-dword".into(), TokenSet::Dword { values: vec![] }),
            (
                "cbs-backup-dword-medium".into(),
                TokenSet::Dword {
                    values: vec![
                        Token::Numeric(NumericToken {
                            id: 420813139,
                            value: 0,
                        }),
                        Token::Named(native(json!({"FchRom3BaseHigh":2044}))?),
                        Token::Named(native(json!({"MemMbistAggressorStaticLaneSelLo":0}))?),
                        Token::Named(native(json!({"MemMbistTgtStaticLaneSelLo":0}))?),
                        Token::Numeric(NumericToken {
                            id: 2411588270,
                            value: 0,
                        }),
                        Token::Named(native(json!({"MemMbistTgtStaticLaneSelHi":0}))?),
                        Token::Numeric(NumericToken {
                            id: 3006347150,
                            value: 1447249234,
                        }),
                        Token::Named(native(json!({"MemMbistAggressorStaticLaneSelHi":0}))?),
                    ],
                },
            ),
            (
                "cbs-active-dword-medium".into(),
                TokenSet::Dword {
                    values: vec![
                        Token::Numeric(NumericToken {
                            id: 420813139,
                            value: 0,
                        }),
                        Token::Named(native(json!({"FchRom3BaseHigh":2044}))?),
                        Token::Named(native(json!({"MemMbistAggressorStaticLaneSelLo":0}))?),
                        Token::Named(native(json!({"MemMbistTgtStaticLaneSelLo":0}))?),
                        Token::Numeric(NumericToken {
                            id: 2411588270,
                            value: 0,
                        }),
                        Token::Named(native(json!({"MemMbistTgtStaticLaneSelHi":0}))?),
                        Token::Named(native(json!({"MemMbistAggressorStaticLaneSelHi":0}))?),
                    ],
                },
            ),
        ]),
        spd_images: BTreeMap::from([]),
    })
}
fn ethanolx_milan_spd_smbus() -> Result<Table> {
    Ok(Table::SpdSmbus {
        slots: native(
            json!([{"socket":0,"channel":0,"dimm":0,"address":160,"mux":148,"mux-channel":128,"mux-control":3},{"socket":0,"channel":0,"dimm":1,"address":162,"mux":148,"mux-channel":128,"mux-control":3},{"socket":0,"channel":1,"dimm":0,"address":164,"mux":148,"mux-channel":128,"mux-control":3},{"socket":0,"channel":1,"dimm":1,"address":166,"mux":148,"mux-channel":128,"mux-control":3},{"socket":0,"channel":2,"dimm":0,"address":168,"mux":148,"mux-channel":128,"mux-control":3},{"socket":0,"channel":2,"dimm":1,"address":170,"mux":148,"mux-channel":128,"mux-control":3},{"socket":0,"channel":3,"dimm":0,"address":172,"mux":148,"mux-channel":128,"mux-control":3},{"socket":0,"channel":3,"dimm":1,"address":174,"mux":148,"mux-channel":128,"mux-control":3},{"socket":0,"channel":4,"dimm":0,"address":160,"mux":148,"mux-channel":64,"mux-control":3},{"socket":0,"channel":4,"dimm":1,"address":162,"mux":148,"mux-channel":64,"mux-control":3},{"socket":0,"channel":5,"dimm":0,"address":164,"mux":148,"mux-channel":64,"mux-control":3},{"socket":0,"channel":5,"dimm":1,"address":166,"mux":148,"mux-channel":64,"mux-control":3},{"socket":0,"channel":6,"dimm":0,"address":168,"mux":148,"mux-channel":64,"mux-control":3},{"socket":0,"channel":6,"dimm":1,"address":170,"mux":148,"mux-channel":64,"mux-control":3},{"socket":0,"channel":7,"dimm":0,"address":172,"mux":148,"mux-channel":64,"mux-control":3},{"socket":0,"channel":7,"dimm":1,"address":174,"mux":148,"mux-channel":64,"mux-control":3},{"socket":1,"channel":0,"dimm":0,"address":160,"mux":148,"mux-channel":32,"mux-control":3},{"socket":1,"channel":0,"dimm":1,"address":162,"mux":148,"mux-channel":32,"mux-control":3},{"socket":1,"channel":1,"dimm":0,"address":164,"mux":148,"mux-channel":32,"mux-control":3},{"socket":1,"channel":1,"dimm":1,"address":166,"mux":148,"mux-channel":32,"mux-control":3},{"socket":1,"channel":2,"dimm":0,"address":168,"mux":148,"mux-channel":32,"mux-control":3},{"socket":1,"channel":2,"dimm":1,"address":170,"mux":148,"mux-channel":32,"mux-control":3},{"socket":1,"channel":3,"dimm":0,"address":172,"mux":148,"mux-channel":32,"mux-control":3},{"socket":1,"channel":3,"dimm":1,"address":174,"mux":148,"mux-channel":32,"mux-control":3},{"socket":1,"channel":4,"dimm":0,"address":160,"mux":148,"mux-channel":16,"mux-control":3},{"socket":1,"channel":4,"dimm":1,"address":162,"mux":148,"mux-channel":16,"mux-control":3},{"socket":1,"channel":5,"dimm":0,"address":164,"mux":148,"mux-channel":16,"mux-control":3},{"socket":1,"channel":5,"dimm":1,"address":166,"mux":148,"mux-channel":16,"mux-control":3},{"socket":1,"channel":6,"dimm":0,"address":168,"mux":148,"mux-channel":16,"mux-control":3},{"socket":1,"channel":6,"dimm":1,"address":170,"mux":148,"mux-channel":16,"mux-control":3},{"socket":1,"channel":7,"dimm":0,"address":172,"mux":148,"mux-channel":16,"mux-control":3},{"socket":1,"channel":7,"dimm":1,"address":174,"mux":148,"mux-channel":16,"mux-control":3}]),
        )?,
    })
}
fn ethanolx_milan_platform_override() -> Result<Table> {
    Ok(Table::PlatformOverride {
        scope: OverrideScope::All,
        operations: vec![
            Override::MaxDimmsPerChannel { value: 1 },
            Override::MaxChannelsPerSocket { value: 8 },
        ],
    })
}
fn ethanolx_milan_board_rdimm_odt() -> Result<Table> {
    Ok(Table::RdimmOdt {
        rows: native(
            json!([{"ranks":[["empty"],["single"],[]],"patterns":[[0,0],[0,0],[0,0],[0,0]]},{"ranks":[["empty"],["dual"],[]],"patterns":[[0,0],[0,0],[0,4],[0,8]]},{"ranks":[["single"],["empty"],[]],"patterns":[[0,0],[0,0],[0,0],[0,0]]},{"ranks":[["dual"],["empty"],[]],"patterns":[[0,1],[0,2],[0,0],[0,0]]},{"ranks":[["single"],["single"],[]],"patterns":[[0,0],[0,0],[0,0],[0,0]]},{"ranks":[["single"],["dual"],[]],"patterns":[[4,4],[0,0],[1,1],[1,1]]},{"ranks":[["dual"],["single"],[]],"patterns":[[4,4],[4,4],[1,1],[0,0]]},{"ranks":[["dual"],["dual"],[]],"patterns":[[12,12],[12,12],[3,3],[3,3]]}]),
        )?,
    })
}
fn ethanolx_milan_rdimm_max_frequency() -> Result<Table> {
    Ok(Table::RdimmMaxFrequency {
        rows: native(
            json!([{"slots":[1],"conditions":[1,1,0,0],"speed":"Ddr3200"},{"slots":[1],"conditions":[1,0,1,0],"speed":"Ddr3200"},{"slots":[2],"conditions":[1,1,0,0],"speed":"Ddr3200"},{"slots":[2],"conditions":[1,0,1,0],"speed":"Ddr3200"},{"slots":[2],"conditions":[2,2,0,0],"speed":"Ddr2933"},{"slots":[2],"conditions":[2,1,1,0],"speed":"Ddr2933"},{"slots":[2],"conditions":[2,0,2,0],"speed":"Ddr2933"}]),
        )?,
    })
}
fn ethanolx_milan_rdimm_stretch_frequency() -> Result<Table> {
    Ok(Table::RdimmStretchFrequency {
        rows: native(
            json!([{"slots":[1],"conditions":[1,1,0,0],"speed":"Ddr3200"},{"slots":[1],"conditions":[1,0,1,0],"speed":"Ddr3200"},{"slots":[2],"conditions":[1,1,0,0],"speed":"Ddr3200"},{"slots":[2],"conditions":[1,0,1,0],"speed":"Ddr3200"},{"slots":[2],"conditions":[2,2,0,0],"speed":"Ddr3200"},{"slots":[2],"conditions":[2,1,1,0],"speed":"Ddr3200"},{"slots":[2],"conditions":[2,0,2,0],"speed":"Ddr3200"}]),
        )?,
    })
}
fn ethanolx_milan_three_ds_max_frequency() -> Result<Table> {
    Ok(Table::ThreeDsMaxFrequency {
        rows: native(
            json!([{"slots":[1],"conditions":[1,0,1,0],"speed":"Ddr3200"},{"slots":[2],"conditions":[1,0,1,0],"speed":"Ddr2933"},{"slots":[2],"conditions":[2,0,2,0],"speed":"Ddr2667"}]),
        )?,
    })
}
fn ethanolx_milan_three_ds_stretch_frequency() -> Result<Table> {
    Ok(Table::ThreeDsStretchFrequency {
        rows: native(
            json!([{"slots":[1],"conditions":[1,0,1,0],"speed":"Ddr3200"},{"slots":[2],"conditions":[1,0,1,0],"speed":"Ddr3200"},{"slots":[2],"conditions":[2,0,2,0],"speed":"Ddr3200"}]),
        )?,
    })
}
fn ethanolx_milan_console() -> Result<Table> {
    Ok(Table::Console {
        settings: native(
            json!({"abl_console_out_control":{"enable_console_logging":true,"enable_mem_flow_logging":true,"enable_mem_setreg_logging":true,"enable_mem_getreg_logging":false,"enable_mem_status_logging":true,"enable_mem_pmu_logging":true,"enable_mem_pmu_sram_read_logging":false,"enable_mem_pmu_sram_write_logging":false,"enable_mem_test_verbose_logging":false,"enable_mem_basic_output_logging":true,"abl_console_port":128},"abl_breakpoint_control":{"enable_breakpoint":false,"break_on_all_dies":false}}),
        )?,
    })
}
fn ethanolx_milan_milan_error_output() -> Result<Table> {
    Ok(Table::MilanErrorOutput {
        settings: native(
            json!({"enable_error_reporting":false,"enable_error_reporting_gpio":false,"enable_error_reporting_beep_codes":false,"enable_using_handshake":false,"input_port":132,"output_delay":15000,"output_port":128,"stop_on_first_fatal_error":false,"input_port_size":"32 Bit","output_port_size":"32 Bit","input_port_type":"FchHtIo","output_port_type":"FchHtIo","clear_acknowledgement":false,"error_reporting_gpio":{"pin":85,"iomux_control":1,"bank_control":192},"beep_code_table":[{"custom_error_type":"General","peak_map":1,"peak_attr":{"peak_count":8,"pulse_width":0,"repeat_count":0}},{"custom_error_type":"Memory","peak_map":2,"peak_attr":{"peak_count":20,"pulse_width":0,"repeat_count":0}},{"custom_error_type":"Df","peak_map":3,"peak_attr":{"peak_count":20,"pulse_width":0,"repeat_count":0}},{"custom_error_type":"Ccx","peak_map":4,"peak_attr":{"peak_count":20,"pulse_width":0,"repeat_count":0}},{"custom_error_type":"Gnb","peak_map":5,"peak_attr":{"peak_count":20,"pulse_width":0,"repeat_count":0}},{"custom_error_type":"Psp","peak_map":6,"peak_attr":{"peak_count":20,"pulse_width":0,"repeat_count":0}},{"custom_error_type":"Smu","peak_map":7,"peak_attr":{"peak_count":20,"pulse_width":0,"repeat_count":0}},{"custom_error_type":"Unknown","peak_map":2,"peak_attr":{"peak_count":4,"pulse_width":0,"repeat_count":0}}],"enable_heart_beat":false,"enable_power_good_gpio":false,"power_good_gpio":{"pin":0,"iomux_control":0,"bank_control":0}}),
        )?,
    })
}
fn ethanolx_milan_voltage_control() -> Result<Table> {
    Ok(Table::VoltageControl {
        settings: native(
            json!({"enabled":true,"input_port":132,"output_port":128,"input_port_size":"32 Bit","output_port_size":"32 Bit","input_port_type":"FchHtIo","output_port_type":"FchHtIo","clear_acknowledgement":false}),
        )?,
    })
}
fn ethanolx_milan_lrdimm_max_frequency() -> Result<Table> {
    Ok(Table::LrdimmMaxFrequency {
        rows: native(
            json!([{"slots":[1],"conditions":[1,0,1,0],"speed":"Ddr3200"},{"slots":[1],"conditions":[1,0,0,1],"speed":"Ddr3200"},{"slots":[2],"conditions":[1,0,1,0],"speed":"Ddr3200"},{"slots":[2],"conditions":[1,0,0,1],"speed":"Ddr3200"},{"slots":[2],"conditions":[2,0,2,0],"speed":"Ddr2933"},{"slots":[2],"conditions":[2,0,0,2],"speed":"Ddr2933"}]),
        )?,
    })
}
fn ethanolx_milan_platform_tuning_end() -> Result<Table> {
    Ok(Table::PlatformTuningEnd {})
}
fn ethanolx_milan_empty_ppr() -> Result<Table> {
    Ok(Table::EmptyPpr { slots: 64 })
}
fn ethanolx_milan_slink() -> Result<Table> {
    Ok(Table::Slink {
        settings: native(
            json!({"regions":[{"size":0,"alignment":0,"socket":0,"phys_nbio_map":0,"interleaving":"256 B"},{"size":0,"alignment":0,"socket":0,"phys_nbio_map":0,"interleaving":"256 B"},{"size":0,"alignment":0,"socket":1,"phys_nbio_map":0,"interleaving":"256 B"},{"size":0,"alignment":0,"socket":1,"phys_nbio_map":0,"interleaving":"256 B"}]}),
        )?,
    })
}
pub(super) fn ethanolx_milan() -> Result<Definition> {
    let mut tables = BTreeMap::from([
        ("spd-smbus".into(), ethanolx_milan_spd_smbus()?),
        (
            "platform-override".into(),
            ethanolx_milan_platform_override()?,
        ),
        ("board-rdimm-odt".into(), ethanolx_milan_board_rdimm_odt()?),
        (
            "rdimm-max-frequency".into(),
            ethanolx_milan_rdimm_max_frequency()?,
        ),
        (
            "rdimm-stretch-frequency".into(),
            ethanolx_milan_rdimm_stretch_frequency()?,
        ),
        (
            "three-ds-max-frequency".into(),
            ethanolx_milan_three_ds_max_frequency()?,
        ),
        (
            "three-ds-stretch-frequency".into(),
            ethanolx_milan_three_ds_stretch_frequency()?,
        ),
        ("console".into(), ethanolx_milan_console()?),
        (
            "milan-error-output".into(),
            ethanolx_milan_milan_error_output()?,
        ),
        ("voltage-control".into(), ethanolx_milan_voltage_control()?),
        (
            "lrdimm-max-frequency".into(),
            ethanolx_milan_lrdimm_max_frequency()?,
        ),
        (
            "platform-tuning-end".into(),
            ethanolx_milan_platform_tuning_end()?,
        ),
        ("empty-ppr".into(), ethanolx_milan_empty_ppr()?),
        ("slink".into(), ethanolx_milan_slink()?),
    ]);
    for (name, table) in electrical_defaults()? {
        if tables.insert(name, table).is_some() {
            return Err("duplicate SP3 default table".into());
        }
    }
    Ok(Definition {
        generation: Generation::Milan,
        blobs: vec![
            Blob {
                name: "board".into(),
                revision: 10005,
                donor_sha256: "632c2a856ce8b5a9ace109ebab5a58c25709b7f709a3b5878112d43a2a89a4ee"
                    .into(),
                bindings: vec![Binding {
                    slot: Slot::Backup,
                    instance: 0,
                }],
                groups: vec![Group::Memory, Group::Token],
                records: vec![
                    Reference {
                        name: "spd-smbus".into(),
                        instance: 0,
                        board_mask: None,
                        priority: None,
                    },
                    Reference {
                        name: "platform-override".into(),
                        instance: 0,
                        board_mask: None,
                        priority: None,
                    },
                    Reference {
                        name: "board-rdimm-odt".into(),
                        instance: 0,
                        board_mask: None,
                        priority: None,
                    },
                    Reference {
                        name: "rdimm-cad".into(),
                        instance: 0,
                        board_mask: None,
                        priority: None,
                    },
                    Reference {
                        name: "rdimm-data".into(),
                        instance: 0,
                        board_mask: None,
                        priority: None,
                    },
                    Reference {
                        name: "rdimm-max-frequency".into(),
                        instance: 0,
                        board_mask: None,
                        priority: None,
                    },
                    Reference {
                        name: "rdimm-stretch-frequency".into(),
                        instance: 0,
                        board_mask: None,
                        priority: None,
                    },
                    Reference {
                        name: "three-ds-max-frequency".into(),
                        instance: 0,
                        board_mask: None,
                        priority: None,
                    },
                    Reference {
                        name: "three-ds-stretch-frequency".into(),
                        instance: 0,
                        board_mask: None,
                        priority: None,
                    },
                    Reference {
                        name: "three-ds-data".into(),
                        instance: 0,
                        board_mask: None,
                        priority: None,
                    },
                    Reference {
                        name: "console".into(),
                        instance: 0,
                        board_mask: None,
                        priority: None,
                    },
                    Reference {
                        name: "milan-error-output".into(),
                        instance: 0,
                        board_mask: None,
                        priority: None,
                    },
                    Reference {
                        name: "voltage-control".into(),
                        instance: 0,
                        board_mask: None,
                        priority: None,
                    },
                    Reference {
                        name: "lrdimm-odt".into(),
                        instance: 0,
                        board_mask: None,
                        priority: None,
                    },
                    Reference {
                        name: "lrdimm-cad".into(),
                        instance: 0,
                        board_mask: None,
                        priority: None,
                    },
                    Reference {
                        name: "lrdimm-data".into(),
                        instance: 0,
                        board_mask: None,
                        priority: None,
                    },
                    Reference {
                        name: "lrdimm-max-frequency".into(),
                        instance: 0,
                        board_mask: None,
                        priority: None,
                    },
                    Reference {
                        name: "platform-tuning-end".into(),
                        instance: 0,
                        board_mask: None,
                        priority: None,
                    },
                ],
                tokens: vec![
                    Reference {
                        name: "board-bool".into(),
                        instance: 0,
                        board_mask: None,
                        priority: None,
                    },
                    Reference {
                        name: "board-byte".into(),
                        instance: 0,
                        board_mask: None,
                        priority: None,
                    },
                    Reference {
                        name: "board-word".into(),
                        instance: 0,
                        board_mask: None,
                        priority: None,
                    },
                    Reference {
                        name: "board-dword".into(),
                        instance: 0,
                        board_mask: None,
                        priority: None,
                    },
                ],
            },
            Blob {
                name: "cbs-backup".into(),
                revision: 8868,
                donor_sha256: "529e2c0f792dc782b226f6ec13efd5bbea1e02037ef016d1a73d60df6898ad42"
                    .into(),
                bindings: vec![Binding {
                    slot: Slot::Backup,
                    instance: 8,
                }],
                groups: vec![Group::Token],
                records: vec![],
                tokens: vec![
                    Reference {
                        name: "empty-bool".into(),
                        instance: 0,
                        board_mask: None,
                        priority: Some(vec![Priority::Low]),
                    },
                    Reference {
                        name: "cbs-backup-bool-medium".into(),
                        instance: 1,
                        board_mask: None,
                        priority: Some(vec![Priority::Medium]),
                    },
                    Reference {
                        name: "empty-byte".into(),
                        instance: 0,
                        board_mask: None,
                        priority: Some(vec![Priority::Low]),
                    },
                    Reference {
                        name: "cbs-backup-byte-medium".into(),
                        instance: 1,
                        board_mask: None,
                        priority: Some(vec![Priority::Medium]),
                    },
                    Reference {
                        name: "empty-word".into(),
                        instance: 0,
                        board_mask: None,
                        priority: Some(vec![Priority::Low]),
                    },
                    Reference {
                        name: "cbs-backup-word-medium".into(),
                        instance: 1,
                        board_mask: None,
                        priority: Some(vec![Priority::Medium]),
                    },
                    Reference {
                        name: "empty-dword".into(),
                        instance: 0,
                        board_mask: None,
                        priority: Some(vec![Priority::Low]),
                    },
                    Reference {
                        name: "cbs-backup-dword-medium".into(),
                        instance: 1,
                        board_mask: None,
                        priority: Some(vec![Priority::Medium]),
                    },
                ],
            },
            Blob {
                name: "post-package-repair".into(),
                revision: 7216,
                donor_sha256: "4e38f72f4c46e66e898ce89c41eb47c79260ff3eefcb4f32dbef8d752b5c89f3"
                    .into(),
                bindings: vec![
                    Binding {
                        slot: Slot::Backup,
                        instance: 9,
                    },
                    Binding {
                        slot: Slot::Active,
                        instance: 1,
                    },
                ],
                groups: vec![Group::Memory],
                records: vec![Reference {
                    name: "empty-ppr".into(),
                    instance: 0,
                    board_mask: None,
                    priority: Some(vec![Priority::EventLogging]),
                }],
                tokens: vec![],
            },
            Blob {
                name: "cbs-active".into(),
                revision: 9203,
                donor_sha256: "b870e11b0c9c8cb22bfb64d62866ed85f0b741f4b1729e4a76e90d34c5e68a59"
                    .into(),
                bindings: vec![Binding {
                    slot: Slot::Active,
                    instance: 0,
                }],
                groups: vec![Group::Df, Group::Memory, Group::Token],
                records: vec![Reference {
                    name: "slink".into(),
                    instance: 0,
                    board_mask: None,
                    priority: Some(vec![Priority::Low]),
                }],
                tokens: vec![
                    Reference {
                        name: "empty-bool".into(),
                        instance: 0,
                        board_mask: None,
                        priority: Some(vec![Priority::Low]),
                    },
                    Reference {
                        name: "cbs-backup-bool-medium".into(),
                        instance: 1,
                        board_mask: None,
                        priority: Some(vec![Priority::Medium]),
                    },
                    Reference {
                        name: "empty-byte".into(),
                        instance: 0,
                        board_mask: None,
                        priority: Some(vec![Priority::Low]),
                    },
                    Reference {
                        name: "cbs-backup-byte-medium".into(),
                        instance: 1,
                        board_mask: None,
                        priority: Some(vec![Priority::Medium]),
                    },
                    Reference {
                        name: "empty-word".into(),
                        instance: 0,
                        board_mask: None,
                        priority: Some(vec![Priority::Low]),
                    },
                    Reference {
                        name: "cbs-backup-word-medium".into(),
                        instance: 1,
                        board_mask: None,
                        priority: Some(vec![Priority::Medium]),
                    },
                    Reference {
                        name: "cbs-active-dword-medium".into(),
                        instance: 1,
                        board_mask: None,
                        priority: Some(vec![Priority::Medium]),
                    },
                    Reference {
                        name: "cbs-active-dword-low".into(),
                        instance: 0,
                        board_mask: None,
                        priority: Some(vec![Priority::Low]),
                    },
                ],
            },
        ],
        tables,
        token_sets: BTreeMap::from([
            (
                "board-bool".into(),
                TokenSet::Bool {
                    values: vec![
                        Token::Named(native(json!({"MemLrdimmCapable":true}))?),
                        Token::Named(native(json!({"MemModeUnganged":true}))?),
                        Token::Named(native(json!({"PspTpPort":true}))?),
                        Token::Named(native(json!({"MemDimmTypeDdr3Capable":false}))?),
                        Token::Named(native(json!({"DfPickerThrottleEnable":false}))?),
                        Token::Named(native(json!({"PspEventLogDisplay":true}))?),
                        Token::Named(native(json!({"GnbAdditionalFeatures":true}))?),
                        Token::Named(native(json!({"MemForcePowerDownThrottleEnable":false}))?),
                        Token::Named(native(json!({"PspPsbAutoFuse":true}))?),
                        Token::Named(native(json!({"MemDqsTrainingControl":true}))?),
                        Token::Named(native(json!({"MemEnableParity":true}))?),
                        Token::Named(native(json!({"MemUdimmCapable":true}))?),
                        Token::Named(native(json!({"MemEnableBankGroupSwap":true}))?),
                        Token::Named(native(json!({"MemChannelInterleaving":false}))?),
                        Token::Named(native(json!({"MemPstate":true}))?),
                        Token::Named(native(json!({"MemAmp":true}))?),
                        Token::Named(native(json!({"MemLimitMemoryToBelow1TiB":true}))?),
                        Token::Named(native(json!({"MemEnableBankSwizzle":false}))?),
                        Token::Named(native(json!({"VgaProgram":true}))?),
                        Token::Named(native(json!({"MemSpdReadOptimizationDdr4":true}))?),
                        Token::Named(native(json!({"DfGroupDPlatform":true}))?),
                        Token::Named(native(json!({"MemHoleRemapping":true}))?),
                        Token::Named(native(json!({"CcxPpinOptIn":false}))?),
                        Token::Named(native(json!({"MemOcVddioControl":false}))?),
                        Token::Named(native(json!({"MemEnableChipSelectInterleaving":false}))?),
                        Token::Named(native(json!({"ConfigureSecondPcieLink":false}))?),
                        Token::Named(native(json!({"MemMbistAggressorStaticLaneControl":false}))?),
                        Token::Named(native(json!({"MemSodimmCapable":true}))?),
                        Token::Named(native(json!({"MemIgnoreSpdChecksum":true}))?),
                        Token::Named(native(json!({"MemRdimmCapable":true}))?),
                        Token::Named(native(json!({"MemEccSyncFlood":false}))?),
                        Token::Named(native(
                            json!({"MemAutoRefreshsCountForThrottling":"Enabled"}),
                        )?),
                        Token::Named(native(json!({"MemNvdimmNDisable":false}))?),
                        Token::Named(native(json!({"GeneralCapsuleMode":true}))?),
                        Token::Named(native(json!({"MemDramDoubleRefreshRateMilan":false}))?),
                        Token::Named(native(
                            json!({"DfNps1With4ChannelInterleavedRdimm4ChannelNonInterleavedNvdimm":false}),
                        )?),
                        Token::Named(native(json!({"DisplayPmuTrainingResults":false}))?),
                        Token::Named(native(
                            json!({"GnbAdditionalFeatureL3PerformanceBias":true}),
                        )?),
                        Token::Named(native(json!({"MemSwCmdThrottleEnable":false}))?),
                        Token::Named(native(json!({"MemEnableBankGroupSwapAlt":true}))?),
                        Token::Named(native(json!({"MemDimmTypeLpddr3Capable":false}))?),
                        Token::Named(native(json!({"MemOnDieThermalSensor":true}))?),
                        Token::Named(native(json!({"MemAllClocks":true}))?),
                        Token::Named(native(json!({"MemEnablePowerDown":true}))?),
                        Token::Named(native(json!({"DxioVgaApiEnable":false}))?),
                        Token::Named(native(json!({"MemUncorrectedEccRetryDdr4":true}))?),
                        Token::Named(native(json!({"MemOdtsCmdThrottleEnable":true}))?),
                        Token::Named(native(json!({"MemClear":false}))?),
                        Token::Named(native(json!({"MemPostPackageRepairEnable":true}))?),
                        Token::Named(native(json!({"MemTsmeModeMilan":false}))?),
                        Token::Named(native(json!({"MemDdr4ForceDataMaskDisable":false}))?),
                        Token::Named(native(json!({"PspErrorDisplay":true}))?),
                        Token::Named(native(json!({"MemEccRedirection":false}))?),
                        Token::Named(native(json!({"MemMbistTgtStaticLaneControl":false}))?),
                        Token::Named(native(json!({"MemDdrRouteBalancedTee":false}))?),
                        Token::Named(native(json!({"MemQuadRankCapable":true}))?),
                        Token::Named(native(json!({"PspStopOnError":false}))?),
                        Token::Named(native(json!({"MemTempControlledRefreshEnable":false}))?),
                        Token::Named(native(json!({"PerformanceTracing":false}))?),
                        Token::Named(native(json!({"MemTempControlledExtendedRefresh":false}))?),
                        Token::Named(native(json!({"PcieResetControl":true}))?),
                        Token::Named(native(json!({"MemEnableEccFeature":true}))?),
                        Token::Named(native(json!({"BmcInitBeforeDram":false}))?),
                        Token::Named(native(json!({"MemRestoreControl":false}))?),
                    ],
                },
            ),
            (
                "board-byte".into(),
                TokenSet::Byte {
                    values: vec![
                        Token::Named(native(json!({"MemMbistAggressorOn":false}))?),
                        Token::Named(native(
                            json!({"MemOverrideDimmSpdMaxActivityCount":"Auto"}),
                        )?),
                        Token::Named(native(json!({"DfGmiEncrypt":"Auto"}))?),
                        Token::Named(native(json!({"MemUrgRefLimit":6}))?),
                        Token::Named(native(json!({"BmcEndLane":129}))?),
                        Token::Named(native(json!({"MemAutoRefreshFineGranMode":"Fixed1Times"}))?),
                        Token::Named(native(json!({"BmcFunction":2}))?),
                        Token::Named(native(json!({"WorkloadProfile":"Disabled"}))?),
                        Token::Named(native(json!({"MemMbistWorseCasGranularity":0}))?),
                        Token::Named(native(json!({"FchSmbusSpeed":{"Value":42}}))?),
                        Token::Named(native(json!({"DfSysStorageAtTopOfMem":"Auto"}))?),
                        Token::Named(native(json!({"DfMemInterleavingSize":"Auto"}))?),
                        Token::Named(native(json!({"MemNvdimmPowerSource":"DeviceManaged"}))?),
                        Token::Named(native(json!({"DfDramNumaPerSocket":"Auto"}))?),
                        Token::Named(native(json!({"GnbAdditionalFeatureDsm2":"Auto"}))?),
                        Token::Named(native(json!({"MemMbistReadDataEyeVoltageStep":1}))?),
                        Token::Named(native(json!({"DfRemapAt1TiB":"Auto"}))?),
                        Token::Named(native(json!({"MemDramAddressCommandParityRetryCount":1}))?),
                        Token::Named(native(json!({"Df4LinkMaxXgmiSpeed":"Auto"}))?),
                        Token::Named(native(json!({"MemMbistDataEyeSilentExecution":false}))?),
                        Token::Named(native(json!({"MemMbistAggressorStaticLaneVal":0}))?),
                        Token::Named(native(json!({"MemDramDoubleRefreshRate":0}))?),
                        Token::Named(native(json!({"MemDataPoison":"Enabled"}))?),
                        Token::Named(native(json!({"MemMbistTgtStaticLaneVal":0}))?),
                        Token::Named(native(json!({"MemMbistDataEyeType":"1D Timing"}))?),
                        Token::Named(native(json!({"Df3LinkMaxXgmiSpeed":"Auto"}))?),
                        Token::Named(native(json!({"MemHealPprType":"SoftRepair"}))?),
                        Token::Named(native(json!({"CcxSevAsidCount":"509"}))?),
                        Token::Named(native(json!({"MemMbistTestMode":"PhysicalInterface"}))?),
                        Token::Named(native(json!({"MemMbistAggressorStaticLaneSelEcc":0}))?),
                        Token::Named(native(json!({"MemMbistReadDataEyeTimingStep":1}))?),
                        Token::Named(native(json!({"MemHealTestSelect":"Normal"}))?),
                        Token::Named(native(json!({"MemRollWindowDepth":{"Memclks":255}}))?),
                        Token::Named(native(json!({"FchConsoleOutSuperIoType":"Auto"}))?),
                        Token::Named(native(json!({"MemHealMaxBankFails":3}))?),
                        Token::Named(native(json!({"DvArbiterMin":1}))?),
                        Token::Named(native(json!({"MemRcdParity":true}))?),
                        Token::Named(native(json!({"DfInvertDramMap":"Auto"}))?),
                        Token::Named(native(json!({"DfProbeFilter":"Auto"}))?),
                        Token::Named(native(json!({"OdtsCmdThrottleCycles":87}))?),
                        Token::Named(native(json!({"MemControllerWritingCrcMaxReplay":8}))?),
                        Token::Named(native(json!({"DfXgmiEncrypt":"Auto"}))?),
                        Token::Named(native(json!({"MemPsErrorHandling":0}))?),
                        Token::Named(native(json!({"DvArbiterMax":10}))?),
                        Token::Named(native(
                            json!({"MemPmuBistTestSelect":{"algorithm_1":true,"algorithm_2":true,"algorithm_3":true,"algorithm_4":true,"algorithm_5":true}}),
                        )?),
                        Token::Named(native(json!({"MemCpuVrefRange":0}))?),
                        Token::Named(native(json!({"DfSaveRestoreMemEncrypt":"Auto"}))?),
                        Token::Named(native(json!({"MemControllerWritingCrcMode":"Disabled"}))?),
                        Token::Named(native(json!({"DimmSensorResolution":1}))?),
                        Token::Named(native(json!({"BmcSocket":0}))?),
                        Token::Named(native(json!({"SecondPcieLinkSpeed":"Gen2"}))?),
                        Token::Named(native(json!({"PcieResetPinSelect":2}))?),
                        Token::Named(native(json!({"MemMbistDataEyeExecutionRepeatCount":1}))?),
                        Token::Named(native(json!({"DfBottomIo":176}))?),
                        Token::Named(native(json!({"FchConsoleOutSerialPortIoBase":"3f8"}))?),
                        Token::Named(native(
                            json!({"MemTccd5ReadCommandSpacingMode":"Disabled"}),
                        )?),
                        Token::Named(native(json!({"MemDataScramble":1}))?),
                        Token::Named(native(json!({"BmcLinkSpeed":"PcieGen1"}))?),
                        Token::Named(native(json!({"DfMemClear":"Auto"}))?),
                        Token::Named(native(json!({"FchConsoleOutBasicEnable":0}))?),
                        Token::Named(native(json!({"MemRcwWeakDriveDisable":"Enabled"}))?),
                        Token::Named(native(json!({"MemMbistTgtStaticLaneSelEcc":0}))?),
                        Token::Named(native(json!({"MemDramVrefRange":0}))?),
                        Token::Named(native(json!({"PspSevMode2":"Enabled"}))?),
                        Token::Named(native(json!({"DfXgmiTxEqMode":"Auto"}))?),
                        Token::Named(native(json!({"AblSerialBaudRate":"115200 Baud"}))?),
                        Token::Named(native(json!({"MemMbistPatternLength":3}))?),
                        Token::Named(native(json!({"BmcRcbCheckingMode":"Auto"}))?),
                        Token::Named(native(json!({"DfPstateModeSelect":"Auto"}))?),
                        Token::Named(native(
                            json!({"MemTrainingHdtControl":"AssertionMessages"}),
                        )?),
                        Token::Named(native(json!({"ReservedDramModuleDrtmMode":"Disabled"}))?),
                        Token::Named(native(json!({"DfXgmiConfig":"Auto"}))?),
                        Token::Named(native(json!({"MemMbistHaltOnError":1}))?),
                        Token::Named(native(json!({"BmcStartLane":129}))?),
                        Token::Named(native(json!({"MemSelfRefreshExitStaggering":"Disabled"}))?),
                        Token::Named(native(json!({"PmuTrainingMode":"1D,2D"}))?),
                        Token::Named(native(json!({"CbsMemUncorrectedEccRetryDdr4":true}))?),
                        Token::Named(native(json!({"MemControllerWritingCrcLimit":0}))?),
                        Token::Named(native(json!({"MemParityErrorMaxReplayDdr4":8}))?),
                        Token::Named(native(json!({"MemActionOnBistFailure":"DoNothing"}))?),
                        Token::Named(native(json!({"MemMbistWriteDataEyeVoltageStep":1}))?),
                        Token::Named(native(json!({"DfMemInterleaving":"Auto"}))?),
                        Token::Named(native(json!({"MemMbistPerBitSlaveDieReport":0}))?),
                        Token::Named(native(json!({"PspEnableDebugMode":"Disabled"}))?),
                        Token::Named(native(
                            json!({"MemRdimmTimingRcdF0Rc0FAdditionalLatency":"Auto"}),
                        )?),
                        Token::Named(native(json!({"BmcDevice":5}))?),
                        Token::Named(native(json!({"MemMbistWriteDataEyeTimingStep":1}))?),
                        Token::Named(native(json!({"FchPreferredIo":"Auto"}))?),
                        Token::Named(native(json!({"MemMbistAggressorsChannels":"Disabled"}))?),
                        Token::Named(native(json!({"SwCmdThrotCycles":0}))?),
                        Token::Named(native(json!({"FchConsoleOutMode":"Disabled"}))?),
                        Token::Named(native(json!({"MemMbistTest":"Disabled"}))?),
                        Token::Named(native(
                            json!({"SecondPcieLinkMaxPayload":"HardwareDefault"}),
                        )?),
                        Token::Named(native(json!({"MemSubUrgRefLowerBound":4}))?),
                        Token::Named(native(json!({"GnbSmuDfPstateFclkLimit":"Auto"}))?),
                        Token::Named(native(json!({"MemMbistPatternSelect":"Prbs"}))?),
                        Token::Named(native(json!({"GnbAdditionalFeatureDsmDetector2":"Auto"}))?),
                        Token::Named(native(json!({"MemHealBistEnable":"Disabled"}))?),
                        Token::Named(native(json!({"FchConsoleOutSerialPort":"Uart1Mmio"}))?),
                    ],
                },
            ),
            (
                "board-word".into(),
                TokenSet::Word {
                    values: vec![
                        Token::Named(native(json!({"Dimm3DsSensorCritical":80}))?),
                        Token::Named(native(json!({"LrdimmSensorCritical":80}))?),
                        Token::Named(native(json!({"ScrubL2Rate":0}))?),
                        Token::Named(native(json!({"Dimm3DsSensorUpper":66}))?),
                        Token::Named(native(json!({"EccSymbolSize":"x16"}))?),
                        Token::Named(native(json!({"DimmSensorCritical":95}))?),
                        Token::Named(native(json!({"DimmSensorConfig":1032}))?),
                        Token::Named(native(json!({"FchI2cSdaHoldOverrideMode":"IgnoreBoth"}))?),
                        Token::Named(native(json!({"FchI2cSdaTxHold":0}))?),
                        Token::Named(native(json!({"ScrubIcacheRate":0}))?),
                        Token::Named(native(json!({"ScrubDramRate":0}))?),
                        Token::Named(native(json!({"FchI2cSdaRxHold":0}))?),
                        Token::Named(native(json!({"ScrubDcacheRate":0}))?),
                        Token::Named(native(json!({"DimmSensorUpper":80}))?),
                        Token::Named(native(json!({"ScrubL3Rate":0}))?),
                        Token::Named(native(json!({"DimmSensorLower":10}))?),
                        Token::Named(native(json!({"FchGppClkMap":"Auto"}))?),
                        Token::Named(native(json!({"LrdimmSensorUpper":66}))?),
                        Token::Named(native(json!({"PspSyshubWatchdogTimerInterval":2600}))?),
                    ],
                },
            ),
            (
                "board-dword".into(),
                TokenSet::Dword {
                    values: vec![
                        Token::Named(native(json!({"DxioPhyParamDc":"Skip"}))?),
                        Token::Named(native(json!({"MemPowerDownMode":0}))?),
                        Token::Named(native(json!({"MemBusFrequencyLimit":"Ddr3200"}))?),
                        Token::Named(native(json!({"DfPciMmioSize":268435456}))?),
                        Token::Named(native(json!({"FchRom3BaseHigh":0}))?),
                        Token::Named(native(json!({"PcieResetGpioPin":4294967295u64}))?),
                        Token::Named(native(json!({"MemMbistAggressorStaticLaneSelLo":0}))?),
                        Token::Named(native(json!({"DxioPhyParamIqofc":"Skip"}))?),
                        Token::Named(native(json!({"MemMbistTgtStaticLaneSelLo":0}))?),
                        Token::Named(native(json!({"GnbAdditionalFeatureOnRamp":70}))?),
                        Token::Named(native(json!({"DfCakeCrcThresholdBounds":{"Value":100}}))?),
                        Token::Named(native(json!({"CcxMinSevAsid":1}))?),
                        Token::Named(native(json!({"MemMbistTgtStaticLaneSelHi":0}))?),
                        Token::Named(native(json!({"DxioPhyParamPole":"Skip"}))?),
                        Token::Named(native(json!({"MemSelfHealBistTimeout":10000}))?),
                        Token::Named(native(json!({"MemClockValue":"Ddr2400"}))?),
                        Token::Named(native(json!({"CpuFetchFromSpiApBase":4293918720u64}))?),
                        Token::Named(native(json!({"PspMeasureConfig":0}))?),
                        Token::Named(native(json!({"DxioPhyParamVga":"Skip"}))?),
                        Token::Named(native(json!({"GnbAdditionalFeatureOffRamp":40}))?),
                        Token::Named(native(json!({"MemMbistAggressorStaticLaneSelHi":0}))?),
                        Token::Named(native(json!({"MemUserTimingMode":"Auto"}))?),
                    ],
                },
            ),
            ("empty-bool".into(), TokenSet::Bool { values: vec![] }),
            (
                "cbs-backup-bool-medium".into(),
                TokenSet::Bool {
                    values: vec![
                        Token::Numeric(NumericToken {
                            id: 218818671,
                            value: 0,
                        }),
                        Token::Named(native(json!({"MemMbistAggressorStaticLaneControl":false}))?),
                        Token::Named(native(json!({"MemPostPackageRepairEnable":false}))?),
                        Token::Named(native(json!({"MemMbistTgtStaticLaneControl":false}))?),
                    ],
                },
            ),
            ("empty-byte".into(), TokenSet::Byte { values: vec![] }),
            (
                "cbs-backup-byte-medium".into(),
                TokenSet::Byte {
                    values: vec![
                        Token::Named(native(json!({"CbsMemAddrCmdParityErrorMaxReplayDdr4":8}))?),
                        Token::Numeric(NumericToken {
                            id: 163953868,
                            value: 8,
                        }),
                        Token::Named(native(json!({"MemUrgRefLimit":6}))?),
                        Token::Named(native(json!({"MemMbistWorseCasGranularity":0}))?),
                        Token::Named(native(json!({"MemMbistReadDataEyeVoltageStep":1}))?),
                        Token::Named(native(json!({"MemMbistAggressorStaticLaneVal":0}))?),
                        Token::Named(native(json!({"MemDramDoubleRefreshRate":0}))?),
                        Token::Numeric(NumericToken {
                            id: 1286431756,
                            value: 0,
                        }),
                        Token::Named(native(json!({"MemMbistTgtStaticLaneVal":0}))?),
                        Token::Named(native(json!({"MemHealPprType":"SoftRepair"}))?),
                        Token::Named(native(json!({"MemMbistAggressorStaticLaneSelEcc":0}))?),
                        Token::Named(native(json!({"MemMbistReadDataEyeTimingStep":1}))?),
                        Token::Named(native(json!({"MemHealTestSelect":"Normal"}))?),
                        Token::Numeric(NumericToken {
                            id: 1698103689,
                            value: 255,
                        }),
                        Token::Numeric(NumericToken {
                            id: 1799957978,
                            value: 16,
                        }),
                        Token::Named(native(
                            json!({"MemPmuBistTestSelect":{"algorithm_1":true,"algorithm_2":true,"algorithm_3":true,"algorithm_4":true,"algorithm_5":true}}),
                        )?),
                        Token::Named(native(json!({"CbsMemWriteCrcErrorMaxReplayDdr4":8}))?),
                        Token::Named(native(
                            json!({"MemTccd5ReadCommandSpacingMode":"Disabled"}),
                        )?),
                        Token::Named(native(json!({"MemMbistTgtStaticLaneSelEcc":0}))?),
                        Token::Named(native(json!({"PspSevMode2":"Enabled"}))?),
                        Token::Named(native(json!({"MemMbistPatternLength":3}))?),
                        Token::Numeric(NumericToken {
                            id: 3022303216,
                            value: 0,
                        }),
                        Token::Named(native(json!({"MemSelfRefreshExitStaggering":"Disabled"}))?),
                        Token::Named(native(json!({"MemMbistWriteDataEyeVoltageStep":1}))?),
                        Token::Named(native(json!({"MemMbistWriteDataEyeTimingStep":1}))?),
                        Token::Named(native(
                            json!({"MemMbistAggressorsChannels":"1 aggressor/(2 ch)"}),
                        )?),
                        Token::Named(native(json!({"MemSubUrgRefLowerBound":4}))?),
                        Token::Named(native(json!({"MemMbistPatternSelect":"Prbs"}))?),
                        Token::Named(native(json!({"MemHealBistEnable":"Disabled"}))?),
                        Token::Numeric(NumericToken {
                            id: 4285882405,
                            value: 1,
                        }),
                    ],
                },
            ),
            ("empty-word".into(), TokenSet::Word { values: vec![] }),
            (
                "cbs-backup-word-medium".into(),
                TokenSet::Word {
                    values: vec![Token::Named(native(
                        json!({"CbsMemPowerDownDelay":{"Value":3000}}),
                    )?)],
                },
            ),
            ("empty-dword".into(), TokenSet::Dword { values: vec![] }),
            (
                "cbs-backup-dword-medium".into(),
                TokenSet::Dword {
                    values: vec![
                        Token::Numeric(NumericToken {
                            id: 420813139,
                            value: 0,
                        }),
                        Token::Named(native(json!({"MemMbistAggressorStaticLaneSelLo":0}))?),
                        Token::Named(native(json!({"MemMbistTgtStaticLaneSelLo":0}))?),
                        Token::Numeric(NumericToken {
                            id: 2411588270,
                            value: 0,
                        }),
                        Token::Named(native(json!({"MemMbistTgtStaticLaneSelHi":0}))?),
                        Token::Numeric(NumericToken {
                            id: 3006347150,
                            value: 1447249234,
                        }),
                        Token::Named(native(json!({"MemMbistAggressorStaticLaneSelHi":0}))?),
                    ],
                },
            ),
            (
                "cbs-active-dword-medium".into(),
                TokenSet::Dword {
                    values: vec![
                        Token::Numeric(NumericToken {
                            id: 420813139,
                            value: 0,
                        }),
                        Token::Named(native(json!({"MemMbistAggressorStaticLaneSelLo":0}))?),
                        Token::Named(native(json!({"MemMbistTgtStaticLaneSelLo":0}))?),
                        Token::Numeric(NumericToken {
                            id: 2411588270,
                            value: 0,
                        }),
                        Token::Named(native(json!({"MemMbistTgtStaticLaneSelHi":0}))?),
                        Token::Numeric(NumericToken {
                            id: 3006347150,
                            value: 284158084,
                        }),
                        Token::Named(native(json!({"MemMbistAggressorStaticLaneSelHi":0}))?),
                    ],
                },
            ),
            (
                "cbs-active-dword-low".into(),
                TokenSet::Dword {
                    values: vec![
                        Token::Named(native(json!({"MemClockValue":"Ddr2400"}))?),
                        Token::Named(native(json!({"MemUserTimingMode":"Auto"}))?),
                    ],
                },
            ),
        ]),
        spd_images: BTreeMap::from([]),
    })
}
