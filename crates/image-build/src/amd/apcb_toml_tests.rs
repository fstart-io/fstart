// SPDX-License-Identifier: MIT OR Apache-2.0
use super::*;
const ROME: &str = include_str!("../../../../boards/tyan/s8030/apcb/rome.toml");
const TABLES: &str = include_str!("../../../platform-sp3/src/host/apcb/ddr4.toml");
const OLD_ROME: &str = include_str!("../../../../boards/tyan/s8030/apcb/rome.json");
const OLD_TABLES: &str = include_str!("../../../platform-sp3/src/host/apcb/ddr4.json");

#[test]
fn rome_toml_is_byte_exact_without_json_templates() {
    let built = compile(ROME, TABLES).unwrap();
    let old = apcb_v3::compile(OLD_ROME, &serde_json::from_str(OLD_TABLES).unwrap()).unwrap();
    assert_eq!(built.len(), 5);
    assert_eq!(built, old);
    let hashes: BTreeSet<_> = built.iter().map(|r| apcb_v3::sha256(&r.bytes)).collect();
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
        ROME.replacen("generation = \"rome\"", "generation = \"milan\"", 1),
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
    let eeprom = &board["definitions"]["Table"]["oneOf"][0]["properties"];
    assert!(eeprom.get("id-offset").is_some());
    assert!(eeprom.get("id_offset").is_none());
    assert!(board.to_string().contains("AblSerialBaudRate"));
}
