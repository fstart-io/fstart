//! Timer service — delay and timestamp abstraction.

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
