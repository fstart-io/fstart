//! Timer service — delay and timestamp abstraction.

/// An embedded-hal delay backed by a firmware timer.
/// Nanosecond requests round up to the timer's microsecond granularity.
pub struct TimerDelay<'a>(pub &'a dyn Timer);

impl embedded_hal::delay::DelayNs for TimerDelay<'_> {
    fn delay_ns(&mut self, ns: u32) {
        self.0.delay_us(u64::from(ns.div_ceil(1000)));
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
