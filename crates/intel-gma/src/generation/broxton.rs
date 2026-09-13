//! Broxton Intel GMA generation support.
//!
//! This module carries Broxton DDI/PHY planning that mirrors the
//! libgfxinit Broxton PLL programming model.  The plans are intentionally pure:
//! callers can validate clock selection and register programming order without
//! touching MMIO.

#![allow(dead_code)]

use crate::error::GmaError;
use crate::generation::{GenerationOps, sealed};
use crate::gtt;
use crate::mmio::Mmio;
use crate::mode::Mode;
use crate::port::PortRegisterOp;
use crate::regs::{
    BXT_PORT_PCS_DW12, BXT_PORT_PLL_EBB0, BXT_PORT_PLL_EBB4, BXT_PORT_PLL_ENABLE, BXT_PORT_PLL0,
    BXT_PORT_PLL1, BXT_PORT_PLL2, BXT_PORT_PLL3, BXT_PORT_PLL6, BXT_PORT_PLL8, BXT_PORT_PLL9,
    BXT_PORT_PLL10,
};
use crate::types::{Generation, Port};

/// Broxton generation marker.
pub struct Broxton;

impl sealed::Sealed for Broxton {}

impl GenerationOps for Broxton {
    const GENERATION: Generation = Generation::Broxton;

    fn init_display(ctx: &mut crate::GmaContext<'_>, mode: Mode) -> Result<(), GmaError> {
        map_gtt(ctx)?;
        // SAFETY: the selected surface is backed by the just-programmed GTT mapping.
        unsafe { ctx.surface.fill_opaque_black()? };
        let port = selected_port(ctx)?;
        let kind = if matches!(port, Port::HdmiA | Port::HdmiB | Port::HdmiC) {
            BxtDisplayKind::Hdmi
        } else {
            BxtDisplayKind::Dp
        };
        let bandwidth = if kind == BxtDisplayKind::Dp {
            Some(BxtDpBandwidth::Hbr)
        } else {
            None
        };
        let plan = bxt_pll_plan(port, kind, mode.pixel_clock_khz as u64 * 1_000, bandwidth)?;
        let mut mmio = ctx.mmio();
        for op in plan.ops {
            apply_port_op(&mut mmio, op);
        }
        Ok(())
    }
}

fn selected_port(ctx: &crate::GmaContext<'_>) -> Result<Port, GmaError> {
    crate::selected_enabled_port(ctx.config.outputs)
}

fn map_gtt(ctx: &crate::GmaContext<'_>) -> Result<(), GmaError> {
    gtt::map_surface_to_stolen(ctx.resources, ctx.config.cpu, &ctx.surface)?;
    gtt::flush_gfx(&ctx.mmio());
    Ok(())
}

fn apply_port_op(mmio: &mut Mmio, op: PortRegisterOp) {
    match op {
        PortRegisterOp::Write { register, value } => mmio.write32(register, value),
        PortRegisterOp::Update {
            register,
            mask_unset,
            mask_set,
        } => mmio.update32(register, mask_unset, mask_set),
    }
}

/// Broxton DPLL selected from a DDI port.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BxtDpll {
    /// DPLL A for DDI/eDP A.
    A,
    /// DPLL B for DDI B.
    B,
    /// DPLL C for DDI C.
    C,
}

/// Broxton DDI display class for PLL allocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BxtDisplayKind {
    /// DisplayPort/eDP uses fixed link-rate PLL settings.
    Dp,
    /// HDMI uses calculated DPLL settings.
    Hdmi,
}

/// Broxton DisplayPort link bandwidth.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BxtDpBandwidth {
    /// 1.62 GHz link clock.
    Rbr,
    /// 2.7 GHz link clock.
    Hbr,
    /// 5.4 GHz link clock.
    Hbr2,
}

/// Broxton DPLL clock tuple.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BxtPllClock {
    /// Fixed-point M2 divider as programmed by libgfxinit.
    pub m2: u64,
    /// P1 divider.
    pub p1: u8,
    /// P2 divider.
    pub p2: u8,
    /// VCO frequency in Hz.
    pub vco_hz: u64,
    /// Effective dot clock in Hz.
    pub dotclock_hz: u64,
}

/// Register block for one Broxton port PLL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BxtPllRegs {
    pub enable: usize,
    pub ebb0: usize,
    pub ebb4: usize,
    pub pll0: usize,
    pub pll1: usize,
    pub pll2: usize,
    pub pll3: usize,
    pub pll6: usize,
    pub pll8: usize,
    pub pll9: usize,
    pub pll10: usize,
    pub pcs_dw12_ln01: usize,
    pub pcs_dw12_grp: usize,
}

/// Complete Broxton PLL programming plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BxtPllPlan {
    /// Selected PLL.
    pub pll: BxtDpll,
    /// Selected clock tuple.
    pub clock: BxtPllClock,
    /// Ordered register operations matching libgfxinit's `Program_DPLL`.
    pub ops: [PortRegisterOp; BXT_PLL_OP_COUNT],
}

const BXT_PLL_OP_COUNT: usize = 16;
const REF_CLK_HZ: u64 = 100_000_000;
const M1: u64 = 2;
const N: u64 = 1;
const FIXED_POINT: u64 = 1 << 22;
const VCO_MIN_HZ: u64 = 4_800_000_000;
const VCO_MAX_HZ: u64 = 6_700_000_000;
const HDMI_MIN_CLOCK_HZ: u64 = 25_000_000;
const HDMI_MAX_CLOCK_HZ: u64 = 300_000_000;
const CLOCK_GAP_FIRST_HZ: u64 = 223_333_334;
const CLOCK_GAP_LAST_HZ: u64 = 239_999_999;

const PORT_PLL_ENABLE_REF_SEL: u32 = BXT_PORT_PLL_ENABLE::REF_SEL::SET.value;
const PORT_PLL_ENABLE: u32 = BXT_PORT_PLL_ENABLE::ENABLE::SET.value;
const PORT_PLL_EBB0_P1_MASK: u32 = BXT_PORT_PLL_EBB0::P1.val(0x07).value;
const PORT_PLL_EBB0_P2_MASK: u32 = BXT_PORT_PLL_EBB0::P2.val(0x1f).value;
const PORT_PLL_EBB4_RECALIBRATE: u32 = BXT_PORT_PLL_EBB4::RECALIBRATE::SET.value;
const PORT_PLL_EBB4_10BIT_CLK_ENABLE: u32 = BXT_PORT_PLL_EBB4::CLK_10BIT_ENABLE::SET.value;
const PORT_PLL_0_M2_INT_MASK: u32 = BXT_PORT_PLL0::M2_INT.val(0xff).value;
const PORT_PLL_1_N_MASK: u32 = BXT_PORT_PLL1::N.val(0x0f).value;
const PORT_PLL_2_M2_FRAC_MASK: u32 = BXT_PORT_PLL2::M2_FRAC.val(0x003f_ffff).value;
const PORT_PLL_3_M2_FRAC_EN_MASK: u32 = BXT_PORT_PLL3::M2_FRAC_ENABLE::SET.value;
const PORT_PLL_6_GAIN_MASK: u32 = BXT_PORT_PLL6::GAIN_CTL.val(0x07).value
    | BXT_PORT_PLL6::INT_COEFF.val(0x1f).value
    | BXT_PORT_PLL6::PROP_COEFF.val(0x0f).value;
const PORT_PLL_8_TARGET_CNT_MASK: u32 = BXT_PORT_PLL8::TARGET_CNT.val(0x3ff).value;
const PORT_PLL_9_LOCK_THRESHOLD_MASK: u32 = BXT_PORT_PLL9::LOCK_THRESHOLD.val(0x07).value;
const PORT_PLL_10_DCO_AMP_MASK: u32 = BXT_PORT_PLL10::DCO_AMP.val(0x0f).value;
const PORT_PLL_10_DCO_AMP_OVR_EN_H: u32 = BXT_PORT_PLL10::DCO_AMP_OVR_EN_H::SET.value;
const PORT_PCS_LANE_STAGGER_MASK: u32 = BXT_PORT_PCS_DW12::LANE_STAGGER.val(0x1f).value;
const PORT_PCS_LANE_STAGGER_STRAP_OVRD: u32 = BXT_PORT_PCS_DW12::LANE_STAGGER_STRAP_OVRD::SET.value;

/// Resolve Broxton DPLL for a logical port, matching libgfxinit's DIGI_A/B/C map.
pub(crate) const fn bxt_dpll_for_port(port: Port) -> Result<BxtDpll, GmaError> {
    match port {
        Port::Edp | Port::HdmiA | Port::DpA => Ok(BxtDpll::A),
        Port::HdmiB | Port::DpB => Ok(BxtDpll::B),
        Port::HdmiC | Port::DpC => Ok(BxtDpll::C),
        _ => Err(GmaError::UnsupportedPort),
    }
}

/// Return register offsets for a Broxton port PLL.
pub(crate) const fn bxt_pll_regs(pll: BxtDpll) -> BxtPllRegs {
    match pll {
        BxtDpll::A => BxtPllRegs {
            enable: 0x46074,
            ebb0: 0x162034,
            ebb4: 0x162038,
            pll0: 0x162100,
            pll1: 0x162104,
            pll2: 0x162108,
            pll3: 0x16210c,
            pll6: 0x162118,
            pll8: 0x162120,
            pll9: 0x162124,
            pll10: 0x162128,
            pcs_dw12_ln01: 0x162430,
            pcs_dw12_grp: 0x162c30,
        },
        BxtDpll::B => BxtPllRegs {
            enable: 0x46078,
            ebb0: 0x6c034,
            ebb4: 0x6c038,
            pll0: 0x6c100,
            pll1: 0x6c104,
            pll2: 0x6c108,
            pll3: 0x6c10c,
            pll6: 0x6c118,
            pll8: 0x6c120,
            pll9: 0x6c124,
            pll10: 0x6c128,
            pcs_dw12_ln01: 0x6c430,
            pcs_dw12_grp: 0x6cc30,
        },
        BxtDpll::C => BxtPllRegs {
            enable: 0x4607c,
            ebb0: 0x6c340,
            ebb4: 0x6c344,
            pll0: 0x6c380,
            pll1: 0x6c384,
            pll2: 0x6c388,
            pll3: 0x6c38c,
            pll6: 0x6c398,
            pll8: 0x6c3a0,
            pll9: 0x6c3a4,
            pll10: 0x6c3a8,
            pcs_dw12_ln01: 0x6c830,
            pcs_dw12_grp: 0x6ce30,
        },
    }
}

/// Select a Broxton PLL clock for DP/eDP or HDMI.
pub(crate) fn bxt_pll_clock(
    kind: BxtDisplayKind,
    dotclock_hz: u64,
    bandwidth: Option<BxtDpBandwidth>,
) -> Result<BxtPllClock, GmaError> {
    match kind {
        BxtDisplayKind::Dp => bxt_dp_clock(bandwidth.ok_or(GmaError::InvalidConfig)?),
        BxtDisplayKind::Hdmi => calculate_bxt_hdmi_clock(dotclock_hz),
    }
}

/// Build a full Broxton PLL programming plan.
pub(crate) fn bxt_pll_plan(
    port: Port,
    kind: BxtDisplayKind,
    dotclock_hz: u64,
    bandwidth: Option<BxtDpBandwidth>,
) -> Result<BxtPllPlan, GmaError> {
    let pll = bxt_dpll_for_port(port)?;
    let clock = bxt_pll_clock(kind, dotclock_hz, bandwidth)?;
    Ok(BxtPllPlan {
        pll,
        clock,
        ops: bxt_pll_ops(bxt_pll_regs(pll), clock),
    })
}

const fn bxt_dp_clock(bandwidth: BxtDpBandwidth) -> Result<BxtPllClock, GmaError> {
    match bandwidth {
        BxtDpBandwidth::Rbr => Ok(BxtPllClock {
            m2: 32 * FIXED_POINT + 1_677_722,
            p1: 4,
            p2: 2,
            vco_hz: 6_480_000_019,
            dotclock_hz: 162_000_000,
        }),
        BxtDpBandwidth::Hbr => Ok(BxtPllClock {
            m2: 27 * FIXED_POINT,
            p1: 4,
            p2: 1,
            vco_hz: 5_400_000_000,
            dotclock_hz: 270_000_000,
        }),
        BxtDpBandwidth::Hbr2 => Ok(BxtPllClock {
            m2: 27 * FIXED_POINT,
            p1: 2,
            p2: 1,
            vco_hz: 5_400_000_000,
            dotclock_hz: 540_000_000,
        }),
    }
}

fn calculate_bxt_hdmi_clock(dotclock_hz: u64) -> Result<BxtPllClock, GmaError> {
    if !(HDMI_MIN_CLOCK_HZ..=HDMI_MAX_CLOCK_HZ).contains(&dotclock_hz)
        || (dotclock_hz * 99 / 100 >= CLOCK_GAP_FIRST_HZ
            && dotclock_hz * 101 / 100 <= CLOCK_GAP_LAST_HZ)
    {
        return Err(GmaError::ModeUnavailable);
    }
    let target_clock = 5 * dotclock_hz;
    let mut best = None;
    let mut p1 = 4;
    while p1 >= 2 {
        let mut p2 = 20;
        loop {
            let m2 = div_round_closest(target_clock * p2 * p1 * N * FIXED_POINT, REF_CLK_HZ * M1);
            let vco = div_round_closest(REF_CLK_HZ * M1 * m2, FIXED_POINT * N);
            let current_clock = div_round_closest(vco, p1 * p2);
            if (VCO_MIN_HZ..=VCO_MAX_HZ).contains(&vco) {
                let dot = div_round_closest(current_clock, 5);
                if best
                    .map(|clock: BxtPllClock| p1 * p2 > clock.p1 as u64 * clock.p2 as u64)
                    .unwrap_or(true)
                {
                    best = Some(BxtPllClock {
                        m2,
                        p1: p1 as u8,
                        p2: p2 as u8,
                        vco_hz: vco,
                        dotclock_hz: dot,
                    });
                }
                break;
            }
            if m2 < min_m2() || p2 == 1 {
                break;
            }
            p2 = if p2 > 10 { p2 - 2 } else { p2 - 1 };
        }
        if p1 == 2 {
            break;
        }
        p1 -= 1;
    }
    best.ok_or(GmaError::ModeUnavailable)
}

const fn bxt_pll_ops(regs: BxtPllRegs, clock: BxtPllClock) -> [PortRegisterOp; BXT_PLL_OP_COUNT] {
    let pcs = lane_stagger(clock.dotclock_hz);
    [
        PortRegisterOp::Update {
            register: regs.enable,
            mask_unset: 0,
            mask_set: PORT_PLL_ENABLE_REF_SEL,
        },
        PortRegisterOp::Update {
            register: regs.ebb4,
            mask_unset: PORT_PLL_EBB4_10BIT_CLK_ENABLE,
            mask_set: 0,
        },
        PortRegisterOp::Update {
            register: regs.ebb0,
            mask_unset: PORT_PLL_EBB0_P1_MASK | PORT_PLL_EBB0_P2_MASK,
            mask_set: BXT_PORT_PLL_EBB0::P1.val(clock.p1 as u32).value
                | BXT_PORT_PLL_EBB0::P2.val(clock.p2 as u32).value,
        },
        PortRegisterOp::Update {
            register: regs.pll0,
            mask_unset: PORT_PLL_0_M2_INT_MASK,
            mask_set: BXT_PORT_PLL0::M2_INT.val((clock.m2 >> 22) as u32).value,
        },
        PortRegisterOp::Update {
            register: regs.pll1,
            mask_unset: PORT_PLL_1_N_MASK,
            mask_set: BXT_PORT_PLL1::N.val(N as u32).value,
        },
        PortRegisterOp::Update {
            register: regs.pll2,
            mask_unset: PORT_PLL_2_M2_FRAC_MASK,
            mask_set: BXT_PORT_PLL2::M2_FRAC
                .val((clock.m2 as u32) & BXT_PORT_PLL2::M2_FRAC.mask)
                .value,
        },
        PortRegisterOp::Update {
            register: regs.pll3,
            mask_unset: PORT_PLL_3_M2_FRAC_EN_MASK,
            mask_set: if (clock.m2 as u32) & PORT_PLL_2_M2_FRAC_MASK != 0 {
                PORT_PLL_3_M2_FRAC_EN_MASK
            } else {
                0
            },
        },
        PortRegisterOp::Update {
            register: regs.pll6,
            mask_unset: PORT_PLL_6_GAIN_MASK,
            mask_set: gain_coeff(clock.vco_hz),
        },
        PortRegisterOp::Update {
            register: regs.pll8,
            mask_unset: PORT_PLL_8_TARGET_CNT_MASK,
            mask_set: BXT_PORT_PLL8::TARGET_CNT
                .val(if clock.vco_hz >= 6_200_000_000 { 8 } else { 9 })
                .value,
        },
        PortRegisterOp::Update {
            register: regs.pll9,
            mask_unset: PORT_PLL_9_LOCK_THRESHOLD_MASK,
            mask_set: BXT_PORT_PLL9::LOCK_THRESHOLD.val(5).value,
        },
        PortRegisterOp::Update {
            register: regs.pll10,
            mask_unset: PORT_PLL_10_DCO_AMP_MASK,
            mask_set: PORT_PLL_10_DCO_AMP_OVR_EN_H | BXT_PORT_PLL10::DCO_AMP.val(15).value,
        },
        PortRegisterOp::Update {
            register: regs.ebb4,
            mask_unset: 0,
            mask_set: PORT_PLL_EBB4_RECALIBRATE,
        },
        PortRegisterOp::Update {
            register: regs.ebb4,
            mask_unset: 0,
            mask_set: PORT_PLL_EBB4_10BIT_CLK_ENABLE,
        },
        PortRegisterOp::Update {
            register: regs.enable,
            mask_unset: 0,
            mask_set: PORT_PLL_ENABLE,
        },
        PortRegisterOp::Update {
            register: regs.pcs_dw12_ln01,
            mask_unset: PORT_PCS_LANE_STAGGER_MASK,
            mask_set: pcs,
        },
        PortRegisterOp::Write {
            register: regs.pcs_dw12_grp,
            value: pcs,
        },
    ]
}

const fn gain_coeff(vco_hz: u64) -> u32 {
    if vco_hz >= 6_200_000_000 {
        bxt_pll6_gain_coeff(3, 9, 4)
    } else if vco_hz != 5_400_000_000 {
        bxt_pll6_gain_coeff(3, 11, 5)
    } else {
        bxt_pll6_gain_coeff(1, 8, 3)
    }
}

/// libgfxinit `PORT_PLL_6_GAIN_COEFF`: gain control in bits 18:16, integral
/// coefficient in 11:8 and proportional coefficient in 3:0.
const fn bxt_pll6_gain_coeff(gain: u32, int: u32, prop: u32) -> u32 {
    BXT_PORT_PLL6::GAIN_CTL.val(gain).value
        | BXT_PORT_PLL6::INT_COEFF.val(int).value
        | BXT_PORT_PLL6::PROP_COEFF.val(prop).value
}

const fn lane_stagger(dotclock_hz: u64) -> u32 {
    BXT_PORT_PCS_DW12::LANE_STAGGER_STRAP_OVRD::SET.value
        | BXT_PORT_PCS_DW12::LANE_STAGGER
            .val(lane_stagger_value(dotclock_hz))
            .value
}

const fn lane_stagger_value(dotclock_hz: u64) -> u32 {
    if dotclock_hz > 270_000_000 {
        0x18
    } else if dotclock_hz > 135_000_000 {
        0x0d
    } else if dotclock_hz > 67_000_000 {
        0x07
    } else if dotclock_hz > 33_000_000 {
        0x04
    } else {
        0x02
    }
}

const fn div_round_closest(n: u64, d: u64) -> u64 {
    (n + d / 2) / d
}

const fn min_m2() -> u64 {
    VCO_MIN_HZ * FIXED_POINT / REF_CLK_HZ / M1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_broxton_ports_to_plls_like_libgfxinit() {
        assert_eq!(bxt_dpll_for_port(Port::Edp), Ok(BxtDpll::A));
        assert_eq!(bxt_dpll_for_port(Port::DpB), Ok(BxtDpll::B));
        assert_eq!(bxt_dpll_for_port(Port::HdmiC), Ok(BxtDpll::C));
        assert_eq!(bxt_dpll_for_port(Port::Vga), Err(GmaError::UnsupportedPort));
        assert_eq!(bxt_pll_regs(BxtDpll::A).enable, 0x46074);
        assert_eq!(bxt_pll_regs(BxtDpll::B).pll10, 0x6c128);
        assert_eq!(bxt_pll_regs(BxtDpll::C).pcs_dw12_grp, 0x6ce30);
    }

    #[test]
    fn fixed_dp_clocks_match_libgfxinit() {
        assert_eq!(
            bxt_pll_clock(BxtDisplayKind::Dp, 0, Some(BxtDpBandwidth::Rbr)).unwrap(),
            BxtPllClock {
                m2: 32 * FIXED_POINT + 1_677_722,
                p1: 4,
                p2: 2,
                vco_hz: 6_480_000_019,
                dotclock_hz: 162_000_000,
            }
        );
        assert_eq!(
            bxt_pll_clock(BxtDisplayKind::Dp, 0, Some(BxtDpBandwidth::Hbr))
                .unwrap()
                .vco_hz,
            5_400_000_000
        );
        assert_eq!(
            bxt_pll_clock(BxtDisplayKind::Dp, 0, Some(BxtDpBandwidth::Hbr2))
                .unwrap()
                .dotclock_hz,
            540_000_000
        );
        assert_eq!(
            bxt_pll_clock(BxtDisplayKind::Dp, 0, None),
            Err(GmaError::InvalidConfig)
        );
    }

    #[test]
    fn hdmi_clock_search_matches_broxton_algorithm() {
        let clock = calculate_bxt_hdmi_clock(148_500_000).unwrap();
        assert_eq!(clock.p1, 3);
        assert_eq!(clock.p2, 3);
        assert_eq!(clock.dotclock_hz, 148_500_000);
        assert_eq!(clock.vco_hz, 6_682_499_981);
        assert_eq!(
            calculate_bxt_hdmi_clock(24_000_000),
            Err(GmaError::ModeUnavailable)
        );
        assert_eq!(
            calculate_bxt_hdmi_clock(230_000_000),
            Err(GmaError::ModeUnavailable)
        );
    }

    #[test]
    fn pll_programming_plan_matches_libgfxinit_order_and_masks() {
        let plan = bxt_pll_plan(Port::HdmiB, BxtDisplayKind::Hdmi, 148_500_000, None).unwrap();
        let regs = bxt_pll_regs(BxtDpll::B);
        assert_eq!(plan.pll, BxtDpll::B);
        assert_eq!(plan.ops.len(), BXT_PLL_OP_COUNT);
        assert_eq!(
            plan.ops[0],
            PortRegisterOp::Update {
                register: regs.enable,
                mask_unset: 0,
                mask_set: PORT_PLL_ENABLE_REF_SEL,
            }
        );
        assert_eq!(
            plan.ops[2],
            PortRegisterOp::Update {
                register: regs.ebb0,
                mask_unset: PORT_PLL_EBB0_P1_MASK | PORT_PLL_EBB0_P2_MASK,
                mask_set: BXT_PORT_PLL_EBB0::P1.val(3).value | BXT_PORT_PLL_EBB0::P2.val(3).value,
            }
        );
        assert_eq!(
            plan.ops[7],
            PortRegisterOp::Update {
                register: regs.pll6,
                mask_unset: PORT_PLL_6_GAIN_MASK,
                mask_set: bxt_pll6_gain_coeff(3, 9, 4),
            }
        );
        assert_eq!(
            plan.ops[13],
            PortRegisterOp::Update {
                register: regs.enable,
                mask_unset: 0,
                mask_set: PORT_PLL_ENABLE,
            }
        );
        assert_eq!(
            plan.ops[15],
            PortRegisterOp::Write {
                register: regs.pcs_dw12_grp,
                value: BXT_PORT_PCS_DW12::LANE_STAGGER_STRAP_OVRD::SET.value
                    | BXT_PORT_PCS_DW12::LANE_STAGGER.val(0x0d).value,
            }
        );
    }
}
