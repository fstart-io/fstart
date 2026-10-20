// SPDX-License-Identifier: MIT OR Apache-2.0
//! DXIO platform descriptors and the engine record of the DXIO firmware,
//! shared by Picasso and Zeppelin. Zeppelin uses the Picasso descriptor
//! format, and both DXIO firmwares take the same 0x28-byte engine record.
use tock_registers::{LocalRegisterCopy, register_bitfields};
use zerocopy::{
    FromBytes, FromZeros, Immutable, IntoBytes, KnownLayout, Unaligned,
    byteorder::{LittleEndian, U16, U32},
};

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
