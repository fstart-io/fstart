//! SP3 DXIO/PCIe: how board lanes become DXIO engines, the orchestration
//! of the firmware (PCIE_DXIO_SP3) and the PCIe work around it. The
//! firmware client, wire records and event loop are [`crate::dxio`].
//!
//! The generations differ in the request sequence and orchestration:
//!
//! - Naples ([`naples`]): each die runs to completion before the next
//!   (PCD 0x1E = 0, the donor path), or all dies in two passes;
//! - Rome/Milan ([`sp3`]): every socket is set up and run to CONFIGURED,
//!   then every socket is resumed to DONE (two-pass, §3.3).
//!
//! Layout: [`engine`] (per-generation encoding, tuning, heaps), [`lanes`]
//! (typed board lanes, translation, validation), [`ports`] (devfn remap,
//! bridges, straps, post-training, SATA, xGMI), [`hotplug`] (SMU hotplug
//! tables/messages). No ALIB: AGESA keeps the AML defaults on SP3 (§3.12).
//!
//! Flow for the integrator: build [`Unit`]s (one per die / socket with
//! engines), call [`train`] with a [`ports::PortSetup`] as the hooks, then
//! [`ports::PortSetup::finish`] and, with hotplug slots, [`hotplug`].

pub mod engine;
pub mod hotplug;
pub mod lanes;
pub mod naples;
pub mod ports;
pub mod sp3;
#[rustfmt::skip]
pub mod tables;

use fstart_core::services::Timer;
use heapless::Vec;

use crate::cpu::Silicon;
use crate::dxio::wire::{Heaps, MAX_ENGINES};
use crate::dxio::{DmaBuffer, Dxio, DxioError, Hooks, TIMEOUT_US, Unit};
use crate::smu::SmuMessages;
use engine::EncodePolicy;
use lanes::Engine;

/// When PERST# is released (§3.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PerstPolicy {
    /// On the firmware's type-4 requests (donor default; Rome PCD 0x160 =
    /// 0, Milan 0x18F = 0). Pass 1 ends at CONFIGURED.
    #[default]
    OnRequest,
    /// Once for all sockets between the passes; pass 1 ends at VETTING and
    /// type-4 requests are ignored.
    Deferred,
}

/// Rome/Milan variables and runtime properties (§3.2, §3.3).
/// [`Sp3Policy::new`] gives the generation's PEI build defaults; the field
/// docs give the Rome / Milan PCD tokens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sp3Policy {
    /// 0x15D / 0x8D (1): SKIP_PSP.
    pub skip_psp: bool,
    /// 0x15E / 0x93 (1): PHY_PROG.
    pub phy_prog: u8,
    /// 0x161 (0) / 0xB3 (1): MAP_EXACT_MATCH.
    pub map_exact_match: bool,
    /// 0xB4 && 0xB7 / any quadrant NTB pair (0): NTB_HP_EN.
    pub ntb_hotplug: bool,
    /// Milan 0x94 (0xFF = not sent): S3_MODE.
    pub s3_mode: Option<u8>,
    /// Milan 0x56 (1): PCIE_COMPL.
    pub pcie_compliance: bool,
    /// (0x157, 0x158) / (0x57, 0x58): SLIP_INTERVAL; `None` sends (0, 0).
    pub slip_interval: Option<u8>,
    /// Milan (0xA5, 0x90) = (5, 0): POWER_OFF_DELAY. Oxide uses (0, 0).
    pub power_off_delay: (u8, u8),
    /// `{1, 3, mask, value}`: Rome 0x155 (1) with PCD16 0x156, which is
    /// uninitialized (0) in the build; Milan 0x59 (1) with 0x5A = 0x4F4F.
    /// `None` = not sent.
    pub clock_gating: Option<(u8, u8)>,
    /// `{1, 2, reset, rxpoll, l0poll}`: 0x141 / 0x10F (0).
    pub training_timers: Option<[u32; 3]>,
    /// SET_CONF `{1, 0, code, value}` per code; `None` = not sent. Rome
    /// codes 4/5/9/0xB (0x111, 0x162, 0x163, 0x15A); Milan 3, 4, 5, 7, 8,
    /// 9, 0xA, 0xB (0x62, 0x5F, 0x5D, !0x60, 0x12F, 0x61, 0x5B, 0x5C).
    /// Default: code 9 = 1 on both.
    pub set_conf: [Option<u32>; 12],
}

impl Sp3Policy {
    pub const fn new(generation: Silicon) -> Self {
        let milan = matches!(generation, Silicon::Gn);
        let mut set_conf = [None; 12];
        set_conf[9] = Some(1);
        Self {
            skip_psp: true,
            phy_prog: 1,
            map_exact_match: milan,
            ntb_hotplug: false,
            s3_mode: None,
            pcie_compliance: true,
            slip_interval: None,
            power_off_delay: (5, 0),
            clock_gating: Some(if milan { (0x4F, 0x4F) } else { (0, 0) }),
            training_timers: None,
            set_conf,
        }
    }
}

/// DXIO platform policy. [`DxioPolicy::new`] gives the donor build
/// defaults of a generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DxioPolicy {
    pub perst: PerstPolicy,
    /// Rome PCD16 0x15F / Milan 0x158: ms waited once per loop call after a
    /// PERST deassert (0).
    pub perst_deassert_delay_ms: u16,
    /// The unit that gets ReloadSm (Platform `+0x14` / Milan `+5`):
    /// `(socket, die)`. AGESA sends it to socket/die 0 by default.
    pub early_train_unit: Option<(u8, u8)>,
    /// Rome/Milan ReloadSm argument: Platform `+0x15 > 0x7F`. Rome copies
    /// `+0x15` from PCD 0xBA ("BMC link location", build default 0x81 =
    /// WAFL port 1, which Tyan keeps), so the donor sends 1 (`ffdfd156`,
    /// `ffe01ce6`). The Milan source is not decoded.
    pub reload_sm_arg: bool,
    /// Naples PCD 0x1E (0): train all dies concurrently.
    pub naples_concurrent: bool,
    pub encode: EncodePolicy,
    pub sp3: Sp3Policy,
    /// Optional board-selected Milan pre-training PCIe capabilities.
    pub pcie_features: Option<ports::PcieFeatures>,
    /// SMU hotplug settings, for boards with I2C-signaled hotplug slots.
    pub hotplug: hotplug::HotplugPolicy,
    pub timeout_us: u64,
}

impl DxioPolicy {
    pub const fn new(generation: Silicon) -> Self {
        Self {
            perst: PerstPolicy::OnRequest,
            perst_deassert_delay_ms: 0,
            early_train_unit: Some((0, 0)),
            reload_sm_arg: matches!(generation, Silicon::Ssp),
            naples_concurrent: false,
            encode: EncodePolicy {
                pspp_policy: 0,
                milan_reset_group_one: false,
                milan_tuning_absolute: false,
            },
            sp3: Sp3Policy::new(generation),
            pcie_features: None,
            hotplug: hotplug::HotplugPolicy {
                i2c_switch_base: 0,
                slot_base: 0,
                poll: false,
                poll_interval: 0,
                naples_gpio_polling: false,
                debug_flags: 0,
                firmware_first: false,
                enable_bit6: false,
            },
            timeout_us: TIMEOUT_US,
        }
    }
}

/// Releases the PCIe slot resets (PERST#) on the firmware's type-4
/// requests or between the passes (§3.4). The donor never pulses them: it
/// drives the FCH PCIE_RST pins high (Rome/Milan) or selects the native
/// PCIE_RST0_L function (Naples). See [`ports::FchPerst`].
pub trait PcieReset {
    fn release(&mut self, socket: u8, die: u8);
}

impl<'s, M: SmuMessages + ?Sized> Unit<'s, M> {
    /// Encode the board engines of `(socket, die)` and place both heaps in
    /// `dma`, with the platform-wide `ancillary` data (for example the
    /// optional APOB PHY override) next to the board tuning. `None` when the
    /// unit has no engines (AGESA skips it).
    pub fn new(
        generation: Silicon,
        (socket, die): (u8, u8),
        dxio: Dxio<'s, M>,
        dma: DmaBuffer,
        board: &[Engine<'_>],
        policy: &DxioPolicy,
        ancillary: &engine::Ancillary<'_>,
    ) -> Result<Option<Self>, DxioError> {
        lanes::validate(generation, board).map_err(|(i, e)| DxioError::Lanes(i, e))?;
        let mine = || {
            board
                .iter()
                .enumerate()
                .filter(move |(_, e)| lanes::unit_of(generation, e) == (socket, die))
        };
        let mut placed: Vec<(usize, lanes::Placement), MAX_ENGINES> = Vec::new();
        for (i, e) in mine() {
            let p = lanes::place(generation, e).map_err(|err| DxioError::Lanes(i, err))?;
            placed.push((i, p)).map_err(|_| DxioError::TooManyEngines)?;
        }
        if placed.is_empty() {
            return Ok(None);
        }
        let wire = placed.iter().map(|&(i, p)| {
            let e = &board[i];
            (
                engine::encode(generation, e, p.phy, p.wire, &policy.encode),
                e.tuning,
                p.wire,
            )
        });
        let heaps = Heaps::build(generation, wire, ancillary, &policy.encode)?;
        Self::with_heaps(
            socket,
            die,
            dxio,
            dma,
            &heaps,
            placed.iter().map(|&(i, _)| i as u8),
        )
    }
}

/// Train every unit of `generation` (Naples: dies, sequential or two-pass;
/// Rome/Milan: sockets, two-pass).
pub fn train<M, H, T>(
    generation: Silicon,
    units: &mut [Unit<'_, M>],
    hooks: &mut H,
    timer: &T,
    policy: &DxioPolicy,
) -> Result<(), DxioError>
where
    M: SmuMessages + ?Sized,
    H: Hooks<Error = DxioError> + ?Sized,
    T: Timer + ?Sized,
{
    match generation {
        Silicon::Zp => naples::train(units, hooks, timer, policy),
        Silicon::Ssp | Silicon::Gn => sp3::train(generation, units, hooks, timer, policy),
        // Fail closed on a generation this build does not carry.
        #[allow(unreachable_patterns)]
        _ => {
            let _ = (units, hooks, timer, policy);
            Err(DxioError::Unsupported)
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    //! A fake SMU that records messages and plays a DXIO firmware.
    use fstart_core::services::ManualTimer;
    use std::cell::RefCell;
    use std::collections::VecDeque;

    use super::*;
    use crate::dxio::*;
    use crate::smu::Error as SmuError;

    #[derive(Default)]
    pub struct FakeSmu {
        pub log: RefCell<std::vec::Vec<(u32, [u32; 6])>>,
        /// GetSmState replies `[result, type, state]`, in order; empty =
        /// DONE.
        pub sm: RefCell<VecDeque<[u32; 3]>>,
        /// Answer every DXIO request with this result (default OK).
        pub fail_op: Option<u32>,
        /// Written into the engines' `mac_port_id` on GetEngineCfg:
        /// `(engine, port)`; needs `dma`.
        pub ports: std::vec::Vec<(usize, u8)>,
        /// Host pointer of the DMA window at phys `DMA_PHYS`.
        pub dma: Option<*mut u8>,
        /// Heap 3 as loaded, returned by GetEngineCfg.
        pub held: RefCell<std::vec::Vec<u8>>,
    }

    pub const DMA_PHYS: u64 = 0x7000_0000;

    impl FakeSmu {
        pub fn sent(&self) -> std::vec::Vec<(u32, u32)> {
            self.log
                .borrow()
                .iter()
                .map(|(id, a)| (*id, a[0]))
                .collect()
        }

        pub fn args(&self) -> std::vec::Vec<[u32; 6]> {
            self.log.borrow().iter().map(|(_, a)| *a).collect()
        }

        /// DXIO opcodes in order (messages 0x08/0x0F only).
        pub fn ops(&self) -> std::vec::Vec<u32> {
            self.log
                .borrow()
                .iter()
                .filter(|(id, _)| *id == 0x08 || *id == 0x0F)
                .map(|(_, a)| a[0])
                .collect()
        }

        pub fn script(self, sm: &[[u32; 3]]) -> Self {
            *self.sm.borrow_mut() = sm.iter().copied().collect();
            self
        }
    }

    impl SmuMessages for FakeSmu {
        fn send(&self, id: u32, args: &mut [u32; 6]) -> Result<(), SmuError> {
            self.log.borrow_mut().push((id, *args));
            if id != 0x08 && id != 0x0F {
                return Ok(());
            }
            let op = args[0];
            if op == op::GET_SM_STATE {
                let [r, t, s] = self
                    .sm
                    .borrow_mut()
                    .pop_front()
                    .unwrap_or([3, 2, state::DONE]);
                *args = [r, t, s, 0, 0, 0];
                return Ok(());
            }
            if let Some(base) = self.dma {
                let at = (args[2] as u64).wrapping_sub(DMA_PHYS) as usize;
                let len = args[3] as usize * 4;
                if op == op::LOAD_DATA && args[5] == heap::ENGINE_CONFIG {
                    // SAFETY: test DMA window.
                    *self.held.borrow_mut() = (0..len)
                        .map(|i| unsafe { base.add(at + i).read() })
                        .collect();
                }
                if op == op::GET_ENGINE_CFG {
                    // The firmware returns the configuration it holds,
                    // with the ports filled in.
                    let held = self.held.borrow();
                    // SAFETY: test DMA window.
                    held.iter()
                        .enumerate()
                        .for_each(|(i, &b)| unsafe { base.add(at + i).write_volatile(b) });
                    for &(e, port) in &self.ports {
                        // SAFETY: test DMA window.
                        unsafe { base.add(at + 0x10 + e * 0x28 + 0x19).write_volatile(port) };
                    }
                }
            }
            args[0] = if self.fail_op == Some(op) { 2 } else { 3 };
            if op == op::GET_VERSION {
                args[2] = 1;
                args[3] = 2;
            }
            Ok(())
        }
    }

    pub struct Window(pub Box<[u8]>);

    impl Window {
        pub fn new() -> Self {
            Self(vec![0u8; DMA_SIZE].into_boxed_slice())
        }
        pub fn dma(&mut self) -> DmaBuffer {
            // SAFETY: owned by the test for the unit's lifetime.
            unsafe { DmaBuffer::new(self.0.as_mut_ptr(), self.0.len(), DMA_PHYS).unwrap() }
        }
    }

    #[derive(Default)]
    pub struct Recorder {
        pub events: std::vec::Vec<(&'static str, u8)>,
    }

    impl Hooks for Recorder {
        type Error = DxioError;

        fn mapped(&mut self, u: &UnitView<'_>) -> Result<(), DxioError> {
            self.events.push(("mapped", u.socket << 4 | u.die));
            Ok(())
        }
        fn configured(&mut self, u: &UnitView<'_>) -> Result<(), DxioError> {
            self.events.push(("configured", u.socket << 4 | u.die));
            Ok(())
        }
        fn early_trained(&mut self, u: &UnitView<'_>) {
            self.events.push(("early", u.socket << 4 | u.die));
        }
        fn perst(&mut self, u: &UnitView<'_>, _: &[u32; 6]) {
            self.events.push(("perst", u.socket << 4 | u.die));
        }
        fn release_all(&mut self) {
            self.events.push(("release", 0xFF));
        }
    }

    #[test]
    fn global_apob_phy_payload_reaches_each_socket_with_board_tuning() {
        use crate::apob::{
            Apob,
            build::{Body, Builder},
            phy::phy_override,
        };
        use crate::psp::Variant;
        use lanes::{Group, Lanes, PcieEngine, SataEngine};
        let payload = [0x11, 0x22, 0x33, 0x44, 0xaa, 0xbb, 0xcc, 0xdd];
        let data = Builder::new(0x18)
            .entry(
                9,
                0x15,
                0,
                &Body::default()
                    .at(0x30, &8u32.to_le_bytes())
                    .at(0x34, &payload)
                    .0,
            )
            .build();
        let apob = Apob::new(&data, Variant::Gn).unwrap();
        let board = [
            Engine::pcie(0, Lanes::sp3(Group::P0, 0, 4), PcieEngine::new()),
            Engine::sata(1, Lanes::sp3(Group::P1, 0, 8), SataEngine::new(0))
                .tuning(&[(0xc1, 1), (0, 0)]),
        ];
        let anc = engine::Ancillary {
            phy_override: phy_override(&apob).unwrap(),
            ..Default::default()
        };
        for socket in 0..2 {
            let smu = FakeSmu::default();
            let mut window = Window::new();
            let u = Unit::new(
                Silicon::Gn,
                (socket, 0),
                Dxio::new(&smu, 8),
                window.dma(),
                &board,
                &DxioPolicy::new(Silicon::Gn),
                &anc,
            )
            .unwrap()
            .unwrap();
            assert_eq!(u.engines().len(), 1, "only this socket's engines");
            let heap = &window.0[ANCILLARY_OFFSET..];
            assert_eq!(
                &heap[4..8],
                &[5, 1, 3, 0],
                "global PHY type 5 / format 1 / 3 dwords"
            );
            assert_eq!(&heap[8..16], &payload);
            if socket == 1 {
                assert_eq!(u.engines()[0].ancillary_offset.get(), 4);
                assert_eq!(heap[16], 3, "per-engine tuning follows global record");
            } else {
                assert_eq!(u.engines()[0].ancillary_offset.get(), 0);
            }
            u.load_heaps().unwrap();
            assert_eq!(
                smu.args()[0],
                [
                    op::LOAD_DATA,
                    0,
                    (DMA_PHYS + ANCILLARY_OFFSET as u64) as u32,
                    u.anc_dwords,
                    0,
                    heap::ANCILLARY
                ]
            );
        }
        // Both absent and explicitly empty global tables leave an ordinary
        // PCIe unit with no heap-6 RPC, as before.
        for phy in [None, Some(&[][..])] {
            let smu = FakeSmu::default();
            let mut window = Window::new();
            let u = Unit::new(
                Silicon::Gn,
                (0, 0),
                Dxio::new(&smu, 8),
                window.dma(),
                &board,
                &DxioPolicy::new(Silicon::Gn),
                &engine::Ancillary {
                    phy_override: phy,
                    ..Default::default()
                },
            )
            .unwrap()
            .unwrap();
            assert_eq!(u.anc_dwords, 0);
            u.load_heaps().unwrap();
            assert!(
                !smu.args()
                    .iter()
                    .any(|a| a[0] == op::LOAD_DATA && a[5] == heap::ANCILLARY)
            );
        }
    }

    #[test]
    fn opcode_names_and_results() {
        let smu = FakeSmu::default();
        let d = Dxio::new(&smu, 0x08);
        d.init().unwrap();
        assert_eq!(d.version().unwrap(), (1, 2));
        d.set_var(var::PHY_PROG, 1, 0).unwrap();
        assert_eq!(smu.args()[0][0], 0x00, "0x00 is Init");
        assert_eq!(smu.args()[1][0], 0x13, "0x13 is GetVersion");
        assert_eq!(smu.args()[2], [0x22, 0x14, 1, 0, 0, 0]);
        assert_eq!(result_of(0x0000_0301), result::OK);
        let bad = FakeSmu {
            fail_op: Some(op::LOAD_CAPS),
            ..Default::default()
        };
        assert_eq!(
            Dxio::new(&bad, 8).load_caps(),
            Err(DxioError::Rpc {
                op: op::LOAD_CAPS,
                result: 2
            })
        );
    }

    #[test]
    fn event_loop_errors_and_timeout() {
        let mut w = Window::new();
        let rules = LoopRules {
            pause_at: &[],
            perst: true,
            perst_deassert_delay_ms: 0,
            resume_delay_us: 0,
        };
        let heaps = Heaps::build(
            Silicon::Ssp,
            core::iter::empty(),
            &Default::default(),
            &Default::default(),
        )
        .unwrap();
        let run_with = |smu: &FakeSmu, w: &mut Window, timeout| {
            let mut u = Unit::with_heaps(
                0,
                0,
                Dxio::new(smu, 8),
                w.dma(),
                &heaps,
                core::iter::empty(),
            )
            .unwrap()
            .unwrap();
            run(
                &mut u,
                &mut Recorder::default(),
                &ManualTimer::ticking(),
                &rules,
                timeout,
            )
        };
        let smu = FakeSmu::default().script(&[[2, 2, 3]]);
        assert_eq!(
            run_with(&smu, &mut w, TIMEOUT_US),
            Err(DxioError::StateMachine { state: 3 })
        );
        let smu = FakeSmu::default().script(&[[5, 0, 0]]);
        assert_eq!(
            run_with(&smu, &mut w, TIMEOUT_US),
            Err(DxioError::NoReturn(5))
        );
        // Error with another type, generic and HPSM replies resume.
        let smu = FakeSmu::default().script(&[[2, 1, 0], [3, 1, 0], [3, 3, 0], [3, 0, 0]]);
        assert_eq!(run_with(&smu, &mut w, TIMEOUT_US), Ok(Stop::Done));
        assert_eq!(smu.ops().iter().filter(|&&o| o == op::SM_RESUME).count(), 3);
        let spin = FakeSmu::default().script(&[[3, 1, 0]; 64]);
        assert_eq!(run_with(&spin, &mut w, 20), Err(DxioError::Timeout));
    }
}
