//! PLDM Platform Monitoring and Control (Type 2, DSP0248).
//!
//! Covers numeric sensor reading (`GetSensorReading`), state effecter control
//! (`SetStateEffecterStates`), and PDR repository discovery (`GetPDRRepositoryInfo`,
//! `GetPDR`).

use crate::{CompletionCode, Error, PldmHeader, PldmType, write_completion};

/// PLDM Type 2 command codes (DSP0248 Table 103).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PlatformCommand {
    GetSensorReading = 0x11,
    SetStateEffecterStates = 0x39,
    GetPdrRepositoryInfo = 0x50,
    GetPdr = 0x51,
}

/// Simple numeric sensor entry (uint32 reading).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NumericSensor {
    pub sensor_id: u16,
    pub reading: u32,
}

/// Simple state effecter entry (1-state effecter).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StateEffecter {
    pub effecter_id: u16,
    pub state: u8,
}

/// Encode `GetSensorReading` request (`rearm = false`).
pub fn encode_get_sensor_reading_req(
    instance_id: u8,
    sensor_id: u16,
    out: &mut [u8],
) -> Result<usize, Error> {
    if out.len() < 6 {
        return Err(Error::BufferTooSmall);
    }
    let hdr = PldmHeader::request(
        instance_id,
        PldmType::Platform,
        PlatformCommand::GetSensorReading as u8,
    );
    out[..3].copy_from_slice(&hdr.to_bytes());
    out[3..5].copy_from_slice(&sensor_id.to_le_bytes());
    out[5] = 0; // rearm = false
    Ok(6)
}

/// Encode `SetStateEffecterStates` request for a single-state effecter.
pub fn encode_set_state_effecter_req(
    instance_id: u8,
    effecter_id: u16,
    state: u8,
    out: &mut [u8],
) -> Result<usize, Error> {
    if out.len() < 8 {
        return Err(Error::BufferTooSmall);
    }
    let hdr = PldmHeader::request(
        instance_id,
        PldmType::Platform,
        PlatformCommand::SetStateEffecterStates as u8,
    );
    out[..3].copy_from_slice(&hdr.to_bytes());
    out[3..5].copy_from_slice(&effecter_id.to_le_bytes());
    out[5] = 1; // composite_effecter_count = 1
    out[6] = 1; // set_request = requestSet
    out[7] = state;
    Ok(8)
}

/// Encode `GetPDRRepositoryInfo` request.
pub fn encode_get_pdr_repo_info_req(instance_id: u8, out: &mut [u8]) -> Result<usize, Error> {
    if out.len() < 3 {
        return Err(Error::BufferTooSmall);
    }
    let hdr = PldmHeader::request(
        instance_id,
        PldmType::Platform,
        PlatformCommand::GetPdrRepositoryInfo as u8,
    );
    out[..3].copy_from_slice(&hdr.to_bytes());
    Ok(3)
}

/// Allocation-free PLDM Platform Monitoring & Control responder.
pub struct PlatformResponder<const SENSORS: usize = 8, const EFFECTERS: usize = 4> {
    sensors: [Option<NumericSensor>; SENSORS],
    effecters: [Option<StateEffecter>; EFFECTERS],
}

impl<const SENSORS: usize, const EFFECTERS: usize> PlatformResponder<SENSORS, EFFECTERS> {
    pub const fn new() -> Self {
        Self {
            sensors: [None; SENSORS],
            effecters: [None; EFFECTERS],
        }
    }

    pub fn add_sensor(&mut self, sensor: NumericSensor) -> Result<(), Error> {
        for slot in &mut self.sensors {
            if slot.is_none() {
                *slot = Some(sensor);
                return Ok(());
            }
        }
        Err(Error::BufferTooSmall)
    }

    pub fn add_effecter(&mut self, effecter: StateEffecter) -> Result<(), Error> {
        for slot in &mut self.effecters {
            if slot.is_none() {
                *slot = Some(effecter);
                return Ok(());
            }
        }
        Err(Error::BufferTooSmall)
    }

    pub fn effecter_state(&self, effecter_id: u16) -> Option<u8> {
        self.effecters
            .iter()
            .flatten()
            .find(|e| e.effecter_id == effecter_id)
            .map(|e| e.state)
    }

    pub fn handle_request(
        &mut self,
        hdr: PldmHeader,
        data: &[u8],
        resp: &mut [u8],
    ) -> Result<usize, Error> {
        match hdr.command_code {
            x if x == PlatformCommand::GetSensorReading as u8 => {
                if data.len() != 3 {
                    return write_completion(hdr, CompletionCode::InvalidLength as u8, resp);
                }
                let sensor_id = u16::from_le_bytes([data[0], data[1]]);
                let Some(sensor) = self
                    .sensors
                    .iter()
                    .flatten()
                    .find(|s| s.sensor_id == sensor_id)
                else {
                    return write_completion(hdr, 0x80, resp); // INVALID_SENSOR_ID
                };
                // DSP0248 §18.2 response: CC(1), dataSize(1)=4(uint32), opState(1)=0(enabled),
                // eventMsgEnable(1)=0, presentState(1)=1(normal), prevState(1)=1, eventState(1)=1,
                // presentReading(4).
                if resp.len() < 14 {
                    return Err(Error::BufferTooSmall);
                }
                write_completion(hdr, CompletionCode::Success as u8, resp)?;
                resp[4] = 4; // uint32
                resp[5] = 0; // enabled
                resp[6] = 0; // noEventGeneration
                resp[7] = 1; // normal
                resp[8] = 1; // normal
                resp[9] = 1; // normal
                resp[10..14].copy_from_slice(&sensor.reading.to_le_bytes());
                Ok(14)
            }
            x if x == PlatformCommand::SetStateEffecterStates as u8 => {
                if data.len() < 5 {
                    return write_completion(hdr, CompletionCode::InvalidLength as u8, resp);
                }
                let effecter_id = u16::from_le_bytes([data[0], data[1]]);
                let count = data[2];
                if count != 1 || data.len() != 5 {
                    return write_completion(hdr, CompletionCode::InvalidData as u8, resp);
                }
                let Some(eff) = self
                    .effecters
                    .iter_mut()
                    .flatten()
                    .find(|e| e.effecter_id == effecter_id)
                else {
                    return write_completion(hdr, 0x80, resp); // INVALID_EFFECTER_ID
                };
                if data[3] == 1 {
                    eff.state = data[4];
                }
                write_completion(hdr, CompletionCode::Success as u8, resp)
            }
            x if x == PlatformCommand::GetPdrRepositoryInfo as u8 => {
                if !data.is_empty() {
                    return write_completion(hdr, CompletionCode::InvalidLength as u8, resp);
                }
                // DSP0248 §26.1: 41 bytes of body after CC (45 total).
                if resp.len() < 45 {
                    return Err(Error::BufferTooSmall);
                }
                let record_count = self.sensors.iter().flatten().count() as u32
                    + self.effecters.iter().flatten().count() as u32;
                write_completion(hdr, CompletionCode::Success as u8, resp)?;
                resp[4] = 0x00; // repositoryState = available
                resp[5..31].fill(0); // updateTime(13) + oemUpdateTime(13)
                resp[31..35].copy_from_slice(&record_count.to_le_bytes());
                resp[35..39].copy_from_slice(&(record_count * 16).to_le_bytes()); // repo size
                resp[39..43].copy_from_slice(&16u32.to_le_bytes()); // largest record size
                resp[43] = 0x00; // dataTransferHandleTimeout
                Ok(44)
            }
            _ => write_completion(hdr, CompletionCode::UnsupportedCmd as u8, resp),
        }
    }
}

impl<const SENSORS: usize, const EFFECTERS: usize> Default
    for PlatformResponder<SENSORS, EFFECTERS>
{
    fn default() -> Self {
        Self::new()
    }
}
