// SPDX-License-Identifier: MIT OR Apache-2.0
//! The DXIO (PCIe/SATA lane) firmware client every Zen SoC shares: the
//! opcodes, the RPC over an SMU message, the wire records ([`wire`]), the
//! DMA window of the heaps and the state-machine event loop.
//!
//! DXIO is an RPC carried by one SMU message: per die on Picasso, Zeppelin
//! and Naples (0x0F on ZP), per IOD on Rome and Milan (0x08). All take the
//! same opcodes, the same 0x28-byte engine record and the same state
//! machine; they differ in how board lanes become engines, in the request
//! sequence and in the work at the events ([`Hooks`]): [`picasso`],
//! [`zeppelin`] and [`crate::sp3::dxio`].

#[cfg(any(feature = "picasso", feature = "zeppelin"))]
mod descriptor;
#[cfg(feature = "picasso")]
pub mod picasso;
pub mod wire;
#[cfg(feature = "zeppelin")]
pub mod zeppelin;

use fstart_core::services::Timer;
use heapless::Vec;
use zerocopy::FromBytes;

use crate::smu::{ARGUMENT_COUNT, Error as SmuError, SmuMessages};
#[cfg(any(feature = "picasso", feature = "zeppelin"))]
pub use descriptor::*;
use wire::{ANCILLARY_MAX, ENGINE_HEAP_MAX, FwEngine, Heaps, MAX_ENGINES};

/// DXIO opcodes (`args[0]`). Names as in Oxide's `milan/dxio_impl.h`,
/// checked against the donor's use of each (§3.1).
pub mod op {
    /// Initialize the firmware.
    pub const INIT: u32 = 0x00;
    pub const GET_SM_STATE: u32 = 0x09;
    /// `| SINGLE` for one engine.
    pub const SET_LINK_SPEED: u32 = 0x10;
    pub const SET_LINK_SPEED_SINGLE: u32 = 0x800;
    /// Reply `[2]`, `[3]` = major, minor.
    pub const GET_VERSION: u32 = 0x13;
    pub const GET_ENGINE_CFG: u32 = 0x14;
    pub const SET_VARIABLE: u32 = 0x22;
    pub const LOAD_DATA: u32 = 0x23;
    pub const LOAD_CAPS: u32 = 0x24;
    /// Only on the early-train unit.
    pub const RELOAD_SM: u32 = 0x2D;
    /// ZP post-training (Zeppelin; Naples packages other than 4/8, unused
    /// on SP3). Its result is ignored.
    pub const POST_TRAINING: u32 = 0x34;
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
    #[cfg(feature = "sp3")]
    Lanes(usize, crate::sp3::dxio::lanes::LaneError),
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

/// The DMA window the SMU reads the heaps from and writes GetEngineCfg
/// into: the shared primitive, with this protocol's minimum size.
pub type DmaBuffer = crate::dma::Window<DMA_SIZE>;

impl From<crate::dma::InvalidWindow> for DxioError {
    fn from(_: crate::dma::InvalidWindow) -> Self {
        Self::Buffer
    }
}

/// Bytes a [`DmaBuffer`] needs: heap 3, then heap 6 at [`ANCILLARY_OFFSET`].
pub const DMA_SIZE: usize = ANCILLARY_OFFSET + ANCILLARY_MAX;
/// Heap 6 offset in the DMA window.
pub const ANCILLARY_OFFSET: usize = ENGINE_HEAP_MAX.next_multiple_of(64);

/// Memory reserved for one [`DmaBuffer`], for a firmware without a heap
/// allocator: a static of this type, identity mapped below 4 GiB.
#[repr(C, align(64))]
pub struct DmaArea(core::cell::UnsafeCell<[u8; DMA_SIZE]>);

impl Default for DmaArea {
    fn default() -> Self {
        Self::new()
    }
}

impl DmaArea {
    pub const fn new() -> Self {
        Self(core::cell::UnsafeCell::new([0; DMA_SIZE]))
    }

    /// The area as the DMA window of one DXIO instance.
    ///
    /// # Safety
    /// The area is identity mapped, coherent DRAM the SMU reaches. It stays
    /// reserved and unmodified until platform reset: a failed or timed-out
    /// request does not establish DMA quiescence.
    pub unsafe fn buffer(&'static mut self) -> Result<DmaBuffer, DxioError> {
        let ptr = self.0.get_mut().as_mut_ptr();
        // SAFETY: the exclusive static borrow; the caller's contract.
        Ok(unsafe { DmaBuffer::new(ptr, DMA_SIZE, ptr as u64) }?)
    }
}

impl DxioError {
    /// A name and up to two numbers, for logs without `Debug`.
    pub fn parts(&self) -> (&'static str, u32, u32) {
        match *self {
            Self::Smu(SmuError::BusyTimeout) => ("SMU mailbox busy", 0, 0),
            Self::Smu(SmuError::CompletionTimeout) => ("SMU mailbox timeout", 0, 0),
            Self::Smu(SmuError::Unavailable) => ("SMU mailbox unavailable", 0, 0),
            Self::Smu(SmuError::Rejected(status)) => ("SMU mailbox status", status, 0),
            Self::Rpc { op, result } => ("RPC result", op, result),
            Self::NoReturn(r) => ("GetSmState no return", r, 0),
            Self::StateMachine { state } => ("state machine error", state, 0),
            Self::Timeout => ("state machine timeout", 0, 0),
            #[cfg(feature = "sp3")]
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
    pub fn raw(&self, op: u32, req: [u32; 5]) -> Result<[u32; ARGUMENT_COUNT], DxioError> {
        let mut args = [op, req[0], req[1], req[2], req[3], req[4]];
        self.smu.send(self.msg, &mut args).inspect_err(|e| {
            let (status, timeout) = match e {
                SmuError::Rejected(s) => (*s, false),
                SmuError::BusyTimeout | SmuError::CompletionTimeout | SmuError::Unavailable => {
                    (0, true)
                }
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
    pub fn rpc(&self, op: u32, req: [u32; 5]) -> Result<[u32; ARGUMENT_COUNT], DxioError> {
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

pub(crate) const fn result_of(word: u32) -> u32 {
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
    /// What the hooks and with them the event loop fail with.
    type Error: From<DxioError>;
    /// State 0xF MAPPED, after the GetEngineCfg readback: port mapping,
    /// devfn remap, straps (`DxioPortMapping`, `DxioCfgBeforeReconfig`).
    fn mapped(&mut self, _unit: &UnitView<'_>) -> Result<(), Self::Error> {
        Ok(())
    }
    /// State 5 CONFIGURED (`DxioCfgAfterReconfig`).
    fn configured(&mut self, _unit: &UnitView<'_>) -> Result<(), Self::Error> {
        Ok(())
    }
    /// State 7 EARLY_TRAINED.
    fn early_trained(&mut self, _unit: &UnitView<'_>) {}
    /// Type 4: a PERST assert/deassert request; `reply` is the raw reply.
    fn perst(&mut self, _unit: &UnitView<'_>, _reply: &[u32; ARGUMENT_COUNT]) {}
    /// Deferred PERST policy: release every socket's resets between the
    /// passes (Rome PCD 0x160, Milan 0x18F "Deassert all resets").
    fn release_all(&mut self) {}
}

/// No-op hooks.
impl Hooks for () {
    type Error = DxioError;
}

/// One DXIO instance: a Naples die or a Rome/Milan socket.
pub struct Unit<'s, M: ?Sized> {
    pub socket: u8,
    pub die: u8,
    pub dxio: Dxio<'s, M>,
    dma: DmaBuffer,
    engine_dwords: u32,
    pub(crate) anc_dwords: u32,
    engines: Vec<FwEngine, MAX_ENGINES>,
    board: Vec<u8, MAX_ENGINES>,
    paused: bool,
    /// GetVersion reply (Rome/Milan; Naples after Init).
    pub version: Option<(u32, u32)>,
}

impl<'s, M: SmuMessages + ?Sized> Unit<'s, M> {
    /// A unit over `dxio` with `heaps` placed in `dma`; `board` is the
    /// board engine index of each engine in heap 3.
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
    /// Wait after each resume (Picasso: 1 ms).
    pub resume_delay_us: u32,
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
) -> Result<Stop, H::Error>
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
            return Err(DxioError::Timeout.into());
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
            (result::ERROR, reply::SM) => {
                return Err(DxioError::StateMachine { state: st }.into());
            }
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
            (other, _) => return Err(DxioError::NoReturn(other).into()),
        }
        unit.dxio.sm_resume()?;
        if rules.resume_delay_us != 0 {
            timer.delay_us(u64::from(rules.resume_delay_us));
        }
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
) -> Result<(), H::Error>
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
    Ok(unit.read_back()?)
}
