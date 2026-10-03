// SPDX-License-Identifier: MIT OR Apache-2.0
//! Picasso DXIO firmware wire protocol and board-to-engine mapping.
pub use super::dxio_hardware::{Buffers, Options, train};
use super::{acp, registers::Smn, smu};
use zerocopy::{
    FromBytes, FromZeros, Immutable, IntoBytes, KnownLayout, Unaligned,
    byteorder::{LittleEndian, U16, U32},
};

pub const PORTS: usize = 8;
pub const REMAPPABLE_PORTS: usize = 7;
pub const DEFAULT_DEVFNS: [u8; PORTS] = [9, 10, 11, 12, 13, 14, 15, 17];
pub const ANCILLARY_PSPP: [u32; 4] = [0x00030000, 0x00020104, 0x600ededf, 0];

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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerPolicy {
    Disabled,
    Balanced,
    Performance,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Descriptor {
    pub engine: EngineType,
    pub start_lane: u8,
    pub end_lane: u8,
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
    pub const fn pcie(start_lane: u8, end_lane: u8, device: u8, function: u8) -> Self {
        Self {
            engine: EngineType::Pcie,
            start_lane,
            end_lane,
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
    fn lanes(self) -> (u8, u8) {
        (
            self.start_lane.min(self.end_lane),
            self.start_lane.max(self.end_lane),
        )
    }
    fn devfn(self) -> u8 {
        self.device << 3 | self.function
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Descriptors,
    EngineCount,
    UnmatchedEngine,
    DuplicatePort,
    Remap,
    Smu(smu::Error),
    Result(u32),
    StateMachine,
    Timeout,
    DmaAddress,
    AudioPolicy,
    Acp(acp::Error),
}

pub fn validate(descriptors: &[Descriptor]) -> Result<(), Error> {
    if descriptors.len() > PORTS {
        return Err(Error::Descriptors);
    }
    for (i, d) in descriptors.iter().enumerate() {
        let (first, last) = d.lanes();
        if last >= 16 || d.device > 31 || d.function > 7 {
            return Err(Error::Descriptors);
        }
        let width = last - first + 1;
        // Picasso's logical-lane bifurcation, not an arbitrary contiguous range.
        if d.engine == EngineType::Pcie
            && (!width.is_power_of_two() || first % width != 0 || width > 8)
        {
            return Err(Error::Descriptors);
        }
        if descriptors[..i].iter().any(|other| {
            let (low, high) = other.lanes();
            first <= high && low <= last
        }) {
            return Err(Error::Descriptors);
        }
        if d.engine == EngineType::Pcie
            && d.devfn() != 0
            && descriptors[..i]
                .iter()
                .any(|other| other.engine == EngineType::Pcie && other.devfn() == d.devfn())
        {
            return Err(Error::Descriptors);
        }
    }
    Ok(())
}

#[repr(C)]
#[derive(Debug, Clone, Copy, FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned)]
pub struct Engine {
    pub kind: u8,
    hotplug: u8,
    pub start_lane: u8,
    pub end_lane: u8,
    gpio_group: u8,
    reserved05: [u8; 3],
    sata_channel: u8,
    reserved09: [u8; 3],
    devfn: U32<LittleEndian>,
    pub port_flags: u8,
    hotplug_type: u8,
    port_flags2: u8,
    reserved13: [u8; 6],
    pub port: u8,
    reserved1a: [u8; 14],
}
#[repr(C)]
#[derive(Debug, Clone, Copy, FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned)]
pub struct EngineConfig {
    reserved00: [u8; 12],
    count: U16<LittleEndian>,
    reserved0e: [u8; 2],
    engines: [Engine; PORTS],
    reserved_tail: [u8; 8],
}
const _: () = assert!(size_of::<Engine>() == 0x28 && size_of::<EngineConfig>() == 344);
impl EngineConfig {
    pub fn build(descriptors: &[Descriptor], policy: PowerPolicy) -> Result<Self, Error> {
        validate(descriptors)?;
        let mut config = Self::new_zeroed();
        config.count = U16::new(descriptors.len() as u16);
        for (engine, d) in config.engines.iter_mut().zip(descriptors) {
            let (first, last) = d.lanes();
            engine.kind = d.engine as u8;
            engine.hotplug = u8::from(d.hotplug != Hotplug::Disabled);
            engine.start_lane = first;
            engine.end_lane = last;
            engine.gpio_group = 1;
            if d.engine == EngineType::Sata {
                engine.sata_channel = d.sata_channel as u8;
                continue;
            }
            engine.devfn = U32::new(u32::from(d.devfn()));
            let speed = if policy == PowerPolicy::Balanced {
                LinkSpeed::Gen1
            } else {
                d.speed
            };
            engine.port_flags = u8::from(d.present)
                | (u8::from(d.end_lane < d.start_lane) << 3)
                | (speed as u8) << 4;
            engine.hotplug_type = if d.hotplug == Hotplug::ServerSsd {
                Hotplug::Server as u8
            } else {
                d.hotplug as u8
            };
            engine.port_flags2 = 8 | (u8::from(d.turn_off_unused_lanes) << 4);
        }
        Ok(config)
    }
    pub fn engines(&self) -> Result<&[Engine], Error> {
        self.engines
            .get(..usize::from(self.count.get()))
            .ok_or(Error::EngineCount)
    }
    /// Active engine records followed by eight zero tail bytes; the compact
    /// tail lies in the next zeroed record when fewer than eight engines exist.
    pub fn transfer_size(&self) -> Result<usize, Error> {
        Ok(24 + self.engines()?.len() * 40)
    }
    fn descriptor<'a>(
        engine: &Engine,
        descriptors: &'a [Descriptor],
    ) -> Result<&'a Descriptor, Error> {
        descriptors
            .iter()
            .find(|d| {
                let (low, high) = d.lanes();
                d.engine as u8 == engine.kind && engine.start_lane == low && engine.end_lane == high
            })
            .ok_or(Error::UnmatchedEngine)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Port {
    pub descriptor: Option<Descriptor>,
    pub devfn: u8,
    pub devfn_index: u8,
    pub trained: bool,
}
#[derive(Debug, Clone, Copy)]
pub struct Topology {
    pub ports: [Port; PORTS],
}
impl Default for Topology {
    fn default() -> Self {
        Self {
            ports: core::array::from_fn(|i| Port {
                descriptor: None,
                devfn: DEFAULT_DEVFNS[i],
                devfn_index: i as u8,
                trained: false,
            }),
        }
    }
}
impl Topology {
    /// Fully preflight the firmware's mapping before issuing a remap write.
    /// Unlike the C helper, a conflicting requested devfn cannot be silently
    /// replaced while keeping an inconsistent software topology.
    pub fn mapped(
        config: &EngineConfig,
        descriptors: &[Descriptor],
    ) -> Result<(Self, [u8; 7]), Error> {
        let mut topology = Self::default();
        let mut map = [0; REMAPPABLE_PORTS];
        let mut claimed = 0u8;
        let engines = config.engines()?;
        if engines.len() != descriptors.len() {
            return Err(Error::EngineCount);
        }
        for engine in engines {
            let d = *EngineConfig::descriptor(engine, descriptors)?;
            let index = descriptors
                .iter()
                .position(|candidate| candidate == &d)
                .ok_or(Error::UnmatchedEngine)?;
            if claimed & (1 << index) != 0 {
                return Err(Error::DuplicatePort);
            }
            claimed |= 1 << index;
            if engine.kind != EngineType::Pcie as u8 {
                continue;
            }
            let port = topology
                .ports
                .get_mut(usize::from(engine.port))
                .ok_or(Error::UnmatchedEngine)?;
            if port.descriptor.is_some() {
                return Err(Error::DuplicatePort);
            }
            let devfn = if d.devfn() == 0 {
                DEFAULT_DEVFNS[usize::from(engine.port)]
            } else {
                d.devfn()
            };
            if usize::from(engine.port) < REMAPPABLE_PORTS {
                if !(9..=15).contains(&devfn) || map.contains(&devfn) {
                    return Err(Error::Remap);
                }
                map[usize::from(engine.port)] = devfn;
                port.devfn_index = devfn - 9;
            } else if devfn != 17 {
                return Err(Error::Remap);
            }
            port.devfn = devfn;
            port.descriptor = Some(d);
        }
        for i in 0..REMAPPABLE_PORTS {
            if map[i] == 0 {
                map[i] = (9..=15)
                    .find(|devfn| !map.contains(devfn))
                    .ok_or(Error::Remap)?;
            }
        }
        Ok((topology, map))
    }
    pub fn training_done(
        &mut self,
        config: &EngineConfig,
        descriptors: &[Descriptor],
        inhibit: u32,
    ) -> Result<(), Error> {
        // Check returned engine identity against the completed mapping before
        // marking any link usable. A malformed reply does not alter topology.
        let (mut done, _) = Self::mapped(config, descriptors)?;
        if done.ports.iter().zip(&self.ports).any(|(after, before)| {
            after.descriptor != before.descriptor || after.devfn != before.devfn
        }) {
            return Err(Error::UnmatchedEngine);
        }
        for engine in config.engines()? {
            if engine.kind != EngineType::Pcie as u8 {
                continue;
            }
            let index = usize::from(engine.port);
            if done.ports[index].devfn != self.ports[index].devfn
                || self.ports[index].descriptor.is_none()
            {
                return Err(Error::UnmatchedEngine);
            }
            done.ports[index].trained = engine.port_flags & 1 != 0 && inhibit & (1 << index) == 0;
        }
        *self = done;
        Ok(())
    }
}

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
    Start = 0x307,
    Resume = 0x308,
}
pub fn rpc(
    smn: &mut impl Smn,
    operation: Operation,
    mut arguments: [u32; 6],
    delay_us: &mut impl FnMut(u32),
) -> Result<[u32; 6], Error> {
    arguments[0] = operation as u32;
    smu::request(smn, smu::DXIO_SERVICE, arguments, delay_us).map_err(Error::Smu)
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    Done,
    Mapped,
    Configured,
    Resume,
}
pub fn event(reply: [u32; 6]) -> Result<Event, Error> {
    let result = reply[0] & 0xff;
    let kind = reply[1] & 0xff;
    let state = reply[2] & 0xff;
    match (result, kind, state) {
        (2, 2, _) => Err(Error::StateMachine),
        (3, 0, _) | (3, 2, 0x0d) => Ok(Event::Done),
        (3, 2, 0x0f) => Ok(Event::Mapped),
        (3, 2, 5) => Ok(Event::Configured),
        (2 | 3, _, _) => Ok(Event::Resume),
        _ => Err(Error::Result(result)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn wire_records_match_the_native_firmware_contract() {
        let mut descriptor = Descriptor::pcie(5, 4, 1, 7);
        descriptor.hotplug = Hotplug::ServerSsd;
        descriptor.turn_off_unused_lanes = true;
        let config = EngineConfig::build(&[descriptor], PowerPolicy::Balanced).unwrap();
        let bytes = config.as_bytes();
        assert_eq!(config.transfer_size().unwrap(), 64);
        assert_eq!(&bytes[12..16], &[1, 0, 0, 0]);
        assert_eq!(&bytes[16..24], &[1, 1, 4, 5, 1, 0, 0, 0]);
        assert_eq!(&bytes[28..35], &[15, 0, 0, 0, 0x19, 2, 0x18]);
        assert_eq!(&bytes[56..64], &[0; 8]);
        assert!(
            EngineConfig::build(&[Descriptor::pcie(1, 2, 1, 1)], PowerPolicy::Disabled).is_err()
        );
        assert!(validate(&[descriptor, Descriptor::pcie(4, 4, 1, 2)]).is_err());
        assert!(validate(&[descriptor, Descriptor::pcie(0, 0, 1, 7)]).is_err());
    }
    #[test]
    fn remapping_and_final_training_are_validated_atomically() {
        let descriptors = [
            Descriptor::pcie(4, 5, 1, 7),
            Descriptor::pcie(0, 0, 1, 2),
            Descriptor::pcie(1, 1, 1, 3),
        ];
        let mut config = EngineConfig::build(&descriptors, PowerPolicy::Performance).unwrap();
        for (engine, port) in config.engines.iter_mut().zip([5, 3, 1]) {
            engine.port = port;
        }
        let (mut topology, map) = Topology::mapped(&config, &descriptors).unwrap();
        assert_eq!(map, [9, 11, 12, 10, 13, 15, 14]);
        assert_eq!(topology.ports[5].devfn_index, 6);
        config.engines[1].port_flags &= !1;
        topology
            .training_done(&config, &descriptors, 1 << 1)
            .unwrap();
        assert!(topology.ports[5].trained);
        assert!(!topology.ports[3].trained);
        assert!(!topology.ports[1].trained);
        config.engines[0].port = 4;
        assert_eq!(
            topology.training_done(&config, &descriptors, 0),
            Err(Error::UnmatchedEngine)
        );
        assert!(topology.ports[5].trained);
        config.engines[0].port = 3;
        assert!(matches!(
            Topology::mapped(&config, &descriptors),
            Err(Error::DuplicatePort)
        ));
        config.count = U16::new(9);
        assert_eq!(config.transfer_size(), Err(Error::EngineCount));
    }
    #[test]
    fn state_errors_and_success_are_distinct() {
        assert_eq!(event([3, 2, 15, 0, 0, 0]), Ok(Event::Mapped));
        assert_eq!(event([3, 2, 5, 0, 0, 0]), Ok(Event::Configured));
        assert_eq!(event([3, 2, 13, 0, 0, 0]), Ok(Event::Done));
        assert_eq!(event([3, 0, 0, 0, 0, 0]), Ok(Event::Done));
        assert_eq!(event([2, 2, 0, 0, 0, 0]), Err(Error::StateMachine));
        assert_eq!(event([2, 1, 0, 0, 0, 0]), Ok(Event::Resume));
        assert_eq!(event([1, 2, 13, 0, 0, 0]), Err(Error::Result(1)));
    }
}
