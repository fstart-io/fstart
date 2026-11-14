//! Host-side measured-boot gate: replay the guest's event log against its PCRs.
//! Shares bank hashing and event constants with firmware; wire fixtures in tests
//! are independent of the firmware's event-log writer.
use base64ct::{Base64, Encoding};
use byteorder::{LittleEndian as LE, ReadBytesExt};
use fstart_core::services::measure::kind::*;
use fstart_crypto::pcr::{self, BankHasher, MAX_DIGEST_SIZE};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;

type Error = Box<dyn std::error::Error + Send + Sync>;
type Result<T> = std::result::Result<T, Error>;
type Readings = BTreeMap<String, BTreeMap<u32, Vec<u8>>>;
const BANKS: [(u16, &str); 4] = [
    (pcr::ALG_SHA1, "sha1"),
    (pcr::ALG_SHA256, "sha256"),
    (pcr::ALG_SHA384, "sha384"),
    (pcr::ALG_SHA512, "sha512"),
];

fn bank(alg: u16) -> Result<&'static str> {
    BANKS
        .iter()
        .find(|(id, _)| *id == alg)
        .map(|(_, name)| *name)
        .ok_or_else(|| "unknown event algorithm".into())
}

fn unhex(text: &str) -> Result<Vec<u8>> {
    if !text.len().is_multiple_of(2) || !text.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("malformed PCR digest".into());
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).map_err(Into::into))
        .collect()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn parse_serial(text: &str) -> Result<(Readings, Vec<u8>)> {
    let mut readings = Readings::new();
    for line in text.lines() {
        if let Some((_, row)) = line.split_once("TPMTEST pcr ") {
            let fields: Vec<_> = row.split_whitespace().collect();
            if fields.len() >= 3 {
                readings
                    .entry(fields[0].into())
                    .or_default()
                    .insert(fields[1].parse()?, unhex(fields[2])?);
            }
        } else if let Some((_, row)) = line.split_once("TPMTEST pcrs PCR-")
            && let Some((index, digest)) = row.split_once(':')
        {
            let digest = digest.split_whitespace().collect::<String>();
            readings
                .entry("sha1".into())
                .or_default()
                .insert(index.parse()?, unhex(&digest)?);
        }
    }
    let (_, log) = text
        .split_once("TPMTEST_LOG_BEGIN\n")
        .or_else(|| text.split_once("TPMTEST_LOG_BEGIN\r\n"))
        .ok_or("no event log in the serial output")?;
    let (log, _) = log
        .split_once("TPMTEST_LOG_END")
        .ok_or("no event log end marker")?;
    let encoded: String = log.split_whitespace().collect();
    Ok((
        readings,
        Base64::decode_vec(&encoded).map_err(|_| "invalid base64 event log")?,
    ))
}

struct Event<'a> {
    pcr: u32,
    kind: u32,
    digests: BTreeMap<u16, Vec<u8>>,
    data: &'a [u8],
}

fn take<'a>(input: &mut &'a [u8], count: usize) -> Result<&'a [u8]> {
    let (data, rest) = input.split_at_checked(count).ok_or("truncated event log")?;
    *input = rest;
    Ok(data)
}

fn parse_log(mut log: &[u8]) -> Result<Vec<Event<'_>>> {
    let pcr = log.read_u32::<LE>()?;
    let kind = log.read_u32::<LE>()?;
    let digest = take(&mut log, 20)?.to_vec();
    let size = log.read_u32::<LE>()? as usize;
    let spec = take(&mut log, size)?;
    if pcr != 0
        || kind != EV_NO_ACTION
        || ![
            b"Spec ID Event00\0",
            b"Spec ID Event02\0",
            b"Spec ID Event03\0",
        ]
        .iter()
        .any(|s| spec.starts_with(*s))
    {
        return Err("missing Spec ID event".into());
    }
    let agile = spec.starts_with(b"Spec ID Event03\0");
    let mut sizes = BTreeMap::from([(pcr::ALG_SHA1, 20usize)]);
    let mut first = BTreeMap::from([(pcr::ALG_SHA1, digest)]);
    if agile {
        let mut algorithms = spec.get(24..).ok_or("truncated Spec ID event")?;
        let count = algorithms.read_u32::<LE>()?;
        if !(1..=BANKS.len() as u32).contains(&count) {
            return Err("invalid Spec ID algorithm list".into());
        }
        sizes.clear();
        first.clear();
        for _ in 0..count {
            let alg = algorithms.read_u16::<LE>()?;
            let size = usize::from(algorithms.read_u16::<LE>()?);
            if pcr::digest_size(alg) != Some(size) || sizes.insert(alg, size).is_some() {
                return Err("invalid Spec ID algorithm or digest size".into());
            }
            first.insert(alg, vec![0; size]);
        }
        let vendor_size = usize::from(algorithms.read_u8()?);
        if algorithms.len() != vendor_size {
            return Err("invalid Spec ID vendor info length".into());
        }
    }
    let mut events = vec![Event {
        pcr,
        kind,
        digests: first,
        data: spec,
    }];
    while !log.is_empty() {
        let pcr = log.read_u32::<LE>()?;
        let kind = log.read_u32::<LE>()?;
        let mut digests = BTreeMap::new();
        if agile {
            let count = log.read_u32::<LE>()?;
            if !(1..=sizes.len() as u32).contains(&count) {
                return Err("invalid event digest count".into());
            }
            for _ in 0..count {
                let alg = log.read_u16::<LE>()?;
                let size = *sizes.get(&alg).ok_or("unknown event algorithm")?;
                if digests
                    .insert(alg, take(&mut log, size)?.to_vec())
                    .is_some()
                {
                    return Err("duplicate event algorithm".into());
                }
            }
        } else {
            digests.insert(pcr::ALG_SHA1, take(&mut log, 20)?.to_vec());
        }
        let size = log.read_u32::<LE>()? as usize;
        events.push(Event {
            pcr,
            kind,
            digests,
            data: take(&mut log, size)?,
        });
    }
    Ok(events)
}

fn describe(kind: u32, data: &[u8]) -> (String, String) {
    let name = match kind {
        EV_POST_CODE => "EV_POST_CODE",
        EV_NO_ACTION => "EV_NO_ACTION",
        EV_SEPARATOR => "EV_SEPARATOR",
        EV_ACTION => "EV_ACTION",
        EV_EVENT_TAG => "EV_EVENT_TAG",
        EV_S_CRTM_CONTENTS => "EV_S_CRTM_CONTENTS",
        EV_S_CRTM_VERSION => "EV_S_CRTM_VERSION",
        EV_PLATFORM_CONFIG_FLAGS => "EV_PLATFORM_CONFIG_FLAGS",
        EV_IPL => "EV_IPL",
        EV_IPL_PARTITION_DATA => "EV_IPL_PARTITION_DATA",
        EV_POST_CODE2 => "EV_POST_CODE2",
        EV_EFI_VARIABLE_DRIVER_CONFIG => "EV_EFI_VARIABLE_DRIVER_CONFIG",
        EV_EFI_VARIABLE_BOOT => "EV_EFI_VARIABLE_BOOT",
        EV_EFI_BOOT_SERVICES_APPLICATION => "EV_EFI_BOOT_SERVICES_APPLICATION",
        EV_EFI_GPT_EVENT => "EV_EFI_GPT_EVENT",
        EV_EFI_ACTION => "EV_EFI_ACTION",
        EV_EFI_PLATFORM_FIRMWARE_BLOB => "EV_EFI_PLATFORM_FIRMWARE_BLOB",
        EV_EFI_HANDOFF_TABLES => "EV_EFI_HANDOFF_TABLES",
        EV_EFI_PLATFORM_FIRMWARE_BLOB2 => "EV_EFI_PLATFORM_FIRMWARE_BLOB2",
        EV_EFI_HANDOFF_TABLES2 => "EV_EFI_HANDOFF_TABLES2",
        EV_EFI_VARIABLE_BOOT2 => "EV_EFI_VARIABLE_BOOT2",
        EV_EFI_VARIABLE_AUTHORITY => "EV_EFI_VARIABLE_AUTHORITY",
        _ => return (format!("{kind:#x}"), String::new()),
    };
    let text = match kind {
        EV_POST_CODE2 | EV_EFI_PLATFORM_FIRMWARE_BLOB2 | EV_S_CRTM_CONTENTS
            if !data.is_empty() && usize::from(data[0]) + 17 == data.len() =>
        {
            String::from_utf8_lossy(&data[1..1 + usize::from(data[0])]).into_owned()
        }
        EV_S_CRTM_VERSION => {
            let units = data.chunks(2).map(|b| {
                if b.len() == 2 {
                    u16::from_le_bytes([b[0], b[1]])
                } else {
                    0xfffd
                }
            });
            char::decode_utf16(units)
                .map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER))
                .collect::<String>()
                .trim_end_matches('\0')
                .into()
        }
        EV_ACTION | EV_EFI_ACTION => String::from_utf8_lossy(data).into_owned(),
        EV_SEPARATOR => hex(data),
        _ => String::new(),
    };
    (name.into(), text)
}

fn digest(alg: u16, parts: &[&[u8]]) -> Vec<u8> {
    let mut hash = BankHasher::new(alg).expect("validated bank");
    for part in parts {
        hash.update(part);
    }
    let mut out = [0; MAX_DIGEST_SIZE];
    let size = hash.finalize(&mut out);
    out[..size].to_vec()
}

/// A successful report is returned only when all required measurements replay
/// and the log holds every `(measure::kind, PCR)` of `expects`. `banks` (TPM
/// algorithm ids) must be exactly those logged and read; any by default.
pub fn verify(serial: &str, expects: &[(u32, u32)], banks: Option<&[u16]>) -> Result<String> {
    let (readings, log) = parse_serial(serial)?;
    let events = parse_log(&log)?;
    let logged: BTreeSet<_> = events[0]
        .digests
        .keys()
        .map(|alg| bank(*alg))
        .collect::<Result<_>>()?;
    let expected: BTreeSet<_> = match banks {
        Some(banks) => banks.iter().map(|alg| bank(*alg)).collect::<Result<_>>()?,
        None => logged.clone(),
    };
    if expected != logged
        || readings.keys().map(String::as_str).collect::<BTreeSet<_>>() != expected
    {
        return Err(format!(
            "bank mismatch: expected {expected:?}, log {logged:?}, readings {:?}",
            readings.keys()
        )
        .into());
    }
    let mut replay: BTreeMap<(u16, u32), Vec<u8>> = BTreeMap::new();
    let mut seen = BTreeSet::new();
    let mut separators = BTreeSet::new();
    let mut output = format!("{} events, {} bytes\n", events.len(), log.len());
    let mut failures = Vec::new();
    for event in events {
        let (name, text) = describe(event.kind, event.data);
        seen.insert((event.kind, event.pcr));
        writeln!(output, "  PCR{:<2} {name:34} {text}", event.pcr)?;
        if event.kind == EV_NO_ACTION {
            continue;
        }
        let measured: BTreeSet<_> = event
            .digests
            .keys()
            .map(|alg| bank(*alg))
            .collect::<Result<_>>()?;
        if measured != expected {
            failures.push(format!(
                "PCR{} event does not measure every bank",
                event.pcr
            ));
        }
        if event.kind == EV_SEPARATOR {
            if ![&[0u8; 4][..], &[0xffu8; 4][..]].contains(&event.data)
                || event
                    .digests
                    .iter()
                    .any(|(alg, value)| *value != digest(*alg, &[event.data]))
            {
                failures.push(format!("PCR{} error or malformed separator", event.pcr));
            }
            separators.insert(event.pcr);
        }
        for (alg, value) in event.digests {
            let old = replay
                .entry((alg, event.pcr))
                .or_insert_with(|| vec![0; pcr::digest_size(alg).expect("validated bank")]);
            *old = digest(alg, &[old, &value]);
        }
    }
    for (alg, name) in BANKS
        .into_iter()
        .filter(|(_, name)| expected.contains(name))
    {
        let size = pcr::digest_size(alg).expect("known bank");
        for index in 0..10 {
            let value = readings[name].get(&index).filter(|v| v.len() == size);
            let Some(value) = value else {
                failures.push(format!("missing or malformed {name} PCR{index} reading"));
                continue;
            };
            let zero = vec![0; size];
            let want = replay.get(&(alg, index)).unwrap_or(&zero);
            if value == want {
                writeln!(output, "ok   {name} PCR{index}: {}", hex(value))?;
            } else {
                failures.push(format!(
                    "{name} PCR{index}: {} replay {}",
                    hex(value),
                    hex(want)
                ));
            }
        }
    }
    if !(0..8).all(|pcr| separators.contains(&pcr)) {
        failures.push("missing PCR 0-7 separators".into());
    }
    for &(kind, pcr) in expects {
        if !seen.contains(&(kind, pcr)) {
            failures.push(format!("no {}@{pcr} in the log", describe(kind, &[]).0));
        }
    }
    if !failures.is_empty() {
        for failure in failures {
            writeln!(output, "FAIL: {failure}")?;
        }
        return Err((output + "FAIL").into());
    }
    Ok(output + "PASS\n")
}

#[cfg(test)]
#[path = "tpm_log_tests.rs"]
mod tests;
