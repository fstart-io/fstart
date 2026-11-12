//! Timer service — delay and timestamp abstraction.
//!
//! Drivers that only wait take an embedded-hal [`DelayNs`]; drivers that
//! bound a wait by elapsed time take a [`Timer`]. [`TimerDelay`] hands a
//! timer to the former.
//!
//! [`DelayNs`]: embedded_hal::delay::DelayNs

/// An embedded-hal delay backed by a firmware timer.
/// Nanosecond requests round up to the timer's microsecond granularity.
#[derive(Clone, Copy)]
pub struct TimerDelay<'a>(pub &'a dyn Timer);

impl embedded_hal::delay::DelayNs for TimerDelay<'_> {
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
}
