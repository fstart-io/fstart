//! A [`Report`] as a coreboot CFR (Coreboot Form Representation) record.
//!
//! The record is `lb_cfr` of coreboot's `commonlib/cfr.h`, the format its
//! payloads' setup menus parse: one form per [`Meta::form`], in the order
//! the forms first appear, holding the forms' knobs. Each option's default
//! is the board's value ([`Setting::default`]), so a menu that finds no
//! stored value shows what the board would boot with. A knob the machine
//! does not have ([`Availability::Hidden`]) is left out; a stored value
//! that was not applied says why in the help text.
//!
//! [`Meta::form`]: crate::Meta::form

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;

use crate::{Availability, Kind, Provenance, Rejection, Report, Setting, SourceError};

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

/// The CFR record of `report`, with its root header.
pub fn encode(report: &Report) -> Vec<u8> {
    let mut forms: Vec<&'static str> = Vec::new();
    let shown = || {
        report
            .settings
            .iter()
            .filter(|s| !matches!(s.availability, Availability::Hidden(_)))
    };
    for setting in shown() {
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
        for setting in shown().filter(|s| s.meta.form == form) {
            option(&mut children, setting, next_id());
        }
        let mut header = Vec::new();
        put_u64(&mut header, next_id());
        put_u64(&mut header, 0);
        put_u32(&mut header, 0);
        record(&mut body, TAG_FORM, &header, &children);
    }
    let mut out = Vec::with_capacity(16 + body.len());
    put_u32(&mut out, TAG_ROOT);
    put_u32(&mut out, (16 + body.len()) as u32);
    put_u32(&mut out, VERSION);
    put_u32(&mut out, crc32(&body));
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
    let flags = 0;
    let mut header = Vec::new();
    put_u64(&mut header, id);
    put_u64(&mut header, 0);
    for word in [flags, setting.default, min, max, step, 0] {
        put_u32(&mut header, word);
    }
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
                &choice.value.to_le_bytes(),
                &name,
            );
        }
    }
    record(out, tag, &header, &children);
}

/// The knob's help, with why it is greyed out or its stored value unused.
fn help(setting: &Setting) -> String {
    let mut help = String::from(setting.meta.help);
    if let Provenance::Rejected { stored, why } = setting.provenance {
        let _ = write!(help, " Stored value {stored} not applied this boot: ");
        let _ = match why {
            Rejection::OutOfRange => help.write_str("not a valid value."),
            Rejection::Hidden => help.write_str("this machine does not have it."),
            Rejection::Source(SourceError::Unavailable) => help.write_str("no settings store."),
            Rejection::Source(SourceError::Corrupt) => help.write_str("the store is corrupt."),
            Rejection::Conflict(why) => write!(help, "{why}."),
        };
    }
    help
}

/// A record: tag, size, the fixed header fields after them, children.
fn record(out: &mut Vec<u8>, tag: u32, header: &[u8], children: &[u8]) {
    put_u32(out, tag);
    put_u32(out, (8 + header.len() + children.len()) as u32);
    out.extend_from_slice(header);
    out.extend_from_slice(children);
}

/// A NUL-terminated string record, padded to 4 bytes as coreboot writes it.
fn varchar(out: &mut Vec<u8>, tag: u32, text: &str) {
    let length = text.len() + 1;
    let padded = length.next_multiple_of(4);
    put_u32(out, tag);
    put_u32(out, (12 + padded) as u32);
    put_u32(out, length as u32);
    out.extend_from_slice(text.as_bytes());
    out.resize(out.len() + padded - text.len(), 0);
}

fn put_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn put_u64(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_le_bytes());
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
