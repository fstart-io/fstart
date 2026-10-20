// SPDX-License-Identifier: MIT OR Apache-2.0
//! Picasso DXIO firmware wire protocol and board-to-engine mapping.
use crate::fam17h::tsc::DelayNs;
pub use super::dxio_hardware::{Buffers, Options, train};
use super::{acp, registers::Smn, smu};
use fstart_pci::ConfigRegionAccess;
pub use crate::fam17h::dxio::{
    Aspm, ClockRequest, Descriptor, Engine, EngineCountError, EngineType, Hotplug, LinkSpeed,
    Operation, SataChannel,
};

pub const PORTS: usize = 8;
pub const REMAPPABLE_PORTS: usize = 7;
pub const DEFAULT_DEVFNS: [u8; PORTS] = [9, 10, 11, 12, 13, 14, 15, 17];
pub const ANCILLARY_PSPP: [u32; 4] = [0x00030000, 0x00020104, 0x600ededf, 0];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerPolicy {
    Disabled,
    Balanced,
    Performance,
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

impl From<EngineCountError> for Error {
    fn from(_: EngineCountError) -> Self {
        Self::EngineCount
    }
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

pub type EngineConfig = crate::fam17h::dxio::EngineConfig<PORTS>;
const _: () = assert!(size_of::<EngineConfig>() == 344);
impl EngineConfig {
    pub fn build(descriptors: &[Descriptor], policy: PowerPolicy) -> Result<Self, Error> {
        validate(descriptors)?;
        let engines = descriptors.iter().map(|d| {
            let speed = if policy == PowerPolicy::Balanced {
                LinkSpeed::Gen1
            } else {
                d.speed
            };
            Engine::new(d, d.lanes(), speed, d.turn_off_unused_lanes)
        });
        Ok(Self::from_engines(engines))
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
            done.ports[index].trained = engine.present() && inhibit & (1 << index) == 0;
        }
        *self = done;
        Ok(())
    }
}

pub fn rpc(
    smn: &Smn<impl ConfigRegionAccess>,
    operation: Operation,
    mut arguments: [u32; 6],
    delay: &mut impl DelayNs,
) -> Result<[u32; 6], Error> {
    arguments[0] = operation as u32;
    smu::request(smn, smu::DXIO_SERVICE, arguments, delay).map_err(Error::Smu)
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
    use zerocopy::IntoBytes;
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
        let flags = &mut config.engines[1].port_flags;
        *flags = zerocopy::byteorder::U32::new(flags.get() & !1);
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
        config.count = zerocopy::byteorder::U16::new(9);
        assert_eq!(config.transfer_size(), Err(EngineCountError));
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
