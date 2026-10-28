//! Rome (SSP) and Milan (GN) DXIO: SMU message 8, one instance per socket,
//! two-pass orchestration (PCIE_DXIO §3.3).
//!
//! Rome `AmdNbioPciePei` 21be6180 (sha256 d64860daa843) `DxioEarlyInitV2`
//! `ffe01c0b`; Milan `AmdNbioPciePei` 3057f0e9 (sha256 08e59d623e75)
//! `ffd14856` with the post-Init requests in `ffd0f53e`. Pass 1 runs every
//! socket to CONFIGURED (or VETTING with the deferred PERST policy), pass 2
//! resumes each to DONE and reads the engines back.
//!
//! Oxide sends a different order (Init first, LoadCaps after the
//! variables) and it trains; the donor order is kept here.

use fstart_core::services::Timer;

use super::lanes::{Engine, EngineKind, Speed};
use super::{
    DxioError, DxioPolicy, Hooks, LoopRules, PerstPolicy, Unit, finish_unit, prop, run, state, var,
};
use crate::smu::SmuMessages;
use crate::sp3::soc::Generation;

/// The DXIO SMU message on Rome and Milan (`smu::v11::msg::DXIO`).
pub const DXIO_MSG: u32 = 0x08;

/// SMN register (socket 0) read by the PCIe speed workaround; parts with
/// bits 10 and 12 both clear are capped at Gen2 ("early samples").
pub const SPEED_FUSE: u32 = 0x0005_D698;
const SPEED_FUSE_OK: u32 = 0x1400;

/// Gen3 EQ search mode "preset" (LC_CNTL4[9:8] encoding).
pub const EQ_SEARCH_PRESET: u8 = 3;

/// The SP3 PCIe speed workaround, run on every board PCIe engine before
/// the engine heap is built (Rome `ffdfca17` from `ffdfbe64`, Milan
/// `ffd0daea`). Engines capped at Gen1/Gen2 are left alone; the others
/// get Gen3 EQ search mode "preset", or a Gen2 cap when `fuse` has bits
/// 10 and 12 clear. `fuse` is `None` when the test is skipped (Milan
/// with PCD 0xAF = 0x40). The mode reaches the wire record (`caps_hi`
/// EQ_OVERRIDE + EQ_MODE) and LC_CNTL4[9:8] at MAPPED.
pub fn speed_fixup(engine: &mut Engine<'_>, fuse: Option<u32>) {
    let EngineKind::Pcie(p) = &mut engine.kind else {
        return;
    };
    if matches!(p.max_speed, Speed::Gen1 | Speed::Gen2) {
        return;
    }
    if fuse.is_some_and(|f| f & SPEED_FUSE_OK == 0) {
        p.max_speed = Speed::Gen2;
    } else {
        p.extra.eq_search_mode = Some(EQ_SEARCH_PRESET);
    }
}

/// Everything up to and including SM_START for one socket.
pub fn setup<M: SmuMessages + ?Sized>(
    generation: Generation,
    unit: &mut Unit<'_, M>,
    policy: &DxioPolicy,
) -> Result<(), DxioError> {
    let d = &unit.dxio;
    let p = &policy.sp3;
    let milan = generation == Generation::Milan;
    if policy.early_train_unit == Some((unit.socket, 0)) {
        d.reload_sm(policy.reload_sm_arg as u32)?;
    }
    unit.version = Some(d.version()?);
    d.init()?;
    let slip = p.slip_interval.map_or((0, 0), |v| (1, v as u32));
    if let Some((mask, value)) = p.clock_gating {
        d.set_prop(
            prop::CLOCK_GATING,
            (mask & 0x7F) as u32,
            (value & 0x7F) as u32,
            0,
        )?;
    }
    if milan {
        // `ffd0f53e`.
        d.set_var(var::PCIE_COMPL, p.pcie_compliance as u32, 0)?;
        d.set_var(var::SLIP_INTERVAL, slip.0, slip.1)?;
        if p.ntb_hotplug {
            d.set_var(var::NTB_HP_EN, 1, 0)?;
        }
        d.set_var(
            var::POWER_OFF_DELAY,
            p.power_off_delay.0 as u32,
            p.power_off_delay.1 as u32,
        )?;
    } else {
        // `ffdfd42f`, then SKIP_PSP = 1 until the policy clears it.
        d.set_var(var::SLIP_INTERVAL, slip.0, slip.1)?;
        d.set_var(var::SKIP_PSP, 1, 0)?;
    }
    [var::RET_AFTER_MAP, var::RET_AFTER_CONF, var::ANCILLARY_V1]
        .iter()
        .try_for_each(|&v| d.set_var(v, 1, 0))?;
    if p.map_exact_match {
        d.set_var(var::MAP_EXACT_MATCH, 1, 0)?;
    }
    d.set_var(var::PHY_PROG, p.phy_prog as u32, 0)?;
    if milan {
        d.set_var(var::SKIP_PSP, p.skip_psp as u32, 0)?;
        if let Some(s3) = p.s3_mode {
            d.set_var(var::S3_MODE, s3 as u32, 0)?;
        }
    } else {
        if !p.skip_psp {
            d.set_var(var::SKIP_PSP, 0, 0)?;
        }
        if p.ntb_hotplug {
            d.set_var(var::NTB_HP_EN, 1, 0)?;
        }
    }
    d.load_caps()?;
    unit.load_heaps()?;
    if let Some([reset, rxpoll, l0poll]) = p.training_timers {
        d.set_prop(prop::TRAINING_TIMERS, reset, rxpoll, l0poll)?;
    }
    p.set_conf
        .iter()
        .enumerate()
        .filter_map(|(code, v)| v.map(|v| (code as u32, v)))
        .try_for_each(|(code, v)| d.set_prop(prop::SET_CONF, code, v, 0))?;
    d.sm_start()
}

/// Pass-1 rules: pause at CONFIGURED (PERST on request) or VETTING
/// (deferred PERST, requests ignored).
pub const fn rules(policy: &DxioPolicy) -> LoopRules {
    match policy.perst {
        PerstPolicy::OnRequest => LoopRules {
            pause_at: &[state::CONFIGURED],
            perst: true,
            perst_deassert_delay_ms: policy.perst_deassert_delay_ms,
        },
        PerstPolicy::Deferred => LoopRules {
            pause_at: &[state::VETTING],
            perst: false,
            perst_deassert_delay_ms: 0,
        },
    }
}

/// Two-pass training of every socket.
pub fn train<M, H, T>(
    generation: Generation,
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
    let rules = rules(policy);
    units.iter_mut().try_for_each(|u| {
        setup(generation, u, policy)?;
        run(u, hooks, timer, &rules, policy.timeout_us).map(|_| ())
    })?;
    if policy.perst == PerstPolicy::Deferred {
        hooks.release_all();
    }
    units
        .iter_mut()
        .try_for_each(|u| finish_unit(u, hooks, timer, &rules, policy.timeout_us))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sp3::dxio::lanes::{Engine, Group, Lanes, PcieEngine};
    use crate::sp3::dxio::tests::{FakeSmu, FakeTimer, Recorder, Window};
    use crate::sp3::dxio::{Dxio, op};

    fn board() -> [Engine<'static>; 2] {
        [
            Engine::pcie(
                0,
                Lanes::sp3(Group::P0, 8, 4),
                PcieEngine::new().devfn(3, 1),
            ),
            Engine::pcie(1, Lanes::sp3(Group::G3, 0, 16), PcieEngine::new()),
        ]
    }

    #[test]
    fn speed_fixup_preset_or_gen2() {
        let pcie = |e: Engine<'static>| e.pcie_config().copied().unwrap();
        let auto = || Engine::pcie(0, Lanes::sp3(Group::P0, 0, 4), PcieEngine::new());
        let mut e = auto();
        speed_fixup(&mut e, Some(0x400));
        assert_eq!(pcie(e).extra.eq_search_mode, Some(EQ_SEARCH_PRESET));
        assert_eq!(pcie(e).max_speed, Speed::Auto);
        // Bits 10 and 12 clear: Gen2 cap, no EQ mode.
        let mut e = auto();
        speed_fixup(&mut e, Some(!0x1400));
        assert_eq!(pcie(e).max_speed, Speed::Gen2);
        assert_eq!(pcie(e).extra.eq_search_mode, None);
        // Skipped fuse test (Milan PCD 0xAF = 0x40).
        let mut e = auto();
        speed_fixup(&mut e, None);
        assert_eq!(pcie(e).extra.eq_search_mode, Some(EQ_SEARCH_PRESET));
        // Gen1/Gen2 board caps are untouched.
        let mut e = Engine::pcie(
            0,
            Lanes::sp3(Group::P0, 0, 4),
            PcieEngine::new().max_speed(Speed::Gen1),
        );
        speed_fixup(&mut e, Some(0));
        assert_eq!(pcie(e), PcieEngine::new().max_speed(Speed::Gen1));
    }

    const SM: [[u32; 3]; 4] = [
        [3, 2, state::MAPPED],
        [3, 4, 1],
        [3, 2, state::CONFIGURED],
        [3, 2, state::DONE],
    ];

    #[test]
    fn rome_two_pass() {
        let (s0, s1) = (
            FakeSmu::default().script(&SM[..3]),
            FakeSmu::default().script(&SM[..3]),
        );
        let (mut w0, mut w1) = (Window::new(), Window::new());
        let policy = DxioPolicy::new(Generation::Rome);
        let mk = |smu, socket, w: &mut Window| {
            Unit::new(
                Generation::Rome,
                (socket, 0),
                Dxio::new(smu, DXIO_MSG),
                w.dma(),
                &board(),
                &policy,
                &Default::default(),
            )
            .unwrap()
            .unwrap()
        };
        let mut units = [mk(&s0, 0, &mut w0), mk(&s1, 1, &mut w1)];
        let mut hooks = Recorder::default();
        train(
            Generation::Rome,
            &mut units,
            &mut hooks,
            &FakeTimer::default(),
            &policy,
        )
        .unwrap();

        use op::*;
        let setup = [
            GET_VERSION,
            INIT,
            SET_RUNTIME_PROP,
            SET_VARIABLE,
            SET_VARIABLE,
            SET_VARIABLE,
            SET_VARIABLE,
            SET_VARIABLE,
            SET_VARIABLE,
            LOAD_CAPS,
            LOAD_DATA,
            LOAD_DATA,
            LOAD_DATA,
            SET_RUNTIME_PROP,
            SM_START,
        ];
        let pass1 = [
            GET_SM_STATE,
            GET_ENGINE_CFG,
            SM_RESUME,
            GET_SM_STATE,
            SM_RESUME,
            GET_SM_STATE,
        ];
        let pass2 = [SM_RESUME, GET_SM_STATE, GET_ENGINE_CFG];
        let expect = |reload: bool| {
            let mut v: std::vec::Vec<u32> = reload.then_some(RELOAD_SM).into_iter().collect();
            v.extend(setup.iter().chain(&pass1).chain(&pass2));
            v
        };
        assert_eq!(
            s0.ops(),
            expect(true),
            "ReloadSm on the early-train socket only"
        );
        assert_eq!(s1.ops(), expect(false));
        // Both sockets reach CONFIGURED before either resumes.
        assert_eq!(
            hooks.events,
            [
                ("mapped", 0),
                ("perst", 0),
                ("configured", 0),
                ("mapped", 0x10),
                ("perst", 0x10),
                ("configured", 0x10)
            ]
        );
        let a = s0.args();
        assert_eq!(
            a[3],
            [SET_RUNTIME_PROP, 1, 3, 0, 0, 0],
            "Rome gating from the uninitialized PCD"
        );
        assert_eq!(a[4], [SET_VARIABLE, var::SLIP_INTERVAL, 0, 0, 0, 0]);
        assert_eq!(a[5][1], var::SKIP_PSP);
        assert_eq!(a[9], [SET_VARIABLE, var::PHY_PROG, 1, 0, 0, 0]);
        assert_eq!(
            a[11],
            [LOAD_DATA, 0, 0, 0, 1, 2],
            "heap 2 empty, no ancillary"
        );
        assert_eq!(a[13][5], 3);
        assert_eq!(a[13][3], (0x18 + 0x28) / 4);
        assert_eq!(a[14], [SET_RUNTIME_PROP, 1, 0, 9, 1, 0], "SET_CONF 9 = 1");
        assert_eq!(units[0].version, Some((1, 2)));
    }

    /// Firmware readback → port map → remap on the NBIO, end to end.
    #[test]
    fn mapped_ports_reach_the_remap() {
        use crate::sp3::dxio::PcieReset;
        use crate::sp3::dxio::ports::PortSetup;
        struct Count(u32);
        impl PcieReset for Count {
            fn release(&mut self, _: u8, _: u8) {
                self.0 += 1;
            }
        }
        let mut w = Window::new();
        let smu = FakeSmu {
            dma: Some(w.0.as_mut_ptr()),
            ports: vec![(0, 4)],
            ..FakeSmu::default()
        }
        .script(&SM[..3]);
        let policy = DxioPolicy::new(Generation::Rome);
        let b = board();
        let mut units = [Unit::new(
            Generation::Rome,
            (0, 0),
            Dxio::new(&smu, DXIO_MSG),
            w.dma(),
            &b,
            &policy,
            &Default::default(),
        )
        .unwrap()
        .unwrap()];
        let (m, n) = crate::sp3::nbio::tests::nbio(Generation::Rome, 0, 0);
        let nbios = [n];
        let mut setup = PortSetup::new(Generation::Rome, &nbios, &b, Count(0));
        train(
            Generation::Rome,
            &mut units,
            &mut setup,
            &FakeTimer::default(),
            &policy,
        )
        .unwrap();
        setup.finish();
        let p = setup.ports()[0];
        assert_eq!((p.core, p.port, p.devfn), (1, 4, 0x19));
        assert_eq!(
            m.0.borrow().smn[&(0, 0x13B1_00B8 + 4 * 12)],
            0x19,
            "3.1 on port 8 + 4"
        );
        assert_eq!(setup.reset.0, 1, "PERST released once");
        assert_eq!(units[0].engines()[0].mac_port_id, 4);
    }

    #[test]
    fn milan_requests_and_deferred_perst() {
        let smu = FakeSmu::default().script(&[
            [3, 2, state::CONFIGURED],
            [3, 4, 1],
            [3, 2, state::VETTING],
        ]);
        let mut w = Window::new();
        let mut policy = DxioPolicy::new(Generation::Milan);
        policy.perst = PerstPolicy::Deferred;
        let mut units = [Unit::new(
            Generation::Milan,
            (0, 0),
            Dxio::new(&smu, DXIO_MSG),
            w.dma(),
            &board(),
            &policy,
            &Default::default(),
        )
        .unwrap()
        .unwrap()];
        let mut hooks = Recorder::default();
        train(
            Generation::Milan,
            &mut units,
            &mut hooks,
            &FakeTimer::default(),
            &policy,
        )
        .unwrap();
        let vars: std::vec::Vec<(u32, u32, u32)> = smu
            .args()
            .iter()
            .filter(|a| a[0] == op::SET_VARIABLE)
            .map(|a| (a[1], a[2], a[3]))
            .collect();
        assert_eq!(
            vars,
            [
                (var::PCIE_COMPL, 1, 0),
                (var::SLIP_INTERVAL, 0, 0),
                (var::POWER_OFF_DELAY, 5, 0),
                (var::RET_AFTER_MAP, 1, 0),
                (var::RET_AFTER_CONF, 1, 0),
                (var::ANCILLARY_V1, 1, 0),
                (var::MAP_EXACT_MATCH, 1, 0),
                (var::PHY_PROG, 1, 0),
                (var::SKIP_PSP, 1, 0),
            ]
        );
        assert!(
            smu.args()
                .contains(&[op::SET_RUNTIME_PROP, 1, 3, 0x4F, 0x4F, 0])
        );
        // Deferred: no PERST hook, pause at VETTING, release between passes.
        assert_eq!(hooks.events, [("configured", 0), ("release", 0xFF)]);
    }
}
