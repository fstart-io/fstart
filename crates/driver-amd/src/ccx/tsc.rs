// SPDX-License-Identifier: MIT OR Apache-2.0
//! The TSC frequency from the hardware-selected highest P-state, and the
//! TSC as the clock of the silicon init.
use super::msr::{PSTATE_0, PSTATE_CURRENT_LIMIT, PstateCurLim, PstateDef};
pub use embedded_hal::delay::DelayNs;
use fstart_arch::x86::cpu::msr::{MsrAccess, NativeMsr};
use tock_registers::{LocalRegisterCopy, interfaces::Readable};

/// Core MHz of a P-state definition, `None` if it isn't enabled.
pub fn pstate_mhz(value: u64) -> Option<u32> {
    let pstate = LocalRegisterCopy::<u64, PstateDef::Register>::new(value);
    if !pstate.is_set(PstateDef::PSTATE_EN) {
        return None;
    }
    let fid = pstate.read(PstateDef::CPU_FID) as u32;
    let divisor = pstate.read(PstateDef::CPU_DFS_ID) as u32;
    if divisor == 0 {
        return Some(4000);
    }
    let mhz =
        if (8..=0x1a).contains(&divisor) || divisor > 0x1a && divisor <= 0x3e && divisor & 1 == 0 {
            200 * fid / divisor
        } else {
            25 * fid
        };
    Some(if mhz == 0 { 4000 } else { mhz })
}
/// # Safety
/// Must run in ring 0 on a supported Family 17h CPU with invariant TSC.
pub unsafe fn frequency_mhz() -> Option<u32> {
    let cpu = unsafe { NativeMsr::new() };
    let highest = cpu
        .reg(PSTATE_CURRENT_LIMIT)
        .read(PstateCurLim::CUR_PSTATE_LIMIT);
    pstate_mhz(cpu.reg(PSTATE_0.offset(highest as u32)).get())
}
/// A monotonic microsecond clock that can also wait: what the silicon
/// init needs for its timeouts.
pub trait Clock: DelayNs {
    fn now_us(&mut self) -> u64;
}

/// The invariant TSC as the time base, at the P0 frequency
/// ([`frequency_mhz`]).
#[derive(Debug, Clone, Copy)]
pub struct Tsc {
    mhz: u32,
}
impl Tsc {
    pub const fn new(mhz: u32) -> Self {
        assert!(mhz != 0);
        Self { mhz }
    }
    pub const fn mhz(&self) -> u32 {
        self.mhz
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
}
#[cfg(target_arch = "x86_64")]
impl DelayNs for Tsc {
    /// At microsecond granularity, rounded up.
    fn delay_ns(&mut self, ns: u32) {
        self.delay_us(ns.div_ceil(1000))
    }
    fn delay_us(&mut self, us: u32) {
        self.wait(u64::from(self.mhz) * u64::from(us))
    }
}
#[cfg(target_arch = "x86_64")]
impl Clock for Tsc {
    fn now_us(&mut self) -> u64 {
        Self::ticks() / u64::from(self.mhz)
    }
}

#[cfg(any(test, feature = "testing"))]
pub mod testing {
    //! Delays for tests.
    use super::{Clock, DelayNs};

    /// Hands every microsecond delay to a closure: `Delay(|us| ...)`.
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

    /// A clock that only moves when the code waits.
    #[derive(Debug, Default)]
    pub struct Elapsed(pub u64);
    impl DelayNs for Elapsed {
        fn delay_ns(&mut self, ns: u32) {
            self.0 += u64::from(ns.div_ceil(1000))
        }
        fn delay_us(&mut self, us: u32) {
            self.0 += u64::from(us)
        }
    }
    impl Clock for Elapsed {
        fn now_us(&mut self) -> u64 {
            self.0
        }
    }
}
