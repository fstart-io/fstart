//! Things that may happen once per boot.
use core::sync::atomic::{AtomicBool, Ordering};

/// A one-way flag: the first [`TakeOnce::take`] gets it, every later one,
/// also after an error of the first taker, doesn't.
#[derive(Debug, Default)]
pub struct TakeOnce(AtomicBool);
impl TakeOnce {
    pub const fn new() -> Self {
        Self(AtomicBool::new(false))
    }
    /// `true` for the first caller only.
    pub fn take(&self) -> bool {
        self.0
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_first_take_gets_it() {
        let once = TakeOnce::new();
        assert!(once.take());
        assert!(!once.take());
    }
}
