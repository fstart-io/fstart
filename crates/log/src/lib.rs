//! Lightweight logging infrastructure for fstart firmware.
//!
//! Provides [`error!`], [`warn!`], [`info!`], [`debug!`], and [`trace!`]
//! macros backed by [`ufmt`] for code-size-efficient formatted output in
//! `no_std` environments.
//!
//! ## Setup
//!
//! Call [`init`] once after the console device is initialised:
//!
//! ```ignore
//! // In a board-owned fixed-flow console step:
//! uart0.console(&mut ctx).unwrap_or_else(|_| halt());
//! // SAFETY: uart0 lives in fstart_main() which never returns.
//! unsafe { fstart_log::init(&uart0) };
//!
//! fstart_log::info!("boot stage {} starting", stage_id);
//! fstart_log::debug!("base={} size={}", fstart_log::Hex(base), size);
//! ```
//!
//! Messages below the current [`max_level`] are discarded at runtime.
//! The default level is [`Level::Info`].
//!
//! ## Build-time verbosity
//!
//! `FSTART_LOG_LEVEL` (`off`, `error`, `warn`, `info`, `debug` or `trace`,
//! set through `cargo fbuild --log-level`) caps the verbosity at build time:
//! messages above it are compiled out, strings included, and it becomes the
//! default runtime level. Unset, everything is compiled in and the runtime
//! default stays `info`.
//!
//! ## Hex formatting
//!
//! Use the [`Hex`] wrapper to format integers as `0x`-prefixed hexadecimal:
//!
//! ```ignore
//! use fstart_log::{info, Hex};
//! info!("addr={}", Hex(0x8000_0000));
//! // Output: [INFO ] addr=0x80000000
//! ```

#![no_std]

// Re-export ufmt for code that uses `fstart_log::ufmt::uWrite` etc.
// Note: fstart_log macros use `::ufmt` paths because ufmt's own
// macros internally expand to `ufmt::` paths. This means any crate
// using fstart_log macros must also depend on `ufmt` directly.
#[doc(hidden)]
pub use ufmt;

use fstart_core::services::{Console, ServiceError};

// ---------------------------------------------------------------------------
// Level
// ---------------------------------------------------------------------------

/// Log severity level.
///
/// Levels are ordered from most severe ([`Error`](Level::Error)) to most
/// verbose ([`Trace`](Level::Trace)). A message is emitted only when its
/// level is `<=` the current [`max_level`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum Level {
    /// Unrecoverable errors.
    Error = 0,
    /// Conditions that may indicate a problem.
    Warn = 1,
    /// Normal operational messages.
    Info = 2,
    /// Verbose diagnostic output.
    Debug = 3,
    /// Very fine-grained tracing.
    Trace = 4,
}

// (No methods — tag strings are hardcoded in macros for compile-time
// constant folding.)

/// Build-time verbosity selected by `FSTART_LOG_LEVEL`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verbosity {
    /// No log output at all.
    Off,
    /// Messages up to and including this level.
    Max(Level),
}

/// `FSTART_LOG_LEVEL` of this build, if it was set.
pub const BUILD_VERBOSITY: Option<Verbosity> = match option_env!("FSTART_LOG_LEVEL") {
    None => None,
    Some(name) => Some(parse_verbosity(name)),
};

/// Levels below this `Level as u8` bound are compiled in.
const BUILD_LIMIT: u8 = match BUILD_VERBOSITY {
    None => Level::Trace as u8 + 1,
    Some(Verbosity::Off) => 0,
    Some(Verbosity::Max(level)) => level as u8 + 1,
};

const fn parse_verbosity(name: &str) -> Verbosity {
    const fn is(name: &str, expected: &str) -> bool {
        let (a, b) = (name.as_bytes(), expected.as_bytes());
        if a.len() != b.len() {
            return false;
        }
        let mut i = 0;
        while i < a.len() {
            if a[i] != b[i] {
                return false;
            }
            i += 1;
        }
        true
    }
    if is(name, "off") {
        Verbosity::Off
    } else if is(name, "error") {
        Verbosity::Max(Level::Error)
    } else if is(name, "warn") {
        Verbosity::Max(Level::Warn)
    } else if is(name, "info") {
        Verbosity::Max(Level::Info)
    } else if is(name, "debug") {
        Verbosity::Max(Level::Debug)
    } else if is(name, "trace") {
        Verbosity::Max(Level::Trace)
    } else {
        panic!("FSTART_LOG_LEVEL must be off, error, warn, info, debug or trace")
    }
}

/// Whether messages at `level` are compiled into this build.
#[doc(hidden)]
pub const fn build_enabled(level: Level) -> bool {
    (level as u8) < BUILD_LIMIT
}

// ---------------------------------------------------------------------------
// Global state
// ---------------------------------------------------------------------------

use core::cell::UnsafeCell;
use portable_atomic::{AtomicBool, Ordering};

/// Guards the console across CPUs.
///
/// Application processors log as well (CPU drivers, MP flight-plan steps) and
/// the console backend has a single transmit path, so two CPUs writing at once
/// interleave their bytes and can wedge the output. One message is written
/// while this is held; coreboot guards its console the same way.
///
/// An SMI that interrupts a CPU while it holds this lock and then logs would
/// deadlock; firmware SMI handlers use their own output path.
static CONSOLE_LOCK: AtomicBool = AtomicBool::new(false);

/// Releases [`CONSOLE_LOCK`] when the writer for one message is dropped.
struct ConsoleLockGuard;

impl ConsoleLockGuard {
    fn try_acquire() -> Result<Self, ServiceError> {
        CONSOLE_LOCK
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .map(|_| Self)
            .map_err(|_| ServiceError::IoError)
    }

    fn acquire() -> Self {
        while CONSOLE_LOCK
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            core::hint::spin_loop();
        }
        Self
    }
}

impl Drop for ConsoleLockGuard {
    fn drop(&mut self) {
        CONSOLE_LOCK.store(false, Ordering::Release);
    }
}

/// Interior-mutable cell that is `Sync` by firmware invariant.
///
/// Console and level are written once during single-threaded init, then only
/// read; concurrent *writes* to the console itself are serialised by
/// [`CONSOLE_LOCK`].
/// This replaces `static mut` which is deprecated in Rust edition 2024.
struct SyncCell<T>(UnsafeCell<T>);

// SAFETY: Firmware runs single-threaded during init; after init the
// values are only read. No concurrent mutation is possible.
unsafe impl<T> Sync for SyncCell<T> {}

impl<T> SyncCell<T> {
    const fn new(val: T) -> Self {
        Self(UnsafeCell::new(val))
    }
}

/// Global console reference, set once by [`init`].
///
/// SAFETY invariant: written exactly once during single-threaded boot
/// (before any log macro executes), then only read for the remainder of
/// execution. Firmware boot is single-core, so there are no data races.
static CONSOLE: SyncCell<Option<&'static dyn Console>> = SyncCell::new(None);

/// Debug consoles every message also goes to, like coreboot running every
/// enabled console driver: the platform adds them next to the board's
/// console when the build selects them (see [`StaticConsole::install_debug`]).
///
/// SAFETY invariant: as for [`CONSOLE`].
#[cfg(feature = "debug-consoles")]
static DEBUG: SyncCell<[Option<&'static dyn Console>; MAX_DEBUG_CONSOLES]> =
    SyncCell::new([None; MAX_DEBUG_CONSOLES]);

/// Debug consoles a stage can add next to the board's console.
#[cfg(feature = "debug-consoles")]
pub const MAX_DEBUG_CONSOLES: usize = 2;

/// The registered backends, the board's console first.
#[cfg(feature = "debug-consoles")]
#[inline]
fn consoles() -> impl Iterator<Item = &'static dyn Console> {
    // SAFETY: written once each during init, then only read.
    let (console, debug) = unsafe { (*CONSOLE.0.get(), *DEBUG.0.get()) };
    core::iter::once(console).chain(debug).flatten()
}

#[cfg(not(feature = "debug-consoles"))]
#[inline]
fn consoles() -> impl Iterator<Item = &'static dyn Console> {
    // SAFETY: written once during single-threaded init, then only read.
    core::iter::once(unsafe { *CONSOLE.0.get() }).flatten()
}

/// Raw payload console for builds opting into firmware debug backends.
/// Output succeeds if any backend accepts it; otherwise its error is returned.
/// Input has fixed board-first priority, falling back past idle/failed backends.
///
/// Each call is serialized with firmware log records. Reentrant or concurrent
/// calls return `IoError` instead of waiting: a backend can itself cause payload
/// logging. This applies identically with and without `debug-consoles`.
pub struct ConsoleMux;

fn mux_output(
    mut operation: impl FnMut(&dyn Console) -> Result<(), ServiceError>,
) -> Result<(), ServiceError> {
    let _guard = ConsoleLockGuard::try_acquire()?;
    let mut error = ServiceError::NotInitialized;
    let mut accepted = false;
    for console in consoles() {
        match timed(|| operation(console)) {
            Ok(()) => accepted = true,
            Err(failure) => error = failure,
        }
    }
    if accepted { Ok(()) } else { Err(error) }
}

impl Console for ConsoleMux {
    fn write_byte(&self, byte: u8) -> Result<(), ServiceError> {
        mux_output(|console| console.write_byte(byte))
    }

    fn write_bytes(&self, bytes: &[u8]) -> Result<(), ServiceError> {
        mux_output(|console| console.write_bytes(bytes))
    }

    fn read_byte(&self) -> Result<Option<u8>, ServiceError> {
        let _guard = ConsoleLockGuard::try_acquire()?;
        let mut error = ServiceError::NotInitialized;
        let mut idle = false;
        for console in consoles() {
            match console.read_byte() {
                Ok(Some(byte)) => return Ok(Some(byte)),
                Ok(None) => idle = true,
                Err(failure) => error = failure,
            }
        }
        if idle { Ok(None) } else { Err(error) }
    }

    fn flush(&self) -> Result<(), ServiceError> {
        mux_output(|console| console.flush())
    }
}

/// Current maximum log level; the build-time level when one was set.
static MAX_LEVEL: SyncCell<Level> = SyncCell::new(match BUILD_VERBOSITY {
    Some(Verbosity::Max(level)) => level,
    Some(Verbosity::Off) => Level::Error,
    None => Level::Info,
});

/// Register the global console backend for log macros.
///
/// # Safety
///
/// The caller must ensure that `console` outlives all subsequent log calls.
/// In fstart firmware this is guaranteed: the console device lives in
/// `fstart_main()` which never returns (it halts or jumps to a payload).
///
/// Must be called exactly once, before any log macro is used. A second
/// call is silently ignored (the first console wins).
pub unsafe fn init(console: &dyn Console) {
    // SAFETY: single-threaded boot — no concurrent access; the caller
    // guarantees the pointee lives for the rest of execution.
    unsafe {
        let slot = &mut *CONSOLE.0.get();
        if slot.is_some() {
            // Already initialised — silently ignore (guard against double init).
            return;
        }
        *slot = Some(core::mem::transmute::<&dyn Console, &'static dyn Console>(
            console,
        ));
    }
}

/// Storage for a stage's console, owned by the stage's `main`.
///
/// The log backend keeps a `'static` reference, so the console must not live
/// in a value that moves. A stage declares one of these as a `static` and
/// hands it to the flow that brings the console up:
///
/// ```ignore
/// static CONSOLE: fstart_log::StaticConsole<Ns16550> = fstart_log::StaticConsole::new();
/// ```
pub struct StaticConsole<C> {
    slot: SyncCell<Option<C>>,
    taken: AtomicBool,
}

impl<C: Console + 'static> StaticConsole<C> {
    pub const fn new() -> Self {
        Self {
            slot: SyncCell::new(None),
            taken: AtomicBool::new(false),
        }
    }

    /// Store `console` and make it the log backend. A second call keeps the
    /// first console and drops `console`.
    pub fn install(&'static self, console: C) -> &'static C {
        if !self.taken.swap(true, Ordering::AcqRel) {
            // SAFETY: the flag admits exactly one writer, before any reader.
            unsafe { *self.slot.0.get() = Some(console) };
        }
        // SAFETY: written once above (or by the first call) and never again;
        // the cell is 'static. No panic path: SRAM stages cannot afford the
        // formatting machinery.
        let console = unsafe { (*self.slot.0.get()).as_ref().unwrap_unchecked() };
        // SAFETY: the console is 'static and never moves.
        unsafe { init(console) };
        console
    }

    /// Store `console` and add it as a debug console next to the board's
    /// console (see [`DEBUG`]). A second call keeps the first console; past
    /// [`MAX_DEBUG_CONSOLES`] it is stored but not added.
    #[cfg(feature = "debug-consoles")]
    pub fn install_debug(&'static self, console: C) -> &'static C {
        let first = !self.taken.swap(true, Ordering::AcqRel);
        if first {
            // SAFETY: the flag admits exactly one writer, before any reader.
            unsafe { *self.slot.0.get() = Some(console) };
        }
        // SAFETY: as in install().
        let console = unsafe { (*self.slot.0.get()).as_ref().unwrap_unchecked() };
        if first {
            // SAFETY: single-threaded boot; the console is 'static and never
            // moves.
            unsafe {
                if let Some(free) = (*DEBUG.0.get()).iter_mut().find(|slot| slot.is_none()) {
                    *free = Some(console);
                }
            }
        }
        console
    }

    /// The installed console, if any.
    pub fn get(&'static self) -> Option<&'static C> {
        if !self.taken.load(Ordering::Acquire) {
            return None;
        }
        // SAFETY: written once before `taken` was observed set.
        unsafe { (*self.slot.0.get()).as_ref() }
    }
}

impl<C: Console + 'static> Default for StaticConsole<C> {
    fn default() -> Self {
        Self::new()
    }
}

/// Replace the global console backend.
///
/// # Safety
///
/// The caller must ensure that `console` outlives all subsequent log calls.
/// This is intended for x86 post-CAR transition, where the previous console
/// object lived in Cache-as-RAM that is about to be torn down.
pub unsafe fn replace_console(console: &'static dyn Console) {
    // SAFETY: single-threaded boot — no concurrent access.
    unsafe { *CONSOLE.0.get() = Some(console) };
}

/// Set the maximum log level. Messages above this level are discarded.
///
/// # Safety
///
/// Must not be called concurrently with log macros. In practice, call
/// this immediately after [`init`] during single-threaded boot.
pub unsafe fn set_max_level(level: Level) {
    // SAFETY: single-threaded boot — no concurrent access.
    unsafe { *MAX_LEVEL.0.get() = level };
}

/// Return the current maximum log level.
#[inline]
pub fn max_level() -> Level {
    // SAFETY: MAX_LEVEL is only written during single-threaded init.
    unsafe { *MAX_LEVEL.0.get() }
}

/// User options (`fstart_options`): their logged resolution, and the
/// console log level as an option under coreboot's name and values (`debug_level`: 3 error, 4 warning, 6 info,
/// 7 debug, 8 spew), so a store coreboot shares reads the same. A level
/// above the build's `FSTART_LOG_LEVEL` prints nothing more: those
/// messages are not compiled in.
pub mod option {
    use super::Level;
    use fstart_options::{Choice, Kind, Knob, Meta, Provenance, Rejection, Setting, Source};

    const CHOICES: &[Choice] = &[
        Choice::new(3, "Error"),
        Choice::new(4, "Warning"),
        Choice::new(6, "Info"),
        Choice::new(7, "Debug"),
        Choice::new(8, "Trace"),
    ];

    /// The coreboot `debug_level` value of `level`.
    pub const fn value(level: Level) -> u32 {
        match level {
            Level::Error => 3,
            Level::Warn => 4,
            Level::Info => 6,
            Level::Debug => 7,
            Level::Trace => 8,
        }
    }

    /// The level of a `debug_level` value of [`KNOBS`].
    pub const fn level(value: u32) -> Level {
        match value {
            0..=3 => Level::Error,
            4 | 5 => Level::Warn,
            6 => Level::Info,
            7 => Level::Debug,
            _ => Level::Trace,
        }
    }

    /// The console log level knob, over the level a flow then sets with
    /// [`super::set_max_level`].
    pub static KNOBS: [Knob<Level>; 1] = [Knob::new(
        Meta {
            key: "debug_level",
            form: "Console",
            label: "Console log level",
            help: "The most verbose firmware messages the console shows.",
            kind: Kind::Enum(CHOICES),
        },
        |l| value(*l),
        |l, v| *l = level(v),
    )];

    /// Log a knob's outcome: an applied override or a rejected value.
    pub fn log(setting: &Setting) {
        let key = setting.meta.key;
        match setting.provenance {
            Provenance::Board => {}
            Provenance::Override => crate::info!(
                "options: {} = {} (board {})",
                key,
                setting.value,
                setting.default
            ),
            Provenance::Rejected { stored, why } => crate::warn!(
                "options: {} = {} not applied ({}), keeping {}",
                key,
                stored,
                match why {
                    Rejection::OutOfRange => "invalid value",
                    Rejection::Inactive => "unavailable",
                    Rejection::Source(_) => "store unreadable",
                    Rejection::Conflict(why) => why,
                },
                setting.value
            ),
        }
    }

    /// Log how many knobs were read from which backends.
    pub fn log_sources(count: usize, source: &dyn Source) {
        let mut names = [""; 3];
        let mut n = 0;
        source.names(&mut |name| {
            if let Some(slot) = names.get_mut(n) {
                *slot = name;
            }
            n += 1;
        });
        match n {
            0 | 1 => crate::info!("options: {} knobs from {}", count, names[0]),
            2 => crate::info!(
                "options: {} knobs from {}, then {}",
                count,
                names[0],
                names[1]
            ),
            _ => crate::info!(
                "options: {} knobs from {}, then {}, then {}",
                count,
                names[0],
                names[1],
                names[2]
            ),
        }
    }

    /// [`fstart_options::resolve_each`], logged. Allocates nothing, so
    /// pre-DRAM stages use it.
    pub fn resolve<C: Clone + 'static>(
        knobs: &'static [Knob<C>],
        source: &dyn Source,
        config: &mut C,
        check: impl FnOnce(&C) -> Result<(), &'static str>,
    ) {
        fstart_options::resolve_each(knobs, source, config, check, |s| log(&s));
        log_sources(knobs.len(), source);
    }

    /// [`fstart_options::resolve_checked`] into `report`, logged.
    #[cfg(feature = "alloc")]
    pub fn resolve_into<C: Clone + 'static>(
        knobs: &'static [Knob<C>],
        source: &dyn Source,
        config: &mut C,
        report: &mut fstart_options::Report,
        check: impl FnOnce(&C) -> Result<(), &'static str>,
    ) {
        let first = report.settings.len();
        let duplicates = report.duplicates.len();
        fstart_options::resolve_checked(knobs, source, config, report, check);
        for meta in &report.duplicates[duplicates..] {
            crate::error!(
                "options: {} is a key of another knob, not resolved",
                meta.key
            );
        }
        report.settings[first..].iter().for_each(log);
        log_sources(report.settings.len() - first, source);
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn every_choice_is_its_levels_value() {
            for choice in CHOICES {
                assert_eq!(value(level(choice.value)), choice.value);
            }
            assert_eq!(value(Level::Info), 6);
        }
    }
}

// ---------------------------------------------------------------------------
// Writer adapter
// ---------------------------------------------------------------------------

/// Run one console operation, accounting its time as console output in the
/// boot timestamps (see `fstart-timestamp`).
#[inline]
fn timed<R>(write: impl FnOnce() -> R) -> R {
    let start = fstart_timestamp::now();
    let result = write();
    fstart_timestamp::add_console_time(fstart_timestamp::now().wrapping_sub(start));
    result
}

/// Zero-sized writer that routes [`ufmt::uWrite`] calls to the global
/// console.
///
/// If no console has been registered via [`init`], writes are silently
/// discarded.
pub struct ConsoleWriter {
    /// Held from `writer()` until the macro's writer goes out of scope, which
    /// brackets the tag, the message and the flush.
    _guard: ConsoleLockGuard,
}

impl ufmt::uWrite for ConsoleWriter {
    type Error = ();

    // One console: the code and its inlining are exactly those of a build
    // without debug consoles support.
    #[cfg(not(feature = "debug-consoles"))]
    #[inline]
    fn write_str(&mut self, s: &str) -> Result<(), Self::Error> {
        // SAFETY: CONSOLE is written once during init, then only read.
        let console = unsafe { *CONSOLE.0.get() };
        if let Some(c) = console {
            // Serial terminals require \r\n, but ufmt's uwriteln! only
            // emits \n.  Translate on the fly so every newline is \r\n.
            let bytes = s.as_bytes();
            let mut start = 0;
            for (i, &b) in bytes.iter().enumerate() {
                if b == b'\n' {
                    if i > start {
                        // SAFETY: slicing a valid UTF-8 string at ASCII
                        // byte boundaries always yields valid UTF-8.
                        let chunk = unsafe { core::str::from_utf8_unchecked(&bytes[start..i]) };
                        timed(|| c.write_str(chunk)).map_err(|_| ())?;
                    }
                    timed(|| c.write_str("\r\n")).map_err(|_| ())?;
                    start = i + 1;
                }
            }
            if start < bytes.len() {
                let tail = unsafe { core::str::from_utf8_unchecked(&bytes[start..]) };
                timed(|| c.write_str(tail)).map_err(|_| ())?;
            }
            Ok(())
        } else {
            Ok(())
        }
    }

    #[cfg(feature = "debug-consoles")]
    #[inline]
    fn write_str(&mut self, s: &str) -> Result<(), Self::Error> {
        write_all(s)
    }
}

/// Write `s` to every console. Out of line: ufmt writes each fragment of a
/// message separately, and every log call site would otherwise carry its own
/// copy of the backend loop.
///
/// An error ends the message for every backend, so report one only when no
/// backend took the text: an offline console (an absent debug gadget) must
/// not cut another's messages short.
#[cfg(feature = "debug-consoles")]
#[inline(never)]
fn write_all(s: &str) -> Result<(), ()> {
    let mut result = Ok(());
    let mut taken = false;
    for c in consoles() {
        match write_translated(c, s) {
            Ok(()) => taken = true,
            Err(error) => result = Err(error),
        }
    }
    if taken { Ok(()) } else { result }
}

/// Write `s` to `c`. Serial terminals require \r\n, but ufmt's uwriteln!
/// only emits \n: translate on the fly so every newline is \r\n.
#[cfg(feature = "debug-consoles")]
#[inline(always)]
fn write_translated(c: &dyn Console, s: &str) -> Result<(), ()> {
    let bytes = s.as_bytes();
    let mut start = 0;
    for (i, &b) in bytes.iter().enumerate() {
        if b == b'\n' {
            if i > start {
                // SAFETY: slicing a valid UTF-8 string at ASCII
                // byte boundaries always yields valid UTF-8.
                let chunk = unsafe { core::str::from_utf8_unchecked(&bytes[start..i]) };
                timed(|| c.write_str(chunk)).map_err(|_| ())?;
            }
            timed(|| c.write_str("\r\n")).map_err(|_| ())?;
            start = i + 1;
        }
    }
    if start < bytes.len() {
        // SAFETY: as above.
        let tail = unsafe { core::str::from_utf8_unchecked(&bytes[start..]) };
        timed(|| c.write_str(tail)).map_err(|_| ())?;
    }
    Ok(())
}

/// Return a [`ConsoleWriter`] for use with [`ufmt::uwrite!`] and log macros.
///
/// This is an implementation detail used by the log macros. Prefer using
/// [`info!`], [`debug!`], etc. directly.
#[doc(hidden)]
#[inline]
pub fn writer() -> ConsoleWriter {
    ConsoleWriter {
        _guard: ConsoleLockGuard::acquire(),
    }
}

/// Console writer for the panic handler.
///
/// A panic may fire while this CPU holds the console lock (for example from
/// inside a log call), so the lock is taken over rather than waited for: the
/// panic is terminal and its message must get out.
#[doc(hidden)]
pub fn panic_writer() -> ConsoleWriter {
    CONSOLE_LOCK.store(true, Ordering::Release);
    ConsoleWriter {
        _guard: ConsoleLockGuard,
    }
}

/// Return `true` if messages at `level` would be emitted.
#[inline]
pub fn log_enabled(level: Level) -> bool {
    build_enabled(level) && (level as u8) <= (max_level() as u8)
}

// ---------------------------------------------------------------------------
// Hex wrapper
// ---------------------------------------------------------------------------

/// Wrapper for displaying a `u64` as `0x`-prefixed hexadecimal via ufmt.
///
/// Smaller integer types can be cast: `Hex(addr as u64)`.
///
/// ```ignore
/// use fstart_log::{info, Hex};
/// info!("base={}", Hex(0x8000_0000));
/// // Output: [INFO ] base=0x80000000
/// ```
pub struct Hex(pub u64);

impl ufmt::uDisplay for Hex {
    fn fmt<W: ufmt::uWrite + ?Sized>(
        &self,
        f: &mut ufmt::Formatter<'_, W>,
    ) -> Result<(), W::Error> {
        f.write_str("0x")?;
        let mut n = self.0;
        if n == 0 {
            return f.write_str("0");
        }
        let hex = b"0123456789abcdef";
        let mut buf = [0u8; 16];
        let mut i = buf.len();
        while n > 0 {
            i -= 1;
            buf[i] = hex[(n & 0xf) as usize];
            n >>= 4;
        }
        // SAFETY: buf[i..] contains only ASCII hex digit bytes.
        let s = unsafe { core::str::from_utf8_unchecked(&buf[i..]) };
        f.write_str(s)
    }
}

// ---------------------------------------------------------------------------
// Hex dump
// ---------------------------------------------------------------------------

/// Write a single raw byte to the console, bypassing log formatting.
///
/// Used by [`hex_dump`] for efficient binary data output.
/// Silently discarded if no console has been registered via [`init`].
#[cfg(not(feature = "debug-consoles"))]
#[inline]
pub fn raw_write_byte(b: u8) {
    // SAFETY: CONSOLE is written once during init, then only read.
    let console = unsafe { *CONSOLE.0.get() };
    if let Some(c) = console {
        let _ = timed(|| c.write_byte(b));
    }
}

/// Flush the registered console, if any.
#[cfg(not(feature = "debug-consoles"))]
#[inline]
pub fn flush() {
    // SAFETY: CONSOLE is written once during init, then only read.
    let console = unsafe { *CONSOLE.0.get() };
    if let Some(c) = console {
        let _ = timed(|| c.flush());
    }
}

/// Write a single raw byte to every console (see [`write_all`]).
#[cfg(feature = "debug-consoles")]
#[inline(never)]
pub fn raw_write_byte(b: u8) {
    for c in consoles() {
        let _ = timed(|| c.write_byte(b));
    }
}

/// Flush every console.
#[cfg(feature = "debug-consoles")]
#[inline(never)]
pub fn flush() {
    for c in consoles() {
        let _ = timed(|| c.flush());
    }
}

/// Dump a byte slice as hex to the console.
///
/// Output format (compatible with `xxd -r -p` for binary reconstruction):
/// ```text
/// ACPI_TABLE_DUMP_START 1234
/// 44534454340100000247...
/// ACPI_TABLE_DUMP_END
/// ```
///
/// Each line contains up to 32 bytes (64 hex chars).  The start/end
/// markers allow reliable extraction from noisy serial logs.
pub fn hex_dump(data: &[u8]) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";

    // Write start marker with size.
    {
        let mut w = writer();
        let _ = ufmt::uwriteln!(w, "ACPI_TABLE_DUMP_START {}", data.len() as u32);
    }

    // Write hex lines — 32 bytes per line (64 hex chars).
    for chunk in data.chunks(32) {
        for &b in chunk {
            raw_write_byte(HEX[(b >> 4) as usize]);
            raw_write_byte(HEX[(b & 0xF) as usize]);
        }
        raw_write_byte(b'\r');
        raw_write_byte(b'\n');
        flush();
    }

    // Write end marker.
    {
        let mut w = writer();
        let _ = ufmt::uwriteln!(w, "ACPI_TABLE_DUMP_END");
    }
}

// ---------------------------------------------------------------------------
// Macros
// ---------------------------------------------------------------------------

/// Log at `ERROR` level.
///
/// ```ignore
/// fstart_log::error!("failed to init device: {}", name);
/// ```
#[macro_export]
macro_rules! error {
    ($($args:tt)*) => {{
        if const { $crate::build_enabled($crate::Level::Error) }
            && $crate::log_enabled($crate::Level::Error)
        {
            let mut _w = $crate::writer();
            let _ = ::ufmt::uwrite!(_w, "[ERROR] ");
            let _ = ::ufmt::uwriteln!(_w, $($args)*);
            $crate::flush();
        }
    }};
}

/// Log at `WARN` level.
///
/// ```ignore
/// fstart_log::warn!("region overlap detected");
/// ```
#[macro_export]
macro_rules! warn {
    ($($args:tt)*) => {{
        if const { $crate::build_enabled($crate::Level::Warn) }
            && $crate::log_enabled($crate::Level::Warn)
        {
            let mut _w = $crate::writer();
            let _ = ::ufmt::uwrite!(_w, "[WARN ] ");
            let _ = ::ufmt::uwriteln!(_w, $($args)*);
            $crate::flush();
        }
    }};
}

/// Log at `INFO` level.
///
/// ```ignore
/// fstart_log::info!("boot stage {} complete", stage_id);
/// ```
#[macro_export]
macro_rules! info {
    ($($args:tt)*) => {{
        if const { $crate::build_enabled($crate::Level::Info) }
            && $crate::log_enabled($crate::Level::Info)
        {
            let mut _w = $crate::writer();
            let _ = ::ufmt::uwrite!(_w, "[INFO ] ");
            let _ = ::ufmt::uwriteln!(_w, $($args)*);
            $crate::flush();
        }
    }};
}

/// Log at `DEBUG` level.
///
/// ```ignore
/// fstart_log::debug!("mem base={} size={}", fstart_log::Hex(base), size);
/// ```
#[macro_export]
macro_rules! debug {
    ($($args:tt)*) => {{
        if const { $crate::build_enabled($crate::Level::Debug) }
            && $crate::log_enabled($crate::Level::Debug)
        {
            let mut _w = $crate::writer();
            let _ = ::ufmt::uwrite!(_w, "[DEBUG] ");
            let _ = ::ufmt::uwriteln!(_w, $($args)*);
            $crate::flush();
        }
    }};
}

/// Log at `TRACE` level.
///
/// ```ignore
/// fstart_log::trace!("entering function");
/// ```
#[macro_export]
macro_rules! trace {
    ($($args:tt)*) => {{
        if const { $crate::build_enabled($crate::Level::Trace) }
            && $crate::log_enabled($crate::Level::Trace)
        {
            let mut _w = $crate::writer();
            let _ = ::ufmt::uwrite!(_w, "[TRACE] ");
            let _ = ::ufmt::uwriteln!(_w, $($args)*);
            $crate::flush();
        }
    }};
}
