// SPDX-License-Identifier: MIT OR Apache-2.0
use super::*;
const ROME: &str = include_str!("../../../../boards/tyan/s8030/apcb/templates/rome.toml");
const TABLES: &str = include_str!("../../../platform-sp3/src/host/apcb/ddr4.toml");

#[test]
fn rome_toml_is_byte_exact_without_json_templates() {
    let built = compile(ROME, TABLES).unwrap();
    assert_eq!(built.len(), 5);
    let hashes: BTreeSet<_> = built
        .iter()
        .map(|r| apcb_source::sha256(&r.bytes))
        .collect();
    assert_eq!(
        hashes,
        BTreeSet::from([
            "6f56aa5889f5b6eaa1be74864af3f962ca4c48ac599f8556caf03dcf8aea42ed".to_owned(),
            "27061c5113b34e7f8875e18303c8f2c797b2afa239e92d1effcbbf916b249afc".to_owned(),
            "20640126271299dc0083d9855eef4909e3743ad2c6474fdd6052e08c6fbcbe69".to_owned(),
        ])
    );
}

#[test]
fn all_v3_toml_sources_preserve_all_eleven_payloads() {
    let mut count = 0;
    for source in [
        ROME,
        include_str!("../../../../boards/tyan/s8030/apcb/templates/milan.toml"),
        include_str!("../../../../boards/amd/ethanolx/apcb/templates/milan.toml"),
    ] {
        count += Configuration::parse(source).unwrap().blobs.len();
        let actual = compile(source, TABLES).unwrap();
        assert_eq!(actual.len(), 5);
        for r in actual {
            assert_eq!(
                apcb_source::sha256(&r.bytes),
                r.donor_sha256,
                "{:?}",
                r.selector
            );
        }
    }
    assert_eq!(count, 11);
}

#[test]
fn legacy_layouts_parameters_and_spd_assets_are_checked() {
    let spds = BTreeMap::new();
    for (text, generation) in [
        ("kind='legacy-rdimm-data'\nrows=[]", Generation::Rome),
        ("kind='legacy-lrdimm-data'\nrows=[]", Generation::Picasso),
        ("kind='sodimm-data'\nrows=[]", Generation::Zeppelin),
        ("kind='rdimm-data'\nrows=[]", Generation::Zeppelin),
    ] {
        let table: Table = toml::from_str(text).unwrap();
        assert!(
            table
                .lower(generation, &spds)
                .unwrap_err()
                .contains("incompatible")
        );
    }
    let mj11 = Configuration::parse(include_str!(
        "../../../../boards/gigabyte/mj11/apcb/templates/ec1.toml"
    ))
    .unwrap();
    let mut settings = mj11
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
            ParameterLayer::Default,
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
            ParameterLayer::Default,
            std::slice::from_ref(&setting)
        )
        .is_ok()
    );
    setting.token = amd_apcb::ParameterTokenConfig::Limit;
    assert!(
        legacy::parameters(Group::Memory, ParameterLayer::Default, &[setting])
            .unwrap_err()
            .contains("termination")
    );
    let vilboz = Configuration::parse(include_str!(
        "../../../../boards/google/zork/apcb/templates/vilboz.toml"
    ))
    .unwrap();
    let mut table = vilboz
        .tables
        .into_values()
        .find(|t| matches!(t, Table::SpdInfo { .. }))
        .unwrap();
    let Table::SpdInfo { slots } = &mut table else {
        unreachable!()
    };
    slots[0].image = Some("absent".into());
    assert!(
        table
            .lower(Generation::Picasso, &spds)
            .unwrap_err()
            .contains("unknown SPD image")
    );
    assert!(
        table
            .lower(
                Generation::Picasso,
                &BTreeMap::from([("absent".into(), "aa".into())])
            )
            .is_err()
    );
}

#[test]
fn legacy_sources_cover_every_ram_id_and_both_mj11_variants() {
    let vilboz = include_str!("../../../../boards/google/zork/apcb/templates/vilboz.toml");
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
    let empty = "schema=1\n[tables]\n";
    let records = compile(vilboz, empty).unwrap();
    assert_eq!(records.len(), 26);
    for r in records {
        assert_eq!(
            apcb_source::sha256(&r.bytes),
            hashes[usize::from(r.selector.instance)]
        );
        assert_eq!(r.bytes.len(), 4672);
        apcb_source::decode(&r.bytes).unwrap();
    }
    for (source, hash) in [
        (
            include_str!("../../../../boards/gigabyte/mj11/apcb/templates/ec0.toml"),
            "aeb727e324f1d51c586e742ceedaae8d41e4ad64c410207d9e8d04f3f12492d0",
        ),
        (
            include_str!("../../../../boards/gigabyte/mj11/apcb/templates/ec1.toml"),
            "fa1f884641edd40ba9bc8a6f327b03d71fd0d12edb80161cd284cffa28a22bc9",
        ),
    ] {
        let built = compile(source, empty).unwrap();
        assert_eq!(built.len(), 2);
        assert_eq!(built[0].bytes, built[1].bytes);
        assert_eq!(
            built[0].selector.kind,
            fstart_amdfw_format::directory::bios::APCB
        );
        assert_eq!(
            built[1].selector.kind,
            fstart_amdfw_format::directory::bios::APCB_BACKUP
        );
        assert_eq!(built[0].bytes.len(), 6416);
        assert_eq!(apcb_source::sha256(&built[0].bytes), hash);
        apcb_source::decode(&built[0].bytes).unwrap();
    }
}

#[test]
fn invalid_authoring_input_is_rejected() {
    for empty in [false, true] {
        let mut config = Configuration::parse(ROME).unwrap();
        if empty {
            config.blobs.clear();
        } else {
            config.schema = 2;
        }
        assert!(config.compile(toml::from_str(TABLES).unwrap()).is_err());
    }
    let bad_sources = [
        format!("typo = 1\n{ROME}"),
        ROME.replacen("name = \"board\"", "name = \"board\"\nrevission = 1", 1),
        ROME.replacen("generation = \"rome\"", "generation = \"genoa\"", 1),
        ROME.replacen("schema = 1", "schema = 2", 1),
        ROME.replacen(
            "kind = \"platform-tuning-end\"",
            "kind = \"platform-tuning-end\"\ntypos = true",
            1,
        ),
        ROME.replacen(
            "[tables.console.settings]",
            "[tables.console.settings]\ntypos = true",
            1,
        ),
        ROME.replacen(
            "{ MemRestoreControl = false }",
            "{ MemRestoreContorl = false }",
            1,
        ),
        ROME.replacen(
            "{ MemRestoreControl = false }",
            "{ MemRestoreControl = \"false\" }",
            1,
        ),
        ROME.replacen("{ use = \"spd-smbus\" }", "{ use = \"not-a-table\" }", 1),
        ROME.replacen("use = \"board-bool\"", "use = \"not-a-token-set\"", 1),
        format!("{ROME}\n[tables.rdimm-odt]\nkind = \"rdimm-odt\"\nrows = []\n"),
        ROME.replacen(
            "priority = [\"hard-force\", \"normal\"]",
            "priority = []",
            1,
        ),
        ROME.replacen(
            "priority = [\"hard-force\", \"normal\"]",
            "priority = [\"normal\", \"normal\"]",
            1,
        ),
        ROME.replacen(
            "id = 0x249e08d5, value = 3",
            "id = 0x249e08d5, value = 256",
            1,
        ),
        ROME.replacen(
            "{ MemRestoreControl = false }",
            "{ MemRestoreControl = false }, { MemRestoreControl = true }",
            1,
        ),
    ];
    for (i, source) in bad_sources.iter().enumerate() {
        assert_ne!(source, ROME, "invalid test mutation {i}");
        assert!(compile(source, TABLES).is_err(), "accepted bad source {i}");
    }
    for tables in [
        TABLES.replacen("schema = 1", "schema = 2", 1),
        TABLES.replacen(
            "slots = [1], rates = [1600]",
            "slots = [5], rates = [1600]",
            1,
        ),
        TABLES.replacen("rates = [1600]", "rates = [1234]", 1),
        TABLES.replacen("rates = [1600]", "rates = []", 1),
        TABLES.replacen("rates = [1600]", "rates = [1600, 1600]", 1),
        TABLES.replacen("addr-cmd = 0x393939", "addr-cmd = 0x393939, typo = 0", 1),
        TABLES.replacen("rank0 = [\"single\", \"dual\"]", "rank0 = [\"lr\"]", 1),
        TABLES.replacen("patterns = [[0, 0]", "patterns = [[16, 0]", 1),
    ] {
        assert_ne!(tables, TABLES);
        assert!(compile(ROME, &tables).is_err());
    }
}

#[test]
fn editing_a_setting_changes_output_not_the_donor_guard() {
    let before = compile(ROME, TABLES).unwrap();
    let changed = ROME.replace("MemRestoreControl = false", "MemRestoreControl = true");
    let after = compile(&changed, TABLES).unwrap();
    assert_eq!(before.len(), after.len());
    let base = before
        .iter()
        .position(|r| r.selector.kind == 0x68 && r.selector.instance == 0)
        .unwrap();
    assert_ne!(before[base].bytes, after[base].bytes);
    for (i, (a, b)) in before.iter().zip(&after).enumerate() {
        assert_eq!(a.selector, b.selector);
        assert_eq!(a.donor_sha256, b.donor_sha256);
        assert_eq!(b.bytes.iter().fold(0u8, |sum, x| sum.wrapping_add(*x)), 0);
        if i != base {
            assert_eq!(a.bytes, b.bytes);
        }
    }
}

#[test]
fn emitted_schemas_have_strict_roots_and_known_tokens() {
    let board = serde_json::to_value(schema()).unwrap();
    let tables = serde_json::to_value(tables_schema()).unwrap();
    assert_eq!(board["additionalProperties"], false);
    assert_eq!(tables["additionalProperties"], false);
    assert_eq!(board["properties"]["schema"]["minimum"], 1.0);
    assert_eq!(board["properties"]["schema"]["maximum"], 1.0);
    assert_eq!(
        board["definitions"]["Ddr4Rate"]["enum"],
        json!([
            400, 533, 667, 800, 1066, 1333, 1600, 1866, 2133, 2400, 2667, 2933, 3200
        ])
    );
    let eeprom = &board["definitions"]["Table"]["oneOf"]
        .as_array()
        .unwrap()
        .iter()
        .find(|variant| variant["properties"]["kind"]["enum"] == json!(["board-id-eeprom"]))
        .unwrap()["properties"];
    assert!(eeprom.get("id-offset").is_some());
    assert!(eeprom.get("id_offset").is_none());
    assert!(board.to_string().contains("AblSerialBaudRate"));
}
