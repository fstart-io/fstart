//! IPMI over the KCS system interface.
#![no_std]
#[cfg(feature = "acpi")]
extern crate alloc;

pub mod kcs;

use zerocopy::{
    Immutable, IntoBytes, KnownLayout,
    little_endian::{U16, U32},
};

/// IPMI 2.0 as a BCD specification revision (ACPI SPMI, `_SRV`).
pub const SPEC_REVISION_2_0: u16 = 0x0200;

pub const NETFN_CHASSIS: u8 = 0x00;
pub const NETFN_APP: u8 = 0x06;
pub const NETFN_STORAGE: u8 = 0x0a;
const GET_CHASSIS_STATUS: u8 = 0x01;
const SET_ACPI_POWER_STATE: u8 = 0x06;
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

/// OEM non-timestamped SEL record (IPMI 2.0 table 32-3). No SDR sensor or
/// manufacturer ID is implied; the payload format belongs to its producer.
#[derive(Debug, Clone, Copy, IntoBytes, Immutable, KnownLayout)]
#[repr(C, packed)]
pub struct OemSelRecord {
    record_id: U16,
    record_type: u8,
    pub data: [u8; 13],
}

impl OemSelRecord {
    pub const fn new(data: [u8; 13]) -> Self {
        Self {
            record_id: U16::ZERO,
            record_type: 0xe0,
            data,
        }
    }
}
const _: () = assert!(size_of::<OemSelRecord>() == 16);

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

/// The `(system, device)` ACPI power states (IPMI 2.0 §20.6) reported for
/// sleep state `sx`, as AMI's BmcAcpi SMM driver reports them for SW SMI
/// `base + sx`: S0/D0, S1/D0, S3/D3, S4/D3, S5/D3; S2 is not reported.
pub const fn sleep_power_states(sx: u8) -> Option<(u8, u8)> {
    match sx {
        0 => Some((0, 0)),
        1 => Some((1, 0)),
        3 => Some((3, 3)),
        4 => Some((4, 3)),
        5 => Some((5, 3)),
        _ => None,
    }
}

/// A board's BMC sleep-state notification (AMI BmcAcpi's SW SMIs): the
/// DSDT writes `sw_smi_base + Sx` to the SMI command port before sleeping
/// and `sw_smi_base` at boot and wake ([`Self::aml`]), and the SMM handler
/// [`Self::report`]s it with Set ACPI Power State over the KCS interface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub struct SleepNotify {
    pub kcs_port: u16,
    pub kcs_spacing: u8,
    pub sw_smi_base: u8,
}

/// The KCS budget of [`SleepNotify::report`]: 2000 polls 10 us apart, 20 ms.
pub const SMM_POLL_BUDGET: u32 = 2000;

impl SleepNotify {
    /// The `(system, device)` states SW SMI `command` reports, if it is a
    /// sleep-state notification.
    pub const fn states(&self, command: u8) -> Option<(u8, u8)> {
        match command.checked_sub(self.sw_smi_base) {
            Some(sx) => sleep_power_states(sx),
            None => None,
        }
    }

    /// Report SW SMI `command`, if it is a notification. The OS shares the
    /// interface: only an idle one is used, and for at most
    /// [`SMM_POLL_BUDGET`], rather than corrupting the OS's transaction or
    /// holding every CPU in SMM. A KCS error is dropped: the sleep goes on
    /// either way.
    ///
    /// # Safety
    /// The KCS ports are the board's, and the caller owns the CPUs (SMM).
    pub unsafe fn report<D: embedded_hal::delay::DelayNs>(&self, command: u8, delay: D) {
        if self.states(command).is_some() {
            // SAFETY: the caller's.
            let kcs = unsafe { kcs::Kcs::new(self.kcs_port, self.kcs_spacing.into(), delay) };
            let _ = self.report_on(kcs, command);
        }
    }

    /// [`Self::report`] on `kcs`; `None` if nothing was sent.
    pub fn report_on<D, P, S>(
        &self,
        kcs: kcs::Kcs<D, P, S>,
        command: u8,
    ) -> Option<Result<(), Error>>
    where
        D: embedded_hal::delay::DelayNs,
        P: tock_registers::interfaces::Readable<T = u8>
            + tock_registers::interfaces::Writeable<T = u8>,
        S: tock_registers::interfaces::Readable<T = u8, R = kcs::Status::Register>
            + tock_registers::interfaces::Writeable<T = u8, R = kcs::Status::Register>,
    {
        let (system, device) = self.states(command)?;
        let mut kcs = kcs.with_poll_budget(SMM_POLL_BUDGET);
        kcs.idle().then(|| kcs.set_acpi_power_state(system, device))
    }

    /// The DSDT's side, raising the SW SMIs on `smi_cmd`: `\_PTS`, `\_WAK`
    /// and `\_SB._INI`.
    #[cfg(feature = "acpi")]
    pub fn aml(&self, smi_cmd: u16) -> alloc::vec::Vec<u8> {
        fstart_acpi::devices::IpmiKcsAcpi::sleep_notify_aml(smi_cmd, self.sw_smi_base)
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

    /// Set ACPI Power State (IPMI 2.0 §20.6): tell the BMC the system and
    /// device power states (bit 7 = set the field).
    pub fn set_acpi_power_state(&mut self, system: u8, device: u8) -> Result<(), Error> {
        let mut buffer = [0; 8];
        let request = [0x80 | system, 0x80 | device];
        self.request(NETFN_APP, SET_ACPI_POWER_STATE, &request, &mut buffer)
            .map(drop)
    }

    /// Add a SEL entry; returns the record ID the BMC gave it.
    pub fn add_sel(&mut self, record: &SelRecord) -> Result<u16, Error> {
        self.add_sel_bytes(record.as_bytes())
    }

    /// Add an OEM non-timestamped entry. Unsupported BMCs return a completion
    /// error; the caller decides whether to disable or defer further logging.
    pub fn add_oem_sel(&mut self, record: &OemSelRecord) -> Result<u16, Error> {
        self.add_sel_bytes(record.as_bytes())
    }

    fn add_sel_bytes(&mut self, record: &[u8]) -> Result<u16, Error> {
        let mut buffer = [0; 8];
        let data = self.request(NETFN_STORAGE, ADD_SEL_ENTRY, record, &mut buffer)?;
        match *data {
            [low, high, ..] => Ok(u16::from_le_bytes([low, high])),
            _ => Err(Error::Short),
        }
    }
}
