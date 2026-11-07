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
//! - [`resolve_each`] applies the stored values to a configuration, runs
//!   the consumer's whole-set validation and drops every override of the
//!   set if it fails, so a bad combination never stops the boot. It hands
//!   each knob's outcome ([`Setting`]) to the caller and allocates nothing:
//!   pre-DRAM stages resolve their knobs with it.
//! - With the `alloc` feature, [`resolve_checked`] keeps the outcomes in a
//!   [`Report`], and [`cfr`] describes a report as a coreboot CFR form,
//!   which setup menus (CrabEFI, edk2) render and store under
//!   [`Meta::key`].
//! - `varstore` (feature) reads options from an EDK2 variable store.
//! - [`cmos`] keeps overrides in a window of CMOS RAM, by key, without a
//!   layout; [`Layered`] reads one backend before another.
//!
//! Values are `u32`, as coreboot's `get_uint_option()` and CFR store them:
//! a bool is 0 or 1, an enum value is its [`Choice::value`].

#![no_std]

#[cfg(feature = "alloc")]
extern crate alloc;

#[cfg(feature = "alloc")]
pub mod cfr;
pub mod cmos;
#[cfg(all(test, feature = "alloc"))]
mod tests;
#[cfg(feature = "varstore")]
pub mod varstore;

#[cfg(feature = "alloc")]
use alloc::vec::Vec;

/// What a knob is, independent of the configuration it changes.
///
/// `repr(C)`: the build reads the knobs a stage links from its ELF
/// ([`meta!`]), so tools see the options of a build without its source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
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
#[repr(C, u32)]
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
#[repr(C)]
pub struct Choice {
    pub value: u32,
    pub label: &'static str,
}

impl Choice {
    pub const fn new(value: u32, label: &'static str) -> Self {
        Self { value, label }
    }
}

/// Whether the running machine has a knob.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Availability {
    Editable,
    /// The machine does not have what the knob changes (no panel, a fused
    /// off group): menus and tools leave it out, a stored value is ignored.
    Hidden(&'static str),
}

/// What the build knows of an option's value before the machine runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C, u32)]
pub enum Baseline {
    /// The board's value with no override: its configuration is a constant.
    Known(u32),
    /// The board detects its configuration at boot.
    Runtime,
}

/// What the build knows of whether a machine offers an option.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C, u32)]
pub enum Offered {
    Editable,
    Hidden(&'static str),
    /// Decided at boot, from what the board detects.
    Runtime,
}

impl Offered {
    pub const fn of(availability: Availability) -> Self {
        match availability {
            Availability::Editable => Self::Editable,
            Availability::Hidden(why) => Self::Hidden(why),
        }
    }
}

/// A knob as the build records it: its [`Meta`] and, where the knob meets a
/// constant configuration ([`option_set!`]), its board value and whether
/// the board offers it. Records sit in `.rodata.fstart.options.*` sections
/// that the stage's knobs reference, so the linker keeps exactly the records
/// of the knobs a stage resolves; fstart's linker scripts collect them
/// between `_fstart_options_start` and `_fstart_options_end`, and image
/// tools read a build's options from there. `repr(C)`, with `meta` first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub struct Record {
    meta: Meta,
    baseline: Baseline,
    offered: Offered,
}

impl Record {
    /// Only the macros make records, so every knob has one in the options
    /// section.
    #[doc(hidden)]
    pub const fn __new(meta: Meta, baseline: Baseline, offered: Offered) -> Self {
        Self {
            meta,
            baseline,
            offered,
        }
    }

    pub const fn meta(&self) -> &Meta {
        &self.meta
    }

    pub const fn baseline(&self) -> Baseline {
        self.baseline
    }

    pub const fn offered(&self) -> Offered {
        self.offered
    }
}

/// A knob's [`Meta`] as a [`Record`] without board facts: for knobs whose
/// configuration the build cannot see.
///
/// ```
/// use fstart_options::{Kind, Knob, Meta, meta};
/// static NMI: Knob<bool> = Knob::new(
///     meta!(Meta::bool("nmi", "Advanced", "NMI sources", "NMIs on SERR#.")),
///     |c| (*c).into(),
///     |c, v| *c = v != 0,
/// );
/// assert_eq!(NMI.meta.kind, Kind::Bool);
/// ```
#[macro_export]
macro_rules! meta {
    ($meta:expr) => {{
        #[unsafe(link_section = $crate::__meta_section!())]
        static RECORD: $crate::Record =
            $crate::Record::__new($meta, $crate::Baseline::Runtime, $crate::Offered::Runtime);
        &RECORD
    }};
}

/// The section of a [`Record`]: unique per invocation, so the linker
/// drops each unused one on its own.
#[doc(hidden)]
#[macro_export]
macro_rules! __meta_section {
    () => {
        concat!(
            ".rodata.fstart.options.",
            module_path!(),
            ".",
            line!(),
            ".",
            column!()
        )
    };
}

/// Define a knob as a type whose accessors are `const fn`, so a set can
/// record the board value and availability at build time
/// ([`option_set!`]):
///
/// ```
/// use fstart_options::{Meta, knob};
/// pub struct Options { pub wlan: bool }
/// knob! {
///     /// The WLAN radio.
///     pub Wlan: Options = Meta::bool("wlan", "Radios", "WLAN", "The WLAN card's radio.");
///     get(o) => o.wlan as u32;
///     set(o, v) => o.wlan = v != 0;
/// }
/// assert_eq!(Wlan::get(&Options { wlan: true }), 1);
/// ```
///
/// `available(o) => ...;` is optional (always editable).
#[macro_export]
macro_rules! knob {
    (
        $(#[$attr:meta])*
        $vis:vis $name:ident: $config:ty = $meta:expr;
        get($g:ident) => $get:expr;
        set($s:ident, $v:ident) => $set:expr;
        $(available($a:ident) => $available:expr;)?
    ) => {
        $(#[$attr])*
        $vis struct $name;

        impl $name {
            pub const META: $crate::Meta = $meta;

            pub const fn get($g: &$config) -> u32 {
                $get
            }

            pub fn set($s: &mut $config, $v: u32) {
                $set;
            }

            #[allow(unused_variables)]
            pub const fn available(config: &$config) -> $crate::Availability {
                $(
                    let $a = config;
                    return $available;
                )?
                #[allow(unreachable_code)]
                $crate::Availability::Editable
            }

            /// The runtime knob of a set whose records a board writes
            /// elsewhere (the chipset's, at its entry point).
            pub const KNOB: $crate::Knob<$config> = $crate::Knob::__unrecorded(
                &Self::META,
                Self::get,
                Self::set,
                Self::available,
            );

            /// The runtime knob `index` of a set with the records `set`.
            pub const fn knob(set: &'static [$crate::Record], index: usize) -> $crate::Knob<$config> {
                $crate::Knob::__new(set, index, Self::get, Self::set, Self::available)
            }
        }
    };
}

/// The knobs of a configuration `C` as one static array, with a [`Record`]
/// per knob. With `C = CONST`, the records carry each knob's board value
/// and availability, evaluated at build time; with `C = CONST; offered at
/// runtime`, the value only (the board detects what it offers); with
/// `C = runtime`, the configuration is detected at boot and the records
/// say so.
///
/// ```
/// use fstart_options::{Meta, knob, option_set};
/// pub struct Options { pub wlan: bool }
/// knob! {
///     pub Wlan: Options = Meta::bool("wlan", "Radios", "WLAN", "The WLAN card's radio.");
///     get(o) => o.wlan as u32;
///     set(o, v) => o.wlan = v != 0;
/// }
/// option_set!(pub static KNOBS: [Options = Options { wlan: true }] = [Wlan]);
/// assert_eq!(KNOBS.len(), 1);
/// ```
#[macro_export]
macro_rules! option_set {
    ($(#[$attr:meta])* $vis:vis static $name:ident: [$config:ty = runtime] = [$($knob:path),+ $(,)?]) => {
        $crate::__option_set!($(#[$attr])* $vis $name: $config; [$($knob),+];
            [$($crate::Record::__new(
                <$knob>::META,
                $crate::Baseline::Runtime,
                $crate::Offered::Runtime,
            )),+]);
    };
    ($(#[$attr:meta])* $vis:vis static $name:ident: [$config:ty = $default:expr; offered at runtime] = [$($knob:path),+ $(,)?]) => {
        $crate::__option_set!($(#[$attr])* $vis $name: $config; [$($knob),+];
            [$($crate::Record::__new(
                <$knob>::META,
                $crate::Baseline::Known(<$knob>::get(&{ const DEFAULT: $config = $default; DEFAULT })),
                $crate::Offered::Runtime,
            )),+]);
    };
    ($(#[$attr:meta])* $vis:vis static $name:ident: [$config:ty = $default:expr] = [$($knob:path),+ $(,)?]) => {
        $crate::__option_set!($(#[$attr])* $vis $name: $config; [$($knob),+];
            [$($crate::record!($knob, &{ const DEFAULT: $config = $default; DEFAULT })),+]);
    };
}

/// A set's records as one ordered array in one section, and its knobs.
#[doc(hidden)]
#[macro_export]
macro_rules! __option_set {
    ($(#[$attr:meta])* $vis:vis $name:ident: $config:ty; [$($knob:path),+]; [$($record:expr),+]) => {
        $(#[$attr])*
        $vis static $name: [$crate::Knob<$config>; $crate::__count!($($knob)+)] = {
            #[unsafe(link_section = $crate::__meta_section!())]
            static RECORDS: [$crate::Record; $crate::__count!($($knob)+)] = [$($record),+];
            $crate::__knobs!(RECORDS; []; []; $($knob),+)
        };
    };
}

/// `[K0::knob(&RECORDS, 0), K1::knob(&RECORDS, 1), ..]`.
#[doc(hidden)]
#[macro_export]
macro_rules! __knobs {
    ($records:ident; [$($done:expr),*]; [$($prev:tt)*]; $head:path $(, $tail:path)*) => {
        $crate::__knobs!($records; [$($done,)* <$head>::knob(&$records, $crate::__count!($($prev)*))]; [$($prev)* x]; $($tail),*)
    };
    ($records:ident; [$($done:expr),*]; [$($prev:tt)*];) => {
        [$($done),*]
    };
}

/// The [`Record`] of a [`knob!`] type over a constant configuration:
/// its board value and availability, evaluated at build time.
#[macro_export]
macro_rules! record {
    ($knob:path, $config:expr) => {
        $crate::Record::__new(
            <$knob>::META,
            $crate::Baseline::Known(<$knob>::get($config)),
            $crate::Offered::of(<$knob>::available($config)),
        )
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __count {
    () => { 0 };
    ($head:tt $($tail:tt)*) => { 1 + $crate::__count!($($tail)*) };
}

/// One overridable field of a configuration `C`, made by [`knob!`],
/// [`option_set!`] or over a [`meta!`] record.
pub struct Knob<C: 'static> {
    pub meta: &'static Meta,
    /// The field's value.
    pub get: fn(&C) -> u32,
    /// Set the field; only called with a value of [`Meta::kind`].
    pub set: fn(&mut C, u32),
    /// Whether this machine, as `C` describes it, has the knob.
    pub available: fn(&C) -> Availability,
    /// The records of the knob's set. Only [`Record`]s make knobs, and a
    /// reference to the whole array keeps the set's records in order: LLVM
    /// splits an array it only sees indexed by constants.
    _set: &'static [Record],
}

impl<C> Knob<C> {
    /// An always available knob described by `record` ([`meta!`]).
    pub const fn new(record: &'static Record, get: fn(&C) -> u32, set: fn(&mut C, u32)) -> Self {
        Self::__new(core::slice::from_ref(record), 0, get, set, |_| {
            Availability::Editable
        })
    }

    /// Knob `index` of the set whose records are `set`.
    #[doc(hidden)]
    pub const fn __new(
        set: &'static [Record],
        index: usize,
        get: fn(&C) -> u32,
        set_value: fn(&mut C, u32),
        available: fn(&C) -> Availability,
    ) -> Self {
        Self {
            meta: set[index].meta(),
            get,
            set: set_value,
            available,
            _set: set,
        }
    }

    /// A knob without a record: its set's records are written elsewhere
    /// (the Intel chipset's, at the board's entry point).
    #[doc(hidden)]
    pub const fn __unrecorded(
        meta: &'static Meta,
        get: fn(&C) -> u32,
        set_value: fn(&mut C, u32),
        available: fn(&C) -> Availability,
    ) -> Self {
        Self {
            meta,
            get,
            set: set_value,
            available,
            _set: &[],
        }
    }

    /// The knob, available as `available` decides.
    pub const fn when(self, available: fn(&C) -> Availability) -> Self {
        Self { available, ..self }
    }
}

/// The first key two of `metas` share, for a board test over every knob
/// set it offers.
pub fn duplicate_key<'a, I>(metas: I) -> Option<&'static str>
where
    I: IntoIterator<Item = &'a Meta> + Clone,
{
    for (i, meta) in metas.clone().into_iter().enumerate() {
        if metas.clone().into_iter().take(i).any(|m| m.key == meta.key) {
            return Some(meta.key);
        }
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
    /// The machine does not have the knob ([`Availability::Hidden`]).
    Hidden,
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

/// Apply the stored value of one knob to `config`.
pub fn resolve_knob<C: 'static>(
    knob: &'static Knob<C>,
    source: &dyn Source,
    config: &mut C,
) -> Setting {
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
            why: Rejection::Hidden,
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
    Setting {
        meta: knob.meta,
        default,
        value: (knob.get)(config),
        provenance,
        availability,
    }
}

/// Apply the stored overrides of `knobs` to `config`, then `check` the
/// whole configuration: if it fails, the configuration is the board's
/// again and every override is rejected with the check's reason. Each
/// knob's outcome goes to `emit`, in order, once the set is decided. A
/// failing source rejects only the knob it was read for. Allocates
/// nothing; the source is read twice.
pub fn resolve_each<C, K>(
    knobs: K,
    source: &dyn Source,
    config: &mut C,
    check: impl FnOnce(&C) -> Result<(), &'static str>,
    mut emit: impl FnMut(Setting),
) where
    C: Clone + 'static,
    K: IntoIterator<Item = &'static Knob<C>> + Clone,
{
    let board = config.clone();
    // Every knob, not until the first override.
    let mut applied = false;
    for knob in knobs.clone() {
        applied |= resolve_knob(knob, source, config).provenance == Provenance::Override;
    }
    let conflict = if applied { check(config).err() } else { None };
    // The outcomes, replayed on the board's values: a knob's default and
    // availability can depend on the knobs before it.
    let mut replay = board.clone();
    for knob in knobs {
        let mut setting = resolve_knob(knob, source, &mut replay);
        if let (Some(why), Provenance::Override) = (conflict, setting.provenance) {
            setting.provenance = Provenance::Rejected {
                stored: setting.value,
                why: Rejection::Conflict(why),
            };
            setting.value = setting.default;
        }
        emit(setting);
    }
    if conflict.is_some() {
        *config = board;
    }
}

/// The outcome of every resolved knob, in resolution order.
#[cfg(feature = "alloc")]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    pub settings: Vec<Setting>,
    /// Knobs not resolved because an earlier knob has their key: two
    /// fields would share one stored value.
    pub duplicates: Vec<&'static Meta>,
}

#[cfg(feature = "alloc")]
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

/// [`resolve_checked`] without a whole-set check.
#[cfg(feature = "alloc")]
pub fn resolve<C: Clone + 'static>(
    knobs: &'static [Knob<C>],
    source: &dyn Source,
    config: &mut C,
    report: &mut Report,
) {
    resolve_checked(knobs, source, config, report, |_| Ok(()));
}

/// [`resolve_each`] into `report`. A knob whose key `report` already has,
/// or an earlier knob of `knobs` has, is left at the board's value and
/// listed in [`Report::duplicates`].
#[cfg(feature = "alloc")]
pub fn resolve_checked<C: Clone + 'static>(
    knobs: &'static [Knob<C>],
    source: &dyn Source,
    config: &mut C,
    report: &mut Report,
    check: impl FnOnce(&C) -> Result<(), &'static str>,
) {
    let mut fresh: Vec<&'static Knob<C>> = Vec::new();
    for knob in knobs {
        let key = knob.meta.key;
        if report.settings.iter().any(|s| s.meta.key == key)
            || fresh.iter().any(|k| k.meta.key == key)
        {
            report.duplicates.push(knob.meta);
        } else {
            fresh.push(knob);
        }
    }
    resolve_each(fresh.iter().copied(), source, config, check, |setting| {
        report.settings.push(setting)
    });
}
