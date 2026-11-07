//! The options a firmware build offers, read from its stage ELFs: the
//! `fstart_options::Record`s the linker keeps between
//! `_fstart_options_start` and `_fstart_options_end`. Each is `repr(C)`:
//! the knob's `Meta` (strings and choices are pointers into the same
//! image), then what the build knows of the board's value and whether the
//! board offers the option. The linker drops the records of knobs a stage
//! does not resolve, so a build's options are exactly what its stages read.

use object::{Object, ObjectSection, ObjectSymbol};
use serde::Serialize;

/// One option, as tools and the web assembler show it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OptionSchema {
    pub key: String,
    pub form: String,
    pub label: String,
    pub help: String,
    pub kind: OptionKind,
    /// The board's value without an override; `None` when the board
    /// detects its configuration at boot.
    pub default: Option<u32>,
    pub availability: OptionAvailability,
}

/// Whether the board offers an option. Options the build knows the board
/// lacks are left out of a build's options.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OptionAvailability {
    Editable,
    Hidden(String),
    /// Decided at boot, from what the board detects.
    Runtime,
}

/// The values an option takes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OptionKind {
    Bool,
    Enum(Vec<OptionChoice>),
    Number { min: u32, max: u32, step: u32 },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OptionChoice {
    pub value: u32,
    pub label: String,
}

/// The records of one ELF, in link order.
pub fn from_elf(data: &[u8]) -> Result<Vec<OptionSchema>, String> {
    let file = object::File::parse(data).map_err(|e| format!("not an ELF: {e}"))?;
    let symbol = |name: &str| {
        file.symbols()
            .find(|s| s.name() == Ok(name))
            .map(|s| s.address())
    };
    let (Some(start), Some(end)) = (
        symbol("_fstart_options_start"),
        symbol("_fstart_options_end"),
    ) else {
        // A stage linked without the option records (or before them).
        return Ok(Vec::new());
    };
    let image = Image {
        file: &file,
        pointer: if file.is_64() { 8 } else { 4 },
    };
    let size = record_size(image.pointer);
    if (end - start) % size != 0 {
        return Err(format!(
            "option records span {:#x} bytes, not a multiple of {size}",
            end - start
        ));
    }
    (0..(end - start) / size)
        .map(|i| image.record(start + i * size))
        .collect()
}

/// The options of a build, from its stages' records in boot order: the
/// last stage's, which hands the options to the payload's menu, without
/// those the build knows the board lacks. An option an earlier stage
/// resolves must be in the last one, described the same way: a menu could
/// not change it otherwise.
pub fn schema(stages: Vec<Vec<OptionSchema>>) -> Result<Vec<OptionSchema>, String> {
    let Some((last, earlier)) = stages.split_last() else {
        return Ok(Vec::new());
    };
    for (stage, options) in earlier.iter().enumerate() {
        for option in options {
            match last.iter().find(|o| o.key == option.key) {
                Some(known) if known == option => {}
                Some(_) => {
                    return Err(format!(
                        "stage {stage} and the last stage describe option `{}` differently",
                        option.key
                    ));
                }
                None => {
                    return Err(format!(
                        "stage {stage} resolves option `{}`, the last stage does not",
                        option.key
                    ));
                }
            }
        }
    }
    let mut options: Vec<OptionSchema> = Vec::new();
    for option in last {
        if options.iter().any(|o| o.key == option.key) {
            return Err(format!("option `{}` is recorded twice", option.key));
        }
        options.push(option.clone());
    }
    options.retain(|o| !matches!(o.availability, OptionAvailability::Hidden(_)));
    Ok(options)
}

/// `size_of::<Meta>()` with `pointer`-byte pointers: four `&str`, then
/// `Kind` (`repr(C, u32)`: the tag, then a union of `&[Choice]` and three
/// `u32`, pointer aligned).
const fn meta_size(pointer: u64) -> u64 {
    8 * pointer + kind_size(pointer)
}

const fn kind_size(pointer: u64) -> u64 {
    let union = if 2 * pointer > 12 { 2 * pointer } else { 12 };
    (union_offset(pointer) + union).next_multiple_of(pointer)
}

/// Where `Kind`'s union starts after its `u32` tag.
const fn union_offset(pointer: u64) -> u64 {
    4u64.next_multiple_of(pointer)
}

/// `size_of::<Record>()`: the `Meta`, `Baseline` (`repr(C, u32)` over a
/// `u32`), then `Offered` (`repr(C, u32)` over a `&str`), pointer aligned.
const fn record_size(pointer: u64) -> u64 {
    (offered_offset(pointer) + union_offset(pointer) + 2 * pointer).next_multiple_of(pointer)
}

const fn offered_offset(pointer: u64) -> u64 {
    (meta_size(pointer) + 8).next_multiple_of(pointer)
}

/// `size_of::<Choice>()`: a `u32`, then a pointer-aligned `&str`.
const fn choice_size(pointer: u64) -> u64 {
    (4 + 2 * pointer).next_multiple_of(pointer)
}

struct Image<'a, 'd> {
    file: &'a object::File<'d>,
    pointer: u64,
}

impl Image<'_, '_> {
    fn bytes(&self, address: u64, len: u64) -> Result<&[u8], String> {
        for section in self.file.sections() {
            let (base, size) = (section.address(), section.size());
            if address >= base && address + len <= base + size {
                let data = section
                    .data()
                    .map_err(|e| format!("section {:?}: {e}", section.name()))?;
                let at = (address - base) as usize;
                return data
                    .get(at..at + len as usize)
                    .ok_or_else(|| format!("{address:#x} is not in the file"));
            }
        }
        Err(format!("{address:#x}+{len:#x} is in no section"))
    }

    fn u32(&self, address: u64) -> Result<u32, String> {
        let b = self.bytes(address, 4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn word(&self, address: u64) -> Result<u64, String> {
        let b = self.bytes(address, self.pointer)?;
        let mut word = [0u8; 8];
        word[..b.len()].copy_from_slice(b);
        Ok(u64::from_le_bytes(word))
    }

    /// A `&str` at `address`.
    fn str(&self, address: u64) -> Result<String, String> {
        let (ptr, len) = (self.word(address)?, self.word(address + self.pointer)?);
        let bytes = self.bytes(ptr, len)?;
        String::from_utf8(bytes.to_vec()).map_err(|_| format!("string at {ptr:#x} is not UTF-8"))
    }

    fn meta(&self, address: u64) -> Result<OptionSchema, String> {
        let p = self.pointer;
        let field = |i: u64| self.str(address + i * 2 * p);
        let kind = address + 8 * p;
        let union = kind + union_offset(p);
        let kind = match self.u32(kind)? {
            0 => OptionKind::Bool,
            1 => {
                let (ptr, len) = (self.word(union)?, self.word(union + p)?);
                let choice = choice_size(p);
                let label = union_offset(p);
                OptionKind::Enum(
                    (0..len)
                        .map(|i| {
                            let at = ptr + i * choice;
                            Ok(OptionChoice {
                                value: self.u32(at)?,
                                label: self.str(at + label)?,
                            })
                        })
                        .collect::<Result<_, String>>()?,
                )
            }
            2 => OptionKind::Number {
                min: self.u32(union)?,
                max: self.u32(union + 4)?,
                step: self.u32(union + 8)?,
            },
            tag => return Err(format!("option record at {address:#x}: kind {tag}")),
        };
        Ok(OptionSchema {
            key: field(0)?,
            form: field(1)?,
            label: field(2)?,
            help: field(3)?,
            kind,
            default: None,
            availability: OptionAvailability::Runtime,
        })
    }

    fn record(&self, address: u64) -> Result<OptionSchema, String> {
        let p = self.pointer;
        let mut option = self.meta(address)?;
        let baseline = address + meta_size(p);
        option.default = match self.u32(baseline)? {
            0 => Some(self.u32(baseline + 4)?),
            1 => None,
            tag => return Err(format!("option record at {address:#x}: baseline {tag}")),
        };
        let offered = address + offered_offset(p);
        option.availability = match self.u32(offered)? {
            0 => OptionAvailability::Editable,
            1 => OptionAvailability::Hidden(self.str(offered + union_offset(p))?),
            2 => OptionAvailability::Runtime,
            tag => return Err(format!("option record at {address:#x}: offered {tag}")),
        };
        Ok(option)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        OptionAvailability, OptionKind, OptionSchema, choice_size, kind_size, meta_size,
        offered_offset, record_size, schema, union_offset,
    };
    use core::mem::{offset_of, size_of};
    use fstart_options::{Baseline, Choice, Kind, Meta, Offered, Record};

    /// The reader's layout is the host's `repr(C)` one (64-bit); 32-bit
    /// stages follow the same rules with 4-byte pointers.
    fn option(key: &str, default: Option<u32>, availability: OptionAvailability) -> OptionSchema {
        OptionSchema {
            key: key.into(),
            form: "f".into(),
            label: "l".into(),
            help: "h".into(),
            kind: OptionKind::Bool,
            default,
            availability,
        }
    }

    #[test]
    fn the_last_stage_has_every_option() {
        use OptionAvailability::{Editable, Hidden};
        let level = option("debug_level", Some(6), Editable);
        let panel = option("tft_brightness", Some(255), Hidden("no panel".into()));
        let last = vec![level.clone(), panel, option("wlan", Some(1), Editable)];
        let options = schema(vec![vec![level.clone()], vec![], last.clone()]).unwrap();
        let keys: Vec<_> = options.iter().map(|o| o.key.as_str()).collect();
        assert_eq!(keys, ["debug_level", "wlan"], "hidden options left out");
        assert!(
            schema(vec![
                vec![option("sata_mode", Some(0), Editable)],
                last.clone()
            ])
            .is_err()
        );
        assert!(schema(vec![vec![option("debug_level", Some(7), Editable)], last]).is_err());
    }

    #[test]
    fn record_layout_matches_the_options_crate() {
        let p = size_of::<usize>() as u64;
        assert_eq!(meta_size(p), size_of::<Meta>() as u64);
        assert_eq!(kind_size(p), size_of::<Kind>() as u64);
        assert_eq!(8 * p, offset_of!(Meta, kind) as u64);
        assert_eq!(choice_size(p), size_of::<Choice>() as u64);
        assert_eq!(union_offset(p), offset_of!(Choice, label) as u64);
        assert_eq!(record_size(p), size_of::<Record>() as u64);
        // The tags and values where the reader looks for them.
        let record = Record::__new(
            Meta::bool("k", "f", "l", "h"),
            Baseline::Known(0x1234_5678),
            Offered::Hidden("why"),
        );
        // SAFETY: `Record` is `repr(C)` and fully initialized here.
        let bytes = unsafe {
            core::slice::from_raw_parts((&raw const record).cast::<u8>(), size_of::<Record>())
        };
        let u32_at = |at: u64| u32::from_le_bytes(bytes[at as usize..][..4].try_into().unwrap());
        assert_eq!(u32_at(meta_size(p)), 0, "Baseline::Known");
        assert_eq!(u32_at(meta_size(p) + 4), 0x1234_5678);
        assert_eq!(u32_at(offered_offset(p)), 1, "Offered::Hidden");
        // i686 stages: 68-byte records, 48-byte metas, 12-byte choices.
        assert_eq!((record_size(4), meta_size(4), choice_size(4)), (68, 48, 12));
    }
}
