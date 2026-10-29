// SPDX-License-Identifier: MIT OR Apache-2.0
//! The board engines of Picasso and Zeppelin, in their logical lanes.

use super::wire::{CAPS, FwEngine};
use zerocopy::FromZeros;
use zerocopy::little_endian::U32;

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

impl Descriptor {
    /// The wire record of this engine with its lanes already translated into
    /// the firmware's numbering, in GPIO group 1 and `reset_group`. Both
    /// SoCs override speed and lane power-off by policy.
    pub(crate) fn engine(
        &self,
        (start, end): (u8, u8),
        reset_group: u8,
        speed: LinkSpeed,
        turn_off_unused_lanes: bool,
    ) -> FwEngine {
        let mut engine = FwEngine::new_zeroed();
        engine.kind = self.engine as u8;
        engine.flags = u8::from(self.hotplug != Hotplug::Disabled);
        engine.start_lane = start;
        engine.end_lane = end;
        engine.groups = U32::new(1 | u32::from(reset_group) << 8);
        if self.engine == EngineType::Sata {
            engine.channel_type = self.sata_channel as u8;
            return engine;
        }
        engine.bdf_num = U32::new(u32::from(self.devfn()));
        let hotplug = match self.hotplug {
            Hotplug::ServerSsd => Hotplug::Server,
            other => other,
        };
        let caps = CAPS::PRESENT.val(u32::from(self.present))
            + CAPS::REVERSE.val(u32::from(self.end_lane < self.start_lane))
            + CAPS::MAX_SPEED.val(speed as u32)
            + CAPS::HOTPLUG.val(hotplug as u32)
            + CAPS::EN_OFF_CONFIG::SET
            + CAPS::OFF_UNUSED.val(u32::from(turn_off_unused_lanes));
        engine.caps = U32::new(caps.value);
        engine
    }
}
