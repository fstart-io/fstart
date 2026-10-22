//! DXIO/PCIe: the DXIO firmware client, engine descriptors, the SP3 lane
//! map, port setup and hotplug (PCIE_DXIO_SP3).
//!
//! DXIO is an RPC carried by one SMU message: 0x0F per ZP die on Naples,
//! 0x08 per IOD (socket) on Rome and Milan (§3.1). All three generations
//! share the opcodes, the 0x28-byte engine record and the state machine;
//! they differ in the request sequence and orchestration:
//!
//! - Naples ([`naples`]): each die runs to completion before the next
//!   (PCD 0x1E = 0, the donor path), or all dies in two passes;
//! - Rome/Milan ([`sp3`]): every socket is set up and run to CONFIGURED,
//!   then every socket is resumed to DONE (two-pass, §3.3).
//!
//! Layout: [`engine`] (wire format, heaps), [`lanes`] (typed board lanes,
//! translation, validation), [`ports`] (devfn remap, bridges, straps,
//! post-training, SATA, xGMI), [`hotplug`] (SMU hotplug tables/messages).
//! No ALIB: AGESA keeps the AML defaults on SP3 (§3.12).
//!
//! Flow for the integrator: build [`Unit`]s (one per die / socket with
//! engines), call [`train`] with a [`ports::PortSetup`] as the hooks, then
//! [`ports::PortSetup::finish`] and, with hotplug slots, [`hotplug`].

pub mod engine;
pub mod hotplug;
pub mod lanes;
#[cfg(feature = "naples")]
pub mod naples;
pub mod ports;
#[cfg(any(feature = "rome", feature = "milan"))]
pub mod sp3;
#[rustfmt::skip]
pub mod tables;

use core::ptr::NonNull;
use core::sync::atomic::{Ordering, compiler_fence};

use fstart_core::services::Timer;
use heapless::Vec;
use zerocopy::FromBytes;

use crate::smu::{SMU_ARGS, SmuError, SmuMessages};
use crate::soc::Generation;
use engine::{ANCILLARY_MAX, ENGINE_HEAP_MAX, EncodePolicy, FwEngine, Heaps, MAX_ENGINES};
use lanes::{Engine, LaneError};

/// DXIO opcodes (`args[0]`). Names as in Oxide's `milan/dxio_impl.h`,
/// checked against the donor's use of each (§3.1).
pub mod op {
    /// Initialize the firmware. fstart's Zeppelin port calls this
    /// "Version".
    pub const INIT: u32 = 0x00;
    pub const GET_SM_STATE: u32 = 0x09;
    /// `| SINGLE` for one engine.
    pub const SET_LINK_SPEED: u32 = 0x10;
    pub const SET_LINK_SPEED_SINGLE: u32 = 0x800;
    /// Reply `[2]`, `[3]` = major, minor. fstart's Zeppelin port calls this
    /// "Init".
    pub const GET_VERSION: u32 = 0x13;
    pub const GET_ENGINE_CFG: u32 = 0x14;
    pub const SET_VARIABLE: u32 = 0x22;
    pub const LOAD_DATA: u32 = 0x23;
    pub const LOAD_CAPS: u32 = 0x24;
    /// Only on the early-train unit.
    pub const RELOAD_SM: u32 = 0x2D;
    /// Naples post-training per port (packages other than 4/8; unused on
    /// SP3).
    pub const NAPLES_POST_TRAINING: u32 = 0x34;
    pub const SET_RUNTIME_PROP: u32 = 0x3A;
    /// Milan 2P xGMI (meaning unnamed, "xGMI18GACOFC", §3.13).
    pub const XGMI_BER_ADAPT: u32 = 0x40;
    pub const INIT_ESM: u32 = 0x53;
    pub const SM_START: u32 = 0x307;
    pub const SM_RESUME: u32 = 0x308;
}

/// Heap types of LOAD_DATA.
pub mod heap {
    pub const MACPCS: u32 = 2;
    pub const ENGINE_CONFIG: u32 = 3;
    pub const GPIO: u32 = 5;
    pub const ANCILLARY: u32 = 6;
}

/// SET_VARIABLE variables (Oxide names, §3.2).
pub mod var {
    pub const SKIP_PSP: u32 = 0x0D;
    pub const RET_AFTER_MAP: u32 = 0x0E;
    pub const RET_AFTER_CONF: u32 = 0x0F;
    pub const ANCILLARY_V1: u32 = 0x10;
    pub const NTB_HP_EN: u32 = 0x11;
    pub const MAP_EXACT_MATCH: u32 = 0x12;
    pub const S3_MODE: u32 = 0x13;
    pub const PHY_PROG: u32 = 0x14;
    pub const PCIE_COMPL: u32 = 0x23;
    pub const SLIP_INTERVAL: u32 = 0x24;
    pub const POWER_OFF_DELAY: u32 = 0x25;
}

/// SET_RUNTIME_PROP `{engine, sub}` groups.
pub mod prop {
    pub const SET_CONF: u32 = 0;
    pub const TRAINING_TIMERS: u32 = 2;
    pub const CLOCK_GATING: u32 = 3;
    /// `args[1]` of the xGMI requests.
    pub const XGMI: u32 = 0x22;
}

/// GetSmState reply types (Oxide `zen/dxio.h`).
pub mod reply {
    pub const NONE: u32 = 0;
    pub const GENERIC: u32 = 1;
    pub const SM: u32 = 2;
    pub const HPSM: u32 = 3;
    pub const RESET: u32 = 4;
}

/// State machine states (Oxide names).
pub mod state {
    pub const INIT: u32 = 0x00;
    pub const DISABLED: u32 = 0x01;
    pub const SCANNED: u32 = 0x02;
    pub const CANNED: u32 = 0x03;
    pub const LOADED: u32 = 0x04;
    pub const CONFIGURED: u32 = 0x05;
    pub const IN_EARLY_TRAIN: u32 = 0x06;
    pub const EARLY_TRAINED: u32 = 0x07;
    pub const VETTING: u32 = 0x08;
    pub const GET_VET: u32 = 0x09;
    pub const NO_VET: u32 = 0x0A;
    pub const GPIO_INIT: u32 = 0x0B;
    pub const NHP_TRAIN: u32 = 0x0C;
    pub const DONE: u32 = 0x0D;
    pub const ERROR: u32 = 0x0E;
    pub const MAPPED: u32 = 0x0F;
}

/// DXIO result codes (`args[0]` low byte).
pub mod result {
    pub const ERROR: u32 = 2;
    pub const OK: u32 = 3;
}

/// The donor waits forever; fstart bounds the state machine at 60 s.
pub const TIMEOUT_US: u64 = 60_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DxioError {
    Smu(SmuError),
    /// A request answered with a result other than OK.
    Rpc {
        op: u32,
        result: u32,
    },
    /// GetSmState answered neither OK nor error ("Mailbox did not return").
    NoReturn(u32),
    /// The state machine reported an error (Rome/Milan return 8, Naples 6).
    StateMachine {
        state: u32,
    },
    Timeout,
    /// Board engine `.0` rejected.
    Lanes(usize, LaneError),
    TooManyEngines,
    /// Unsupported tuning ID.
    Tuning(u8),
    /// DMA buffer too small or not reachable below 4 GiB.
    Buffer,
    /// The generation is not built into this firmware.
    Unsupported,
    /// A requested devfn is not in the default devfn table.
    BadDevfn(u8),
}

impl From<SmuError> for DxioError {
    fn from(e: SmuError) -> Self {
        Self::Smu(e)
    }
}

/// A DMA window the SMU reads the heaps from and writes GetEngineCfg into.
/// Accesses are volatile: the firmware writes it behind the compiler's back.
pub struct DmaBuffer {
    ptr: NonNull<u8>,
    len: usize,
    phys: u32,
}

/// Bytes a [`DmaBuffer`] needs: heap 3, then heap 6 at [`ANCILLARY_OFFSET`].
pub const DMA_SIZE: usize = ANCILLARY_OFFSET + ANCILLARY_MAX;
/// Heap 6 offset in the DMA window.
pub const ANCILLARY_OFFSET: usize = ENGINE_HEAP_MAX.next_multiple_of(64);

impl DmaBuffer {
    /// # Safety
    /// `ptr..ptr + len` is memory the caller owns for the buffer's lifetime,
    /// not accessed through other references, and visible to the SMU at
    /// physical address `phys`.
    pub unsafe fn new(ptr: *mut u8, len: usize, phys: u64) -> Result<Self, DxioError> {
        let fits = len >= DMA_SIZE && phys.checked_add(len as u64).is_some_and(|e| e <= 1 << 32);
        match NonNull::new(ptr) {
            Some(ptr) if fits => Ok(Self {
                ptr,
                len,
                phys: phys as u32,
            }),
            _ => Err(DxioError::Buffer),
        }
    }

    pub fn phys(&self, offset: usize) -> u32 {
        self.phys + offset as u32
    }

    fn write(&self, offset: usize, data: &[u8]) {
        assert!(offset + data.len() <= self.len);
        data.iter().enumerate().for_each(|(i, &b)| {
            // SAFETY: in bounds; constructor contract.
            unsafe { self.ptr.as_ptr().add(offset + i).write_volatile(b) }
        });
        compiler_fence(Ordering::SeqCst);
    }

    fn read(&self, offset: usize, out: &mut [u8]) {
        assert!(offset + out.len() <= self.len);
        compiler_fence(Ordering::SeqCst);
        out.iter_mut().enumerate().for_each(|(i, b)| {
            // SAFETY: in bounds; constructor contract.
            *b = unsafe { self.ptr.as_ptr().add(offset + i).read_volatile() }
        });
    }
}

impl DxioError {
    /// A name and up to two numbers, for logs without `Debug`.
    pub fn parts(&self) -> (&'static str, u32, u32) {
        match *self {
            Self::Smu(SmuError::Timeout) => ("SMU mailbox timeout", 0, 0),
            Self::Smu(SmuError::Status(status)) => ("SMU mailbox status", status, 0),
            Self::Rpc { op, result } => ("RPC result", op, result),
            Self::NoReturn(r) => ("GetSmState no return", r, 0),
            Self::StateMachine { state } => ("state machine error", state, 0),
            Self::Timeout => ("state machine timeout", 0, 0),
            Self::Lanes(engine, _) => ("board engine rejected", engine as u32, 0),
            Self::TooManyEngines => ("too many engines", 0, 0),
            Self::Tuning(id) => ("unsupported tuning", id as u32, 0),
            Self::Buffer => ("DMA buffer", 0, 0),
            Self::Unsupported => ("generation not built", 0, 0),
            Self::BadDevfn(d) => ("devfn not a root port", d as u32, 0),
        }
    }
}

/// The DXIO RPC of one SMU instance.
pub struct Dxio<'s, M: ?Sized> {
    smu: &'s M,
    msg: u32,
}

impl<'s, M: SmuMessages + ?Sized> Dxio<'s, M> {
    /// DXIO over `smu` with SMU message `msg` (V9 0x0F, V11 0x08).
    pub fn new(smu: &'s M, msg: u32) -> Self {
        Self { smu, msg }
    }

    /// Send `op` with request words `req`; returns the six reply words
    /// without judging the result.
    pub fn raw(&self, op: u32, req: [u32; 5]) -> Result<[u32; SMU_ARGS], DxioError> {
        let mut args = [op, req[0], req[1], req[2], req[3], req[4]];
        self.smu.send(self.msg, &mut args).inspect_err(|e| {
            let (status, timeout) = match e {
                SmuError::Status(s) => (*s, false),
                SmuError::Timeout => (0, true),
            };
            fstart_log::error!(
                "dxio: SMU message {:#x} op {:#x}: status {:#x} timeout {}",
                self.msg,
                op,
                status,
                timeout
            );
        })?;
        Ok(args)
    }

    /// Send `op` and require result OK. Rome normalizes
    /// `(word & 0xFFFFFF00) == 0x300` to OK; the low byte carries the code.
    pub fn rpc(&self, op: u32, req: [u32; 5]) -> Result<[u32; SMU_ARGS], DxioError> {
        let r = self.raw(op, req)?;
        fstart_log::info!(
            "dxio: op {:#x} req {:x} {:x} {:x} {:x} {:x} -> {:x} {:x} {:x} {:x} {:x} {:x}",
            op,
            req[0],
            req[1],
            req[2],
            req[3],
            req[4],
            r[0],
            r[1],
            r[2],
            r[3],
            r[4],
            r[5]
        );
        match result_of(r[0]) {
            result::OK => Ok(r),
            result => Err(DxioError::Rpc { op, result }),
        }
    }

    pub fn init(&self) -> Result<(), DxioError> {
        self.rpc(op::INIT, [0; 5]).map(|_| ())
    }

    /// `(major, minor)`.
    pub fn version(&self) -> Result<(u32, u32), DxioError> {
        self.rpc(op::GET_VERSION, [0; 5]).map(|r| (r[2], r[3]))
    }

    pub fn set_var(&self, var: u32, value: u32, value2: u32) -> Result<(), DxioError> {
        self.rpc(op::SET_VARIABLE, [var, value, value2, 0, 0])
            .map(|_| ())
    }

    /// `{1, sub, a, b, c}`.
    pub fn set_prop(&self, sub: u32, a: u32, b: u32, c: u32) -> Result<(), DxioError> {
        self.rpc(op::SET_RUNTIME_PROP, [1, sub, a, b, c])
            .map(|_| ())
    }

    pub fn load_caps(&self) -> Result<(), DxioError> {
        self.rpc(op::LOAD_CAPS, [0; 5]).map(|_| ())
    }

    /// LOAD_DATA of `dwords` at `addr` (below 4 GiB).
    pub fn load_heap(&self, heap: u32, addr: u32, dwords: u32) -> Result<(), DxioError> {
        self.rpc(op::LOAD_DATA, [0, addr, dwords, 0, heap])
            .map(|_| ())
    }

    /// An empty heap (flag 1): heaps 2 and 5 are always sent this way.
    pub fn load_empty(&self, heap: u32) -> Result<(), DxioError> {
        self.rpc(op::LOAD_DATA, [0, 0, 0, 1, heap]).map(|_| ())
    }

    pub fn get_engine_cfg(&self, addr: u32, dwords: u32) -> Result<(), DxioError> {
        self.rpc(op::GET_ENGINE_CFG, [0, addr, dwords, 0, 0])
            .map(|_| ())
    }

    pub fn reload_sm(&self, arg: u32) -> Result<(), DxioError> {
        self.rpc(op::RELOAD_SM, [arg, 0, 0, 0, 0]).map(|_| ())
    }

    pub fn sm_start(&self) -> Result<(), DxioError> {
        self.rpc(op::SM_START, [0; 5]).map(|_| ())
    }

    pub fn sm_resume(&self) -> Result<(), DxioError> {
        self.rpc(op::SM_RESUME, [0; 5]).map(|_| ())
    }
}

const fn result_of(word: u32) -> u32 {
    if word & 0xFFFF_FF00 == 0x300 {
        result::OK
    } else {
        word & 0xFF
    }
}

/// What the hooks see of a DXIO instance.
pub struct UnitView<'u> {
    pub socket: u8,
    /// Die within the socket (Naples; 0 on Rome/Milan).
    pub die: u8,
    /// The wire engines (after a GetEngineCfg readback where one happened).
    pub engines: &'u [FwEngine],
    /// Board engine index of each wire engine.
    pub board: &'u [u8],
}

/// x86 work at state machine events (§3.4). Every event is followed by a
/// resume unless the orchestrator pauses there.
pub trait Hooks {
    /// State 0xF MAPPED, after the GetEngineCfg readback: port mapping,
    /// devfn remap, straps (`DxioPortMapping`, `DxioCfgBeforeReconfig`).
    fn mapped(&mut self, _unit: &UnitView<'_>) -> Result<(), DxioError> {
        Ok(())
    }
    /// State 5 CONFIGURED (`DxioCfgAfterReconfig`).
    fn configured(&mut self, _unit: &UnitView<'_>) -> Result<(), DxioError> {
        Ok(())
    }
    /// State 7 EARLY_TRAINED.
    fn early_trained(&mut self, _unit: &UnitView<'_>) {}
    /// Type 4: a PERST assert/deassert request; `reply` is the raw reply.
    fn perst(&mut self, _unit: &UnitView<'_>, _reply: &[u32; SMU_ARGS]) {}
    /// Deferred PERST policy: release every socket's resets between the
    /// passes (Rome PCD 0x160, Milan 0x18F "Deassert all resets").
    fn release_all(&mut self) {}
}

/// No-op hooks.
impl Hooks for () {}

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
    pub const fn new(generation: Generation) -> Self {
        let milan = matches!(generation, Generation::Milan);
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
    pub timeout_us: u64,
}

impl DxioPolicy {
    pub const fn new(generation: Generation) -> Self {
        Self {
            perst: PerstPolicy::OnRequest,
            perst_deassert_delay_ms: 0,
            early_train_unit: Some((0, 0)),
            reload_sm_arg: matches!(generation, Generation::Rome),
            naples_concurrent: false,
            encode: EncodePolicy {
                pspp_policy: 0,
                milan_reset_group_one: false,
                milan_tuning_absolute: false,
            },
            sp3: Sp3Policy::new(generation),
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

/// One DXIO instance: a Naples die or a Rome/Milan socket.
pub struct Unit<'s, M: ?Sized> {
    pub socket: u8,
    pub die: u8,
    pub dxio: Dxio<'s, M>,
    dma: DmaBuffer,
    engine_dwords: u32,
    anc_dwords: u32,
    engines: Vec<FwEngine, MAX_ENGINES>,
    board: Vec<u8, MAX_ENGINES>,
    paused: bool,
    /// GetVersion reply (Rome/Milan; Naples after Init).
    pub version: Option<(u32, u32)>,
}

impl<'s, M: SmuMessages + ?Sized> Unit<'s, M> {
    /// Encode the board engines of `(socket, die)` and place both heaps in
    /// `dma`. `None` when the unit has no engines (AGESA skips it).
    pub fn new(
        generation: Generation,
        socket: u8,
        die: u8,
        dxio: Dxio<'s, M>,
        dma: DmaBuffer,
        board: &[Engine<'_>],
        policy: &DxioPolicy,
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
        let heaps = Heaps::build(
            generation,
            wire,
            &engine::Ancillary::default(),
            &policy.encode,
        )?;
        Self::with_heaps(
            socket,
            die,
            dxio,
            dma,
            &heaps,
            placed.iter().map(|&(i, _)| i as u8),
        )
    }

    /// As [`new`](Self::new) with heaps built by the caller (ancillary
    /// PSPP/PHY data).
    pub fn with_heaps(
        socket: u8,
        die: u8,
        dxio: Dxio<'s, M>,
        dma: DmaBuffer,
        heaps: &Heaps,
        board: impl Iterator<Item = u8>,
    ) -> Result<Option<Self>, DxioError> {
        dma.write(0, heaps.engine_bytes());
        dma.write(ANCILLARY_OFFSET, heaps.ancillary_bytes());
        Ok(Some(Self {
            socket,
            die,
            dxio,
            engine_dwords: heaps.engines_len.div_ceil(4) as u32,
            anc_dwords: (heaps.ancillary_len / 4) as u32,
            engines: heaps.fw_engines().iter().copied().collect(),
            board: board.collect(),
            dma,
            paused: false,
            version: None,
        }))
    }

    /// The wire engines as last sent or read back.
    pub fn engines(&self) -> &[FwEngine] {
        &self.engines
    }

    pub fn view(&self) -> UnitView<'_> {
        UnitView {
            socket: self.socket,
            die: self.die,
            engines: &self.engines,
            board: &self.board,
        }
    }

    /// Heaps 6 (if any), 2, 5 and 3, the order of every generation.
    pub fn load_heaps(&self) -> Result<(), DxioError> {
        if self.anc_dwords != 0 {
            self.dxio.load_heap(
                heap::ANCILLARY,
                self.dma.phys(ANCILLARY_OFFSET),
                self.anc_dwords,
            )?;
        }
        self.dxio.load_empty(heap::MACPCS)?;
        self.dxio.load_empty(heap::GPIO)?;
        self.dxio
            .load_heap(heap::ENGINE_CONFIG, self.dma.phys(0), self.engine_dwords)
    }

    /// GetEngineCfg into the DMA window and copy the engines back.
    pub fn read_back(&mut self) -> Result<(), DxioError> {
        self.dma
            .write(0, &[0; ENGINE_HEAP_MAX][..self.engine_dwords as usize * 4]);
        self.dxio
            .get_engine_cfg(self.dma.phys(0), self.engine_dwords)?;
        let mut buf = [0u8; ENGINE_HEAP_MAX];
        self.dma
            .read(0, &mut buf[..self.engine_dwords as usize * 4]);
        let n = self.engines.len();
        let returned = buf
            .get(0x10..0x10 + n * 0x28)
            .and_then(|b| <[FwEngine]>::ref_from_bytes(b).ok())
            .ok_or(DxioError::Buffer)?;
        self.engines.clear();
        self.engines.extend(returned.iter().copied());
        Ok(())
    }
}

/// How one run of the event loop ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stop {
    Done,
    /// Paused at this state; the next pass resumes.
    Paused(u32),
}

/// Per-generation event loop differences (§3.4).
#[derive(Debug, Clone, Copy)]
pub struct LoopRules {
    /// States at which the loop returns without resuming.
    pub pause_at: &'static [u32],
    /// PERST requests are forwarded to the hooks.
    pub perst: bool,
    pub perst_deassert_delay_ms: u16,
}

/// Run the state machine of `unit` until DONE, a pause state or an error
/// (Rome `ffe02383`, Milan `ffd156a0`, Naples `ffdf13fc`). Bounded by
/// `timeout_us` where the donor polls forever.
pub fn run<M, H, T>(
    unit: &mut Unit<'_, M>,
    hooks: &mut H,
    timer: &T,
    rules: &LoopRules,
    timeout_us: u64,
) -> Result<Stop, DxioError>
where
    M: SmuMessages + ?Sized,
    H: Hooks + ?Sized,
    T: Timer + ?Sized,
{
    let start = timer.timestamp_us();
    let mut delayed = false;
    let mut last = None;
    loop {
        if timer.timestamp_us().wrapping_sub(start) >= timeout_us {
            return Err(DxioError::Timeout);
        }
        let r = unit.dxio.raw(op::GET_SM_STATE, [0; 5])?;
        let (res, kind, st) = (result_of(r[0]), r[1] & 0xFF, r[2]);
        // Bring-up trace: every distinct state-machine reply.
        if last != Some(r) {
            fstart_log::info!(
                "dxio: socket {} sm {:x} {:x} {:x} {:x} {:x} {:x}",
                unit.socket,
                r[0],
                r[1],
                r[2],
                r[3],
                r[4],
                r[5]
            );
            last = Some(r);
        }
        match (res, kind) {
            (result::ERROR, reply::SM) => return Err(DxioError::StateMachine { state: st }),
            // Error with another type: the donor logs (Naples event
            // a1271100) and resumes.
            (result::ERROR, _) => {}
            (result::OK, reply::NONE) => return Ok(Stop::Done),
            (result::OK, reply::SM) => {
                match st {
                    state::MAPPED => {
                        unit.read_back()?;
                        hooks.mapped(&unit.view())?;
                    }
                    state::CONFIGURED => hooks.configured(&unit.view())?,
                    state::EARLY_TRAINED => hooks.early_trained(&unit.view()),
                    state::DONE => return Ok(Stop::Done),
                    _ => {}
                }
                if rules.pause_at.contains(&st) {
                    unit.paused = true;
                    return Ok(Stop::Paused(st));
                }
            }
            (result::OK, reply::RESET) => {
                if rules.perst {
                    hooks.perst(&unit.view(), &r);
                    // Deassert (state ≠ 0) waits once per loop call.
                    if st != 0 && !delayed && rules.perst_deassert_delay_ms != 0 {
                        timer.delay_ms(rules.perst_deassert_delay_ms as u64);
                        delayed = true;
                    }
                }
            }
            // GENERIC, HPSM and unknown types resume.
            (result::OK, _) => {}
            (other, _) => return Err(DxioError::NoReturn(other)),
        }
        unit.dxio.sm_resume()?;
    }
}

/// Resume a paused unit and run it to DONE, then read the engines back
/// (the end of every orchestration: 0x308, loop, 0x14).
pub fn finish_unit<M, H, T>(
    unit: &mut Unit<'_, M>,
    hooks: &mut H,
    timer: &T,
    rules: &LoopRules,
    timeout_us: u64,
) -> Result<(), DxioError>
where
    M: SmuMessages + ?Sized,
    H: Hooks + ?Sized,
    T: Timer + ?Sized,
{
    if unit.paused {
        unit.paused = false;
        unit.dxio.sm_resume()?;
        let rules = LoopRules {
            pause_at: &[],
            ..*rules
        };
        run(unit, hooks, timer, &rules, timeout_us)?;
    }
    unit.read_back()
}

/// Train every unit of `generation` (Naples: dies, sequential or two-pass;
/// Rome/Milan: sockets, two-pass).
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
    match generation {
        #[cfg(feature = "naples")]
        Generation::Naples => naples::train(units, hooks, timer, policy),
        #[cfg(any(feature = "rome", feature = "milan"))]
        Generation::Rome | Generation::Milan => sp3::train(generation, units, hooks, timer, policy),
        // Fail closed on a generation this build does not carry.
        #[allow(unreachable_patterns)]
        _ => Err(DxioError::Unsupported),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    //! A fake SMU that records messages and plays a DXIO firmware.
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

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

    #[derive(Default)]
    pub struct FakeTimer(pub AtomicU64);

    impl Timer for FakeTimer {
        fn delay_us(&self, us: u64) {
            self.0.fetch_add(us, Ordering::Relaxed);
        }
        fn timestamp_us(&self) -> u64 {
            self.0.fetch_add(1, Ordering::Relaxed)
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
        };
        let heaps = Heaps::build(
            Generation::Rome,
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
                &FakeTimer::default(),
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
