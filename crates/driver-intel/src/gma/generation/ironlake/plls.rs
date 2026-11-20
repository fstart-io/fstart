//! PCH display PLLs (libgfxinit `ironlake/hw-gfx-gma-plls`).
//!
//! The two PCH DPLLs are shared: an output whose mode (or DP link rate)
//! matches a running PLL reuses it, otherwise it takes a free one.

use super::{Display, LVDS_DUAL_THRESHOLD_KHZ, PortConfig};
use crate::gma::dp_aux::DpLinkRate;
use crate::gma::error::GmaError;
use crate::gma::mmio::{Mmio, delay_us};
use crate::gma::mode::Mode;
use crate::gma::regs::{PCH_DPLL, PCH_FP};
use crate::gma::types::Pipe;

/// One of the two PCH DPLLs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PchPll {
    /// DPLL A.
    A,
    /// DPLL B.
    B,
}

impl PchPll {
    const ALL: [Self; 2] = [Self::A, Self::B];

    const fn dpll(self) -> usize {
        0xc6014 + 4 * self as usize
    }

    const fn fp0(self) -> usize {
        0xc6040 + 8 * self as usize
    }

    const fn fp1(self) -> usize {
        self.fp0() + 4
    }
}

/// libgfxinit `PLL_State`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Slot {
    use_count: u8,
    used_for_dp: bool,
    link_rate: Option<DpLinkRate>,
    mode: Option<Mode>,
}

/// PCH PLL use counts and the PLL of each pipe (libgfxinit `PLLs` and
/// `Allocated_PLLs`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DpllState {
    slots: [Slot; 2],
    pipes: [Option<PchPll>; 3],
}

impl Default for DpllState {
    fn default() -> Self {
        Self::new()
    }
}

/// `Count_Range'Last`: a PLL drives at most two transcoders.
const MAX_USERS: u8 = 2;

impl DpllState {
    /// libgfxinit `PLLs.Initialize`: no PLL in use.
    pub(crate) const fn new() -> Self {
        const FREE: Slot = Slot {
            use_count: 0,
            used_for_dp: false,
            link_rate: None,
            mode: None,
        };
        Self {
            slots: [FREE; 2],
            pipes: [None; 3],
        }
    }

    /// libgfxinit `PLLs.Alloc`: eDP needs none; otherwise share a matching
    /// PLL or program a free one.
    pub(crate) fn alloc(
        &mut self,
        mmio: &Mmio,
        cfg: &PortConfig,
    ) -> Result<Option<PchPll>, GmaError> {
        if cfg.route.fdi.is_none() {
            return Ok(None);
        }
        let dp = cfg.route.display == Display::Dp;
        for pll in PchPll::ALL {
            let slot = &mut self.slots[pll as usize];
            let matches = slot.used_for_dp == dp
                && if dp {
                    slot.link_rate == Some(cfg.dp_link.link_rate)
                } else {
                    slot.mode == Some(cfg.mode)
                };
            if slot.use_count != 0 && slot.use_count != MAX_USERS && matches {
                slot.use_count += 1;
                return Ok(Some(pll));
            }
        }
        let pll = PchPll::ALL
            .into_iter()
            .find(|pll| self.slots[*pll as usize].use_count == 0)
            .ok_or(GmaError::PllNoSolution)?;
        on(mmio, pll, cfg)?;
        self.slots[pll as usize] = Slot {
            use_count: 1,
            used_for_dp: dp,
            link_rate: Some(cfg.dp_link.link_rate),
            mode: Some(cfg.mode),
        };
        Ok(Some(pll))
    }

    /// libgfxinit `PLLs.Free`: drop one user, stop the PLL with the last.
    pub(crate) fn free(&mut self, mmio: &Mmio, pll: Option<PchPll>) {
        let Some(pll) = pll else {
            return;
        };
        let slot = &mut self.slots[pll as usize];
        if slot.use_count != 0 {
            slot.use_count -= 1;
            if slot.use_count == 0 {
                off(mmio, pll);
            }
        }
    }

    /// Outputs using `pll`.
    #[cfg(test)]
    pub(crate) const fn users(&self, pll: PchPll) -> u8 {
        self.slots[pll as usize].use_count
    }

    /// Record the PLL a pipe's output was enabled with.
    pub(crate) fn set_pipe(&mut self, pipe: Pipe, pll: Option<PchPll>) {
        self.pipes[pipe as usize] = pll;
    }

    /// Take the PLL recorded for `pipe`.
    pub(crate) fn take_pipe(&mut self, pipe: Pipe) -> Option<PchPll> {
        self.pipes[pipe as usize].take()
    }
}

/// libgfxinit `PLLs.All_Off`.
pub(super) fn all_off(mmio: &Mmio) {
    for pll in PchPll::ALL {
        off(mmio, pll);
    }
}

fn off(mmio: &Mmio, pll: PchPll) {
    mmio.update32(pll.dpll(), PCH_DPLL::VCO_ENABLE::SET.value, 0);
}

/// PLL divider tuple (libgfxinit `Clock_Type`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Clock {
    n: u64,
    m1: u64,
    m2: u64,
    p1: u64,
    p2: u64,
}

/// libgfxinit `Limits_Type`.
struct Limits {
    n: (u64, u64),
    m: (u64, u64),
    m1: (u64, u64),
    m2: (u64, u64),
    p: (u64, u64),
    p1: (u64, u64),
    p2_fast: u64,
    p2_slow: u64,
    p2_threshold_hz: u64,
}

const VCO_HZ: (u64, u64) = (1_760_000_000, 3_510_000_000);
const REFERENCE_HZ: u64 = 120_000_000;
const MAX_DOTCLOCK_HZ: u64 = 340_000_000;

const LVDS_SINGLE: Limits = Limits {
    n: (3, 5),
    m: (79, 118),
    m1: (14, 22),
    m2: (7, 11),
    p: (28, 112),
    p1: (2, 8),
    p2_fast: 14,
    p2_slow: 14,
    p2_threshold_hz: 0,
};

const LVDS_DUAL: Limits = Limits {
    n: (3, 5),
    m: (79, 127),
    m1: (14, 24),
    m2: (7, 11),
    p: (14, 56),
    p1: (2, 8),
    p2_fast: 7,
    p2_slow: 7,
    p2_threshold_hz: 0,
};

const ALL_OTHER: Limits = Limits {
    n: (3, 7),
    m: (79, 127),
    m1: (14, 24),
    m2: (7, 11),
    p: (5, 80),
    p1: (1, 8),
    p2_fast: 5,
    p2_slow: 10,
    p2_threshold_hz: 225_000_000,
};

const fn within(value: u64, range: (u64, u64)) -> bool {
    range.0 <= value && value <= range.1
}

/// libgfxinit `Verify_Parameters`: the dotclock of a valid tuple.
const fn verify(limits: &Limits, clock: Clock) -> Option<u64> {
    let m = 5 * clock.m1 + clock.m2;
    let p = clock.p1 * clock.p2;
    let vco = REFERENCE_HZ * m / clock.n;
    if within(clock.p1, limits.p1)
        && (clock.p2 == limits.p2_fast || clock.p2 == limits.p2_slow)
        && within(p, limits.p)
        && within(clock.m1, limits.m1)
        && within(clock.m2, limits.m2)
        && within(clock.n, limits.n)
        && within(m, limits.m)
        && within(vco, VCO_HZ)
    {
        Some(vco / p)
    } else {
        None
    }
}

/// libgfxinit `Calculate_Clock_Parameters`: the tuple closest to `target_hz`,
/// searching N up and M1, M2, P1 down; the first of equally close wins.
pub(crate) fn calculate(display: Display, target_hz: u64) -> Option<Clock> {
    let limits = match display {
        Display::Lvds if target_hz >= u64::from(LVDS_DUAL_THRESHOLD_KHZ) * 1000 => &LVDS_DUAL,
        Display::Lvds => &LVDS_SINGLE,
        _ => &ALL_OTHER,
    };
    let p2 = if target_hz <= limits.p2_threshold_hz {
        limits.p2_slow
    } else {
        limits.p2_fast
    };
    let mut best = None;
    let mut best_delta = u64::MAX;
    for n in limits.n.0..=limits.n.1 {
        for m1 in (limits.m1.0..=limits.m1.1).rev() {
            for m2 in (limits.m2.0..=limits.m2.1).rev() {
                for p1 in (limits.p1.0..=limits.p1.1).rev() {
                    let clock = Clock { n, m1, m2, p1, p2 };
                    if let Some(dotclock) = verify(limits, clock) {
                        let delta = dotclock.abs_diff(target_hz);
                        if delta < best_delta {
                            best_delta = delta;
                            best = Some(clock);
                        }
                    }
                }
            }
        }
    }
    best
}

/// The fixed DP tuples of libgfxinit `PLLs.On`.
const fn dp_clock(rate: DpLinkRate) -> Option<Clock> {
    match rate {
        DpLinkRate::Rbr => Some(Clock {
            n: 3,
            m1: 14,
            m2: 11,
            p1: 2,
            p2: 10,
        }),
        DpLinkRate::Hbr => Some(Clock {
            n: 4,
            m1: 16,
            m2: 10,
            p1: 1,
            p2: 10,
        }),
        _ => None,
    }
}

/// `FPx` value of a tuple.
const fn fp(clock: Clock) -> u32 {
    PCH_FP::N.val(clock.n as u32 - 2).value
        | PCH_FP::M1.val(clock.m1 as u32 - 2).value
        | PCH_FP::M2.val(clock.m2 as u32 - 2).value
}

/// `DPLL` value of a tuple without the VCO enable (libgfxinit
/// `Program_DPLL`), P1 written both shifted and unshifted.
const fn dpll(display: Display, clock: Clock) -> u32 {
    let mode = match display {
        Display::Lvds => PCH_DPLL::MODE::Lvds.value | PCH_DPLL::REFCLK::Ssc.value,
        Display::Dp => {
            PCH_DPLL::MODE::Dac.value
                | PCH_DPLL::REFCLK::Ssc.value
                | PCH_DPLL::HIGH_SPEED::SET.value
        }
        _ => {
            PCH_DPLL::MODE::Dac.value
                | PCH_DPLL::REFCLK::Dref.value
                | PCH_DPLL::HIGH_SPEED::SET.value
        }
    };
    let p2 = if clock.p2 == 5 || clock.p2 == 7 {
        PCH_DPLL::P2_5_OR_7::SET.value
    } else {
        0
    };
    let p1 = 1u32 << (clock.p1 - 1);
    mode | p2 | PCH_DPLL::P1_DIVIDER.val(p1).value | p1
}

/// libgfxinit `PLLs.On`: program and start `pll`, then wait 150 µs.
fn on(mmio: &Mmio, pll: PchPll, cfg: &PortConfig) -> Result<(), GmaError> {
    let display = cfg.route.display;
    let target_hz = u64::from(cfg.mode.pixel_clock_khz) * 1000;
    let clock = if display == Display::Dp {
        dp_clock(cfg.dp_link.link_rate)
    } else if target_hz <= MAX_DOTCLOCK_HZ {
        calculate(display, target_hz)
    } else {
        None
    }
    .ok_or(GmaError::PllNoSolution)?;
    mmio.write32(pll.fp0(), fp(clock));
    mmio.write32(pll.fp1(), fp(clock));
    mmio.write32(pll.dpll(), dpll(display, clock));
    mmio.update32(pll.dpll(), 0, PCH_DPLL::VCO_ENABLE::SET.value);
    mmio.posting_read(pll.dpll());
    delay_us(150);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn dotclock(clock: Clock) -> u64 {
        REFERENCE_HZ * (5 * clock.m1 + clock.m2) / clock.n / (clock.p1 * clock.p2)
    }

    #[test]
    fn registers_match_libgfxinit() {
        assert_eq!((PchPll::A.dpll(), PchPll::B.dpll()), (0xc6014, 0xc6018));
        assert_eq!((PchPll::B.fp0(), PchPll::B.fp1()), (0xc6048, 0xc604c));
    }

    #[test]
    fn lvds_and_hdmi_tuples_respect_the_limits() {
        // X220 panel: 1366x768 at 69.3 MHz, single channel.
        let lvds = calculate(Display::Lvds, 69_300_000).unwrap();
        assert_eq!(lvds.p2, 14);
        assert!(dotclock(lvds).abs_diff(69_300_000) < 300_000);
        // Dual channel above 95 MHz.
        assert_eq!(calculate(Display::Lvds, 148_500_000).unwrap().p2, 7);
        // Other outputs: P2 10 up to 225 MHz, else 5.
        let hdmi = calculate(Display::Hdmi, 148_500_000).unwrap();
        assert_eq!(hdmi.p2, 10);
        assert_eq!(dotclock(hdmi), 148_500_000);
        assert_eq!(calculate(Display::Vga, 300_000_000).unwrap().p2, 5);
    }

    #[test]
    fn dp_uses_fixed_tuples() {
        let hbr = dp_clock(DpLinkRate::Hbr).unwrap();
        assert_eq!(dotclock(hbr), 270_000_000);
        assert_eq!(dotclock(dp_clock(DpLinkRate::Rbr).unwrap()), 162_000_000);
        assert_eq!(dp_clock(DpLinkRate::Hbr2), None);
        assert_eq!(fp(hbr), (2 << 16) | (14 << 8) | 8);
        assert_eq!(
            dpll(Display::Dp, hbr),
            (1 << 30) | (1 << 26) | (3 << 13) | (1 << 16) | 1
        );
    }
}
