// SPDX-License-Identifier: MIT OR Apache-2.0
use super::*;
use crate::{BoardData, test_data};
fn zeppelin() -> Definition {
    Abi::Zeppelin
        .definition(&BoardData::parse(test_data::MJ11, "ec1").unwrap())
        .unwrap()
}
fn picasso() -> Definition {
    Abi::Picasso
        .definition(&BoardData::parse(test_data::VILBOZ, "").unwrap())
        .unwrap()
}

#[test]
fn family_models_cover_every_legacy_ram_id_and_mj11_variant() {
    let hashes = [
        "142412100b3d54382a3dbee2e4eed65eccb6aba55ff9780d357298e824d6fba5",
        "142412100b3d54382a3dbee2e4eed65eccb6aba55ff9780d357298e824d6fba5",
        "98f61312c86f37c167e8e19eec96a791872d86daa986c326ec9ba75764992ae1",
        "98f61312c86f37c167e8e19eec96a791872d86daa986c326ec9ba75764992ae1",
        "98f61312c86f37c167e8e19eec96a791872d86daa986c326ec9ba75764992ae1",
        "90c47cd6f665e004988ece4dd620586ff66948719b9f896913080f59e1131663",
        "90c47cd6f665e004988ece4dd620586ff66948719b9f896913080f59e1131663",
        "5ccda7dcba6668369b6b8e0b99dab7990682a62380f3d606b03f5e150119c309",
        "a70492cf9c71464e13ffc9332f4e08c078bdaaca6b6756a415c3765659947d3b",
        "a70492cf9c71464e13ffc9332f4e08c078bdaaca6b6756a415c3765659947d3b",
        "98f61312c86f37c167e8e19eec96a791872d86daa986c326ec9ba75764992ae1",
        "98f61312c86f37c167e8e19eec96a791872d86daa986c326ec9ba75764992ae1",
        "a70492cf9c71464e13ffc9332f4e08c078bdaaca6b6756a415c3765659947d3b",
    ];
    let built = picasso().compile().unwrap();
    assert_eq!(built.len(), 26);
    for replacement in built {
        assert_eq!(replacement.bytes.len(), 4672);
        assert_eq!(
            apcb_source::sha256(&replacement.bytes),
            hashes[usize::from(replacement.selector.instance)]
        );
        let decoded = apcb_source::decode(&replacement.bytes).unwrap();
        assert_eq!(
            apcb_source::build(&decoded, &Default::default()).unwrap(),
            replacement.bytes
        );
    }
    for (variant, hash) in [
        (
            "ec0",
            "aeb727e324f1d51c586e742ceedaae8d41e4ad64c410207d9e8d04f3f12492d0",
        ),
        (
            "ec1",
            "fa1f884641edd40ba9bc8a6f327b03d71fd0d12edb80161cd284cffa28a22bc9",
        ),
    ] {
        let data = BoardData::parse(test_data::MJ11, variant).unwrap();
        let built = Abi::Zeppelin.definition(&data).unwrap().compile().unwrap();
        assert_eq!(built.len(), 2);
        assert_eq!(built[0].bytes, built[1].bytes);
        for replacement in built {
            assert_eq!(replacement.bytes.len(), 6416);
            assert_eq!(apcb_source::sha256(&replacement.bytes), hash);
            let decoded = apcb_source::decode(&replacement.bytes).unwrap();
            assert_eq!(
                apcb_source::build(&decoded, &Default::default()).unwrap(),
                replacement.bytes
            );
        }
    }
}

#[test]
fn malformed_models_and_legacy_parameters_are_rejected() {
    let mut model = zeppelin();
    model.blobs.clear();
    assert!(model.lower().is_err());
    let mut model = zeppelin();
    model.blobs[0].records[0].name = "missing".into();
    assert!(model.lower().is_err());
    let mut model = zeppelin();
    model.blobs[0].groups.extend([Group::Psp, Group::Psp]);
    assert!(model.lower().is_err());
    let model = zeppelin();
    let mut settings = model
        .tables
        .into_values()
        .find_map(|table| match table {
            Table::Parameters { values, .. } if !values.is_empty() => Some(values),
            _ => None,
        })
        .unwrap();
    let mut setting = settings.remove(0);
    setting.width = legacy::Width::U8;
    setting.value = 256;
    assert!(
        legacy::parameters(
            Group::Memory,
            legacy::ParameterLayer::Default,
            std::slice::from_ref(&setting)
        )
        .unwrap_err()
        .contains("width")
    );
    setting.width = legacy::Width::U64;
    setting.value = u64::MAX;
    assert!(
        legacy::parameters(
            Group::Memory,
            legacy::ParameterLayer::Default,
            std::slice::from_ref(&setting)
        )
        .is_ok()
    );
    setting.token = amd_apcb::ParameterTokenConfig::Limit;
    assert!(
        legacy::parameters(Group::Memory, legacy::ParameterLayer::Default, &[setting])
            .unwrap_err()
            .contains("termination")
    );
}

#[test]
fn family_layouts_and_spd_references_are_checked() {
    let model = zeppelin();
    let table = model.tables.get("rdimm-data").unwrap();
    assert!(
        table
            .lower(Generation::Rome, &BTreeMap::new())
            .unwrap_err()
            .contains("incompatible")
    );
    let mut model = picasso();
    let table = model
        .tables
        .values_mut()
        .find(|t| matches!(t, Table::SpdInfo { .. }))
        .unwrap();
    let Table::SpdInfo { slots } = table else {
        unreachable!()
    };
    slots[0].image = Some("missing".into());
    assert!(
        table
            .lower(Generation::Picasso, &BTreeMap::new())
            .unwrap_err()
            .contains("unknown SPD image")
    );
}
