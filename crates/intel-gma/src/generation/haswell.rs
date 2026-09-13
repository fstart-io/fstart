//! Haswell/Broadwell Intel GMA generation support.
//!
//! This module wires the shared DDI planning helpers into a generation-local
//! modeset plan.  The plan mirrors libgfxinit's HSW/BDW ordering at the level
//! this crate can express today: display power/clock preparation, DDI PLL/clock
//! routing, pipe/transcoder programming, port programming, and DP link training.

#![allow(dead_code)]

use tock_registers::interfaces::{Readable, Writeable};

use crate::ddi::{
    DdiClockRouting, DdiDpInitStep, DdiPortRegs,
    HSW_PORT_CLK_SEL_BASE, HSW_WRPLL_BASE, HSW_WRPLL_CTL_REG, HswDdiPllPlan, HswDpInitParams,
    HswDpInitSequencePlan, HswHdmiInitParams, HswHdmiInitSequencePlan, HswPllSelect,
    HswPortClockSelectRegs, HswWrpllRegs, SKL_DPLL_CTRL2_REG, hsw_dp_init_sequence_plan,
    hsw_hdmi_init_sequence_plan,
};
use crate::error::GmaError;
use crate::generation::{GenerationOps, sealed};
use crate::gtt;
use crate::mmio::Mmio;
use crate::mode::Mode;
use crate::types::{Cpu, Generation, Pipe, Port};

/// Haswell generation marker.
pub struct Haswell;

impl sealed::Sealed for Haswell {}

impl GenerationOps for Haswell {
    const GENERATION: Generation = Generation::Haswell;

    }

fn selected_port(ctx: &crate::GmaContext<'_>) -> Result<Port, GmaError> {
    crate::selected_enabled_port(ctx.config.outputs)
}

fn ddi_pipe_for_port(port: Port) -> Pipe {
    match port {
        Port::Edp | Port::DpA | Port::HdmiA => Pipe::A,
        Port::DpB | Port::HdmiB => Pipe::B,
        _ => Pipe::C,
    }
}

fn map_gtt(ctx: &crate::GmaContext<'_>) -> Result<(), GmaError> {
    gtt::map_surface_to_stolen(ctx.resources, ctx.config.cpu, &ctx.surface)?;
    gtt::flush_gfx(&ctx.mmio());
    Ok(())
}

fn execute_hsw_modeset_plan(mmio: &mut Mmio, plan: HswDdiModesetPlan) -> Result<(), GmaError> {
    match plan.protocol {
        HswDdiProtocolPlan::Hdmi(hdmi) => {
            program_hsw_pll(mmio, hdmi.routed.pll);
            apply_routing(mmio, hdmi.routed.routing);
            write_ddi_port(
                mmio,
                hdmi.routed.port.regs.buf_ctl,
                hdmi.routed.port.buf_ctl,
                None,
            );
        }
        HswDdiProtocolPlan::Dp(dp) => {
            program_hsw_pll(mmio, dp.routed.pll);
            apply_routing(mmio, dp.routed.routing);
            write_ddi_port(
                mmio,
                dp.routed.port.regs.buf_ctl,
                dp.routed.port.buf_ctl,
                dp.routed.port.dp_tp_ctl,
            );
        }
    }
    Ok(())
}

fn write_ddi_port(mmio: &Mmio, buf_ctl_register: usize, buf_ctl: u32, dp_tp_ctl: Option<u32>) {
    // SAFETY: DDI plans only carry `buf_ctl` offsets returned by
    // `DdiRegisters::for_port`, whose local block layout matches `DdiPortRegs`.
    let regs = unsafe { mmio.reg_block::<DdiPortRegs>(buf_ctl_register) };
    regs.buf_ctl.set(buf_ctl);
    if let Some(value) = dp_tp_ctl {
        regs.dp_tp_ctl.set(value);
    }
}

fn program_hsw_pll(mmio: &Mmio, pll: HswDdiPllPlan) {
    if let HswDdiPllPlan::Wrpll { pll, plan } = pll {
        // libgfxinit waits 20 us after enabling the WRPLL before the port can
        // lock to it.
        // SAFETY: `HSW_WRPLL_BASE` is the WRPLL0 register, and `HswWrpllRegs`
        // models the WRPLL0/WRPLL1 spacing used by HSW/BDW.
        let regs = unsafe { mmio.reg_block::<HswWrpllRegs>(HSW_WRPLL_BASE) };
        let value = HSW_WRPLL_CTL_REG::RAW.val(plan.encode_ctl());
        match pll {
            HswPllSelect::Wrpll0 => {
                regs.wrpll0.write(value);
                let _ = regs.wrpll0.get();
            }
            HswPllSelect::Wrpll1 => {
                regs.wrpll1.write(value);
                let _ = regs.wrpll1.get();
            }
            _ => {}
        }
        crate::mmio::delay_us(20);
    }
}

fn apply_routing(mmio: &Mmio, routing: DdiClockRouting) {
    let op = match routing {
        DdiClockRouting::PortClkSel { op } | DdiClockRouting::DpllCtrl2 { op } => op,
    };
    if let Some(reg) = port_clock_select_reg(mmio, op.register) {
        let value = (reg.get() & !op.mask_unset) | op.mask_set;
        reg.set(value);
        return;
    }
    // SAFETY: DDI routing operations carry MMIO offsets from typed DDI planning
    // helpers. This fallback covers the shared DPLL_CTRL2-style route register.
    let reg = unsafe {
        mmio.reg_block::<fstart_core::mmio::MmioReadWrite<u32, SKL_DPLL_CTRL2_REG::Register>>(
            op.register,
        )
    };
    reg.set((reg.get() & !op.mask_unset) | op.mask_set);
}

fn port_clock_select_reg(
    mmio: &Mmio,
    register: usize,
) -> Option<
    &'static fstart_core::mmio::MmioReadWrite<u32, crate::ddi::PORT_CLK_SEL_REG::Register>,
> {
    if register < HSW_PORT_CLK_SEL_BASE {
        return None;
    }
    let offset = register - HSW_PORT_CLK_SEL_BASE;
    // SAFETY: `HSW_PORT_CLK_SEL_BASE` is DDI A's port-clock select register;
    // the following fields cover the contiguous DDI A-E selector registers.
    let regs = unsafe { mmio.reg_block::<HswPortClockSelectRegs>(HSW_PORT_CLK_SEL_BASE) };
    match offset {
        0x00 => Some(&regs.ddi_a),
        0x04 => Some(&regs.ddi_b),
        0x08 => Some(&regs.ddi_c),
        0x0c => Some(&regs.ddi_d),
        0x10 => Some(&regs.ddi_e),
        _ => None,
    }
}

/// Display power and clock preparation stage for HSW/BDW DDI modesets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HswPowerClockPlan {
    /// CPU family being planned.
    pub cpu: Cpu,
    /// Logical port that requires power wells and display clocks.
    pub port: Port,
    /// Whether the port uses AUX/DDI DP-style power sequencing.
    pub uses_aux_power: bool,
    /// Whether this path must keep PCH split resources available.
    pub requires_pch_power: bool,
}

/// HSW/BDW output protocol planned for one DDI port.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HswDdiProtocolPlan {
    /// HDMI/DVI DDI sequence.
    Hdmi(HswHdmiInitSequencePlan),
    /// DisplayPort/eDP DDI sequence.
    Dp(HswDpInitSequencePlan),
}

/// Ordered generation-level steps surrounding the shared DDI sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HswDdiModesetStep {
    /// Enable required display power wells and clocks.
    PreparePowerAndClocks,
    /// Execute the shared DDI protocol sequence.
    ProgramDdiProtocol,
    /// Commit the pipe/transcoder/port state.
    CommitDisplay,
}

/// Complete HSW/BDW DDI modeset plan for one output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HswDdiModesetPlan {
    /// Power/clock preparation requirements.
    pub power: HswPowerClockPlan,
    /// Protocol-specific DDI sequence.
    pub protocol: HswDdiProtocolPlan,
    /// Generation-level ordering around the shared protocol sequence.
    pub steps: [HswDdiModesetStep; 3],
}

impl HswDdiModesetPlan {
    /// Return the pipe selected by the protocol sequence.
    pub const fn pipe(self) -> Pipe {
        match self.protocol {
            HswDdiProtocolPlan::Hdmi(plan) => plan.pipe.pipe,
            HswDdiProtocolPlan::Dp(plan) => plan.pipe.pipe,
        }
    }

    /// Return whether the protocol sequence includes DP AUX link training.
    pub const fn trains_dp_link(self) -> bool {
        match self.protocol {
            HswDdiProtocolPlan::Hdmi(_) => false,
            HswDdiProtocolPlan::Dp(plan) => has_dp_training_step(plan.steps),
        }
    }
}

/// Parameters for a Haswell/Broadwell HDMI/DVI DDI modeset plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HswHdmiModesetParams {
    pub cpu: Cpu,
    pub port: Port,
    pub pipe: Pipe,
    pub mode: Mode,
    pub pll: HswPllSelect,
    pub per_ddi_clock_sel: bool,
    pub hdmi_translation: u8,
}

/// Build a Haswell/Broadwell HDMI/DVI DDI modeset plan.
pub(crate) fn hdmi_modeset_plan(
    params: HswHdmiModesetParams,
) -> Result<HswDdiModesetPlan, GmaError> {
    let HswHdmiModesetParams {
        cpu,
        port,
        pipe,
        mode,
        pll,
        per_ddi_clock_sel,
        hdmi_translation,
    } = params;
    let protocol = hsw_hdmi_init_sequence_plan(HswHdmiInitParams {
        cpu,
        port,
        pipe,
        dotclock_hz: mode.pixel_clock_khz as u64 * 1_000,
        pll,
        per_ddi_clock_sel,
        hdmi_translation,
    })?;
    Ok(HswDdiModesetPlan {
        power: power_clock_plan(cpu, port)?,
        protocol: HswDdiProtocolPlan::Hdmi(protocol),
        steps: modeset_steps(),
    })
}

/// Build a Haswell/Broadwell DP/eDP DDI modeset plan.
pub(crate) fn dp_modeset_plan(params: HswDpInitParams) -> Result<HswDdiModesetPlan, GmaError> {
    let protocol = hsw_dp_init_sequence_plan(params)?;
    let HswDpInitParams { cpu, port, .. } = params;
    Ok(HswDdiModesetPlan {
        power: power_clock_plan(cpu, port)?,
        protocol: HswDdiProtocolPlan::Dp(protocol),
        steps: modeset_steps(),
    })
}

const fn power_clock_plan(cpu: Cpu, port: Port) -> Result<HswPowerClockPlan, GmaError> {
    match cpu {
        Cpu::Haswell | Cpu::Broadwell => Ok(HswPowerClockPlan {
            cpu,
            port,
            uses_aux_power: matches!(
                port,
                Port::Edp | Port::DpA | Port::DpB | Port::DpC | Port::DpD
            ),
            requires_pch_power: matches!(port, Port::HdmiB | Port::HdmiC | Port::DpB | Port::DpC),
        }),
        _ => Err(GmaError::UnsupportedPlatform),
    }
}

const fn modeset_steps() -> [HswDdiModesetStep; 3] {
    [
        HswDdiModesetStep::PreparePowerAndClocks,
        HswDdiModesetStep::ProgramDdiProtocol,
        HswDdiModesetStep::CommitDisplay,
    ]
}

const fn has_dp_training_step(steps: [DdiDpInitStep; crate::ddi::DDI_DP_INIT_STEP_COUNT]) -> bool {
    matches!(steps[6], DdiDpInitStep::TrainDpLink)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ddi::{
        DdiClockRouting, DdiLaneCount, DdiPort, DdiRegisterOp, DpTrainingPattern, HswDdiPllPlan,
        HswHdmiInitStep, HswWrpllPlan,
    };

    #[test]
    fn haswell_hdmi_modeset_plan_wraps_shared_ddi_sequence() {
        let plan = hdmi_modeset_plan(HswHdmiModesetParams {
            cpu: Cpu::Haswell,
            port: Port::HdmiB,
            pipe: Pipe::B,
            mode: Mode::XGA_1024X768_60,
            pll: HswPllSelect::Wrpll0,
            per_ddi_clock_sel: true,
            hdmi_translation: 7,
        })
        .unwrap();
        assert_eq!(plan.power.cpu, Cpu::Haswell);
        assert_eq!(plan.power.port, Port::HdmiB);
        assert!(!plan.power.uses_aux_power);
        assert!(plan.power.requires_pch_power);
        assert_eq!(plan.pipe(), Pipe::B);
        assert!(!plan.trains_dp_link());
        assert_eq!(plan.steps, modeset_steps());
        let HswDdiProtocolPlan::Hdmi(hdmi) = plan.protocol else {
            panic!("expected HDMI protocol plan");
        };
        assert_eq!(hdmi.cpu, Cpu::Haswell);
        assert_eq!(hdmi.routed.port.port, DdiPort::B);
        assert_eq!(hdmi.routed.port.dp_tp_ctl, None);
        assert!(matches!(
            hdmi.routed.pll,
            HswDdiPllPlan::Wrpll {
                pll: HswPllSelect::Wrpll0,
                plan: HswWrpllPlan { .. },
            }
        ));
        assert_eq!(
            hdmi.steps,
            [
                HswHdmiInitStep::ProgramWrpll,
                HswHdmiInitStep::LoadBufferTranslations,
                HswHdmiInitStep::RouteDdiClock,
                HswHdmiInitStep::ProgramPipe,
                HswHdmiInitStep::ProgramTranscoder,
                HswHdmiInitStep::ProgramDdiPort,
            ]
        );
    }

    #[test]
    fn broadwell_edp_modeset_plan_includes_aux_training_and_low_vswing_table() {
        let plan = dp_modeset_plan(HswDpInitParams {
            cpu: Cpu::Broadwell,
            port: Port::Edp,
            pipe: Pipe::A,
            lanes: DdiLaneCount::Two,
            pll: HswPllSelect::Lcpll0,
            pattern: DpTrainingPattern::Pattern1,
            enhanced_framing: true,
            per_ddi_clock_sel: true,
            iboost_enabled: true,
        })
        .unwrap();
        assert_eq!(plan.power.cpu, Cpu::Broadwell);
        assert!(plan.power.uses_aux_power);
        assert!(!plan.power.requires_pch_power);
        assert_eq!(plan.pipe(), Pipe::A);
        assert!(plan.trains_dp_link());
        let HswDdiProtocolPlan::Dp(dp) = plan.protocol else {
            panic!("expected DP protocol plan");
        };
        assert_eq!(dp.routed.port.port, DdiPort::A);
        assert!(dp.routed.port.dp_tp_ctl.is_some());
        assert_eq!(dp.routed.pll, HswDdiPllPlan::Fixed(HswPllSelect::Lcpll0));
        assert_eq!(dp.buffer_translations[0], 0x00ff_ffff);
        assert_eq!(dp.buffer_translations[9], 0x0002_0011);
        assert_eq!(
            dp.steps,
            [
                DdiDpInitStep::ProgramDpPll,
                DdiDpInitStep::LoadBufferTranslations,
                DdiDpInitStep::RouteDdiClock,
                DdiDpInitStep::ProgramPipe,
                DdiDpInitStep::ProgramTranscoder,
                DdiDpInitStep::ProgramDdiPort,
                DdiDpInitStep::TrainDpLink,
            ]
        );
    }

    #[test]
    fn haswell_dp_modeset_plan_routes_pch_ddi_clock() {
        let plan = dp_modeset_plan(HswDpInitParams {
            cpu: Cpu::Haswell,
            port: Port::DpC,
            pipe: Pipe::C,
            lanes: DdiLaneCount::Four,
            pll: HswPllSelect::Spll,
            pattern: DpTrainingPattern::Pattern2,
            enhanced_framing: true,
            per_ddi_clock_sel: true,
            iboost_enabled: false,
        })
        .unwrap();
        assert!(plan.power.uses_aux_power);
        assert!(plan.power.requires_pch_power);
        let HswDdiProtocolPlan::Dp(dp) = plan.protocol else {
            panic!("expected DP protocol plan");
        };
        assert_eq!(
            dp.routed.routing,
            DdiClockRouting::PortClkSel {
                op: DdiRegisterOp {
                    register: 0x46108,
                    mask_unset: u32::MAX,
                    mask_set: 3 << 29,
                }
            }
        );
    }

    #[test]
    fn haswell_modeset_plans_reject_wrong_cpu_or_legacy_port() {
        assert_eq!(
            hdmi_modeset_plan(HswHdmiModesetParams {
                cpu: Cpu::Skylake,
                port: Port::HdmiB,
                pipe: Pipe::B,
                mode: Mode::XGA_1024X768_60,
                pll: HswPllSelect::Wrpll0,
                per_ddi_clock_sel: true,
                hdmi_translation: 7,
            }),
            Err(GmaError::UnsupportedPlatform)
        );
        assert_eq!(
            dp_modeset_plan(HswDpInitParams {
                cpu: Cpu::Haswell,
                port: Port::Vga,
                pipe: Pipe::A,
                lanes: DdiLaneCount::Four,
                pll: HswPllSelect::Spll,
                pattern: DpTrainingPattern::Pattern1,
                enhanced_framing: true,
                per_ddi_clock_sel: true,
                iboost_enabled: false,
            }),
            Err(GmaError::UnsupportedPort)
        );
    }
}
