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

/// Encode `GetPDR` request (`transferOperationFlag = 0x01` GetFirstPart).
pub fn encode_get_pdr_req(
    instance_id: u8,
    record_handle: u32,
    request_count: u16,
    out: &mut [u8],
) -> Result<usize, Error> {
    if out.len() < 16 {
        return Err(Error::BufferTooSmall);
    }
    let hdr = PldmHeader::request(
        instance_id,
        PldmType::Platform,
        PlatformCommand::GetPdr as u8,
    );
    out[..3].copy_from_slice(&hdr.to_bytes());
    out[3..7].copy_from_slice(&record_handle.to_le_bytes());
    out[7..11].copy_from_slice(&0u32.to_le_bytes()); // dataTransferHandle
    out[11] = 0x01; // GetFirstPart
    out[12..14].copy_from_slice(&request_count.to_le_bytes());
    out[14..16].copy_from_slice(&0u16.to_le_bytes()); // recordChangeNumber
    Ok(16)
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
                if data.len() != 5 {
                    return write_completion(hdr, CompletionCode::InvalidLength as u8, resp);
                }
                let effecter_id = u16::from_le_bytes([data[0], data[1]]);
                let count = data[2];
                let set_req = data[3];
                if count != 1 || (set_req != 0 && set_req != 1) {
                    return write_completion(hdr, CompletionCode::InvalidData as u8, resp);
                }
                if resp.len() < 4 {
                    return Err(Error::BufferTooSmall);
                }
                let Some(eff) = self
                    .effecters
                    .iter_mut()
                    .flatten()
                    .find(|e| e.effecter_id == effecter_id)
                else {
                    return write_completion(hdr, 0x80, resp); // INVALID_EFFECTER_ID
                };
                if set_req == 1 {
                    eff.state = data[4];
                }
                write_completion(hdr, CompletionCode::Success as u8, resp)
            }
            x if x == PlatformCommand::GetPdrRepositoryInfo as u8 => {
                if !data.is_empty() {
                    return write_completion(hdr, CompletionCode::InvalidLength as u8, resp);
                }
                // DSP0248 §26.1: 40 bytes after CC (44 total).
                if resp.len() < 44 {
                    return Err(Error::BufferTooSmall);
                }
                let record_count = self.sensors.iter().flatten().count() as u32
                    + self.effecters.iter().flatten().count() as u32;
                // Valid DSP0248 timestamp104: 2026-01-01 00:00:00 UTC.
                const TS_2026: [u8; 13] = [0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 0xEA, 0x07, 0];
                write_completion(hdr, CompletionCode::Success as u8, resp)?;
                resp[4] = 0x00; // repositoryState = available
                resp[5..18].copy_from_slice(&TS_2026);
                resp[18..31].copy_from_slice(&TS_2026);
                resp[31..35].copy_from_slice(&record_count.to_le_bytes());
                resp[35..39].copy_from_slice(&record_count.saturating_mul(16).to_le_bytes());
                resp[39..43].copy_from_slice(&16u32.to_le_bytes()); // largest record size
                resp[43] = 0x00; // dataTransferHandleTimeout
                Ok(44)
            }
            x if x == PlatformCommand::GetPdr as u8 => {
                // DSP0248 §26.2: recordHandle(4) + dataTransferHandle(4) +
                // transferOperationFlag(1) + requestCount(2) + recordChangeNumber(2) = 13 bytes.
                if data.len() != 13 {
                    return write_completion(hdr, CompletionCode::InvalidLength as u8, resp);
                }
                let record_handle = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
                let transfer_op_flag = data[8];
                let request_count = u16::from_le_bytes([data[9], data[10]]);
                if transfer_op_flag != 0x01 {
                    return write_completion(hdr, CompletionCode::InvalidData as u8, resp);
                }
                let sensor_count = self.sensors.iter().flatten().count();
                let effecter_count = self.effecters.iter().flatten().count();
                let total = sensor_count + effecter_count;
                if total == 0 {
                    if record_handle != 0 {
                        return write_completion(hdr, 0x82, resp); // INVALID_RECORD_HANDLE
                    }
                    if resp.len() < 15 {
                        return Err(Error::BufferTooSmall);
                    }
                    write_completion(hdr, CompletionCode::Success as u8, resp)?;
                    resp[4..8].copy_from_slice(&0u32.to_le_bytes());
                    resp[8..12].copy_from_slice(&0u32.to_le_bytes());
                    resp[12] = 0x05; // StartAndEnd
                    resp[13..15].copy_from_slice(&0u16.to_le_bytes());
                    return Ok(15);
                }
                let idx = if record_handle == 0 {
                    0usize
                } else if (record_handle as usize) <= total {
                    record_handle as usize - 1
                } else {
                    return write_completion(hdr, 0x82, resp); // INVALID_RECORD_HANDLE
                };
                if request_count == 0 {
                    return write_completion(hdr, CompletionCode::InvalidData as u8, resp);
                }
                let handle = (idx + 1) as u32;
                let next_handle = if idx + 1 < total {
                    (idx + 2) as u32
                } else {
                    0u32
                };
                let (pdr_type, entity_id) = if idx < sensor_count {
                    let s = self.sensors.iter().flatten().nth(idx).unwrap();
                    (0x02u8, s.sensor_id) // Numeric Sensor PDR
                } else {
                    let e = self
                        .effecters
                        .iter()
                        .flatten()
                        .nth(idx - sensor_count)
                        .unwrap();
                    (0x0Bu8, e.effecter_id) // State Effecter PDR
                };
                // 10-byte common PDR header (DSP0248 §28.1) + 6-byte compact descriptor = 16 bytes.
                let mut record = [0u8; 16];
                record[0..4].copy_from_slice(&handle.to_le_bytes());
                record[4] = 0x01; // PDRHeaderVersion = 1
                record[5] = pdr_type;
                record[6..8].copy_from_slice(&0u16.to_le_bytes()); // recordChangeNumber
                record[8..10].copy_from_slice(&6u16.to_le_bytes()); // dataLength = 6
                record[10..12].copy_from_slice(&0u16.to_le_bytes()); // PLDMTerminusHandle
                record[12..14].copy_from_slice(&entity_id.to_le_bytes());
                record[14..16].copy_from_slice(&1u16.to_le_bytes()); // entityType

                let copy_len = (request_count as usize).min(record.len());
                if resp.len() < 15 + copy_len {
                    return Err(Error::BufferTooSmall);
                }
                write_completion(hdr, CompletionCode::Success as u8, resp)?;
                resp[4..8].copy_from_slice(&next_handle.to_le_bytes());
                resp[8..12].copy_from_slice(&0u32.to_le_bytes()); // nextDataTransferHandle
                resp[12] = 0x05; // StartAndEnd
                resp[13..15].copy_from_slice(&(copy_len as u16).to_le_bytes());
                resp[15..15 + copy_len].copy_from_slice(&record[..copy_len]);
                Ok(15 + copy_len)
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
