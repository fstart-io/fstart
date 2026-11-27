//! A [`Report`] as a coreboot CFR (Coreboot Form Representation) record.
//!
//! The record is `lb_cfr` of coreboot's `commonlib/cfr.h`, the format its
//! payloads' setup menus parse: one form per [`Meta::form`], in the order
//! the forms first appear, holding the forms' knobs. Each option's default
//! is the board's value ([`Setting::default`]), so a menu that finds no
//! stored value shows what the board would boot with. A knob that is
//! [`Availability::Inactive`] is greyed out with its reason; a stored value
//! that was not applied says why in the help text.
//!
//! [`Meta::form`]: crate::Meta::form

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;
use zerocopy::little_endian::{U32, U64};
use zerocopy::{Immutable, IntoBytes};

use crate::{Availability, Kind, Provenance, Rejection, Report, Setting, SourceError};

#[repr(C)]
#[derive(Clone, Copy, IntoBytes, Immutable)]
struct CfrRootHeader {
    tag: U32,
    size: U32,
    version: U32,
    checksum: U32,
}

#[repr(C)]
#[derive(Clone, Copy, IntoBytes, Immutable)]
struct CfrRecordPrefix {
    tag: U32,
    size: U32,
}

#[repr(C)]
#[derive(Clone, Copy, IntoBytes, Immutable)]
struct CfrFormHeader {
    object_id: U64,
    dependency_id: U64,
    flags: U32,
}

#[repr(C)]
#[derive(Clone, Copy, IntoBytes, Immutable)]
struct CfrOptionHeader {
    object_id: U64,
    dependency_id: U64,
    flags: U32,
    default_value: U32,
    min: U32,
    max: U32,
    step: U32,
    display_flags: U32,
}

#[repr(C)]
#[derive(Clone, Copy, IntoBytes, Immutable)]
struct CfrVarcharPrefix {
    tag: U32,
    size: U32,
    data_length: U32,
}

const _: () = assert!(core::mem::size_of::<CfrRootHeader>() == 16);
const _: () = assert!(core::mem::size_of::<CfrRecordPrefix>() == 8);
const _: () = assert!(core::mem::size_of::<CfrFormHeader>() == 20);
const _: () = assert!(core::mem::size_of::<CfrOptionHeader>() == 40);
const _: () = assert!(core::mem::size_of::<CfrVarcharPrefix>() == 12);

/// Coreboot table tag of the CFR root record.
pub const TAG_ROOT: u32 = 0x47;
/// CFR format version.
pub const VERSION: u32 = 0;

const TAG_FORM: u32 = 1;
const TAG_ENUM_VALUE: u32 = 2;
const TAG_ENUM: u32 = 3;
const TAG_NUMBER: u32 = 4;
const TAG_BOOL: u32 = 5;
const TAG_OPT_NAME: u32 = 7;
const TAG_UI_NAME: u32 = 8;
const TAG_UI_HELPTEXT: u32 = 9;

/// Option flag: shown, not editable.
pub const FLAG_INACTIVE: u32 = 1 << 1;

/// The CFR record of `report`, with its root header.
pub fn encode(report: &Report) -> Vec<u8> {
    let mut forms: Vec<&'static str> = Vec::new();
    for setting in &report.settings {
        if !forms.contains(&setting.meta.form) {
            forms.push(setting.meta.form);
        }
    }
    let mut ids = 0u64;
    let mut next_id = || {
        ids += 1;
        ids
    };
    let mut body = Vec::new();
    for form in forms {
        let mut children = Vec::new();
        varchar(&mut children, TAG_UI_NAME, form);
        for setting in report.settings.iter().filter(|s| s.meta.form == form) {
            option(&mut children, setting, next_id());
        }
        let header = CfrFormHeader {
            object_id: U64::new(next_id()),
            dependency_id: U64::ZERO,
            flags: U32::ZERO,
        };
        record(&mut body, TAG_FORM, header.as_bytes(), &children);
    }
    let root = CfrRootHeader {
        tag: U32::new(TAG_ROOT),
        size: U32::new((core::mem::size_of::<CfrRootHeader>() + body.len()) as u32),
        version: U32::new(VERSION),
        checksum: U32::new(crc32(&body)),
    };
    let mut out = Vec::with_capacity(core::mem::size_of::<CfrRootHeader>() + body.len());
    out.extend_from_slice(root.as_bytes());
    out.extend_from_slice(&body);
    out
}

fn option(out: &mut Vec<u8>, setting: &Setting, id: u64) {
    let meta = setting.meta;
    let (tag, min, max, step) = match meta.kind {
        Kind::Bool => (TAG_BOOL, 0, 1, 1),
        Kind::Enum(_) => (TAG_ENUM, 0, 0, 0),
        Kind::Number { min, max, step } => (TAG_NUMBER, min, max, step.max(1)),
    };
    let flags = match setting.availability {
        Availability::Editable => 0,
        Availability::Inactive(_) => FLAG_INACTIVE,
    };
    let header = CfrOptionHeader {
        object_id: U64::new(id),
        dependency_id: U64::ZERO,
        flags: U32::new(flags),
        default_value: U32::new(setting.default),
        min: U32::new(min),
        max: U32::new(max),
        step: U32::new(step),
        display_flags: U32::ZERO,
    };
    let mut children = Vec::new();
    varchar(&mut children, TAG_OPT_NAME, meta.key);
    varchar(&mut children, TAG_UI_NAME, meta.label);
    varchar(&mut children, TAG_UI_HELPTEXT, &help(setting));
    if let Kind::Enum(choices) = meta.kind {
        for choice in choices {
            let mut name = Vec::new();
            varchar(&mut name, TAG_UI_NAME, choice.label);
            record(
                &mut children,
                TAG_ENUM_VALUE,
                U32::new(choice.value).as_bytes(),
                &name,
            );
        }
    }
    record(out, tag, header.as_bytes(), &children);
}

/// The knob's help, with why it is greyed out or its stored value unused.
fn help(setting: &Setting) -> String {
    let mut help = String::from(setting.meta.help);
    if let Availability::Inactive(why) = setting.availability {
        let _ = write!(help, " Unavailable: {why}.");
    }
    if let Provenance::Rejected { stored, why } = setting.provenance {
        let _ = write!(help, " Stored value {stored} not applied this boot: ");
        let _ = match why {
            Rejection::OutOfRange => help.write_str("not a valid value."),
            Rejection::Inactive => help.write_str("the option is unavailable."),
            Rejection::Source(SourceError::Unavailable) => help.write_str("no settings store."),
            Rejection::Source(SourceError::Corrupt) => help.write_str("the store is corrupt."),
            Rejection::Conflict(why) => write!(help, "{why}."),
        };
    }
    help
}

/// A record: tag, size, the fixed header fields after them, children.
fn record(out: &mut Vec<u8>, tag: u32, header: &[u8], children: &[u8]) {
    let prefix = CfrRecordPrefix {
        tag: U32::new(tag),
        size: U32::new(
            (core::mem::size_of::<CfrRecordPrefix>() + header.len() + children.len()) as u32,
        ),
    };
    out.extend_from_slice(prefix.as_bytes());
    out.extend_from_slice(header);
    out.extend_from_slice(children);
}

/// A NUL-terminated string record, padded to 4 bytes as coreboot writes it.
fn varchar(out: &mut Vec<u8>, tag: u32, text: &str) {
    let length = text.len() + 1;
    let padded = length.next_multiple_of(4);
    let prefix = CfrVarcharPrefix {
        tag: U32::new(tag),
        size: U32::new((core::mem::size_of::<CfrVarcharPrefix>() + padded) as u32),
        data_length: U32::new(length as u32),
    };
    out.extend_from_slice(prefix.as_bytes());
    out.extend_from_slice(text.as_bytes());
    out.resize(out.len() + padded - text.len(), 0);
}

/// coreboot's CRC32 of the CFR body: MSB first, polynomial `0x04c11db7`,
/// initial value and final XOR 0.
pub fn crc32(data: &[u8]) -> u32 {
    data.iter().fold(0u32, |crc, &byte| {
        (0..8).fold(crc ^ (u32::from(byte) << 24), |crc, _| {
            if crc & 0x8000_0000 != 0 {
                (crc << 1) ^ 0x04c1_1db7
            } else {
                crc << 1
            }
        })
    })
}
