// SPDX-License-Identifier: MIT OR Apache-2.0
//! The DXIO (PCIe/SATA lane) firmware client of Picasso and Zeppelin: the
//! platform descriptors, the engine record, the requests, the DMA slot of
//! the engine configuration and the event loop of the training. Zeppelin
//! uses the Picasso descriptor format, and both DXIO firmwares take the
//! same 0x28-byte engine record.
//!
//! The firmware runs on the SMU of each die and is reached through the SoC's
//! DXIO service message ([`Soc::DXIO_SERVICE`]) with the operation in the
//! first argument. The SoC owns the lane mapping, the PCIe core registers
//! and the NBIF settings around the training ([`Training`]): [`picasso`]
//! and [`zeppelin`].
use crate::{
    ccx::tsc::{Clock, DelayNs},
    df::Die,
    smu,
    soc::Soc,
};
use core::{
    cell::UnsafeCell,
    sync::atomic::{Ordering, compiler_fence},
};
use fstart_log::{Hex, debug, error, warn};
use fstart_pci::ConfigRegionAccess;
use tock_registers::{LocalRegisterCopy, register_bitfields};
use zerocopy::{
    FromBytes, FromZeros, Immutable, IntoBytes, KnownLayout, Unaligned,
    byteorder::{LittleEndian, U16, U32},
};

#[cfg(feature = "picasso")]
pub mod picasso;
#[cfg(feature = "zeppelin")]
pub mod zeppelin;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum EngineType {
    Pcie = 1,
    Sata = 3,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum LinkSpeed {
    Maximum = 0,
    Gen1 = 1,
    Gen2 = 2,
    Gen3 = 3,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Hotplug {
    Disabled,
    Basic,
    Server,
    Enhanced,
    Inboard,
    ServerSsd,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Aspm {
    Disabled,
    L0s,
    L1,
    L0sL1,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ClockRequest {
    Disabled = 0,
    Pin0,
    Pin1,
    Pin2,
    Pin3,
    Pin4,
    Pin5,
    Pin6,
    Pin7,
    Pin8,
    Graphics = 12,
    Always = 255,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum SataChannel {
    Other,
    Short,
    Long,
}

/// One engine (PCIe port or SATA lane group) of a board. The lanes are
/// logical lanes; `start_lane > end_lane` means lane reversal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Descriptor {
    pub engine: EngineType,
    pub start_lane: u8,
    pub end_lane: u8,
    /// `gpio_group_id`: unused on Picasso, passed through on Zeppelin.
    pub gpio_group: u8,
    pub device: u8,
    pub function: u8,
    pub present: bool,
    pub speed: LinkSpeed,
    pub hotplug: Hotplug,
    pub aspm: Aspm,
    pub clock_request: ClockRequest,
    pub l1_1: bool,
    pub l1_2: bool,
    pub turn_off_unused_lanes: bool,
    pub sata_channel: SataChannel,
}
impl Descriptor {
    /// A present PCIe port at maximum speed without hotplug or ASPM.
    pub const fn pcie(start_lane: u8, end_lane: u8, device: u8, function: u8) -> Self {
        Self {
            engine: EngineType::Pcie,
            start_lane,
            end_lane,
            gpio_group: 1,
            device,
            function,
            present: true,
            speed: LinkSpeed::Maximum,
            hotplug: Hotplug::Disabled,
            aspm: Aspm::Disabled,
            clock_request: ClockRequest::Disabled,
            l1_1: false,
            l1_2: false,
            turn_off_unused_lanes: false,
            sata_channel: SataChannel::Other,
        }
    }
    pub const fn sata(start_lane: u8, end_lane: u8, channel: SataChannel) -> Self {
        Self {
            engine: EngineType::Sata,
            present: false,
            device: 0,
            function: 0,
            sata_channel: channel,
            ..Self::pcie(start_lane, end_lane, 0, 0)
        }
    }
    /// (lowest, highest) lane.
    pub const fn lanes(&self) -> (u8, u8) {
        if self.start_lane <= self.end_lane {
            (self.start_lane, self.end_lane)
        } else {
            (self.end_lane, self.start_lane)
        }
    }
    pub const fn devfn(&self) -> u8 {
        fstart_pci::devfn(self.device, self.function)
    }
}

register_bitfields![u32,
    /// Engine record +0x10, PCIe engines only.
    pub PortFlags [
        PRESENT OFFSET(0) NUMBITS(1) [],
        LANE_REVERSAL OFFSET(3) NUMBITS(1) [],
        SPEED OFFSET(4) NUMBITS(2) [],
        HOTPLUG OFFSET(8) NUMBITS(3) [],
        /// Always set by the vendor code (meaning unknown).
        FLAG_19 OFFSET(19) NUMBITS(1) [],
        TURN_OFF_UNUSED_LANES OFFSET(20) NUMBITS(1) []
    ]
];

/// The engine record of the DXIO firmware's platform descriptor. The
/// firmware fills in `port` (the port of the engine's PCIe core) and clears
/// `PortFlags::PRESENT` of ports it could not use.
#[repr(C)]
#[derive(Debug, Clone, Copy, FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned)]
pub struct Engine {
    pub(crate) kind: u8,
    pub(crate) hotplug: u8,
    pub(crate) start_lane: u8,
    pub(crate) end_lane: u8,
    pub(crate) gpio_group: u8,
    /// Zeppelin: the descriptor's GPIO group; zero on Picasso.
    pub(crate) gpio_group2: u8,
    reserved06: [u8; 2],
    pub(crate) sata_channel: u8,
    reserved09: [u8; 3],
    pub(crate) devfn: U32<LittleEndian>,
    pub(crate) port_flags: U32<LittleEndian>,
    reserved14: [u8; 5],
    pub(crate) port: u8,
    reserved1a: [u8; 14],
}
const _: () = assert!(size_of::<Engine>() == 0x28);
impl Engine {
    /// Encode `d` with its lanes already translated into the firmware's
    /// numbering. Both SoCs override speed and lane power-off by policy.
    pub(crate) fn new(
        d: &Descriptor,
        (start, end): (u8, u8),
        speed: LinkSpeed,
        turn_off_unused_lanes: bool,
    ) -> Self {
        let mut engine = Self::new_zeroed();
        engine.kind = d.engine as u8;
        engine.hotplug = u8::from(d.hotplug != Hotplug::Disabled);
        engine.start_lane = start;
        engine.end_lane = end;
        engine.gpio_group = 1;
        if d.engine == EngineType::Sata {
            engine.sata_channel = d.sata_channel as u8;
            return engine;
        }
        engine.devfn = U32::new(u32::from(d.devfn()));
        let hotplug = match d.hotplug {
            Hotplug::ServerSsd => Hotplug::Server,
            other => other,
        };
        let flags = PortFlags::PRESENT.val(u32::from(d.present))
            + PortFlags::LANE_REVERSAL.val(u32::from(d.end_lane < d.start_lane))
            + PortFlags::SPEED.val(speed as u32)
            + PortFlags::HOTPLUG.val(hotplug as u32)
            + PortFlags::FLAG_19::SET
            + PortFlags::TURN_OFF_UNUSED_LANES.val(u32::from(turn_off_unused_lanes));
        engine.port_flags = U32::new(flags.value);
        engine
    }
    pub fn kind(&self) -> u8 {
        self.kind
    }
    /// Lanes in the firmware's numbering.
    pub fn lanes(&self) -> (u8, u8) {
        (self.start_lane, self.end_lane)
    }
    /// Port of the engine's PCIe core, as the firmware assigned it.
    pub fn port(&self) -> u8 {
        self.port
    }
    pub fn port_flags(&self) -> LocalRegisterCopy<u32, PortFlags::Register> {
        LocalRegisterCopy::new(self.port_flags.get())
    }
    /// The firmware kept the port: it can train.
    pub fn present(&self) -> bool {
        self.port_flags().is_set(PortFlags::PRESENT)
    }
}

/// The platform descriptor the DXIO firmware reads (LoadData type 3) and
/// returns (EngineConfig): a header, `N` engine records and an 8-byte tail.
#[repr(C)]
#[derive(Debug, Clone, Copy, FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned)]
pub struct EngineConfig<const N: usize> {
    reserved00: [u8; 12],
    pub(crate) count: U16<LittleEndian>,
    reserved0e: [u8; 2],
    pub(crate) engines: [Engine; N],
    reserved_tail: [u8; 8],
}
impl<const N: usize> EngineConfig<N> {
    /// The first `count` records hold engines.
    pub(crate) fn from_engines(engines: impl IntoIterator<Item = Engine>) -> Self {
        let mut config = Self::new_zeroed();
        let count = config
            .engines
            .iter_mut()
            .zip(engines)
            .map(|(slot, engine)| *slot = engine)
            .count();
        config.count = U16::new(count as u16);
        config
    }
    /// Fails if the firmware returned more engines than fit.
    pub fn engines(&self) -> Result<&[Engine], EngineCountError> {
        self.engines
            .get(..usize::from(self.count.get()))
            .ok_or(EngineCountError)
    }
    /// Active engine records followed by eight zero tail bytes; the compact
    /// tail lies in the next zeroed record when fewer than `N` engines exist.
    pub fn transfer_size(&self) -> Result<usize, EngineCountError> {
        Ok(16 + size_of_val(self.engines()?) + 8)
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EngineCountError;

/// DXIO operations, the first argument of the SMU DXIO service message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum Operation {
    Version = 0,
    State = 9,
    Init = 0x13,
    EngineConfig = 0x14,
    SetVariable = 0x22,
    LoadData = 0x23,
    LoadCapabilities = 0x24,
    CleanupEarly = 0x2d,
    PostTraining = 0x34,
    Start = 0x307,
    Resume = 0x308,
}

/// A failed exchange with the DXIO firmware.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Smu(smu::Error),
    /// Unexpected version response.
    Version(u32),
    /// A request didn't return.
    Result(u8),
    /// The state machine failed in this state, or reported it out of order.
    StateMachine(u8),
    Timeout,
    /// The DMA buffer is not below 4 GiB.
    DmaAddress,
    /// The firmware returned more engines than it got.
    EngineCount,
}
impl From<EngineCountError> for Error {
    fn from(_: EngineCountError) -> Self {
        Self::EngineCount
    }
}

const RESULT_ERROR: u8 = 2;
const RESULT_OK: u8 = 3;
const EVENT_NONE: u8 = 0;
const EVENT_RESUME: u8 = 1;
const EVENT_SM: u8 = 2;
const EVENT_PERST: u8 = 4;
/// The ports are configured, not trained yet.
pub const SM_CONFIGURED: u8 = 0x05;
const SM_DONE: u8 = 0x0d;
/// The firmware mapped the engines to ports.
pub const SM_MAPPED: u8 = 0x0f;
/// Return to the host after mapping, after configuring, and use the
/// version 1 ancillary data.
pub const VAR_RET_AFTER_MAP: u32 = 0x0e;
pub const VAR_RET_AFTER_CONF: u32 = 0x0f;
pub const VAR_ANCILLARY_V1: u32 = 0x10;
/// LoadData types.
pub const DATA_MACPCS: u32 = 2;
pub const DATA_ENGINE_CONFIG: u32 = 3;
pub const DATA_GPIO: u32 = 5;
pub const DATA_ANCILLARY: u32 = 6;
const TIMEOUT_US: u64 = 60_000_000;

/// A reply to the state request of the event loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    Done,
    State(u8),
    Perst {
        deassert: bool,
    },
    /// An error event the vendor code goes on after.
    Error(u8),
    Resume,
    Unknown(u8),
}
pub fn event(reply: [u32; 6]) -> Result<Event, Error> {
    let [result, kind, state] = [reply[0] as u8, reply[1] as u8, reply[2] as u8];
    match (result, kind) {
        (RESULT_ERROR, EVENT_SM) => Err(Error::StateMachine(state)),
        (RESULT_ERROR, _) => Ok(Event::Error(kind)),
        (RESULT_OK, EVENT_NONE) => Ok(Event::Done),
        (RESULT_OK, EVENT_SM) if state == SM_DONE => Ok(Event::Done),
        (RESULT_OK, EVENT_SM) => Ok(Event::State(state)),
        (RESULT_OK, EVENT_PERST) => Ok(Event::Perst {
            deassert: state != 0,
        }),
        (RESULT_OK, EVENT_RESUME) => Ok(Event::Resume),
        (RESULT_OK, _) => Ok(Event::Unknown(kind)),
        _ => Err(Error::Result(result)),
    }
}

/// Send `operation` to the DXIO firmware of `die`.
pub fn rpc<S: Soc, P: ConfigRegionAccess + Copy>(
    die: &Die<S, P>,
    operation: Operation,
    mut arguments: [u32; 6],
    delay: &mut impl DelayNs,
) -> Result<[u32; 6], Error> {
    arguments[0] = operation as u32;
    let reply = die.smu(S::DXIO_SERVICE, arguments, delay).map_err(|e| {
        error!(
            "DXIO operation {} on die {} failed",
            Hex(operation as u64),
            die.index()
        );
        Error::Smu(e)
    })?;
    fstart_log::trace!(
        "DXIO {} op {}: {} {} {}",
        die.index(),
        Hex(operation as u64),
        Hex(reply[0].into()),
        Hex(reply[1].into()),
        Hex(reply[2].into())
    );
    Ok(reply)
}

/// The firmware version the client speaks; afterwards Init.
pub fn init<S: Soc, P: ConfigRegionAccess + Copy>(
    die: &Die<S, P>,
    delay: &mut impl DelayNs,
) -> Result<(), Error> {
    let version = rpc(die, Operation::Version, [0; 6], delay)?;
    if version[0] as u8 != RESULT_OK {
        error!(
            "DXIO {}: unexpected version response {}",
            die.index(),
            Hex(version[0].into())
        );
        return Err(Error::Version(version[0]));
    }
    rpc(die, Operation::Init, [0; 6], delay).map(drop)
}

pub fn set_variable<S: Soc, P: ConfigRegionAccess + Copy>(
    die: &Die<S, P>,
    variable: u32,
    value: u32,
    delay: &mut impl DelayNs,
) -> Result<(), Error> {
    rpc(
        die,
        Operation::SetVariable,
        [0, variable, value, 0, 0, 0],
        delay,
    )
    .map(drop)
}

/// LoadData `kind` from DMA `address`, `dwords` long. Without an address,
/// the firmware's defaults of that kind.
pub fn load_data<S: Soc, P: ConfigRegionAccess + Copy>(
    die: &Die<S, P>,
    kind: u32,
    data: Option<(u32, u32)>,
    delay: &mut impl DelayNs,
) -> Result<(), Error> {
    let arguments = match data {
        Some((address, dwords)) => [0, 0, address, dwords, 0, kind],
        None => [0, 0, 0, 0, 1, kind],
    };
    rpc(die, Operation::LoadData, arguments, delay).map(drop)
}

/// The engine configuration of a die in DRAM, where the firmware reads it
/// (LoadData) and writes it back (EngineConfig) with DMA.
#[repr(C, align(16))]
pub struct Slot<const N: usize>(UnsafeCell<EngineConfig<N>>);
impl<const N: usize> Default for Slot<N> {
    fn default() -> Self {
        Self::new()
    }
}
impl<const N: usize> Slot<N> {
    pub const fn new() -> Self {
        // SAFETY: an EngineConfig is plain bytes; all zeros is valid.
        Self(UnsafeCell::new(unsafe { core::mem::zeroed() }))
    }
    /// The DMA address, 16-byte aligned below 4 GiB.
    pub fn address(&self) -> Result<u32, Error> {
        let start = self.0.get() as usize;
        let end = start + size_of::<EngineConfig<N>>();
        if start & 15 != 0 || end > u32::MAX as usize {
            return Err(Error::DmaAddress);
        }
        Ok(start as u32)
    }
    /// Put `config` where the firmware loads it from.
    pub fn load(&mut self, config: &EngineConfig<N>) {
        *self.0.get_mut() = *config;
        compiler_fence(Ordering::SeqCst);
    }
    /// The engine configuration of the firmware now (EngineConfig), `size`
    /// bytes, at most what it was loaded with.
    pub fn read_back<S: Soc, P: ConfigRegionAccess + Copy>(
        &self,
        die: &Die<S, P>,
        size: usize,
        delay: &mut impl DelayNs,
    ) -> Result<EngineConfig<N>, Error> {
        // SAFETY: no DXIO request is in flight; the slot is ours.
        unsafe { core::ptr::write_bytes(self.0.get(), 0, 1) };
        compiler_fence(Ordering::SeqCst);
        let arguments = [0, 0, self.address()?, (size / 4) as u32, 0, 0];
        rpc(die, Operation::EngineConfig, arguments, delay)?;
        compiler_fence(Ordering::SeqCst);
        // SAFETY: the synchronous request completed its DMA. DRAM, not MMIO.
        let config = unsafe { self.0.get().read_volatile() };
        if config.transfer_size()? > size {
            return Err(Error::EngineCount);
        }
        Ok(config)
    }
}

/// What a SoC does at the events of a die's training.
pub trait Training {
    type Error: From<Error>;
    /// Wait after each resume.
    const PAUSE_US: u32 = 0;
    /// [`SM_MAPPED`]: the firmware mapped the engines to ports.
    fn mapped(&mut self, delay: &mut impl DelayNs) -> Result<(), Self::Error>;
    /// [`SM_CONFIGURED`]: the ports are configured, not trained yet.
    fn configured(&mut self) -> Result<(), Self::Error>;
    /// The firmware asks to assert or deassert PERST.
    fn perst(&mut self, deassert: bool);
}

/// Start the training of `die` and answer its events until it is done or
/// fails, at most 60 s.
pub fn run<S: Soc, P: ConfigRegionAccess + Copy, T: Training>(
    die: &Die<S, P>,
    training: &mut T,
    clock: &mut impl Clock,
) -> Result<(), T::Error> {
    let started = clock.now_us();
    rpc(die, Operation::Start, [0; 6], clock)?;
    // Also bound the states if the clock malfunctions.
    for _ in 0..TIMEOUT_US / u64::from(T::PAUSE_US.max(1)) {
        if clock.now_us().wrapping_sub(started) >= TIMEOUT_US {
            break;
        }
        let reply = rpc(die, Operation::State, [0; 6], clock)?;
        let event = event(reply).inspect_err(|e| match *e {
            Error::StateMachine(state) => {
                error!("DXIO state machine error, state {}", Hex(state.into()))
            }
            Error::Result(result) => {
                error!("DXIO request didn't return: {}", Hex(result.into()))
            }
            _ => {}
        })?;
        match event {
            Event::Done => return Ok(()),
            Event::State(state) => {
                debug!("DXIO {} state {}", die.index(), Hex(state.into()));
                match state {
                    SM_CONFIGURED => training.configured()?,
                    SM_MAPPED => training.mapped(clock)?,
                    _ => {}
                }
            }
            Event::Perst { deassert } => {
                debug!(
                    "DXIO {}: PERST {}",
                    die.index(),
                    if deassert { "deassert" } else { "assert" }
                );
                training.perst(deassert);
            }
            Event::Error(kind) => warn!("DXIO error event {}", Hex(kind.into())),
            Event::Unknown(kind) => warn!("DXIO: unknown event {}", Hex(kind.into())),
            Event::Resume => {}
        }
        rpc(die, Operation::Resume, [0; 6], clock)?;
        if T::PAUSE_US != 0 {
            clock.delay_us(T::PAUSE_US);
        }
    }
    error!("DXIO timeout");
    Err(Error::Timeout.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_events() {
        assert_eq!(event([3, 2, 0x0f, 0, 0, 0]), Ok(Event::State(SM_MAPPED)));
        assert_eq!(
            event([3, 2, 0x05, 0, 0, 0]),
            Ok(Event::State(SM_CONFIGURED))
        );
        assert_eq!(event([3, 2, 0x0d, 0, 0, 0]), Ok(Event::Done));
        assert_eq!(event([3, 0, 0, 0, 0, 0]), Ok(Event::Done));
        assert_eq!(event([3, 1, 0, 0, 0, 0]), Ok(Event::Resume));
        assert_eq!(
            event([3, 4, 1, 0, 0, 0]),
            Ok(Event::Perst { deassert: true })
        );
        assert_eq!(event([3, 9, 0, 0, 0, 0]), Ok(Event::Unknown(9)));
        assert_eq!(event([2, 4, 0, 0, 0, 0]), Ok(Event::Error(4)));
        assert_eq!(event([2, 2, 7, 0, 0, 0]), Err(Error::StateMachine(7)));
        assert_eq!(event([1, 2, 0x0d, 0, 0, 0]), Err(Error::Result(1)));
    }
}
