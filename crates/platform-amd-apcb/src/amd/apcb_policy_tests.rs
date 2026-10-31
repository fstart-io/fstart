// SPDX-License-Identifier: MIT OR Apache-2.0
use super::*;
use crate::test_data;
const EC1: &str = include_str!("../../../../boards/gigabyte/mj11/apcb/ec1.toml");

#[test]
fn semantic_policies_preserve_all_board_data_without_registry_dispatch() {
    for (policy, abi, compat, variant) in [
        (
            include_str!("../../../../boards/tyan/s8030/apcb/rome.toml"),
            Abi::Rome,
            test_data::ROME,
            "",
        ),
        (
            include_str!("../../../../boards/tyan/s8030/apcb/milan.toml"),
            Abi::Milan,
            test_data::MILAN,
            "",
        ),
        (
            include_str!("../../../../boards/amd/ethanolx/apcb/milan.toml"),
            Abi::Milan,
            test_data::ETHANOLX,
            "",
        ),
        (
            include_str!("../../../../boards/gigabyte/mj11/apcb/ec0.toml"),
            Abi::Zeppelin,
            test_data::MJ11,
            "ec0",
        ),
        (EC1, Abi::Zeppelin, test_data::MJ11, "ec1"),
        (
            include_str!("../../../../boards/google/zork/apcb/vilboz.toml"),
            Abi::Picasso,
            test_data::VILBOZ,
            "",
        ),
    ] {
        let data = BoardData::parse(compat, variant).unwrap();
        assert_eq!(
            compile(policy, abi, &data).unwrap(),
            abi.definition(&data).unwrap().compile().unwrap()
        );
    }
}
#[test]
fn new_board_identity_and_values_need_no_compiler_change() {
    let mut data = BoardData::parse(test_data::MJ11, "ec1").unwrap();
    let before = compile(EC1, Abi::Zeppelin, &data).unwrap();
    data.provenance = "A previously unseen board and firmware release".into();
    assert_eq!(compile(EC1, Abi::Zeppelin, &data).unwrap(), before);
    let stock = compile("schema=1", Abi::Zeppelin, &data).unwrap();
    let previous = data
        .parameters
        .get_mut("memory-override")
        .unwrap()
        .insert("0x0701".into(), 225)
        .unwrap();
    assert_ne!(
        compile("schema=1", Abi::Zeppelin, &data).unwrap()[0].bytes,
        stock[0].bytes
    );
    data.parameters
        .get_mut("memory-override")
        .unwrap()
        .insert("0x0701".into(), previous);
    let edited = EC1.replace("ecc = true", "ecc = false");
    let after = compile(&edited, Abi::Zeppelin, &data).unwrap();
    assert_ne!(before[0].bytes, after[0].bytes);
    assert_eq!(before[0].bytes.len(), after[0].bytes.len());
    assert_eq!(before[0].donor_sha256, after[0].donor_sha256);
    assert_eq!(
        after[0].bytes.iter().fold(0u8, |s, b| s.wrapping_add(*b)),
        0
    );
    let a = lower(EC1, Abi::Zeppelin, &data).unwrap();
    let b = lower(&edited, Abi::Zeppelin, &data).unwrap();
    for (a, b) in a.blobs[0].apcb["entries"]
        .as_array()
        .unwrap()
        .iter()
        .zip(b.blobs[0].apcb["entries"].as_array().unwrap())
    {
        if let (Some(a), Some(b)) = (a["parameters"].as_array(), b["parameters"].as_array()) {
            for (a, b) in a.iter().zip(b) {
                assert_eq!(a["value_size"], b["value_size"]);
                assert_eq!(a["time_point"], b["time_point"]);
            }
        }
    }
}
#[test]
fn policy_and_data_cannot_specify_abi_storage_or_instructions() {
    for input in [
        "schema=1\nwidth='u8'",
        "schema=1\ngeneration='zeppelin'",
        "schema=1\nblobs=[]",
        "schema=1\ntemplate='x'",
        "schema=1\n[memory]\nvalue-size=1",
        "schema=1\n[memory]\necc=1",
    ] {
        assert!(Configuration::parse(input).is_err());
    }
    for input in ["records=[]", "width='u8'", "profile='mainboard-version'"] {
        assert!(BoardData::parse(&format!("schema=1\nprovenance='x'\n{input}\n[images.board]\nrevision=1\ndonor-sha256='x'"),"").is_err());
    }
    for name in ["s8030-rome-v404", "mj11-ec1-f09", "vilboz"] {
        assert!(name.parse::<Abi>().is_err());
    }
    let schema = serde_json::to_string(&schema()).unwrap();
    for key in [
        "board-mask",
        "priority",
        "value_size",
        "ParameterTokenConfig",
        "header_size",
        "Generation",
        "template",
    ] {
        assert!(!schema.contains(key), "{key}");
    }
}
#[test]
fn unsupported_meanings_and_malformed_populations_are_not_guessed() {
    let data = BoardData::parse(test_data::VILBOZ, "").unwrap();
    assert!(
        compile("schema=1\n[memory]\necc=true", Abi::Picasso, &data)
            .unwrap_err()
            .contains("not yet catalogued")
    );
    let data = BoardData::parse(test_data::MJ11, "ec1").unwrap();
    assert!(compile("schema=1\n[[memory.speed-limits]]\nmodule='rdimm'\nslots-per-channel=[1]\nsingle-rank-dimms=2\nmaximum-mt-s=2400",Abi::Zeppelin,&data).unwrap_err().contains("population"));
}
