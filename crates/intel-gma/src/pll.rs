//! Legacy GMCH DPLL helpers, matching libgfxinit's G45 and i945 PLL models.

use fstart_core::mmio::MmioReadWrite;
use tock_registers::interfaces::{Readable, Writeable};

use crate::dp_aux::DpLinkRate;
use crate::error::GmaError;
use crate::mmio::{Mmio, delay_us};
use crate::mode::Mode;
use crate::regs::{DPLL, FP};
use crate::types::{Cpu, Port};

const DPLL_VCO_ENABLE: u32 = DPLL::VCO_ENABLE::SET.value;
const DPLL_VGA_MODE_DIS: u32 = DPLL::VGA_MODE_DIS::SET.value;
const DPLL_P2_5_OR_7: u32 = DPLL::P2_5_OR_7::SET.value;
const DPLL_P1_DIVIDER_SHIFT: u32 = 16;
const DPLL_PINEVIEW_P1_DIVIDER_SHIFT: u32 = 15;
const DPLL_PULSE_PHASE_6: u32 = DPLL::PULSE_PHASE::Phase6.value;
const DPLL_HIGH_SPEED: u32 = DPLL::HIGH_SPEED::SET.value;
const DPLL_MODE_LVDS: u32 = DPLL::MODE::Lvds.value;
const DPLL_MODE_DAC: u32 = DPLL::MODE::Dac.value;
const DPLL_DREFCLK: u32 = DPLL::REFCLK::Dref.value;
const DPLL_SSC: u32 = DPLL::REFCLK::Ssc.value;

const FP_N_SHIFT: u32 = 16;
const FP_M1_SHIFT: u32 = 8;

/// Legacy GMCH DPLL selector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LegacyPll {
    /// DPLL A.
    A,
    /// DPLL B.
    B,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LegacyClock {
    n: u32,
    m1: u32,
    m2: u32,
    p1: u32,
    p2: u32,
    dotclock_hz: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Limits {
    n_min: u32,
    n_max: u32,
    m_min: u32,
    m_max: u32,
    m1_min: u32,
    m1_max: u32,
    m2_min: u32,
    m2_max: u32,
    p_min: u32,
    p_max: u32,
    p1_min: u32,
    p1_max: u32,
    p2_fast: u32,
    p2_slow: u32,
    p2_threshold_hz: u64,
    vco_min: u64,
    vco_max: u64,
}

const I9XX_LVDS_LIMITS: Limits = Limits {
    n_min: 3,
    n_max: 8,
    m_min: 70,
    m_max: 120,
    m1_min: 10,
    m1_max: 20,
    m2_min: 5,
    m2_max: 9,
    p_min: 7,
    p_max: 98,
    p1_min: 1,
    p1_max: 8,
    p2_fast: 7,
    p2_slow: 14,
    p2_threshold_hz: 112_000_000,
    vco_min: 1_400_000_000,
    vco_max: 2_800_000_000,
};

const I9XX_OTHER_LIMITS: Limits = Limits {
    n_min: 3,
    n_max: 8,
    m_min: 70,
    m_max: 120,
    m1_min: 10,
    m1_max: 20,
    m2_min: 5,
    m2_max: 9,
    p_min: 5,
    p_max: 80,
    p1_min: 1,
    p1_max: 8,
    p2_fast: 5,
    p2_slow: 10,
    p2_threshold_hz: 200_000_000,
    vco_min: 1_400_000_000,
    vco_max: 2_800_000_000,
};

const G45_LVDS_SINGLE_LIMITS: Limits = Limits {
    n_min: 3,
    n_max: 5,
    m_min: 104,
    m_max: 138,
    m1_min: 19,
    m1_max: 25,
    m2_min: 7,
    m2_max: 13,
    p_min: 28,
    p_max: 112,
    p1_min: 2,
    p1_max: 8,
    p2_fast: 14,
    p2_slow: 14,
    p2_threshold_hz: 0,
    vco_min: 1_750_000_000,
    vco_max: 3_500_000_000,
};

const G45_LVDS_DUAL_LIMITS: Limits = Limits {
    n_min: 3,
    n_max: 5,
    m_min: 104,
    m_max: 138,
    m1_min: 19,
    m1_max: 25,
    m2_min: 7,
    m2_max: 13,
    p_min: 14,
    p_max: 56,
    p1_min: 2,
    p1_max: 6,
    p2_fast: 7,
    p2_slow: 7,
    p2_threshold_hz: 0,
    vco_min: 1_750_000_000,
    vco_max: 3_500_000_000,
};

const G45_ANALOG_LIMITS: Limits = Limits {
    n_min: 3,
    n_max: 6,
    m_min: 104,
    m_max: 138,
    m1_min: 18,
    m1_max: 25,
    m2_min: 7,
    m2_max: 13,
    p_min: 5,
    p_max: 80,
    p1_min: 1,
    p1_max: 8,
    p2_fast: 5,
    p2_slow: 10,
    p2_threshold_hz: 165_000_000,
    vco_min: 1_750_000_000,
    vco_max: 3_500_000_000,
};

const PINEVIEW_LVDS_LIMITS: Limits = Limits {
    n_min: 3,
    n_max: 6,
    m_min: 2,
    m_max: 256,
    m1_min: 0,
    m1_max: 0,
    m2_min: 0,
    m2_max: 254,
    p_min: 7,
    p_max: 112,
    p1_min: 1,
    p1_max: 8,
    p2_fast: 14,
    p2_slow: 14,
    p2_threshold_hz: 112_000_000,
    vco_min: 1_700_000_000,
    vco_max: 3_500_000_000,
};

const PINEVIEW_ANALOG_LIMITS: Limits = Limits {
    n_min: 3,
    n_max: 6,
    m_min: 2,
    m_max: 256,
    m1_min: 0,
    m1_max: 0,
    m2_min: 0,
    m2_max: 254,
    p_min: 5,
    p_max: 80,
    p1_min: 1,
    p1_max: 8,
    p2_fast: 5,
    p2_slow: 10,
    p2_threshold_hz: 200_000_000,
    vco_min: 1_700_000_000,
    vco_max: 3_500_000_000,
};

/// libgfxinit `Config.LVDS_Dual_Threshold`: LVDS dot clocks at or above this
/// value need the dual-channel PLL limits.
const LVDS_DUAL_THRESHOLD_HZ: u64 = 95_000_000;
/// libgfxinit's G45/GM965 `On` rejects dot clocks above 340 MHz.
const MAX_DOTCLOCK_HZ: u64 = 340_000_000;
/// libgfxinit's i945/Pineview `On` rejects dot clocks above 400 MHz.
const MAX_DOTCLOCK_I945_HZ: u64 = 400_000_000;

/// True for the two Pineview parts, which use the one-hot-N DPLL encoding.
const fn is_pineview(cpu: Cpu) -> bool {
    matches!(cpu, Cpu::Pineview | Cpu::PineviewM)
}

/// True for CPUs whose display block is the Gen3/i945 generation.
const fn is_i945_generation(cpu: Cpu) -> bool {
    matches!(
        cpu,
        Cpu::I945G | Cpu::I945GM | Cpu::Pineview | Cpu::PineviewM
    )
}

/// Select the legacy PLL limits for a CPU and port, returning `None` for ports
/// that use a different PLL model (DisplayPort).
fn select_legacy_limits(cpu: Cpu, port: Port, target_hz: u64) -> Option<Limits> {
    match (cpu, port) {
        (_, Port::DpA | Port::DpB | Port::DpC | Port::DpD | Port::Edp) => None,
        (Cpu::Pineview | Cpu::PineviewM, Port::Lvds) => Some(PINEVIEW_LVDS_LIMITS),
        (Cpu::Pineview | Cpu::PineviewM, _) => Some(PINEVIEW_ANALOG_LIMITS),
        (Cpu::I945G | Cpu::I945GM | Cpu::Gm965, Port::Lvds) => Some(I9XX_LVDS_LIMITS),
        (Cpu::I945G | Cpu::I945GM | Cpu::Gm965, _) => Some(I9XX_OTHER_LIMITS),
        (_, Port::Lvds) => Some(if target_hz >= LVDS_DUAL_THRESHOLD_HZ {
            G45_LVDS_DUAL_LIMITS
        } else {
            G45_LVDS_SINGLE_LIMITS
        }),
        (_, _) => Some(G45_ANALOG_LIMITS),
    }
}

/// Find the best legacy PLL tuple for a mode and port.
pub(crate) fn find_legacy_clock(cpu: Cpu, port: Port, mode: Mode) -> Result<LegacyClock, GmaError> {
    let target_hz = u64::from(mode.pixel_clock_khz) * 1000;
    let limits = select_legacy_limits(cpu, port, target_hz).ok_or(GmaError::UnsupportedPort)?;
    let max_dotclock = if is_i945_generation(cpu) {
        MAX_DOTCLOCK_I945_HZ
    } else {
        MAX_DOTCLOCK_HZ
    };
    if target_hz > max_dotclock {
        return Err(GmaError::PllNoSolution);
    }
    if is_pineview(cpu) {
        return calculate_pineview_clock(target_hz, 96_000_000, limits);
    }
    calculate_clock(target_hz, 96_000_000, limits)
}

/// Fixed DPLL tuples libgfxinit programs for GMCH DisplayPort links.
///
/// The DP PLL is not derived from the pixel clock. libgfxinit selects these
/// constants from the negotiated DP bandwidth, and GMCH only supports 1.62 and
/// 2.7 Gbit/s; 5.4 Gbit/s needs a newer DPLL. `dotclock_hz` is unused for DP.
pub(crate) fn find_legacy_dp_clock(rate: DpLinkRate) -> Result<LegacyClock, GmaError> {
    let (n, m1, m2, p1, p2) = match rate {
        DpLinkRate::Rbr => (4, 25, 10, 2, 10),
        DpLinkRate::Hbr => (3, 16, 4, 1, 10),
        _ => return Err(GmaError::PllNoSolution),
    };
    Ok(LegacyClock {
        n,
        m1,
        m2,
        p1,
        p2,
        dotclock_hz: 0,
    })
}

fn calculate_pineview_clock(
    target_hz: u64,
    reference_hz: u64,
    limits: Limits,
) -> Result<LegacyClock, GmaError> {
    let p2 = if target_hz <= limits.p2_threshold_hz {
        limits.p2_slow
    } else {
        limits.p2_fast
    };
    let mut best: Option<(LegacyClock, u64)> = None;
    // libgfxinit iterates N ascending and M2/P1 descending, so equal-delta
    // parameter sets prefer the higher divider values.
    for n in limits.n_min..=limits.n_max {
        for m2 in (limits.m2_min..=limits.m2_max).rev() {
            for p1 in (limits.p1_min..=limits.p1_max).rev() {
                let m = m2 + 2;
                let p = p1 * p2;
                let vco = reference_hz * u64::from(m) / u64::from(n);
                let dotclock_hz = vco / u64::from(p);
                if m < limits.m_min
                    || m > limits.m_max
                    || p < limits.p_min
                    || p > limits.p_max
                    || vco < limits.vco_min
                    || vco > limits.vco_max
                {
                    continue;
                }
                let delta = dotclock_hz.abs_diff(target_hz);
                let clock = LegacyClock {
                    n,
                    m1: 0,
                    m2,
                    p1,
                    p2,
                    dotclock_hz,
                };
                if best
                    .map(|(_, best_delta)| delta < best_delta)
                    .unwrap_or(true)
                {
                    best = Some((clock, delta));
                }
            }
        }
    }
    best.map(|(clock, _)| clock).ok_or(GmaError::PllNoSolution)
}

fn calculate_clock(
    target_hz: u64,
    reference_hz: u64,
    limits: Limits,
) -> Result<LegacyClock, GmaError> {
    let p2 = if target_hz <= limits.p2_threshold_hz {
        limits.p2_slow
    } else {
        limits.p2_fast
    };
    let mut best: Option<(LegacyClock, u64)> = None;
    for n in limits.n_min..=limits.n_max {
        for m1 in (limits.m1_min..=limits.m1_max).rev() {
            for m2 in (limits.m2_min..=limits.m2_max).rev() {
                if m2 > m1 {
                    continue;
                }
                for p1 in (limits.p1_min..=limits.p1_max).rev() {
                    let m = 5 * m1 + m2;
                    let p = p1 * p2;
                    let vco = reference_hz * u64::from(m) / u64::from(n);
                    let dotclock_hz = vco / u64::from(p);
                    if m < limits.m_min
                        || m > limits.m_max
                        || p < limits.p_min
                        || p > limits.p_max
                        || vco < limits.vco_min
                        || vco > limits.vco_max
                    {
                        continue;
                    }
                    let delta = dotclock_hz.abs_diff(target_hz);
                    let clock = LegacyClock {
                        n,
                        m1,
                        m2,
                        p1,
                        p2,
                        dotclock_hz,
                    };
                    if best
                        .map(|(_, best_delta)| delta < best_delta)
                        .unwrap_or(true)
                    {
                        best = Some((clock, delta));
                    }
                }
            }
        }
    }
    best.map(|(clock, _)| clock).ok_or(GmaError::PllNoSolution)
}

/// Program and enable a legacy GMCH DPLL.
pub(crate) fn program_legacy_pll(
    mmio: &Mmio,
    cpu: Cpu,
    pll: LegacyPll,
    port: Port,
    clock: LegacyClock,
) {
    let (dpll, fp0, fp1) = pll_regs(pll);
    let fp = encode_legacy_fp(cpu, clock);
    // SAFETY: `pll_regs` returns generation-defined legacy GMCH DPLL/FP
    // offsets inside the decoded display MMIO BAR supplied by chipset code.
    let fp0_reg = unsafe { mmio.reg_block::<MmioReadWrite<u32, FP::Register>>(fp0) };
    // SAFETY: see `fp0_reg` above.
    let fp1_reg = unsafe { mmio.reg_block::<MmioReadWrite<u32, FP::Register>>(fp1) };
    // SAFETY: see `fp0_reg` above.
    let dpll_reg = unsafe { mmio.reg_block::<MmioReadWrite<u32, DPLL::Register>>(dpll) };
    fp0_reg.set(fp);
    fp1_reg.set(fp);

    dpll_reg.set(encode_legacy_dpll(cpu, port, clock));
    dpll_reg.set(dpll_reg.get() | DPLL::VCO_ENABLE::SET.value);
    let _ = dpll_reg.get();
    delay_us(150);
}

fn encode_legacy_fp(cpu: Cpu, clock: LegacyClock) -> u32 {
    if is_pineview(cpu) {
        (1u32 << clock.n) << FP_N_SHIFT | clock.m2
    } else {
        ((clock.n - 2) << FP_N_SHIFT) | ((clock.m1 - 2) << FP_M1_SHIFT) | (clock.m2 - 2)
    }
}

fn encode_legacy_dpll(cpu: Cpu, port: Port, clock: LegacyClock) -> u32 {
    let encoded_p1 = 1u32 << (clock.p1 - 1);
    let encoded_p2 = if clock.p2 == 5 || clock.p2 == 7 {
        DPLL_P2_5_OR_7
    } else {
        0
    };
    // libgfxinit g45 `DPLL_Mode`: LVDS and DP use the SSC reference clock,
    // VGA uses DREF, and HDMI/DP enable the high-speed (DVO 2x) path.
    let mode_bits = match port {
        Port::Lvds => DPLL_MODE_LVDS | DPLL_SSC,
        Port::Vga => DPLL_MODE_DAC | DPLL_DREFCLK,
        Port::DpA | Port::DpB | Port::DpC | Port::DpD | Port::Edp => {
            DPLL_MODE_DAC | DPLL_SSC | DPLL_HIGH_SPEED
        }
        _ => DPLL_MODE_DAC | DPLL_DREFCLK | DPLL_HIGH_SPEED,
    };
    let p1_shift = if is_pineview(cpu) {
        DPLL_PINEVIEW_P1_DIVIDER_SHIFT
    } else {
        DPLL_P1_DIVIDER_SHIFT
    };
    // i945/Pineview have no PULSE_PHASE field (Gen4+ only).
    let pulse_phase = if is_i945_generation(cpu) {
        0
    } else {
        DPLL_PULSE_PHASE_6
    };

    mode_bits | DPLL_VGA_MODE_DIS | pulse_phase | encoded_p2 | (encoded_p1 << p1_shift)
}

/// Data-only operation needed to update one legacy PLL register.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PllRegisterOp {
    /// Clear selected register bits.
    ClearBits {
        /// MMIO register offset.
        register: usize,
        /// Bits to clear.
        mask: u32,
    },
}

/// Data-only libgfxinit-style release plan for one legacy GMCH DPLL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LegacyPllReleasePlan {
    /// PLL being released.
    pub pll: LegacyPll,
    /// DPLL control register.
    pub dpll_register: usize,
    /// FP0 register paired with the PLL.
    pub fp0_register: usize,
    /// FP1 register paired with the PLL.
    pub fp1_register: usize,
    /// Ordered register operations for the release path.
    pub ops: [Option<PllRegisterOp>; 1],
}

impl LegacyPllReleasePlan {
    /// Build the release plan used before reprogramming or after port off.
    pub const fn for_pll(pll: LegacyPll) -> Self {
        let (dpll_register, fp0_register, fp1_register) = pll_regs(pll);
        Self {
            pll,
            dpll_register,
            fp0_register,
            fp1_register,
            ops: [Some(PllRegisterOp::ClearBits {
                register: dpll_register,
                mask: DPLL_VCO_ENABLE,
            })],
        }
    }
}

/// Disable a legacy GMCH DPLL.
pub(crate) fn disable_legacy_pll(mmio: &Mmio, pll: LegacyPll) {
    if let Some(op) = LegacyPllReleasePlan::for_pll(pll).ops[0] {
        apply_pll_register_op(mmio, op);
    }
}

fn apply_pll_register_op(mmio: &Mmio, op: PllRegisterOp) {
    match op {
        PllRegisterOp::ClearBits { register, mask } => {
            // SAFETY: release plans are built from `pll_regs`, which returns
            // valid legacy DPLL control register offsets for this MMIO window.
            let dpll = unsafe { mmio.reg_block::<MmioReadWrite<u32, DPLL::Register>>(register) };
            dpll.set(dpll.get() & !mask);
        }
    }
}

const fn pll_regs(pll: LegacyPll) -> (usize, usize, usize) {
    match pll {
        LegacyPll::A => (0x06014, 0x06040, 0x06044),
        LegacyPll::B => (0x06018, 0x06048, 0x0604c),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_gm965_lvds_xga_clock() {
        let clock = find_legacy_clock(Cpu::Gm965, Port::Lvds, Mode::XGA_1024X768_60).unwrap();
        let target = 65_000_000u64;
        assert!(clock.dotclock_hz.abs_diff(target) < 250_000);
    }

    #[test]
    fn finds_gm965_vga_xga_clock() {
        let clock = find_legacy_clock(Cpu::Gm965, Port::Vga, Mode::XGA_1024X768_60).unwrap();
        let target = 65_000_000u64;
        assert!(clock.dotclock_hz.abs_diff(target) < 250_000);
    }

    #[test]
    fn finds_g45_vga_xga_clock() {
        let clock = find_legacy_clock(Cpu::G45, Port::Vga, Mode::XGA_1024X768_60).unwrap();
        let target = 65_000_000u64;
        assert!(clock.dotclock_hz.abs_diff(target) < 250_000);
    }

    #[test]
    fn finds_pineview_vga_xga_clock_with_pineview_encoding() {
        let clock = find_legacy_clock(Cpu::Pineview, Port::Vga, Mode::XGA_1024X768_60).unwrap();
        let target = 65_000_000u64;
        assert_eq!(clock.m1, 0);
        assert_eq!(
            clock.m2 + 2,
            ((clock.dotclock_hz * u64::from(clock.p1 * clock.p2) * u64::from(clock.n) / 96_000_000)
                as u32)
        );
        assert!(clock.dotclock_hz.abs_diff(target) < 250_000);

        let fp = encode_legacy_fp(Cpu::Pineview, clock);
        assert_eq!(fp & 0x0000_ff00, 0);
        assert_eq!((fp >> FP_N_SHIFT).count_ones(), 1);
        assert_eq!(fp & 0xff, clock.m2);

        let dpll = encode_legacy_dpll(Cpu::Pineview, Port::Vga, clock);
        assert_eq!(
            dpll & (0x00ff_8000),
            (1u32 << (clock.p1 - 1)) << DPLL_PINEVIEW_P1_DIVIDER_SHIFT
        );
        assert_eq!(dpll & DPLL_MODE_DAC, DPLL_MODE_DAC);
    }

    #[test]
    fn legacy_pll_release_plan_clears_vco_and_exposes_registers() {
        assert_eq!(
            LegacyPllReleasePlan::for_pll(LegacyPll::A),
            LegacyPllReleasePlan {
                pll: LegacyPll::A,
                dpll_register: 0x06014,
                fp0_register: 0x06040,
                fp1_register: 0x06044,
                ops: [Some(PllRegisterOp::ClearBits {
                    register: 0x06014,
                    mask: DPLL_VCO_ENABLE,
                })],
            }
        );
        assert_eq!(
            LegacyPllReleasePlan::for_pll(LegacyPll::B).ops[0],
            Some(PllRegisterOp::ClearBits {
                register: 0x06018,
                mask: DPLL_VCO_ENABLE,
            })
        );
    }

    #[test]
    fn selects_g45_dual_lvds_limits_at_libgfxinit_threshold() {
        // Below the 95 MHz dual-channel threshold: single-channel limits.
        let single = select_legacy_limits(Cpu::G45, Port::Lvds, 94_999_999).unwrap();
        assert_eq!((single.p_min, single.p_max, single.p1_max), (28, 112, 8));
        // At or above it: dual-channel limits.
        let dual = select_legacy_limits(Cpu::G45, Port::Lvds, LVDS_DUAL_THRESHOLD_HZ).unwrap();
        assert_eq!((dual.p_min, dual.p_max, dual.p1_max), (14, 56, 6));
        assert_eq!((dual.p2_fast, dual.p2_slow), (7, 7));
        // GM965 keeps the i9xx limits regardless of dot clock.
        let gm965 = select_legacy_limits(Cpu::Gm965, Port::Lvds, 200_000_000).unwrap();
        assert_eq!((gm965.p_min, gm965.p_max), (7, 98));
        // VGA still uses the analog limits.
        let vga = select_legacy_limits(Cpu::G45, Port::Vga, 65_000_000).unwrap();
        assert_eq!((vga.m1_min, vga.p_max), (18, 80));
    }

    #[test]
    fn gmch_dp_clock_matches_libgfxinit_static_tuples() {
        let rbr = find_legacy_dp_clock(DpLinkRate::Rbr).unwrap();
        assert_eq!((rbr.n, rbr.m1, rbr.m2, rbr.p1, rbr.p2), (4, 25, 10, 2, 10));
        let hbr = find_legacy_dp_clock(DpLinkRate::Hbr).unwrap();
        assert_eq!((hbr.n, hbr.m1, hbr.m2, hbr.p1, hbr.p2), (3, 16, 4, 1, 10));
        // GMCH has no 5.4 Gbit/s DPLL.
        assert_eq!(
            find_legacy_dp_clock(DpLinkRate::Hbr2),
            Err(GmaError::PllNoSolution)
        );
    }

    #[test]
    fn dp_ports_use_the_fixed_dp_clock_not_the_pixel_clock_search() {
        assert_eq!(
            find_legacy_clock(Cpu::G45, Port::DpA, Mode::XGA_1024X768_60),
            Err(GmaError::UnsupportedPort)
        );
        assert_eq!(select_legacy_limits(Cpu::G45, Port::DpB, 65_000_000), None);
    }

    #[test]
    fn gmch_dpll_mode_bits_match_libgfxinit() {
        let clock = find_legacy_clock(Cpu::Gm965, Port::Lvds, Mode::XGA_1024X768_60).unwrap();
        let lvds = encode_legacy_dpll(Cpu::Gm965, Port::Lvds, clock);
        assert_eq!(
            lvds & (DPLL_MODE_LVDS | DPLL_SSC),
            DPLL_MODE_LVDS | DPLL_SSC
        );
        assert_eq!(lvds & DPLL_HIGH_SPEED, 0);

        let vga = encode_legacy_dpll(Cpu::Gm965, Port::Vga, clock);
        assert_eq!(vga & DPLL_MODE_DAC, DPLL_MODE_DAC);
        assert_eq!(vga & DPLL_SSC, 0);
        assert_eq!(vga & DPLL_HIGH_SPEED, 0);

        // HDMI uses DREF, DP keeps the SSC reference (libgfxinit MODE_DPLL_DP).
        let hdmi = encode_legacy_dpll(Cpu::G45, Port::HdmiA, clock);
        assert_eq!(hdmi & DPLL_HIGH_SPEED, DPLL_HIGH_SPEED);
        assert_eq!(hdmi & DPLL_SSC, 0);
        let dp = encode_legacy_dpll(Cpu::G45, Port::DpA, clock);
        assert_eq!(dp & DPLL_HIGH_SPEED, DPLL_HIGH_SPEED);
        assert_eq!(dp & DPLL_SSC, DPLL_SSC);
    }

    #[test]
    fn pineview_dpll_omits_pulse_phase_and_shifts_p1_by_15() {
        let clock = find_legacy_clock(Cpu::Pineview, Port::Vga, Mode::XGA_1024X768_60).unwrap();
        let dpll = encode_legacy_dpll(Cpu::Pineview, Port::Vga, clock);
        assert_eq!(dpll & DPLL_PULSE_PHASE_6, 0);
        assert_eq!(
            dpll & (0x00ff_8000),
            (1u32 << (clock.p1 - 1)) << DPLL_PINEVIEW_P1_DIVIDER_SHIFT
        );
        // G45 still sets the Gen4+ pulse phase field.
        let g45 = encode_legacy_dpll(Cpu::G45, Port::Vga, clock);
        assert_eq!(g45 & DPLL_PULSE_PHASE_6, DPLL_PULSE_PHASE_6);
    }

    #[test]
    fn i945_uses_i9xx_limits_and_standard_encoding_without_pulse_phase() {
        let clock = find_legacy_clock(Cpu::I945GM, Port::Lvds, Mode::XGA_1024X768_60).unwrap();
        assert!(clock.dotclock_hz.abs_diff(65_000_000) < 250_000);
        let dpll = encode_legacy_dpll(Cpu::I945GM, Port::Lvds, clock);
        assert_eq!(dpll & DPLL_PULSE_PHASE_6, 0);
        // P1 shift 16 on i945; only Pineview shifts by 15.
        assert_eq!(
            dpll & 0x00ff_0000,
            (1u32 << (clock.p1 - 1)) << DPLL_P1_DIVIDER_SHIFT
        );
        // Standard (N-2, M1-2, M2-2) FP encoding, not the Pineview one-hot N.
        assert_eq!(
            encode_legacy_fp(Cpu::I945GM, clock),
            ((clock.n - 2) << 16) | ((clock.m1 - 2) << 8) | (clock.m2 - 2)
        );
    }

    #[test]
    fn i945_accepts_dot_clocks_up_to_400mhz() {
        let mut mode = Mode::XGA_1024X768_60;
        mode.pixel_clock_khz = 350_000;
        assert!(find_legacy_clock(Cpu::I945GM, Port::Vga, mode).is_ok());
        assert!(find_legacy_clock(Cpu::I945G, Port::Vga, mode).is_ok());
        mode.pixel_clock_khz = 401_000;
        assert!(find_legacy_clock(Cpu::I945GM, Port::Vga, mode).is_err());
    }

    #[test]
    fn pineview_m_uses_the_pineview_encoding() {
        let pm = find_legacy_clock(Cpu::PineviewM, Port::Vga, Mode::XGA_1024X768_60).unwrap();
        assert_eq!(pm.m1, 0);
        let fp = encode_legacy_fp(Cpu::PineviewM, pm);
        assert_eq!((fp >> FP_N_SHIFT).count_ones(), 1);
        let dpll = encode_legacy_dpll(Cpu::PineviewM, Port::Vga, pm);
        assert_eq!(dpll & DPLL_PULSE_PHASE_6, 0);
        assert_eq!(
            dpll & 0x00ff_8000,
            (1u32 << (pm.p1 - 1)) << DPLL_PINEVIEW_P1_DIVIDER_SHIFT
        );
    }

    #[test]
    fn pineview_accepts_dot_clocks_above_the_g45_cap() {
        let mut mode = Mode::XGA_1024X768_60;
        mode.pixel_clock_khz = 350_000;
        assert!(find_legacy_clock(Cpu::Gm965, Port::Vga, mode).is_err());
        assert!(find_legacy_clock(Cpu::Pineview, Port::Vga, mode).is_ok());
        mode.pixel_clock_khz = 401_000;
        assert!(find_legacy_clock(Cpu::Pineview, Port::Vga, mode).is_err());
    }
}
