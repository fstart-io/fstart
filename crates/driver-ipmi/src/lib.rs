//! IPMI over the KCS system interface.
#![no_std]

pub mod kcs;

use zerocopy::{
    Immutable, IntoBytes, KnownLayout,
    little_endian::{U16, U32},
};

pub const NETFN_CHASSIS: u8 = 0x00;
pub const NETFN_STORAGE: u8 = 0x0a;
const GET_CHASSIS_STATUS: u8 = 0x01;
const ADD_SEL_ENTRY: u8 = 0x44;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Timeout,
    /// The interface left the expected KCS state; carries the status.
    State(u8),
    /// The response was shorter than its header or the data asked for.
    Short,
    /// A nonzero completion code.
    Completion(u8),
}

/// A standard SEL record (IPMI 2.0 table 32-1); the BMC assigns the ID and
/// the timestamp.
#[derive(Debug, Clone, Copy, IntoBytes, Immutable, KnownLayout)]
#[repr(C, packed)]
pub struct SelRecord {
    pub record_id: U16,
    pub record_type: u8,
    pub timestamp: U32,
    pub generator_id: U16,
    pub event_message_revision: u8,
    pub sensor_type: u8,
    pub sensor_number: u8,
    pub event_dir_type: u8,
    pub event_data: [u8; 3],
}
const _: () = assert!(size_of::<SelRecord>() == 16);

/// What the BMC wants after AC power returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestorePolicy {
    Off,
    Previous,
    On,
    Unknown,
}

/// Get Chassis Status (IPMI 2.0 28.2): the current power state byte.
#[derive(Debug, Clone, Copy)]
pub struct ChassisStatus {
    pub power_state: u8,
}
impl ChassisStatus {
    /// Power restore policy, bits 6:5.
    pub const fn restore_policy(self) -> RestorePolicy {
        match (self.power_state >> 5) & 3 {
            0 => RestorePolicy::Off,
            1 => RestorePolicy::Previous,
            2 => RestorePolicy::On,
            _ => RestorePolicy::Unknown,
        }
    }
}

impl<D, P, S> kcs::Kcs<D, P, S>
where
    D: embedded_hal::delay::DelayNs,
    P: tock_registers::interfaces::Readable<T = u8> + tock_registers::interfaces::Writeable<T = u8>,
    S: tock_registers::interfaces::Readable<T = u8, R = kcs::Status::Register>
        + tock_registers::interfaces::Writeable<T = u8, R = kcs::Status::Register>,
{
    pub fn chassis_status(&mut self) -> Result<ChassisStatus, Error> {
        let mut buffer = [0; 8];
        let data = self.request(NETFN_CHASSIS, GET_CHASSIS_STATUS, &[], &mut buffer)?;
        let &power_state = data.first().ok_or(Error::Short)?;
        Ok(ChassisStatus { power_state })
    }

    /// Add a SEL entry; returns the record ID the BMC gave it.
    pub fn add_sel(&mut self, record: &SelRecord) -> Result<u16, Error> {
        let mut buffer = [0; 8];
        let data = self.request(NETFN_STORAGE, ADD_SEL_ENTRY, record.as_bytes(), &mut buffer)?;
        match *data {
            [low, high, ..] => Ok(u16::from_le_bytes([low, high])),
            _ => Err(Error::Short),
        }
    }
}
