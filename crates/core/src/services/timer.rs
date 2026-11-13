//! Timer service — delay and timestamp abstraction.
//!
//! Drivers that only wait take an embedded-hal [`DelayNs`]; drivers that
//! bound a wait by elapsed time take a [`Timer`]. [`TimerDelay`] hands a
//! timer to the former.
//!
//! [`DelayNs`]: embedded_hal::delay::DelayNs

/// An embedded-hal delay backed by a firmware timer, any `T` or a
/// `dyn Timer`. Nanosecond requests round up to the timer's microsecond
/// granularity.
pub struct TimerDelay<'a, T: Timer + ?Sized + 'a = dyn Timer + 'a>(pub &'a T);

impl<T: Timer + ?Sized> Clone for TimerDelay<'_, T> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<T: Timer + ?Sized> Copy for TimerDelay<'_, T> {}

impl<T: Timer + ?Sized> embedded_hal::delay::DelayNs for TimerDelay<'_, T> {
    fn delay_ns(&mut self, ns: u32) {
        self.0.delay_us(u64::from(ns.div_ceil(1000)));
    }

    fn delay_us(&mut self, us: u32) {
        self.0.delay_us(u64::from(us));
    }

    fn delay_ms(&mut self, ms: u32) {
        self.0.delay_ms(u64::from(ms));
    }
}

/// Timer for delays and timestamps.
pub trait Timer: Send + Sync {
    /// Busy-wait for the specified number of microseconds.
    fn delay_us(&self, us: u64);

    /// Get a monotonic timestamp in microseconds.
    fn timestamp_us(&self) -> u64;

    /// Busy-wait for the specified number of milliseconds.
    fn delay_ms(&self, ms: u64) {
        self.delay_us(ms * 1000);
    }
}

/// Poll `f` until it returns `Some` or `timeout_us` elapses on `timer`.
///
/// `f` is checked once before the deadline is examined and once more
/// after it, so a condition met right at the end is not reported as a
/// timeout. Elapsed time uses wrapping subtraction, so a wrapping
/// timestamp source is fine. For contexts without a clock (only a
/// delay), use [`poll_tries`]. Pre-service x86 code that cannot hold a
/// `Timer` at all uses `fstart_arch::wait_us` instead.
pub fn poll_until<T>(
    timer: &dyn Timer,
    timeout_us: u64,
    mut f: impl FnMut() -> Option<T>,
) -> Option<T> {
    let start = timer.timestamp_us();
    loop {
        if let Some(value) = f() {
            return Some(value);
        }
        if timer.timestamp_us().wrapping_sub(start) >= timeout_us {
            return f();
        }
        core::hint::spin_loop();
    }
}

/// Poll `f` up to `tries` times, waiting `interval_us` between tries,
/// then check `f` one final time.
///
/// This is a try count, not a timeout: use it where no usable clock
/// exists yet (pre-DRAM, SMM) but a delay does. Where a clock exists,
/// prefer [`poll_until`] so slow device reads do not stretch the wait.
pub fn poll_tries<T>(
    timer: &dyn Timer,
    tries: u32,
    interval_us: u64,
    mut f: impl FnMut() -> Option<T>,
) -> Option<T> {
    for _ in 0..tries {
        if let Some(value) = f() {
            return Some(value);
        }
        timer.delay_us(interval_us);
    }
    f()
}

/// A timer for tests: time only moves when the code waits, and, if
/// [`ticking`](ManualTimer::ticking), by one microsecond per read, so that
/// a poll loop without waits still reaches its deadline.
#[cfg(any(test, feature = "testing"))]
#[derive(Debug, Default)]
pub struct ManualTimer {
    now: core::sync::atomic::AtomicU64,
    tick: u64,
}

#[cfg(any(test, feature = "testing"))]
impl ManualTimer {
    pub const fn new() -> Self {
        Self {
            now: core::sync::atomic::AtomicU64::new(0),
            tick: 0,
        }
    }

    pub const fn ticking() -> Self {
        Self {
            now: core::sync::atomic::AtomicU64::new(0),
            tick: 1,
        }
    }

    /// The time, without a tick.
    pub fn now(&self) -> u64 {
        self.now.load(core::sync::atomic::Ordering::Relaxed)
    }
}

#[cfg(any(test, feature = "testing"))]
impl Timer for ManualTimer {
    fn delay_us(&self, us: u64) {
        self.now
            .fetch_add(us, core::sync::atomic::Ordering::Relaxed);
    }

    fn timestamp_us(&self) -> u64 {
        self.now
            .fetch_add(self.tick, core::sync::atomic::Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::cell::Cell;
    use core::sync::atomic::{AtomicU32, Ordering};
    use embedded_hal::delay::DelayNs;

    struct RecordingTimer(AtomicU32);

    impl Timer for RecordingTimer {
        fn delay_us(&self, us: u64) {
            self.0.store(u32::try_from(us).unwrap(), Ordering::Relaxed);
        }

        fn timestamp_us(&self) -> u64 {
            u64::from(self.0.load(Ordering::Relaxed))
        }
    }

    #[test]
    fn nanosecond_delays_round_up() {
        let timer = RecordingTimer(AtomicU32::new(0));
        for (ns, us) in [
            (0, 0),
            (1, 1),
            (999, 1),
            (1000, 1),
            (1001, 2),
            (u32::MAX, 4_294_968),
        ] {
            TimerDelay(&timer).delay_ns(ns);
            assert_eq!(timer.timestamp_us(), us);
        }
    }
    #[test]
    fn poll_until_immediate() {
        let timer = ManualTimer::new();
        assert_eq!(poll_until(&timer as &dyn Timer, 10, || Some(42)), Some(42));
    }

    #[test]
    fn poll_until_timeout() {
        let timer = ManualTimer::ticking();
        let calls = Cell::new(0);
        assert_eq!(
            poll_until(&timer as &dyn Timer, 10, || {
                calls.set(calls.get() + 1);
                None::<()>
            }),
            None
        );
        assert!(calls.get() >= 2);
    }

    #[test]
    fn poll_until_zero_timeout_still_checks() {
        let timer = ManualTimer::ticking();
        let calls = Cell::new(0);
        assert_eq!(
            poll_until(&timer as &dyn Timer, 0, || {
                calls.set(calls.get() + 1);
                (calls.get() == 2).then_some(7)
            }),
            Some(7)
        );
    }

    #[test]
    fn poll_tries_third_call() {
        let timer = ManualTimer::new();
        let calls = Cell::new(0);
        assert_eq!(
            poll_tries(&timer as &dyn Timer, 5, 7, || {
                calls.set(calls.get() + 1);
                (calls.get() == 3).then_some(())
            }),
            Some(())
        );
        assert_eq!(timer.now(), 14);
    }

    #[test]
    fn poll_tries_final_check() {
        let timer = ManualTimer::new();
        let calls = Cell::new(0);
        assert_eq!(
            poll_tries(&timer as &dyn Timer, 2, 5, || {
                calls.set(calls.get() + 1);
                (calls.get() == 3).then_some(())
            }),
            Some(())
        );
        assert_eq!(timer.now(), 10);
    }

    #[test]
    fn poll_tries_exhaustion() {
        let timer = ManualTimer::new();
        let calls = Cell::new(0);
        assert_eq!(
            poll_tries(&timer as &dyn Timer, 3, 5, || {
                calls.set(calls.get() + 1);
                None::<()>
            }),
            None
        );
        assert_eq!(calls.get(), 4);
        assert_eq!(timer.now(), 15);
    }
}
