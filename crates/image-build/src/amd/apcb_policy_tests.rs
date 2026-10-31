// SPDX-License-Identifier: MIT OR Apache-2.0
use super::*;
const EC1: &str = include_str!("../../../../boards/gigabyte/mj11/apcb/ec1.toml");

#[test]
fn semantic_policies_preserve_every_existing_payload_and_binding() {
    for (policy, profile) in [
        (
            include_str!("../../../../boards/tyan/s8030/apcb/rome.toml"),
            Profile::S8030RomeV404,
        ),
        (
            include_str!("../../../../boards/tyan/s8030/apcb/milan.toml"),
            Profile::S8030MilanV404,
        ),
        (
            include_str!("../../../../boards/amd/ethanolx/apcb/milan.toml"),
            Profile::EthanolxMilanRxm100cb,
        ),
        (
            include_str!("../../../../boards/gigabyte/mj11/apcb/ec0.toml"),
            Profile::Mj11Ec0F02,
        ),
        (EC1, Profile::Mj11Ec1F09),
        (
            include_str!("../../../../boards/google/zork/apcb/vilboz.toml"),
            Profile::Vilboz,
        ),
    ] {
        assert_eq!(
            compile(policy, profile).unwrap(),
            profile.definition().unwrap().compile().unwrap()
        );
    }
}

#[test]
fn changing_semantics_keeps_storage_and_donor_guards_internal() {
    let before = compile(EC1, Profile::Mj11Ec1F09).unwrap();
    let edited = EC1.replace("ecc = true", "ecc = false");
    assert_ne!(edited, EC1);
    let after = compile(&edited, Profile::Mj11Ec1F09).unwrap();
    assert_ne!(before[0].bytes, after[0].bytes);
    assert_eq!(before[0].bytes.len(), after[0].bytes.len());
    assert_eq!(before[0].donor_sha256, after[0].donor_sha256);
    assert_eq!(
        after[0].bytes.iter().fold(0u8, |s, b| s.wrapping_add(*b)),
        0
    );
    let a = lower(EC1, Profile::Mj11Ec1F09).unwrap();
    let b = lower(&edited, Profile::Mj11Ec1F09).unwrap();
    for (before, after) in a.blobs[0].apcb["entries"]
        .as_array()
        .unwrap()
        .iter()
        .zip(b.blobs[0].apcb["entries"].as_array().unwrap())
    {
        if let (Some(a), Some(b)) = (
            before["parameters"].as_array(),
            after["parameters"].as_array(),
        ) {
            for (a, b) in a.iter().zip(b) {
                assert_eq!(a["value_size"], b["value_size"]);
                assert_eq!(a["time_point"], b["time_point"]);
            }
        }
    }
}

#[test]
fn authoring_schema_has_no_firmware_layout_or_storage_knobs() {
    for input in [
        "schema=1\nwidth='u8'",
        "schema=1\ngeneration='zeppelin'",
        "schema=1\nblobs=[]",
        "schema=1\n[memory]\ntoken='MemEnableEccFeature'",
        "schema=1\n[memory]\nvalue-size=1",
        "schema=1\n[memory]\necc=1",
        "schema=1\n[memory]\nrecords=[]",
        "schema=1\ntemplate='layout.toml'",
    ] {
        assert!(Configuration::parse(input).is_err(), "{input}");
    }
    let schema = serde_json::to_string(&schema()).unwrap();
    for leaked in [
        "board-mask",
        "priority",
        "value_size",
        "ParameterTokenConfig",
        "LegacyDdr4",
        "header_size",
        "Generation",
        "template",
    ] {
        assert!(!schema.contains(leaked), "{leaked}");
    }
}

#[test]
fn unsupported_or_inconsistent_semantics_are_not_silently_ignored() {
    assert!(
        compile("schema=1\n[memory]\necc=true", Profile::Vilboz)
            .unwrap_err()
            .contains("not yet catalogued")
    );
    assert!(compile("schema=1\n[[memory.speed-limits]]\nmodule='rdimm'\nslots-per-channel=[1]\nsingle-rank-dimms=2\nmaximum-mt-s=2400",Profile::Mj11Ec1F09).unwrap_err().contains("population"));
    assert!(
        compile("schema=1\n[[ram]]\nid=0\nspd='absent'", Profile::Mj11Ec1F09)
            .unwrap_err()
            .contains("unknown SPD")
    );
    assert!("arbitrary-template.toml".parse::<Profile>().is_err());
}
