// SPDX-License-Identifier: MIT OR Apache-2.0
use super::*;

const TABLES: &str = include_str!("../../../platform-sp3/src/host/apcb/ddr4.json");
const ROME: &str = include_str!("../../../../boards/tyan/s8030/apcb/rome.json");
const MILAN: &str = include_str!("../../../../boards/tyan/s8030/apcb/milan.json");
const ETHANOLX: &str = include_str!("../../../../boards/amd/ethanolx/apcb/milan.json");

fn tables() -> Tables {
    serde_json::from_str(TABLES).unwrap()
}

#[test]
fn every_source_reconstructs_the_original_declared_payload() {
    // Independent golden digests: unused flash-slot padding is excluded.
    // Includes active/backup CBS and PPR, not just the three base APCBs.
    let expected = [
        (
            ROME,
            vec![
                "6f56aa5889f5b6eaa1be74864af3f962ca4c48ac599f8556caf03dcf8aea42ed",
                "27061c5113b34e7f8875e18303c8f2c797b2afa239e92d1effcbbf916b249afc",
                "20640126271299dc0083d9855eef4909e3743ad2c6474fdd6052e08c6fbcbe69",
            ],
        ),
        (
            MILAN,
            vec![
                "59a7be1a1dc8457b610e266ba5298572f386e4e4bf10f00daaf2f2fb33147de5",
                "32eec005935662bc68a358ea01e5fc086443c1b62efa6313a69a689df73cb226",
                "cb3278a0024f69ebb78a36c61ee470ddf9df326f1bd75b4fab438259d471f410",
                "e29b0902d900e3f0df2b8f4cf413cf9a25408985a4cc9d604fde166cc5a67ecb",
            ],
        ),
        (
            ETHANOLX,
            vec![
                "632c2a856ce8b5a9ace109ebab5a58c25709b7f709a3b5878112d43a2a89a4ee",
                "529e2c0f792dc782b226f6ec13efd5bbea1e02037ef016d1a73d60df6898ad42",
                "4e38f72f4c46e66e898ce89c41eb47c79260ff3eefcb4f32dbef8d752b5c89f3",
                "b870e11b0c9c8cb22bfb64d62866ed85f0b741f4b1729e4a76e90d34c5e68a59",
            ],
        ),
    ];
    for (text, hashes) in expected {
        let sources: Sources = serde_json::from_str(text).unwrap();
        assert_eq!(sources.blobs.len(), hashes.len());
        for (source, hash) in sources.blobs.iter().zip(hashes) {
            let bytes =
                build(&source.apcb, &tables()).unwrap_or_else(|e| panic!("{}: {e}", source.name));
            assert_eq!(sha256(&bytes), hash, "{}", source.name);
            let exported = decode(&bytes).unwrap();
            assert_eq!(build(&exported, &Tables::new()).unwrap(), bytes);
        }
        let compiled = compile(text, &tables()).unwrap();
        assert_eq!(compiled.len(), 5); // 0x68 i0/i8/i9 and 0x60 i0/i1
    }
}

#[test]
fn editing_a_typed_token_changes_the_configuration_not_the_donor_pin() {
    let mut sources: Value = serde_json::from_str(MILAN).unwrap();
    let blob = &mut sources["blobs"][0];
    let before = build(&blob["apcb"], &tables()).unwrap();
    let mut changed = false;
    for entry in blob["apcb"]["entries"].as_array_mut().unwrap() {
        if let Some(tokens) = entry.get_mut("tokens").and_then(Value::as_array_mut) {
            for token in tokens {
                if let Some(value) = token
                    .get_mut("Byte")
                    .and_then(|b| b.get_mut("AblSerialBaudRate"))
                {
                    *value = Value::String("9600 Baud".into());
                    changed = true;
                }
            }
        }
    }
    assert!(changed);
    let after = build(&blob["apcb"], &tables()).unwrap();
    assert_ne!(after, before);
    assert_eq!(blob["donor_sha256"].as_str().unwrap(), sha256(&before));
    assert_eq!(after.iter().fold(0u8, |sum, b| sum.wrapping_add(*b)), 0);
}

fn fixture(replacements: &[Replacement], capacity: usize) -> Vec<u8> {
    use fstart_amdfw_format::directory::write_table;
    let mut bytes = vec![0xa5; 0x20_000];
    let entries: Vec<_> = replacements
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let at = 0x1000 + i * 0x3000;
            bytes[at..at + r.bytes.len()].copy_from_slice(&r.bytes);
            BiosEntry::new(
                r.selector.kind,
                r.selector.subprogram,
                r.selector.instance,
                BiosEntry::READ_ONLY,
                capacity as u32,
                at as u64,
                u64::MAX,
            )
        })
        .collect();
    write_table(&mut bytes[0x100..], *b"$BHD", 0x10, &entries).unwrap();
    bytes
}

fn apply(original: &[u8], output: &mut [u8], replacements: &[Replacement]) -> Result<()> {
    use fstart_amdfw_format::{Flash, Profile, ROM2_BASE, efs::BiosDirectorySlot};
    let flash = Flash::region(
        original,
        0,
        ROM2_BASE,
        Profile::for_slot(BiosDirectorySlot::Family17hModel30),
    );
    let directories = Levels::read_checked(flash, 0x100).unwrap();
    replace(output, &directories, replacements)
}

#[test]
fn replacing_all_slots_preserves_firmware_and_padding() {
    let mut replacements = compile(ROME, &tables()).unwrap();
    let original = fixture(&replacements, 8192);
    let mut output = original.clone();
    apply(&original, &mut output, &replacements).unwrap();
    assert_eq!(output, original);
    // Make a genuine configuration edit, keeping its original donor guard.
    let source: Sources = serde_json::from_str(ROME).unwrap();
    let mut document = source.blobs[0].apcb.clone();
    document["header"]["unique_apcb_instance"] = Value::from(1234);
    let edited = build(&document, &tables()).unwrap();
    let index = replacements
        .iter()
        .position(|r| r.selector.instance == 0 && r.selector.kind == bios::APCB_BACKUP)
        .unwrap();
    replacements[index].bytes = edited.clone();
    let at = 0x1000 + index * 0x3000;
    apply(&original, &mut output, &replacements).unwrap();
    let mut expected = original;
    expected[at..at + edited.len()].copy_from_slice(&edited);
    assert_eq!(output, expected);
}

#[test]
fn toml_payloads_use_the_existing_replacement_guards() {
    let text = include_str!("../../../../boards/tyan/s8030/apcb/rome.toml");
    let shared = include_str!("../../../platform-sp3/src/host/apcb/ddr4.toml");
    let replacements = super::super::apcb_toml::compile(text, shared).unwrap();
    let old = compile(ROME, &tables()).unwrap();
    let original = fixture(&old, 8192);
    let mut output = original.clone();
    apply(&original, &mut output, &replacements).unwrap();
    assert_eq!(output, original);

    let edited = super::super::apcb_toml::compile(
        &text.replace("MemRestoreControl = false", "MemRestoreControl = true"),
        shared,
    )
    .unwrap();
    apply(&original, &mut output, &edited).unwrap();
    let mut expected = original.clone();
    for (i, replacement) in edited.iter().enumerate() {
        let at = 0x1000 + i * 0x3000;
        expected[at..at + replacement.bytes.len()].copy_from_slice(&replacement.bytes);
    }
    assert_eq!(output, expected);

    let mut bad_pin = replacements;
    bad_pin[0].donor_sha256 = "0".repeat(64);
    let mut unchanged = original.clone();
    assert!(apply(&original, &mut unchanged, &bad_pin).is_err());
    assert_eq!(unchanged, original);
}

#[test]
fn bad_pin_missing_selector_absent_selector_and_capacity_fail_without_writes() {
    let replacements = compile(ROME, &tables()).unwrap();
    let original = fixture(&replacements, 8192);
    for scenario in 0..4 {
        let mut bad = replacements.clone();
        match scenario {
            0 => bad[4].donor_sha256 = "0".repeat(64),
            1 => {
                bad.pop();
            }
            2 => {
                let mut extra = bad[0].clone();
                extra.selector.instance = 7;
                bad.push(extra);
            }
            3 => {
                let sources: Sources = serde_json::from_str(ROME).unwrap();
                let mut doc = sources.blobs[0].apcb.clone();
                let rows = doc["entries"][1]["DimmInfoSmbusElement"]
                    .as_array_mut()
                    .unwrap();
                let row = rows[0].clone();
                rows.resize(256, row);
                bad[4].bytes = build(&doc, &tables()).unwrap();
                assert!(bad[4].bytes.len() > 8192);
            }
            _ => unreachable!(),
        }
        let mut output = original.clone();
        let error = apply(&original, &mut output, &bad).unwrap_err();
        if scenario == 3 {
            assert!(error.contains("slot capacity"), "{error}");
        }
        assert_eq!(output, original);
    }
}

#[test]
fn source_rejects_missing_tables_duplicate_selectors_and_invalid_fields() {
    assert!(
        compile(ROME, &Tables::new())
            .unwrap_err()
            .contains("unknown APCB table")
    );
    let mut source: Value = serde_json::from_str(ROME).unwrap();
    let duplicate = source["blobs"][0]["selectors"][0].clone();
    source["blobs"][0]["selectors"]
        .as_array_mut()
        .unwrap()
        .push(duplicate);
    assert!(
        compile(&source.to_string(), &tables())
            .unwrap_err()
            .contains("duplicate APCB selector")
    );
    let source: Sources = serde_json::from_str(ROME).unwrap();
    let mut doc = source.blobs[0].apcb.clone();
    doc["entries"][0]["unknown_field"] = Value::Bool(true);
    assert!(build(&doc, &tables()).is_err());
    doc = source.blobs[0].apcb.clone();
    doc["header"]["version"] = Value::from(0x20);
    assert!(build(&doc, &tables()).unwrap_err().contains("v3 only"));
    assert!(decode(b"APCB").is_err());
}

#[test]
fn aliased_slots_must_agree_and_duplicate_plan_selectors_fail() {
    use fstart_amdfw_format::directory::write_table;
    let mut replacements = compile(ROME, &tables()).unwrap()[..2].to_vec();
    replacements[1].donor_sha256 = replacements[0].donor_sha256.clone();
    replacements[1].bytes = replacements[0].bytes.clone();
    let mut original = fixture(&replacements, 8192);
    let entries: Vec<_> = replacements
        .iter()
        .map(|r| {
            BiosEntry::new(
                r.selector.kind,
                r.selector.subprogram,
                r.selector.instance,
                BiosEntry::READ_ONLY,
                8192,
                0x1000,
                u64::MAX,
            )
        })
        .collect();
    write_table(&mut original[0x100..], *b"$BHD", 0x10, &entries).unwrap();
    let mut output = original.clone();
    apply(&original, &mut output, &replacements).unwrap();
    assert_eq!(output, original);
    let mut doc = decode(&replacements[1].bytes).unwrap();
    doc["header"]["unique_apcb_instance"] = Value::from(1234);
    replacements[1].bytes = build(&doc, &Tables::new()).unwrap();
    assert!(
        apply(&original, &mut output, &replacements)
            .unwrap_err()
            .contains("overlap")
    );
    assert_eq!(output, original);
    replacements[1] = replacements[0].clone();
    assert!(
        apply(&original, &mut output, &replacements)
            .unwrap_err()
            .contains("duplicate")
    );
    assert_eq!(output, original);
}
