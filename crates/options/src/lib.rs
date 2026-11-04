//! User overrides of a board's configuration.
//!
//! A board's configuration is resolved in layers: the SoC defaults, the
//! board's constants, what the board detects (straps, risers, fuses), then
//! the user's overrides, then the SoC's validation of the whole set. This
//! crate is the override layer:
//!
//! - A [`Knob`] is one overridable field of a configuration type `C`: its
//!   description ([`Meta`]), how to read and write it, and whether the
//!   running machine allows changing it ([`Availability`]). The SoC code
//!   that consumes a field says which values are legal ([`Kind`]); a board
//!   picks the knobs it offers and narrows them.
//! - A [`Source`] reads stored overrides by [`Meta::key`]. Backends are
//!   anything that stores small numbers by name: a flash variable store,
//!   CMOS, a BMC, a secure-world service. Reading is all a boot needs; the
//!   writer is whatever owns the backend (the setup menu, the OS).
//! - [`resolve`] applies the stored values to a configuration and records
//!   the outcome of every knob in a [`Report`]. [`resolve_checked`] also
//!   runs the consumer's whole-set validation and drops every override of
//!   the set if it fails, so a bad combination never stops the boot.
//! - [`cfr`] describes a report as a coreboot CFR form, which setup menus
//!   (CrabEFI, edk2) render and store under [`Meta::key`].
//! - [`cmos`] keeps overrides in a window of CMOS RAM, by key, without a
//!   layout; [`Layered`] reads one backend before another.
//!
//! Values are `u32`, as coreboot's `get_uint_option()` and CFR store them:
//! a bool is 0 or 1, an enum value is its [`Choice::value`].

#![no_std]

extern crate alloc;

pub mod cfr;
pub mod cmos;
#[cfg(test)]
mod tests;

use alloc::vec::Vec;

/// What a knob is, independent of the configuration it changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Meta {
    /// Storage name: the CFR option name, the EFI variable name under the
    /// CFR GUID, the CMOS layout entry. Stable across builds; ASCII.
    pub key: &'static str,
    /// The menu form (page) the knob is shown on.
    pub form: &'static str,
    pub label: &'static str,
    pub help: &'static str,
    pub kind: Kind,
}

impl Meta {
    /// An on/off knob.
    pub const fn bool(
        key: &'static str,
        form: &'static str,
        label: &'static str,
        help: &'static str,
    ) -> Self {
        Self {
            key,
            form,
            label,
            help,
            kind: Kind::Bool,
        }
    }
}

/// The values a knob takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// 0 or 1.
    Bool,
    /// One of the listed values.
    Enum(&'static [Choice]),
    /// `min..=max` in steps of `step` from `min`.
    Number { min: u32, max: u32, step: u32 },
}

impl Kind {
    /// Whether `value` is one of the kind's values.
    pub fn contains(&self, value: u32) -> bool {
        match *self {
            Self::Bool => value <= 1,
            Self::Enum(choices) => choices.iter().any(|c| c.value == value),
            Self::Number { min, max, step } => {
                (min..=max).contains(&value) && (value - min).is_multiple_of(step.max(1))
            }
        }
    }
}

/// One value of an enum knob.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Choice {
    pub value: u32,
    pub label: &'static str,
}

impl Choice {
    pub const fn new(value: u32, label: &'static str) -> Self {
        Self { value, label }
    }
}

/// Whether the running machine lets the user change a knob.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Availability {
    Editable,
    /// Shown greyed out with the reason; a stored value is ignored.
    Inactive(&'static str),
}

/// One overridable field of a configuration `C`.
pub struct Knob<C: 'static> {
    pub meta: Meta,
    /// The field's value.
    pub get: fn(&C) -> u32,
    /// Set the field; only called with a value of [`Meta::kind`].
    pub set: fn(&mut C, u32),
    /// Whether this machine, as `C` describes it, lets the user change it.
    pub available: fn(&C) -> Availability,
}

impl<C> Knob<C> {
    /// An always available knob.
    pub const fn new(meta: Meta, get: fn(&C) -> u32, set: fn(&mut C, u32)) -> Self {
        Self {
            meta,
            get,
            set,
            available: |_| Availability::Editable,
        }
    }

    /// The knob, available as `available` decides.
    pub const fn when(self, available: fn(&C) -> Availability) -> Self {
        Self { available, ..self }
    }
}

/// The first key two of `metas` share, for a board test over every knob
/// set it offers ([`resolve`] skips the later knob at boot).
pub fn duplicate_key<'a>(metas: impl IntoIterator<Item = &'a Meta>) -> Option<&'static str> {
    let mut seen: Vec<&'static str> = Vec::new();
    for meta in metas {
        if seen.contains(&meta.key) {
            return Some(meta.key);
        }
        seen.push(meta.key);
    }
    None
}

/// Why a source could not be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceError {
    /// The backend is absent or not set up (an erased store).
    Unavailable,
    /// The stored data is malformed.
    Corrupt,
}

/// Stored overrides, by [`Meta::key`].
pub trait Source {
    /// For the log.
    fn name(&self) -> &'static str;
    /// The names of the backends read, in order: [`Self::name`], or each
    /// layer's.
    fn names(&self, each: &mut dyn FnMut(&'static str)) {
        each(self.name())
    }
    /// The stored value of `key`; `Ok(None)` when none is stored.
    fn read(&self, key: &str) -> Result<Option<u32>, SourceError>;
}

/// The source of a build without stored overrides.
pub struct NoSource;

impl Source for NoSource {
    fn name(&self) -> &'static str {
        "none"
    }
    fn read(&self, _key: &str) -> Result<Option<u32>, SourceError> {
        Ok(None)
    }
}

impl<S: Source + ?Sized> Source for &S {
    fn name(&self) -> &'static str {
        (**self).name()
    }
    fn names(&self, each: &mut dyn FnMut(&'static str)) {
        (**self).names(each)
    }
    fn read(&self, key: &str) -> Result<Option<u32>, SourceError> {
        (**self).read(key)
    }
}

/// An absent backend stores nothing.
impl<S: Source> Source for Option<S> {
    fn name(&self) -> &'static str {
        self.as_ref().map_or("no store", Source::name)
    }
    fn read(&self, key: &str) -> Result<Option<u32>, SourceError> {
        self.as_ref().map_or(Ok(None), |source| source.read(key))
    }
}

/// `0`'s values, and `1`'s for the keys `0` does not store.
pub struct Layered<A, B>(pub A, pub B);

impl<A: Source, B: Source> Source for Layered<A, B> {
    fn name(&self) -> &'static str {
        self.0.name()
    }
    fn names(&self, each: &mut dyn FnMut(&'static str)) {
        self.0.names(each);
        self.1.names(each);
    }
    fn read(&self, key: &str) -> Result<Option<u32>, SourceError> {
        match self.0.read(key)? {
            Some(value) => Ok(Some(value)),
            None => self.1.read(key),
        }
    }
}

/// Why a stored value was not applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rejection {
    /// Not one of the knob's values.
    OutOfRange,
    /// The knob is [`Availability::Inactive`] on this machine.
    Inactive,
    /// The source failed.
    Source(SourceError),
    /// The configuration with every override failed the consumer's check.
    Conflict(&'static str),
}

/// Where a knob's effective value came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provenance {
    /// The board's value: its constant or what it detected.
    Board,
    /// The stored override.
    Override,
    /// A stored value (`stored`) that was not applied.
    Rejected { stored: u32, why: Rejection },
}

/// The outcome of one knob.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Setting {
    pub meta: &'static Meta,
    /// The board's value, what the knob has without an override.
    pub default: u32,
    /// The effective value.
    pub value: u32,
    pub provenance: Provenance,
    pub availability: Availability,
}

/// The outcome of every resolved knob, in resolution order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    pub settings: Vec<Setting>,
    /// Knobs not resolved because an earlier knob has their key: two
    /// fields would share one stored value.
    pub duplicates: Vec<&'static Meta>,
}

impl Report {
    pub const fn new() -> Self {
        Self {
            settings: Vec::new(),
            duplicates: Vec::new(),
        }
    }

    /// The settings whose value is not the board's.
    pub fn overrides(&self) -> impl Iterator<Item = &Setting> {
        self.settings
            .iter()
            .filter(|s| s.provenance == Provenance::Override)
    }
}

/// Apply the stored overrides of `knobs` to `config`, appending one
/// [`Setting`] per knob to `report`. A failing source rejects only the
/// knob it was read for. A knob whose key `report` already has is left
/// at the board's value and listed in [`Report::duplicates`].
pub fn resolve<C>(
    knobs: &'static [Knob<C>],
    source: &dyn Source,
    config: &mut C,
    report: &mut Report,
) {
    for knob in knobs {
        if report.settings.iter().any(|s| s.meta.key == knob.meta.key) {
            report.duplicates.push(&knob.meta);
            continue;
        }
        let default = (knob.get)(config);
        let availability = (knob.available)(config);
        let provenance = match source.read(knob.meta.key) {
            Ok(None) => Provenance::Board,
            Ok(Some(stored)) if stored == default => Provenance::Board,
            Ok(Some(stored)) if !knob.meta.kind.contains(stored) => Provenance::Rejected {
                stored,
                why: Rejection::OutOfRange,
            },
            Ok(Some(stored)) if availability != Availability::Editable => Provenance::Rejected {
                stored,
                why: Rejection::Inactive,
            },
            Ok(Some(stored)) => {
                (knob.set)(config, stored);
                Provenance::Override
            }
            Err(error) => Provenance::Rejected {
                stored: default,
                why: Rejection::Source(error),
            },
        };
        report.settings.push(Setting {
            meta: &knob.meta,
            default,
            value: (knob.get)(config),
            provenance,
            availability,
        });
    }
}

/// [`resolve`], then `check` the whole configuration. If it fails, the
/// configuration is the board's again and every override of `knobs` is
/// rejected with the check's reason.
pub fn resolve_checked<C: Clone>(
    knobs: &'static [Knob<C>],
    source: &dyn Source,
    config: &mut C,
    report: &mut Report,
    check: impl FnOnce(&C) -> Result<(), &'static str>,
) {
    let board = config.clone();
    let first = report.settings.len();
    resolve(knobs, source, config, report);
    let applied = report.settings[first..]
        .iter()
        .any(|s| s.provenance == Provenance::Override);
    if !applied {
        return;
    }
    if let Err(why) = check(config) {
        *config = board;
        for setting in &mut report.settings[first..] {
            if setting.provenance == Provenance::Override {
                setting.provenance = Provenance::Rejected {
                    stored: setting.value,
                    why: Rejection::Conflict(why),
                };
                setting.value = setting.default;
            }
        }
    }
}
