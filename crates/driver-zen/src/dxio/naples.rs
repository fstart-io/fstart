//! Naples (ZP) DXIO: SMU message 0x0F, one instance per die
//! (PCIE_DXIO §3.3, contract `dxio.sequence.zp.sequential`).
//!
//! `AmdNbioPcieZPPei` b268252e (sha256 875af953cc06): `DxioEarlyInitV1`
//! `ffdf2047` picks the orchestration by PCD 0x1E. The donor default (0)
//! runs each die to completion (`ffdf1d2f`); 1 sets up and starts every
//! die, then loops, resumes and reads back each (`ffdf194d`). The SP3
//! Naples build is older than SnowyPI's: no NTB variable, no AncDataPhy.

use fstart_core::services::Timer;

use super::{DxioError, DxioPolicy, Hooks, LoopRules, Unit, finish_unit, run, state, var};
use crate::smu::SmuMessages;

/// The DXIO SMU message on Naples (`smu::v9::msg::DXIO`).
pub const DXIO_MSG: u32 = 0x0F;

/// Everything up to and including SM_START for one die.
pub fn setup<M: SmuMessages + ?Sized>(
    unit: &mut Unit<'_, M>,
    policy: &DxioPolicy,
) -> Result<(), DxioError> {
    let d = &unit.dxio;
    if policy.early_train_unit == Some((unit.socket, unit.die)) {
        d.reload_sm(0)?;
    }
    // Init first; GetVersion and the variables only after it succeeds.
    d.init()?;
    unit.version = Some(d.version()?);
    [var::RET_AFTER_MAP, var::RET_AFTER_CONF, var::ANCILLARY_V1]
        .iter()
        .try_for_each(|&v| d.set_var(v, 1, 0))?;
    d.load_caps()?;
    unit.load_heaps()?;
    d.sm_start()
}

/// Sequential (state 5 resumes, no pause) or concurrent (pause at
/// VETTING) rules. Naples always forwards PERST requests.
pub const fn rules(concurrent: bool) -> LoopRules {
    LoopRules {
        pause_at: if concurrent { &[state::VETTING] } else { &[] },
        perst: true,
        perst_deassert_delay_ms: 0,
    }
}

pub fn train<M, H, T>(
    units: &mut [Unit<'_, M>],
    hooks: &mut H,
    timer: &T,
    policy: &DxioPolicy,
) -> Result<(), DxioError>
where
    M: SmuMessages + ?Sized,
    H: Hooks + ?Sized,
    T: Timer + ?Sized,
{
    let rules = rules(policy.naples_concurrent);
    if policy.naples_concurrent {
        units.iter_mut().try_for_each(|u| setup(u, policy))?;
        units
            .iter_mut()
            .try_for_each(|u| run(u, hooks, timer, &rules, policy.timeout_us).map(|_| ()))?;
    } else {
        units.iter_mut().try_for_each(|u| {
            setup(u, policy)?;
            run(u, hooks, timer, &rules, policy.timeout_us).map(|_| ())
        })?;
    }
    units
        .iter_mut()
        .try_for_each(|u| finish_unit(u, hooks, timer, &rules, policy.timeout_us))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dxio::lanes::{Engine, Lanes, PcieEngine};
    use crate::dxio::tests::{FakeSmu, FakeTimer, Recorder, Window};
    use crate::dxio::{Dxio, op};
    use crate::soc::Generation;

    #[test]
    fn sequential_per_die() {
        let board = [
            Engine::pcie(0, Lanes::naples(0, 0, 16), PcieEngine::new()),
            Engine::pcie(0, Lanes::naples(1, 16, 8), PcieEngine::new().devfn(3, 1)),
        ];
        let sm = [[3, 2, state::MAPPED], [3, 2, state::CONFIGURED], [3, 0, 0]];
        let (mut w0, mut w1) = (Window::new(), Window::new());
        let fake = |w: &mut Window| {
            FakeSmu {
                dma: Some(w.0.as_mut_ptr()),
                ..FakeSmu::default()
            }
            .script(&sm)
        };
        let (d0, d1) = (fake(&mut w0), fake(&mut w1));
        let policy = DxioPolicy::new(Generation::Naples);
        let mk = |smu, die, w: &mut Window| {
            Unit::new(
                Generation::Naples,
                0,
                die,
                Dxio::new(smu, DXIO_MSG),
                w.dma(),
                &board,
                &policy,
            )
            .unwrap()
            .unwrap()
        };
        let mut units = [mk(&d0, 0, &mut w0), mk(&d1, 1, &mut w1)];
        // Core 1 die lanes 16–23 are wire 20–27.
        assert_eq!(
            (
                units[1].engines()[0].start_lane,
                units[1].engines()[0].end_lane
            ),
            (20, 27)
        );
        let mut hooks = Recorder::default();
        train(&mut units, &mut hooks, &FakeTimer::default(), &policy).unwrap();
        use op::*;
        let tail = [
            INIT,
            GET_VERSION,
            SET_VARIABLE,
            SET_VARIABLE,
            SET_VARIABLE,
            LOAD_CAPS,
            LOAD_DATA,
            LOAD_DATA,
            LOAD_DATA,
            SM_START,
            GET_SM_STATE,
            GET_ENGINE_CFG,
            SM_RESUME,
            GET_SM_STATE,
            SM_RESUME,
            GET_SM_STATE,
            GET_ENGINE_CFG,
        ];
        assert_eq!(d0.ops()[0], RELOAD_SM);
        assert_eq!(d0.ops()[1..], tail);
        assert_eq!(d1.ops(), tail);
        assert!(d0.log.borrow().iter().all(|(id, _)| *id == 0x0F));
        // Die 0 finishes before die 1 starts.
        assert_eq!(
            hooks.events,
            [
                ("mapped", 0),
                ("configured", 0),
                ("mapped", 1),
                ("configured", 1)
            ]
        );
        assert_eq!(units[1].engines()[0].bdf_num.get(), 0x19);
    }
}
