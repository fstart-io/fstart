use super::*;

struct Fixture {
    readings: Readings,
    log: Vec<u8>,
    separator_data: usize,
}

// Encode the specification's wire format independently of the parser and the
// firmware writer, including PCR readings computed from the original events.
fn fixture(
    banks: &[u16],
    agile: bool,
    separator: u32,
    omit_separator: Option<u32>,
    omit_event: Option<u32>,
    omit_digest: Option<u16>,
) -> Fixture {
    let mut readings = Readings::new();
    for &alg in banks {
        readings.insert(
            bank(alg).unwrap().into(),
            (0..10)
                .map(|pcr| (pcr, vec![0; pcr::digest_size(alg).unwrap()]))
                .collect(),
        );
    }
    let mut spec = if agile {
        b"Spec ID Event03\0".to_vec()
    } else {
        b"Spec ID Event00\0".to_vec()
    };
    spec.extend(0u32.to_le_bytes());
    spec.extend(if agile { [0, 2, 106, 2] } else { [2, 1, 1, 0] });
    if agile {
        spec.extend((banks.len() as u32).to_le_bytes());
        for &alg in banks {
            spec.extend(alg.to_le_bytes());
            spec.extend((pcr::digest_size(alg).unwrap() as u16).to_le_bytes());
        }
    }
    spec.push(0);
    let mut log = Vec::new();
    log.extend(0u32.to_le_bytes());
    log.extend(EV_NO_ACTION.to_le_bytes());
    log.extend([0; 20]);
    log.extend((spec.len() as u32).to_le_bytes());
    log.extend(spec);
    let mut events = vec![
        (0, EV_S_CRTM_VERSION, b"version".to_vec()),
        (0, EV_S_CRTM_CONTENTS, b"firmware".to_vec()),
        (
            0,
            if agile { EV_POST_CODE2 } else { EV_POST_CODE },
            b"payload".to_vec(),
        ),
    ];
    events.extend(
        (0..8)
            .filter(|pcr| Some(*pcr) != omit_separator)
            .map(|pcr| (pcr, EV_SEPARATOR, separator.to_le_bytes().to_vec())),
    );
    events.push((4, EV_IPL, b"boot sector".to_vec()));
    let mut separator_data = 0;
    for (index, kind, data) in events
        .into_iter()
        .filter(|(_, kind, _)| Some(*kind) != omit_event)
    {
        let mut digests = Vec::new();
        for &alg in banks.iter().filter(|alg| Some(**alg) != omit_digest) {
            let mut measured = [0; MAX_DIGEST_SIZE];
            let len = pcr::hash(alg, &data, &mut measured).unwrap();
            let value = measured[..len].to_vec();
            let old = readings
                .get_mut(bank(alg).unwrap())
                .unwrap()
                .get_mut(&index)
                .unwrap();
            let mut extend = old.clone();
            extend.extend(&value);
            pcr::hash(alg, &extend, &mut measured).unwrap();
            *old = measured[..len].to_vec();
            digests.push((alg, value));
        }
        log.extend(index.to_le_bytes());
        log.extend(kind.to_le_bytes());
        if agile {
            log.extend((digests.len() as u32).to_le_bytes());
        }
        for (alg, value) in digests {
            if agile {
                log.extend(alg.to_le_bytes());
            }
            log.extend(value);
        }
        log.extend((data.len() as u32).to_le_bytes());
        if kind == EV_SEPARATOR && separator_data == 0 {
            separator_data = log.len();
        }
        log.extend(data);
    }
    Fixture {
        readings,
        log,
        separator_data,
    }
}

impl Fixture {
    fn serial(&self, legacy: bool) -> String {
        let mut text = String::new();
        for (bank, values) in &self.readings {
            for (pcr, value) in values {
                if legacy {
                    let spaced = value
                        .iter()
                        .map(|b| format!("{b:02X}"))
                        .collect::<Vec<_>>()
                        .join(" ");
                    writeln!(text, "TPMTEST pcrs PCR-{pcr:02}: {spaced}\r").unwrap();
                } else {
                    writeln!(text, "TPMTEST pcr {bank} {pcr} {}", hex(value)).unwrap();
                }
            }
        }
        write!(
            text,
            "TPMTEST_LOG_BEGIN\n{}\nTPMTEST_LOG_END\n",
            Base64::encode_string(&self.log)
        )
        .unwrap();
        text
    }
    fn verify(&self, expects: &[(u32, u32)], banks: Option<&[u16]>) -> Result<String> {
        verify(&self.serial(false), expects, banks)
    }
}

fn legacy() -> Fixture {
    fixture(&[pcr::ALG_SHA1], false, 0, None, None, None)
}

#[test]
fn complete_logs_and_all_banks() {
    for (banks, agile) in [
        (vec![4], false),
        (vec![4, 11], true),
        (vec![11, 12], true),
        (vec![13], true),
    ] {
        for separator in [0, u32::MAX] {
            let f = fixture(&banks, agile, separator, None, None, None);
            let payload = if agile { EV_POST_CODE2 } else { EV_POST_CODE };
            assert!(
                f.verify(
                    &[
                        (EV_S_CRTM_VERSION, 0),
                        (EV_S_CRTM_CONTENTS, 0),
                        (payload, 0)
                    ],
                    Some(&banks)
                )
                .unwrap()
                .ends_with("PASS\n")
            );
        }
    }
}

#[test]
fn legacy_linux_readings_and_crlf_log() {
    let f = legacy();
    assert!(verify(&f.serial(true).replace('\n', "\r\n"), &[], None).is_ok());
}

#[test]
fn every_pcr_reading_is_required() {
    for index in 0..10 {
        let mut f = legacy();
        f.readings.get_mut("sha1").unwrap().remove(&index);
        assert!(f.verify(&[], None).is_err());
    }
    let mut f = legacy();
    f.readings
        .get_mut("sha1")
        .unwrap()
        .retain(|pcr, _| *pcr == 9);
    assert!(
        f.verify(&[(EV_S_CRTM_VERSION, 0), (EV_S_CRTM_CONTENTS, 0)], None)
            .is_err()
    );
}

#[test]
fn bank_readings_and_expected_banks_must_match() {
    let mut f = fixture(&[4, 11], true, 0, None, None, None);
    f.readings.remove("sha256");
    assert!(f.verify(&[], None).is_err());
    let f = fixture(&[11], true, 0, None, None, None);
    assert!(
        f.verify(&[], Some(&[pcr::ALG_SHA256, pcr::ALG_SHA384]))
            .is_err()
    );
    assert!(f.verify(&[], Some(&[0x99])).is_err());
}

#[test]
fn every_event_must_measure_all_banks() {
    let f = fixture(&[4, 11], true, 0, None, None, Some(11));
    assert!(f.verify(&[], None).is_err());
}

#[test]
fn pcr_digest_size_and_replay_must_match() {
    for value in [vec![0; 19], vec![0; 20]] {
        let mut f = legacy();
        f.readings.get_mut("sha1").unwrap().insert(0, value);
        assert!(f.verify(&[], None).is_err());
    }
}

#[test]
fn every_separator_is_required_even_with_matching_pcrs() {
    for index in 0..8 {
        let f = fixture(&[4], false, 0, Some(index), None, None);
        assert!(f.verify(&[], None).is_err());
    }
}

#[test]
fn error_and_malformed_separators_fail_even_with_matching_replay() {
    for (banks, agile) in [(vec![4], false), (vec![4, 11], true)] {
        assert!(
            fixture(&banks, agile, 1, None, None, None)
                .verify(&[], None)
                .is_err()
        );
    }
    let mut f = legacy();
    f.log[f.separator_data..f.separator_data + 4].fill(0xff);
    assert!(f.verify(&[], None).is_err());
}

#[test]
fn payload_and_seabios_measurements_are_required() {
    assert!(
        fixture(&[4], false, 0, None, Some(EV_POST_CODE), None)
            .verify(&[(EV_POST_CODE, 0)], None)
            .is_err()
    );
    assert!(
        fixture(&[4], false, 0, None, Some(EV_IPL), None)
            .verify(&[(EV_POST_CODE, 0), (EV_IPL, 4)], None)
            .is_err()
    );
}

#[test]
fn truncation_trailing_bytes_and_invalid_base64_fail() {
    for (banks, agile) in [(vec![4], false), (vec![4, 11], true)] {
        let mut f = fixture(&banks, agile, 0, None, None, None);
        let original = f.log.clone();
        for end in [0, 10, 31, 40, original.len() - 1] {
            f.log = original[..end].to_vec();
            assert!(f.verify(&[], None).is_err());
        }
        f.log = original;
        f.log.push(1);
        assert!(f.verify(&[], None).is_err());
    }
    assert!(parse_serial("TPMTEST_LOG_BEGIN\n!!!!\nTPMTEST_LOG_END\n").is_err());
    assert!(parse_serial("").is_err());
}

#[test]
fn spec_algorithm_sizes_duplicates_and_vendor_lengths_are_validated() {
    let f = fixture(&[4, 11], true, 0, None, None, None);
    for (offset, bytes) in [
        (56, 0u32.to_le_bytes().to_vec()),
        (60, 0xffffu16.to_le_bytes().to_vec()),
        (62, 19u16.to_le_bytes().to_vec()),
        (64, 4u16.to_le_bytes().to_vec()),
        (68, vec![1]),
    ] {
        let mut bad = fixture(&[4, 11], true, 0, None, None, None);
        bad.log[offset..offset + bytes.len()].copy_from_slice(&bytes);
        assert!(bad.verify(&[], None).is_err());
    }
    assert!(f.verify(&[], None).is_ok());
}
