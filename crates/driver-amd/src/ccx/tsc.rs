// SPDX-License-Identifier: MIT OR Apache-2.0
//! The TSC as the clock of the silicon init, at
//! `fstart_arch::x86::cpu::amd::msr::tsc_mhz`.
use core::num::{NonZeroU32, NonZeroU64};

use embedded_hal::delay::DelayNs;

/// The invariant TSC as the time base, at a nonzero frequency, so that its
/// divisions have no panic path (SMM images link it and may not contain
/// one).
#[derive(Debug, Clone, Copy)]
pub struct Tsc {
    mhz: NonZeroU32,
}
impl Tsc {
    pub const fn new(mhz: NonZeroU32) -> Self {
        Self { mhz }
    }
    pub const fn mhz(&self) -> u32 {
        self.mhz.get()
    }

    /// The TSC at the frequency of its P-state, `None` if the P-state is
    /// disabled.
    pub fn measured(msr: &impl fstart_arch::x86::cpu::msr::MsrAccess) -> Option<Self> {
        fstart_arch::x86::cpu::amd::msr::tsc_mhz(msr)
            .and_then(NonZeroU32::new)
            .map(Self::new)
    }

    /// The TSC at the frequency of its P-state, 1 GHz if the P-state is
    /// disabled.
    pub fn native(msr: &impl fstart_arch::x86::cpu::msr::MsrAccess) -> Self {
        const GHZ: NonZeroU32 = NonZeroU32::new(1000).unwrap();
        Self::measured(msr).unwrap_or(Self::new(GHZ))
    }
}
#[cfg(target_arch = "x86_64")]
impl Tsc {
    fn ticks() -> u64 {
        // SAFETY: RDTSC has no side effects.
        unsafe { core::arch::x86_64::_rdtsc() }
    }
    fn wait(&self, ticks: u64) {
        let start = Self::ticks();
        while Self::ticks().wrapping_sub(start) < ticks {
            core::hint::spin_loop();
        }
    }
    fn now(&self) -> u64 {
        Self::ticks() / NonZeroU64::from(self.mhz)
    }
}
#[cfg(target_arch = "x86_64")]
impl DelayNs for Tsc {
    /// At microsecond granularity, rounded up.
    fn delay_ns(&mut self, ns: u32) {
        self.delay_us(ns.div_ceil(1000))
    }
    fn delay_us(&mut self, us: u32) {
        self.wait(u64::from(self.mhz.get()) * u64::from(us))
    }
}
#[cfg(target_arch = "x86_64")]
impl fstart_core::services::Timer for Tsc {
    fn delay_us(&self, us: u64) {
        self.wait(us.saturating_mul(u64::from(self.mhz.get())))
    }
    fn timestamp_us(&self) -> u64 {
        self.now()
    }
}

#[cfg(any(test, feature = "testing"))]
pub mod testing {
    //! Delays for tests.
    use super::DelayNs;

    /// Hands every microsecond delay to a closure: `Delay(|us| ...)`.
    #[derive(Clone, Copy)]
    pub struct Delay<F: FnMut(u32)>(pub F);
    impl<F: FnMut(u32)> DelayNs for Delay<F> {
        fn delay_ns(&mut self, ns: u32) {
            (self.0)(ns.div_ceil(1000))
        }
        fn delay_us(&mut self, us: u32) {
            (self.0)(us)
        }
    }
    /// No delay at all.
    pub fn none() -> Delay<impl FnMut(u32)> {
        Delay(|_| {})
    }
}
