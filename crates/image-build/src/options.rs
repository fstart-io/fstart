//! The options a firmware build offers, as its plan lists them for tools:
//! the web assembler's settings form, image presets, an OS tool. The
//! board's Rust is the only description: its platform plan runs the
//! board's option sets on the host (`fstart_options::Describe`) and this
//! module checks and serializes what they say.

use fstart_options::{Availability, Entry, Kind, cmos};
use serde::{Deserialize, Serialize};

/// A build's options and where the board keeps their stored values.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Options {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub store: Option<OptionStore>,
    pub options: Vec<OptionSchema>,
}

impl Options {
    pub fn is_empty(&self) -> bool {
        self.store.is_none() && self.options.is_empty()
    }

    /// Settings a new image's store starts with (`KEY=VALUE`, as
    /// `fbuild assemble --setting` takes them), checked against the
    /// build's options: each key one the build offers and the board has,
    /// set once, each value one its option takes. A value is a number, or
    /// `true`/`false` for an on/off and a choice's label for a choice.
    pub fn settings(&self, settings: &[(String, String)]) -> Result<Vec<(String, u32)>, String> {
        let mut values: Vec<(String, u32)> = Vec::with_capacity(settings.len());
        for (key, value) in settings {
            let option = (self.options.iter())
                .find(|option| &option.key == key)
                .ok_or_else(|| format!("setting `{key}`: the build offers no such option"))?;
            if let Some(why) = &option.unavailable {
                return Err(format!("setting `{key}`: the board lacks it ({why})"));
            }
            if values.iter().any(|(set, _)| set == key) {
                return Err(format!("setting `{key}` is given twice"));
            }
            let parsed = option
                .kind
                .parse(value)
                .ok_or_else(|| format!("setting `{key}`: `{value}` is not one of its values"))?;
            values.push((key.clone(), parsed));
        }
        Ok(values)
    }

    /// The settings store of a new image with `settings` ([`Self::settings`])
    /// in it, and where it goes: none without settings. Only a store in
    /// flash can start with them.
    pub fn seed(&self, settings: &[(String, String)]) -> Result<Option<Seed>, String> {
        let values = self.settings(settings)?;
        if values.is_empty() {
            return Ok(None);
        }
        let Some(OptionStore::Varstore { offset, size }) = self.store else {
            return Err("settings: the board keeps no options in its flash image".into());
        };
        let values: Vec<_> = values.iter().map(|(k, v)| (k.as_str(), *v)).collect();
        let bytes = fstart_options::varstore::store(size as usize, &values)
            .ok_or("settings: they do not fit the settings store")?;
        Ok(Some(Seed { offset, bytes }))
    }
}

/// A settings store to write into a new image ([`Options::seed`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Seed {
    /// The store's flash offset ([`OptionStore::Varstore`]).
    pub offset: u32,
    /// The whole store, as large as its slot.
    pub bytes: Vec<u8>,
}

/// Where stored values live, for the tool that writes them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OptionStore {
    /// `fstart_options::cmos` records in CMOS RAM bank 0, `first..=last`.
    Cmos { first: u8, last: u8 },
    /// An EDK2 variable store in flash, one variable per option under
    /// coreboot's CFR GUID.
    Varstore { offset: u32, size: u32 },
}

/// One option, as tools show it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OptionSchema {
    pub key: String,
    pub form: String,
    pub label: String,
    pub help: String,
    pub kind: OptionKind,
    /// The board's value without an override.
    pub default: u32,
    /// Why the board lacks the option, if it does: the boot ignores a
    /// stored value for it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unavailable: Option<String>,
}

/// The values an option takes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OptionKind {
    Bool,
    Enum(Vec<OptionChoice>),
    Number { min: u32, max: u32, step: u32 },
}

impl OptionKind {
    /// The value `text` names, if the option takes it.
    pub fn parse(&self, text: &str) -> Option<u32> {
        let named = match self {
            Self::Bool => match text {
                "true" | "on" => Some(1),
                "false" | "off" => Some(0),
                _ => None,
            },
            Self::Enum(choices) => (choices.iter())
                .find(|choice| choice.label.eq_ignore_ascii_case(text))
                .map(|choice| choice.value),
            Self::Number { .. } => None,
        };
        let value = named.or_else(|| match text.strip_prefix("0x") {
            Some(hex) => u32::from_str_radix(hex, 16).ok(),
            None => text.parse().ok(),
        })?;
        self.contains(value).then_some(value)
    }

    /// Whether the option takes `value` (`fstart_options::Kind::contains`).
    pub fn contains(&self, value: u32) -> bool {
        match self {
            Self::Bool => value <= 1,
            Self::Enum(choices) => choices.iter().any(|choice| choice.value == value),
            Self::Number { min, max, step } => {
                (*min..=*max).contains(&value) && (value - min).is_multiple_of((*step).max(1))
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OptionChoice {
    pub value: u32,
    pub label: String,
}

impl From<Entry> for OptionSchema {
    fn from(entry: Entry) -> Self {
        let meta = entry.meta;
        Self {
            key: meta.key.into(),
            form: meta.form.into(),
            label: meta.label.into(),
            help: meta.help.into(),
            kind: match meta.kind {
                Kind::Bool => OptionKind::Bool,
                Kind::Enum(choices) => OptionKind::Enum(
                    choices
                        .iter()
                        .map(|choice| OptionChoice {
                            value: choice.value,
                            label: choice.label.into(),
                        })
                        .collect(),
                ),
                Kind::Number { min, max, step } => OptionKind::Number { min, max, step },
            },
            default: entry.default,
            unavailable: match entry.availability {
                Availability::Editable => None,
                Availability::Inactive(why) => Some(why.into()),
            },
        }
    }
}

/// The options `describe` lists, checked for what would only show on the
/// board: two options under one key, a board value its own option does not
/// take, and for a CMOS store two keys with one record name or more
/// options than the window holds when every one is changed.
pub fn collect(
    store: Option<OptionStore>,
    describe: impl FnOnce(&mut dyn FnMut(Entry)),
) -> Result<Options, String> {
    let mut entries = Vec::new();
    describe(&mut |entry| entries.push(entry));
    for (i, entry) in entries.iter().enumerate() {
        let key = entry.meta.key;
        if entries[..i].iter().any(|e| e.meta.key == key) {
            return Err(format!("option `{key}` is offered twice"));
        }
        if !entry.meta.kind.contains(entry.default) {
            return Err(format!(
                "option `{key}`: the board's value {} is not one of its values",
                entry.default
            ));
        }
    }
    if let Some(OptionStore::Cmos { first, last }) = store {
        let window = cmos::Window { first, last };
        if !window.check() {
            return Err(format!(
                "CMOS option window {first:#x}..={last:#x} is invalid"
            ));
        }
        if let Some((a, b)) = cmos::collision(entries.iter().map(|e| e.meta)) {
            return Err(format!("options `{a}` and `{b}` share a CMOS record name"));
        }
        // Every option changed, each to the value with the longest record.
        let needed: usize = entries.iter().map(|e| cmos::record_size(largest(e))).sum();
        if needed > window.capacity() {
            return Err(format!(
                "{} options need {needed} bytes of CMOS, the window holds {}",
                entries.len(),
                window.capacity()
            ));
        }
    }
    Ok(Options {
        store,
        options: entries.into_iter().map(Into::into).collect(),
    })
}

/// The largest value of an entry's option.
fn largest(entry: &Entry) -> u32 {
    match entry.meta.kind {
        Kind::Bool => 1,
        Kind::Enum(choices) => choices.iter().map(|c| c.value).max().unwrap_or(0),
        Kind::Number { max, .. } => max,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fstart_options::{Choice, Describe, Knob, Meta, OptionSet};

    #[derive(Clone)]
    struct Radios {
        wlan: bool,
        volume: u8,
    }
    static KNOBS: [Knob<Radios>; 2] = [
        Knob::new(
            Meta::bool("wlan", "Radios", "WLAN", "The WLAN card's radio."),
            |r| r.wlan.into(),
            |r, v| r.wlan = v != 0,
        ),
        Knob::new(
            Meta {
                key: "volume",
                form: "Audio",
                label: "Volume",
                help: "",
                kind: Kind::Enum(&[Choice::new(0, "Mute"), Choice::new(3, "Normal")]),
            },
            |r| r.volume.into(),
            |r, v| r.volume = v as u8,
        ),
    ];
    static SET: OptionSet<Radios> = OptionSet {
        knobs: &KNOBS,
        board: Radios {
            wlan: true,
            volume: 3,
        },
    };

    #[test]
    fn a_set_lists_its_options_with_the_boards_values() {
        let cmos = Some(OptionStore::Cmos {
            first: 0x40,
            last: 0x7f,
        });
        let options = collect(cmos, |each| SET.describe(each)).unwrap();
        assert_eq!(options.store, cmos);
        let [wlan, volume] = &options.options[..] else {
            panic!("two options")
        };
        assert_eq!((wlan.key.as_str(), wlan.default), ("wlan", 1));
        assert_eq!(wlan.kind, OptionKind::Bool);
        assert_eq!((volume.form.as_str(), volume.default), ("Audio", 3));
        assert_eq!(
            serde_json::to_value(&options).unwrap()["options"][1]["kind"]["enum"][1]["label"],
            "Normal"
        );
    }

    #[test]
    fn settings_name_an_offered_option_and_one_of_its_values() {
        let store = OptionStore::Varstore {
            offset: 0x10_0000,
            size: 0x4000,
        };
        let options = collect(Some(store), |each| SET.describe(each)).unwrap();
        let set = |pairs: &[(&str, &str)]| {
            let pairs: Vec<_> = (pairs.iter())
                .map(|&(k, v)| (k.to_owned(), v.to_owned()))
                .collect();
            options.settings(&pairs)
        };
        assert_eq!(
            set(&[("wlan", "off"), ("volume", "mute")]).unwrap(),
            [("wlan".into(), 0), ("volume".into(), 0)]
        );
        assert_eq!(set(&[("volume", "3"), ("wlan", "1")]).unwrap()[0].1, 3);
        for bad in [
            &[("bluetooth", "1")][..],
            &[("volume", "2")],
            &[("wlan", "2")],
            &[("wlan", "yes")],
            &[("wlan", "1"), ("wlan", "0")],
        ] {
            assert!(set(bad).is_err(), "{bad:?}");
        }
        // The seeded store is the whole slot and reads back.
        let pairs = [("volume".to_owned(), "Mute".to_owned())];
        let seed = options.seed(&pairs).unwrap().unwrap();
        assert_eq!((seed.offset, seed.bytes.len()), (0x10_0000, 0x4000));
        use fstart_options::Source;
        let stored = fstart_options::varstore::VarstoreOptions::new(&seed.bytes);
        assert_eq!(stored.read("volume"), Ok(Some(0)));
        assert_eq!(options.seed(&[]), Ok(None));
        // A CMOS store cannot be seeded in an image.
        let cmos = Options {
            store: Some(OptionStore::Cmos {
                first: 0x40,
                last: 0x7f,
            }),
            ..options.clone()
        };
        assert!(cmos.seed(&pairs).is_err());
    }

    #[test]
    fn what_would_only_fail_on_the_board_fails_the_build() {
        let twice = collect(None, |each| {
            SET.describe(each);
            SET.describe(each)
        });
        assert_eq!(twice.unwrap_err(), "option `wlan` is offered twice");
        // Seven record bytes: the set's two 3-byte records fit, a 7-byte
        // one on top does not.
        static BIG: OptionSet<u32> = OptionSet {
            knobs: &[Knob::new(
                Meta {
                    key: "big",
                    form: "",
                    label: "",
                    help: "",
                    kind: Kind::Number {
                        min: 0,
                        max: u32::MAX,
                        step: 1,
                    },
                },
                |n| *n,
                |n, v| *n = v,
            )],
            board: 0,
        };
        let window = Some(OptionStore::Cmos {
            first: 0x40,
            last: 0x48,
        });
        assert!(collect(window, |each| SET.describe(each)).is_ok());
        let error = collect(window, |each| {
            SET.describe(each);
            BIG.describe(each)
        })
        .unwrap_err();
        assert!(error.contains("need 13 bytes of CMOS"), "{error}");
    }
}
