//! Skylake/Kabylake Intel GMA generation support.
//!
//! The shared DDI module owns the pure DPLL/DDI calculations. This module adds
//! generation-local sequence expansion and register-executor scaffolding so the
//! Skylake family has an executable modeset skeleton rather than only detached
//! helper plans.

#![allow(dead_code)]

use heapless::Vec;

use crate::ddi::{
    DdiClockRouting, DdiDpInitStep, DdiRegisterOp, SKL_DPLL_CTL_REG, SKL_DPLL_STATUS_REG,
    SklDdiPllPlan, SklDpInitParams, SklDpll, SklHdmiInitParams, SklHdmiInitStep,
    skl_dp_dpll_ctrl1_update, skl_dp_init_sequence_plan, skl_dpll_enable_ops,
    skl_hdmi_dpll_ctrl1_update, skl_hdmi_init_sequence_plan,
};
use crate::error::GmaError;
use crate::generation::{GenerationOps, sealed};
use crate::gtt;
use crate::mmio::Mmio;
use crate::types::{Generation, Pipe, Port};

/// Skylake generation marker.
pub struct Skylake;

impl sealed::Sealed for Skylake {}

impl GenerationOps for Skylake {
    const GENERATION: Generation = Generation::Skylake;
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

/// Typed DPLL enable write derived from Skylake DPLL register metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SklDpllEnableOp {
    /// DPLL being enabled.
    pub pll: SklDpll,
    /// DPLL control register offset.
    pub register: usize,
    /// Typed `PLL_ENABLE` bit value.
    pub value: u32,
}

impl SklDpllEnableOp {
    const fn for_pll(pll: SklDpll) -> Self {
        Self {
            pll,
            register: pll.registers().ctl,
            value: SKL_DPLL_CTL_REG::PLL_ENABLE::SET.value,
        }
    }
}

/// Typed DPLL lock poll derived from Skylake DPLL register metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SklDpllLockPoll {
    /// DPLL being polled.
    pub pll: SklDpll,
    /// Shared DPLL status register offset.
    pub status_register: usize,
    /// Typed status register lock mask for this DPLL's lock bit.
    pub lock_mask: u32,
}

impl SklDpllLockPoll {
    const fn for_pll(pll: SklDpll) -> Self {
        let (_, _, status_register, status_mask) = skl_dpll_enable_ops(pll);
        Self {
            pll,
            status_register,
            lock_mask: SKL_DPLL_STATUS_REG::LOCK.val(status_mask).value,
        }
    }
}

/// Ordered executable register operation for Skylake/Kabylake DDI sequences.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SklDdiOp {
    /// Write a full 32-bit register value.
    Write {
        /// Register offset.
        register: usize,
        /// Value to write.
        value: u32,
    },
    /// Clear selected bits and set selected bits.
    Update(DdiRegisterOp),
    /// Enable a configurable Skylake DPLL.
    EnableDpll(SklDpllEnableOp),
    /// Poll until a configurable Skylake DPLL reports locked.
    PollDpllLocked(SklDpllLockPoll),
    /// Generic poll used by executor unit tests and non-DPLL register waits.
    PollSet {
        /// Register offset.
        register: usize,
        /// Required bits.
        mask: u32,
    },
    /// CPU pipe timing placeholder retained in execution order.
    ProgramPipe(Pipe),
    /// CPU transcoder placeholder retained in execution order.
    ProgramTranscoder(Pipe),
    /// Link-training placeholder retained in execution order.
    TrainDpLink(Port),
}

/// Executable Skylake/Kabylake DDI operation list.
pub(crate) type SklDdiOps = Vec<SklDdiOp, 16>;

/// Minimal register sink used by Skylake DDI executor tests and live MMIO.
pub(crate) trait SklDdiMmioSink {
    /// Read a 32-bit register.
    fn read32(&mut self, register: usize) -> u32;
    /// Write a 32-bit register.
    fn write32(&mut self, register: usize, value: u32);
    /// Clear and set selected bits.
    fn update32(&mut self, register: usize, mask_unset: u32, mask_set: u32) {
        let value = (self.read32(register) & !mask_unset) | mask_set;
        self.write32(register, value);
    }
}

impl SklDdiMmioSink for Mmio {
    fn read32(&mut self, register: usize) -> u32 {
        Mmio::read32(self, register)
    }

    fn write32(&mut self, register: usize, value: u32) {
        Mmio::write32(self, register, value);
    }

    fn update32(&mut self, register: usize, mask_unset: u32, mask_set: u32) {
        Mmio::update32(self, register, mask_unset, mask_set);
    }
}

/// Build executable HDMI register operations from existing Skylake DDI/DPLL plans.
pub(crate) fn skl_hdmi_sequence_ops(params: SklHdmiInitParams) -> Result<SklDdiOps, GmaError> {
    let plan = skl_hdmi_init_sequence_plan(params)?;
    let SklHdmiInitParams { port: _, pipe, .. } = params;
    let mut ops = SklDdiOps::new();
    for step in plan.steps {
        match step {
            SklHdmiInitStep::ProgramDpll => push_skl_pll_ops(&mut ops, plan.routed.pll)?,
            SklHdmiInitStep::RouteDdiClock => push_routing_op(&mut ops, plan.routed.routing)?,
            SklHdmiInitStep::ProgramPipe => push_op(&mut ops, SklDdiOp::ProgramPipe(pipe))?,
            SklHdmiInitStep::ProgramTranscoder => {
                push_op(&mut ops, SklDdiOp::ProgramTranscoder(pipe))?
            }
            SklHdmiInitStep::ProgramDdiPort => {
                push_op(
                    &mut ops,
                    SklDdiOp::Write {
                        register: plan.routed.port.regs.buf_ctl,
                        value: plan.routed.port.buf_ctl,
                    },
                )?;
            }
        }
    }
    Ok(ops)
}

/// Build executable DP/eDP register operations from existing Skylake DDI/DPLL plans.
pub(crate) fn skl_dp_sequence_ops(params: SklDpInitParams) -> Result<SklDdiOps, GmaError> {
    let plan = skl_dp_init_sequence_plan(params)?;
    let SklDpInitParams { port, pipe, .. } = params;
    let mut ops = SklDdiOps::new();
    for step in plan.steps {
        match step {
            DdiDpInitStep::ProgramDpPll => push_skl_pll_ops(&mut ops, plan.routed.pll)?,
            DdiDpInitStep::LoadBufferTranslations => {}
            DdiDpInitStep::RouteDdiClock => push_routing_op(&mut ops, plan.routed.routing)?,
            DdiDpInitStep::ProgramPipe => push_op(&mut ops, SklDdiOp::ProgramPipe(pipe))?,
            DdiDpInitStep::ProgramTranscoder => {
                push_op(&mut ops, SklDdiOp::ProgramTranscoder(pipe))?
            }
            DdiDpInitStep::ProgramDdiPort => {
                push_op(
                    &mut ops,
                    SklDdiOp::Write {
                        register: plan.routed.port.regs.buf_ctl,
                        value: plan.routed.port.buf_ctl,
                    },
                )?;
                if let Some(value) = plan.routed.port.dp_tp_ctl {
                    push_op(
                        &mut ops,
                        SklDdiOp::Write {
                            register: plan.routed.port.regs.dp_tp_ctl,
                            value,
                        },
                    )?;
                }
            }
            DdiDpInitStep::TrainDpLink => push_op(&mut ops, SklDdiOp::TrainDpLink(port))?,
        }
    }
    Ok(ops)
}

/// Execute register-affecting operations. Placeholders are intentionally no-ops.
pub(crate) fn execute_skl_ddi_ops<S: SklDdiMmioSink>(
    sink: &mut S,
    ops: &SklDdiOps,
) -> Result<(), GmaError> {
    for op in ops {
        match *op {
            SklDdiOp::Write { register, value } => sink.write32(register, value),
            SklDdiOp::Update(update) => {
                sink.update32(update.register, update.mask_unset, update.mask_set)
            }
            SklDdiOp::EnableDpll(enable) => sink.write32(enable.register, enable.value),
            SklDdiOp::PollDpllLocked(poll) => {
                if !poll_set(sink, poll.status_register, poll.lock_mask) {
                    return Err(GmaError::HardwareError);
                }
            }
            SklDdiOp::PollSet { register, mask } => {
                if !poll_set(sink, register, mask) {
                    return Err(GmaError::HardwareError);
                }
            }
            SklDdiOp::ProgramPipe(_)
            | SklDdiOp::ProgramTranscoder(_)
            | SklDdiOp::TrainDpLink(_) => {}
        }
    }
    Ok(())
}

fn push_skl_pll_ops(ops: &mut SklDdiOps, pll: SklDdiPllPlan) -> Result<(), GmaError> {
    match pll {
        SklDdiPllPlan::Fixed(_) => Ok(()),
        SklDdiPllPlan::Hdmi { pll, plan } => {
            let dpll = pll.configurable().ok_or(GmaError::InvalidConfig)?;
            let regs = dpll.registers();
            push_op(
                ops,
                SklDdiOp::Write {
                    register: regs.cfgr1,
                    value: plan.encode_cfgr1(),
                },
            )?;
            push_op(
                ops,
                SklDdiOp::Write {
                    register: regs.cfgr2,
                    value: plan.encode_cfgr2(),
                },
            )?;
            let (register, mask_unset, mask_set) = skl_hdmi_dpll_ctrl1_update(dpll);
            push_op(
                ops,
                SklDdiOp::Update(DdiRegisterOp {
                    register,
                    mask_unset,
                    mask_set,
                }),
            )?;
            push_dpll_enable_ops(ops, dpll)
        }
        SklDdiPllPlan::Dp { pll, clock } => {
            let dpll = pll.configurable().ok_or(GmaError::InvalidConfig)?;
            let (register, mask_unset, mask_set) = skl_dp_dpll_ctrl1_update(dpll, clock)?;
            push_op(
                ops,
                SklDdiOp::Update(DdiRegisterOp {
                    register,
                    mask_unset,
                    mask_set,
                }),
            )?;
            push_dpll_enable_ops(ops, dpll)
        }
    }
}

fn push_dpll_enable_ops(ops: &mut SklDdiOps, pll: SklDpll) -> Result<(), GmaError> {
    push_op(ops, SklDdiOp::EnableDpll(SklDpllEnableOp::for_pll(pll)))?;
    push_op(ops, SklDdiOp::PollDpllLocked(SklDpllLockPoll::for_pll(pll)))
}

fn push_routing_op(ops: &mut SklDdiOps, routing: DdiClockRouting) -> Result<(), GmaError> {
    match routing {
        DdiClockRouting::PortClkSel { op } | DdiClockRouting::DpllCtrl2 { op } => {
            push_op(ops, SklDdiOp::Update(op))
        }
    }
}

fn push_op(ops: &mut SklDdiOps, op: SklDdiOp) -> Result<(), GmaError> {
    ops.push(op).map_err(|_| GmaError::InvalidConfig)
}

fn poll_set<S: SklDdiMmioSink>(sink: &mut S, register: usize, mask: u32) -> bool {
    let mut tries = 0;
    while tries < 10 {
        if sink.read32(register) & mask == mask {
            return true;
        }
        tries += 1;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ddi::{
        DdiClockSelect, DdiLaneCount, DpTrainingPattern, SklCentralFrequency, SklDpll, SklDpllPlan,
        SklPllSelect,
    };
    use crate::types::Cpu;

    #[test]
    fn hdmi_sequence_expands_dpll_route_and_port_ops() {
        let ops = skl_hdmi_sequence_ops(SklHdmiInitParams {
            cpu: Cpu::Skylake,
            port: Port::HdmiC,
            pipe: Pipe::C,
            dotclock_hz: 297_000_000,
            pll: SklPllSelect::Dpll3,
        })
        .unwrap();
        let regs = SklDpll::Dpll3.registers();
        let expected_plan = SklDpllPlan {
            central_frequency: SklCentralFrequency::Cf9000,
            dco_hz: 8_910_000_000,
            pdiv: 2,
            qdiv: 1,
            kdiv: 3,
        };
        assert_eq!(
            ops[0],
            SklDdiOp::Write {
                register: regs.cfgr1,
                value: expected_plan.encode_cfgr1(),
            }
        );
        assert_eq!(
            ops[1],
            SklDdiOp::Write {
                register: regs.cfgr2,
                value: expected_plan.encode_cfgr2(),
            }
        );
        assert!(matches!(ops[2], SklDdiOp::Update(_)));
        assert_eq!(
            ops[3],
            SklDdiOp::EnableDpll(SklDpllEnableOp::for_pll(SklDpll::Dpll3))
        );
        assert_eq!(
            ops[4],
            SklDdiOp::PollDpllLocked(SklDpllLockPoll::for_pll(SklDpll::Dpll3))
        );
        assert!(matches!(ops[5], SklDdiOp::Update(_)));
        assert_eq!(ops[6], SklDdiOp::ProgramPipe(Pipe::C));
        assert_eq!(ops[7], SklDdiOp::ProgramTranscoder(Pipe::C));
        assert!(matches!(
            ops[8],
            SklDdiOp::Write {
                register: 0x64200,
                ..
            }
        ));
    }

    #[test]
    fn dp_sequence_skips_fixed_dpll0_programming_and_keeps_training_marker() {
        let ops = skl_dp_sequence_ops(SklDpInitParams {
            cpu: Cpu::Kabylake,
            port: Port::Edp,
            pipe: Pipe::A,
            lanes: DdiLaneCount::Two,
            pll: SklPllSelect::Dpll0,
            clock: DdiClockSelect::Lcpll2700,
            pattern: DpTrainingPattern::Pattern1,
            enhanced_framing: true,
        })
        .unwrap();
        assert!(matches!(ops[0], SklDdiOp::Update(_)));
        assert_eq!(ops[1], SklDdiOp::ProgramPipe(Pipe::A));
        assert_eq!(ops[2], SklDdiOp::ProgramTranscoder(Pipe::A));
        assert!(matches!(
            ops[3],
            SklDdiOp::Write {
                register: 0x64000,
                ..
            }
        ));
        assert!(matches!(
            ops[4],
            SklDdiOp::Write {
                register: 0x64040,
                ..
            }
        ));
        assert_eq!(ops[5], SklDdiOp::TrainDpLink(Port::Edp));
    }

    #[test]
    fn dp_sequence_programs_configurable_dpll() {
        let ops = skl_dp_sequence_ops(SklDpInitParams {
            cpu: Cpu::Skylake,
            port: Port::DpC,
            pipe: Pipe::C,
            lanes: DdiLaneCount::Four,
            pll: SklPllSelect::Dpll2,
            clock: DdiClockSelect::Lcpll2700,
            pattern: DpTrainingPattern::Pattern2,
            enhanced_framing: true,
        })
        .unwrap();
        assert!(matches!(ops[0], SklDdiOp::Update(_)));
        assert_eq!(
            ops[1],
            SklDdiOp::EnableDpll(SklDpllEnableOp::for_pll(SklDpll::Dpll2))
        );
        assert_eq!(
            ops[2],
            SklDdiOp::PollDpllLocked(SklDpllLockPoll::for_pll(SklDpll::Dpll2))
        );
    }

    #[test]
    fn executor_applies_register_ops_and_fails_unlocked_dpll() {
        let mut ops = SklDdiOps::new();
        push_op(
            &mut ops,
            SklDdiOp::Write {
                register: 0x10,
                value: 0xaa55,
            },
        )
        .unwrap();
        push_op(
            &mut ops,
            SklDdiOp::Update(DdiRegisterOp {
                register: 0x10,
                mask_unset: 0x00f0,
                mask_set: 0x000f,
            }),
        )
        .unwrap();
        push_op(
            &mut ops,
            SklDdiOp::PollSet {
                register: 0x20,
                mask: 0x4,
            },
        )
        .unwrap();
        let mut sink = MockSink::default();
        sink.write32(0x20, 0x4);
        assert_eq!(execute_skl_ddi_ops(&mut sink, &ops), Ok(()));
        assert_eq!(sink.latest(0x10), 0xaa0f);

        let mut locked = MockSink::default();
        assert_eq!(
            execute_skl_ddi_ops(&mut locked, &ops),
            Err(GmaError::HardwareError)
        );
    }

    #[derive(Default)]
    struct MockSink {
        writes: std::vec::Vec<(usize, u32)>,
    }

    impl MockSink {
        fn latest(&self, register: usize) -> u32 {
            self.writes
                .iter()
                .rev()
                .find(|(written_register, _)| *written_register == register)
                .map(|(_, value)| *value)
                .unwrap_or(0)
        }
    }

    impl SklDdiMmioSink for MockSink {
        fn read32(&mut self, register: usize) -> u32 {
            self.latest(register)
        }

        fn write32(&mut self, register: usize, value: u32) {
            self.writes.push((register, value));
        }
    }
}
