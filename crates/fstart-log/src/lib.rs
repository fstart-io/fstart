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
//! // In generated fstart_main():
//! uart0.init().unwrap_or_else(|_| halt());
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

// Re-export ufmt so that macro-generated code can reference it
// without consumers needing a direct ufmt dependency.
#[doc(hidden)]
pub use ufmt;

use fstart_services::Console;

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

// ---------------------------------------------------------------------------
// Global state
// ---------------------------------------------------------------------------

use core::cell::UnsafeCell;

/// Interior-mutable cell that is `Sync` by firmware invariant.
///
/// Firmware boot is single-threaded (one hart/core active). All writes
/// happen during init before any concurrent access is possible.
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

/// Current maximum log level.
static MAX_LEVEL: SyncCell<Level> = SyncCell::new(Level::Info);

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
    // SAFETY: single-threaded boot — no concurrent access.
    let slot = &mut *CONSOLE.0.get();
    if slot.is_some() {
        // Already initialised — silently ignore (guard against double init).
        return;
    }
    // SAFETY: Extend the borrow lifetime to `'static`. The caller
    // guarantees the pointee lives for the rest of execution.
    *slot = Some(core::mem::transmute::<&dyn Console, &'static dyn Console>(
        console,
    ));
}

/// Set the maximum log level. Messages above this level are discarded.
///
/// # Safety
///
/// Must not be called concurrently with log macros. In practice, call
/// this immediately after [`init`] during single-threaded boot.
pub unsafe fn set_max_level(level: Level) {
    // SAFETY: single-threaded boot — no concurrent access.
    *MAX_LEVEL.0.get() = level;
}

/// Return the current maximum log level.
#[inline]
pub fn max_level() -> Level {
    // SAFETY: MAX_LEVEL is only written during single-threaded init.
    unsafe { *MAX_LEVEL.0.get() }
}

// ---------------------------------------------------------------------------
// Writer adapter
// ---------------------------------------------------------------------------

/// Zero-sized writer that routes [`ufmt::uWrite`] calls to the global
/// console.
///
/// If no console has been registered via [`init`], writes are silently
/// discarded.
pub struct ConsoleWriter;

impl ufmt::uWrite for ConsoleWriter {
    type Error = ();

    #[inline]
    fn write_str(&mut self, s: &str) -> Result<(), Self::Error> {
        // SAFETY: CONSOLE is written once during init, then only read.
        let console = unsafe { *CONSOLE.0.get() };
        if let Some(c) = console {
            c.write_str(s).map_err(|_| ())
        } else {
            Ok(())
        }
    }
}

/// Return a [`ConsoleWriter`] for use with [`ufmt::uwrite!`] and log macros.
///
/// This is an implementation detail used by the log macros. Prefer using
/// [`info!`], [`debug!`], etc. directly.
#[doc(hidden)]
#[inline]
pub fn writer() -> ConsoleWriter {
    ConsoleWriter
}

/// Return `true` if messages at `level` would be emitted.
#[doc(hidden)]
#[inline]
pub fn log_enabled(level: Level) -> bool {
    (level as u8) <= (max_level() as u8)
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
        if $crate::log_enabled($crate::Level::Error) {
            let mut _w = $crate::writer();
            let _ = $crate::ufmt::uwrite!(_w, "[ERROR] ");
            let _ = $crate::ufmt::uwriteln!(_w, $($args)*);
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
        if $crate::log_enabled($crate::Level::Warn) {
            let mut _w = $crate::writer();
            let _ = $crate::ufmt::uwrite!(_w, "[WARN ] ");
            let _ = $crate::ufmt::uwriteln!(_w, $($args)*);
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
        if $crate::log_enabled($crate::Level::Info) {
            let mut _w = $crate::writer();
            let _ = $crate::ufmt::uwrite!(_w, "[INFO ] ");
            let _ = $crate::ufmt::uwriteln!(_w, $($args)*);
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
        if $crate::log_enabled($crate::Level::Debug) {
            let mut _w = $crate::writer();
            let _ = $crate::ufmt::uwrite!(_w, "[DEBUG] ");
            let _ = $crate::ufmt::uwriteln!(_w, $($args)*);
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
        if $crate::log_enabled($crate::Level::Trace) {
            let mut _w = $crate::writer();
            let _ = $crate::ufmt::uwrite!(_w, "[TRACE] ");
            let _ = $crate::ufmt::uwriteln!(_w, $($args)*);
        }
    }};
}
