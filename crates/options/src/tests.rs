extern crate std;

use super::*;
use std::collections::BTreeMap;
use std::string::String;
use std::vec::Vec;

#[derive(Debug, Clone, PartialEq)]
struct Config {
    power: u32,
    smee: bool,
    width: [u8; 2],
    fused: bool,
}

const BOARD: Config = Config {
    power: 2,
    smee: true,
    width: [16, 16],
    fused: false,
};

const POWER: &[Choice] = &[
    Choice::new(0, "Off"),
    Choice::new(1, "On"),
    Choice::new(2, "Last state"),
];
const WIDTH: &[Choice] = &[
    Choice::new(16, "x16"),
    Choice::new(8, "x8x8"),
    Choice::new(4, "x4x4x4x4"),
];

static KNOBS: [Knob<Config>; 4] = [
    Knob::new(
        crate::meta!(Meta {
            key: "power_on_after_fail",
            form: "Power",
            label: "Restore AC power loss",
            help: "State after power returns.",
            kind: Kind::Enum(POWER),
        }),
        |c| c.power,
        |c, v| c.power = v,
    ),
    Knob::new(
        crate::meta!(Meta {
            key: "smee",
            form: "CPU",
            label: "Memory encryption",
            help: "SYSCFG.SMEE.",
            kind: Kind::Bool,
        }),
        |c| c.smee.into(),
        |c, v| c.smee = v != 0,
    ),
    Knob::new(
        crate::meta!(Meta {
            key: "slot0",
            form: "PCIe",
            label: "Slot 0",
            help: "Bifurcation.",
            kind: Kind::Enum(WIDTH),
        }),
        |c| c.width[0].into(),
        |c, v| c.width[0] = v as u8,
    ),
    Knob::<Config>::new(
        crate::meta!(Meta {
            key: "slot1",
            form: "PCIe",
            label: "Slot 1",
            help: "Bifurcation.",
            kind: Kind::Enum(WIDTH),
        }),
        |c| c.width[1].into(),
        |c, v| c.width[1] = v as u8,
    )
    .when(|c| {
        if c.fused {
            Availability::Inactive("fused off")
        } else {
            Availability::Editable
        }
    }),
];

struct Map(BTreeMap<&'static str, Result<u32, SourceError>>);

impl Source for Map {
    fn name(&self) -> &'static str {
        "map"
    }
    fn read(&self, key: &str) -> Result<Option<u32>, SourceError> {
        self.0.get(key).copied().transpose()
    }
}

fn source(entries: &[(&'static str, Result<u32, SourceError>)]) -> Map {
    Map(entries.iter().copied().collect())
}

#[test]
fn no_stored_value_keeps_the_board_configuration() {
    let mut config = BOARD;
    let mut report = Report::new();
    resolve(&KNOBS, &NoSource, &mut config, &mut report);
    assert_eq!(config, BOARD);
    assert_eq!(report.settings.len(), 4);
    assert!(
        report
            .settings
            .iter()
            .all(|s| s.provenance == Provenance::Board && s.value == s.default)
    );
}

#[test]
fn stored_values_override_valid_and_reject_the_rest() {
    let mut config = BOARD;
    let mut report = Report::new();
    let source = source(&[
        ("power_on_after_fail", Ok(0)),
        ("smee", Ok(7)),
        ("slot0", Err(SourceError::Corrupt)),
        ("slot1", Ok(8)),
    ]);
    resolve(&KNOBS, &source, &mut config, &mut report);
    assert_eq!(
        config,
        Config {
            power: 0,
            width: [16, 8],
            ..BOARD
        }
    );
    let provenance: Vec<_> = report.settings.iter().map(|s| s.provenance).collect();
    assert_eq!(
        provenance,
        [
            Provenance::Override,
            Provenance::Rejected {
                stored: 7,
                why: Rejection::OutOfRange
            },
            Provenance::Rejected {
                stored: 16,
                why: Rejection::Source(SourceError::Corrupt)
            },
            Provenance::Override,
        ]
    );
    assert_eq!(report.overrides().count(), 2);
}

#[test]
fn a_stored_board_value_is_not_an_override() {
    let mut config = BOARD;
    let mut report = Report::new();
    resolve(
        &KNOBS,
        &source(&[("smee", Ok(1))]),
        &mut config,
        &mut report,
    );
    assert_eq!(report.settings[1].provenance, Provenance::Board);
}

#[test]
fn inactive_knobs_ignore_their_stored_value() {
    let mut config = Config {
        fused: true,
        ..BOARD
    };
    let mut report = Report::new();
    resolve(
        &KNOBS,
        &source(&[("slot1", Ok(4))]),
        &mut config,
        &mut report,
    );
    assert_eq!(config.width[1], 16);
    assert_eq!(
        report.settings[3].provenance,
        Provenance::Rejected {
            stored: 4,
            why: Rejection::Inactive
        }
    );
    assert_eq!(
        report.settings[3].availability,
        Availability::Inactive("fused off")
    );
}

#[test]
fn a_failed_whole_set_check_restores_the_board_configuration() {
    let mut config = BOARD;
    let mut report = Report::new();
    let source = source(&[
        ("slot0", Ok(4)),
        ("slot1", Ok(4)),
        ("power_on_after_fail", Ok(1)),
    ]);
    resolve_checked(&KNOBS, &source, &mut config, &mut report, |c| {
        if c.width == [4, 4] {
            Err("too many ports on one core")
        } else {
            Ok(())
        }
    });
    assert_eq!(config, BOARD);
    for index in [0, 2, 3] {
        let s = report.settings[index];
        assert_eq!(s.value, s.default);
        assert!(matches!(
            s.provenance,
            Provenance::Rejected {
                why: Rejection::Conflict("too many ports on one core"),
                ..
            }
        ));
    }
    assert_eq!(report.settings[1].provenance, Provenance::Board);
}

#[test]
fn a_second_knob_with_a_known_key_is_not_resolved() {
    let mut config = BOARD;
    let mut report = Report::new();
    let stored = source(&[("power_on_after_fail", Ok(0))]);
    resolve(&KNOBS, &stored, &mut config, &mut report);
    assert_eq!(config.power, 0);
    // The same knobs again: every key is taken, nothing changes.
    let mut again = BOARD;
    resolve(&KNOBS, &stored, &mut again, &mut report);
    assert_eq!(again, BOARD);
    assert_eq!(report.settings.len(), 4);
    assert_eq!(report.duplicates.len(), 4);
    assert_eq!(report.duplicates[0].key, "power_on_after_fail");
    let metas = KNOBS.iter().chain(&KNOBS).map(|k| k.meta);
    assert_eq!(duplicate_key(metas), Some("power_on_after_fail"));
    assert_eq!(duplicate_key(KNOBS.iter().map(|k| k.meta)), None);
}

#[test]
fn bool_metas_are_bool_knobs() {
    assert_eq!(Meta::bool("k", "f", "l", "h").kind, Kind::Bool);
}

#[test]
fn resolve_each_needs_no_report() {
    let stored = source(&[("slot1", Ok(8)), ("power_on_after_fail", Ok(7))]);
    let mut config = BOARD;
    let mut seen = [None; 4];
    let mut n = 0;
    resolve_each(
        &KNOBS,
        &stored,
        &mut config,
        |_| Ok(()),
        |setting| {
            seen[n] = Some((setting.meta.key, setting.provenance));
            n += 1;
        },
    );
    assert_eq!(config.width, [16, 8]);
    assert_eq!(n, 4);
    assert_eq!(seen[3], Some(("slot1", Provenance::Override)));
    assert!(matches!(
        seen[0],
        Some((
            "power_on_after_fail",
            Provenance::Rejected {
                stored: 7,
                why: Rejection::OutOfRange
            }
        ))
    ));
    // The same set, rejected as a whole: the board's values, reported so.
    let mut config = BOARD;
    let mut rejected = 0;
    resolve_each(
        &KNOBS,
        &stored,
        &mut config,
        |_| Err("no"),
        |setting| {
            rejected += matches!(
                setting.provenance,
                Provenance::Rejected {
                    why: Rejection::Conflict("no"),
                    ..
                }
            ) as u32;
        },
    );
    assert_eq!((config, rejected), (BOARD, 1));
}

#[test]
fn kinds_check_their_values() {
    assert!(Kind::Bool.contains(1) && !Kind::Bool.contains(2));
    let number = Kind::Number {
        min: 10,
        max: 20,
        step: 5,
    };
    assert!(number.contains(15) && !number.contains(16) && !number.contains(25));
    assert!(Kind::Enum(WIDTH).contains(8) && !Kind::Enum(WIDTH).contains(2));
}

/// A CFR record: tag, the header words after the size, children.
#[derive(Debug)]
struct Record {
    tag: u32,
    header: Vec<u8>,
    children: Vec<Record>,
    text: Option<String>,
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

/// Parse records the way coreboot's payloads do: every record is tag,
/// size, a fixed header for its tag, then child records.
fn parse(mut b: &[u8]) -> Vec<Record> {
    let mut out = Vec::new();
    while !b.is_empty() {
        let (tag, size) = (u32_at(b, 0), u32_at(b, 4) as usize);
        assert!(
            size >= 8 && size <= b.len() && size % 4 == 0,
            "{tag} {size}"
        );
        let record = &b[..size];
        let fixed = match tag {
            1 => 20,
            2 => 4,
            3..=5 => 40,
            7..=9 => {
                let len = u32_at(record, 8) as usize;
                let text = &record[12..12 + len];
                assert_eq!(text.last(), Some(&0));
                out.push(Record {
                    tag,
                    header: Vec::new(),
                    children: Vec::new(),
                    text: Some(String::from_utf8(text[..len - 1].to_vec()).unwrap()),
                });
                b = &b[size..];
                continue;
            }
            _ => panic!("tag {tag}"),
        };
        out.push(Record {
            tag,
            header: record[8..8 + fixed].to_vec(),
            children: parse(&record[8 + fixed..]),
            text: None,
        });
        b = &b[size..];
    }
    out
}

fn text(children: &[Record], tag: u32) -> &str {
    children
        .iter()
        .find(|r| r.tag == tag)
        .and_then(|r| r.text.as_deref())
        .unwrap()
}

#[test]
fn cfr_has_one_form_per_page_with_board_defaults() {
    let mut config = Config {
        fused: true,
        ..BOARD
    };
    let mut report = Report::new();
    let source = source(&[("power_on_after_fail", Ok(0)), ("slot1", Ok(4))]);
    resolve(&KNOBS, &source, &mut config, &mut report);
    let blob = cfr::encode(&report);
    assert_eq!(u32_at(&blob, 0), cfr::TAG_ROOT);
    assert_eq!(u32_at(&blob, 4) as usize, blob.len());
    assert_eq!(u32_at(&blob, 8), cfr::VERSION);
    assert_eq!(u32_at(&blob, 12), cfr::crc32(&blob[16..]));

    let forms = parse(&blob[16..]);
    let names: Vec<_> = forms.iter().map(|f| text(&f.children, 8)).collect();
    assert_eq!(names, ["Power", "CPU", "PCIe"]);
    let power = &forms[0].children[1];
    assert_eq!(power.tag, 3);
    assert_eq!(text(&power.children, 7), "power_on_after_fail");
    // Default: the board's value, not the override.
    assert_eq!(u32_at(&power.header, 20), 2);
    let values: Vec<_> = power
        .children
        .iter()
        .filter(|r| r.tag == 2)
        .map(|r| (u32_at(&r.header, 0), text(&r.children, 8)))
        .collect();
    assert_eq!(values, [(0, "Off"), (1, "On"), (2, "Last state")]);

    let smee = &forms[1].children[1];
    assert_eq!((smee.tag, u32_at(&smee.header, 20)), (5, 1));

    let slot1 = &forms[2].children[2];
    assert_eq!(text(&slot1.children, 7), "slot1");
    assert_eq!(u32_at(&slot1.header, 16), cfr::FLAG_INACTIVE);
    let help = text(&slot1.children, 9);
    assert!(help.contains("Unavailable: fused off."), "{help}");
    assert!(help.contains("Stored value 4 not applied"), "{help}");

    // Object IDs are unique and nonzero.
    let mut ids: Vec<u64> = forms
        .iter()
        .flat_map(|f| {
            core::iter::once(&f.header).chain(
                f.children
                    .iter()
                    .filter(|c| c.text.is_none())
                    .map(|c| &c.header),
            )
        })
        .filter(|h| h.len() >= 8)
        .map(|h| u64::from_le_bytes(h[..8].try_into().unwrap()))
        .collect();
    let count = ids.len();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), count);
    assert!(!ids.contains(&0));
}

#[test]
fn crc_matches_coreboot() {
    // coreboot's crc32_byte over "123456789": the MSB-first CRC-32 with
    // zero initial value and no final XOR (CRC-32/MPEG-2 without the init).
    assert_eq!(cfr::crc32(b"123456789"), 0x89a1_897f);
}

mod cmos {
    use crate::cmos::{Cmos, Ram, Window, WriteError, collision, hash};
    use crate::{Layered, Meta, Source, SourceError};
    use core::cell::RefCell;

    struct Bank(RefCell<[u8; 128]>);
    impl Ram for Bank {
        fn read(&self, index: u8) -> u8 {
            self.0.borrow()[usize::from(index)]
        }
        fn write(&self, index: u8, value: u8) {
            self.0.borrow_mut()[usize::from(index)] = value;
        }
    }

    const WINDOW: Window = Window {
        first: 0x40,
        last: 0x4f,
    };
    const _: () = assert!(WINDOW.check());

    #[test]
    fn an_unset_checksum_holds_nothing_until_the_first_write() {
        let bank = Bank(RefCell::new([0x77; 128]));
        let cmos = Cmos::new(WINDOW, &bank);
        assert!(!cmos.valid());
        assert_eq!(cmos.read("debug_level"), Ok(None));
        assert_eq!(cmos.write("debug_level", 4), Ok(()));
        assert!(cmos.valid());
        assert_eq!(cmos.read("debug_level"), Ok(Some(4)));
        assert_eq!(cmos.read("power_on_after_fail"), Ok(None));
        // The checksum: the byte sum, big-endian in the last two bytes.
        let sum: u16 = (0x40..0x4e).map(|i| u16::from(bank.read(i))).sum();
        assert_eq!([bank.read(0x4e), bank.read(0x4f)], sum.to_be_bytes());
        // Bytes outside the window keep their contents.
        assert_eq!(bank.read(0x3f), 0x77);
        assert_eq!(bank.read(0x50), 0x77);
        // Later writes keep and replace records.
        assert_eq!(cmos.write("power_on_after_fail", 0x1_0002), Ok(()));
        assert_eq!(cmos.write("debug_level", 7), Ok(()));
        assert_eq!(cmos.read("debug_level"), Ok(Some(7)));
        assert_eq!(cmos.read("power_on_after_fail"), Ok(Some(0x1_0002)));
        // A cleared CMOS is unset again.
        bank.write(0x45, 0xaa);
        assert_eq!(cmos.read("debug_level"), Ok(None));
        // All-zero CMOS sums to its zero checksum: valid and empty.
        let zero = Bank(RefCell::new([0; 128]));
        assert_eq!(Cmos::new(WINDOW, &zero).read("debug_level"), Ok(None));
    }

    #[test]
    fn records_fill_the_window_and_no_further() {
        let bank = Bank(RefCell::new([0; 128]));
        let cmos = Cmos::new(WINDOW, &bank);
        // 14 record bytes: three 1-byte records (4 bytes each) fit.
        for key in ["a", "b", "c"] {
            assert_eq!(cmos.write(key, 1), Ok(()));
        }
        assert_eq!(cmos.write("d", 1), Err(WriteError::Full));
        assert_eq!(cmos.read("c"), Ok(Some(1)));
        assert_eq!(cmos.read("d"), Ok(None));
    }

    #[test]
    fn a_malformed_record_is_corrupt() {
        let bank = Bank(RefCell::new([0; 128]));
        let cmos = Cmos::new(WINDOW, &bank);
        cmos.write("a", 1).unwrap();
        bank.write(0x40, 9);
        let sum: u16 = (0x40..0x4e).map(|i| u16::from(bank.read(i))).sum();
        let [high, low] = sum.to_be_bytes();
        bank.write(0x4e, high);
        bank.write(0x4f, low);
        assert_eq!(cmos.read("a"), Err(SourceError::Corrupt));
    }

    #[test]
    fn cmos_shadows_the_store_behind_it_for_its_keys() {
        let bank = Bank(RefCell::new([0; 128]));
        let cmos = Cmos::new(WINDOW, &bank);
        cmos.write("debug_level", 7).unwrap();
        struct Flash;
        impl Source for Flash {
            fn name(&self) -> &'static str {
                "flash"
            }
            fn read(&self, _: &str) -> Result<Option<u32>, SourceError> {
                Ok(Some(3))
            }
        }
        let both = Layered(&cmos, Flash);
        assert_eq!(both.read("debug_level"), Ok(Some(7)));
        assert_eq!(both.read("smee"), Ok(Some(3)));
        let mut names = super::Vec::new();
        both.names(&mut |name| names.push(name));
        assert_eq!(names, ["CMOS", "flash"]);
    }

    #[test]
    fn windows_stay_in_bank_0_and_keys_need_distinct_hashes() {
        assert!(
            !Window {
                first: 0x0d,
                last: 0x4f
            }
            .check()
        );
        assert!(
            !Window {
                first: 0x40,
                last: 0x80
            }
            .check()
        );
        assert!(
            !Window {
                first: 0x40,
                last: 0x47
            }
            .check()
        );
        assert_ne!(hash("debug_level"), hash("power_on_after_fail"));
        const fn meta(key: &'static str) -> Meta {
            Meta::bool(key, "", "", "")
        }
        let a = meta("a");
        let b = meta("b");
        assert_eq!(
            collision([&a, &b, &a]),
            None,
            "the same key is not a collision"
        );
    }
}
