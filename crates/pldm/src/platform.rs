//! PLDM Platform Monitoring and Control (Type 2, DSP0248).
//!
//! Covers numeric sensor reading (`GetSensorReading`), state effecter control
//! (`SetStateEffecterStates`), and PDR repository discovery (`GetPDRRepositoryInfo`,
//! `GetPDR`).

use crate::{CompletionCode, Error, PldmHeader, PldmType, write_completion};
use zerocopy::little_endian::{U16 as LeU16, U32 as LeU32};
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned};

#[derive(FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct GetSensorReadingReq {
    sensor_id: LeU16,
    rearm: u8,
}

#[derive(FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct GetSensorReadingResp {
    data_size: u8,
    op_state: u8,
    event_msg_enable: u8,
    present_state: u8,
    prev_state: u8,
    event_state: u8,
    present_reading: LeU32,
}

#[derive(FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct SetStateEffecterReq {
    effecter_id: LeU16,
    composite_count: u8,
    set_request: u8,
    state: u8,
}

#[derive(FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct GetPdrRepoInfoResp {
    repo_state: u8,
    update_time: [u8; 13],
    oem_update_time: [u8; 13],
    record_count: LeU32,
    repo_size: LeU32,
    largest_record_size: LeU32,
    timeout: u8,
}

#[derive(FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct GetPdrReq {
    record_handle: LeU32,
    data_transfer_handle: LeU32,
    transfer_op_flag: u8,
    request_count: LeU16,
    record_change_number: LeU16,
}

#[derive(FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct GetPdrRespHeader {
    next_record_handle: LeU32,
    next_data_transfer_handle: LeU32,
    transfer_flag: u8,
    response_count: LeU16,
}

#[derive(FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct CompactPdrRecord {
    record_handle: LeU32,
    hdr_version: u8,
    pdr_type: u8,
    record_change_number: LeU16,
    data_length: LeU16,
    terminus_handle: LeU16,
    entity_id: LeU16,
    entity_type: LeU16,
}

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
    let total = PldmHeader::LEN + core::mem::size_of::<GetSensorReadingReq>();
    if out.len() < total {
        return Err(Error::BufferTooSmall);
    }
    let hdr = PldmHeader::request(
        instance_id,
        PldmType::Platform,
        PlatformCommand::GetSensorReading as u8,
    );
    out[..PldmHeader::LEN].copy_from_slice(&hdr.to_bytes());
    let body = GetSensorReadingReq {
        sensor_id: LeU16::new(sensor_id),
        rearm: 0,
    };
    body.write_to_prefix(&mut out[PldmHeader::LEN..])
        .map_err(|_| Error::BufferTooSmall)?;
    Ok(total)
}

/// Encode `SetStateEffecterStates` request for a single-state effecter.
pub fn encode_set_state_effecter_req(
    instance_id: u8,
    effecter_id: u16,
    state: u8,
    out: &mut [u8],
) -> Result<usize, Error> {
    let total = PldmHeader::LEN + core::mem::size_of::<SetStateEffecterReq>();
    if out.len() < total {
        return Err(Error::BufferTooSmall);
    }
    let hdr = PldmHeader::request(
        instance_id,
        PldmType::Platform,
        PlatformCommand::SetStateEffecterStates as u8,
    );
    out[..PldmHeader::LEN].copy_from_slice(&hdr.to_bytes());
    let body = SetStateEffecterReq {
        effecter_id: LeU16::new(effecter_id),
        composite_count: 1,
        set_request: 1, // requestSet
        state,
    };
    body.write_to_prefix(&mut out[PldmHeader::LEN..])
        .map_err(|_| Error::BufferTooSmall)?;
    Ok(total)
}

/// Encode `GetPDRRepositoryInfo` request.
pub fn encode_get_pdr_repo_info_req(instance_id: u8, out: &mut [u8]) -> Result<usize, Error> {
    if out.len() < PldmHeader::LEN {
        return Err(Error::BufferTooSmall);
    }
    let hdr = PldmHeader::request(
        instance_id,
        PldmType::Platform,
        PlatformCommand::GetPdrRepositoryInfo as u8,
    );
    out[..PldmHeader::LEN].copy_from_slice(&hdr.to_bytes());
    Ok(PldmHeader::LEN)
}

/// Encode `GetPDR` request (`transferOperationFlag = 0x01` GetFirstPart).
pub fn encode_get_pdr_req(
    instance_id: u8,
    record_handle: u32,
    request_count: u16,
    out: &mut [u8],
) -> Result<usize, Error> {
    let total = PldmHeader::LEN + core::mem::size_of::<GetPdrReq>();
    if out.len() < total {
        return Err(Error::BufferTooSmall);
    }
    let hdr = PldmHeader::request(
        instance_id,
        PldmType::Platform,
        PlatformCommand::GetPdr as u8,
    );
    out[..PldmHeader::LEN].copy_from_slice(&hdr.to_bytes());
    let body = GetPdrReq {
        record_handle: LeU32::new(record_handle),
        data_transfer_handle: LeU32::new(0),
        transfer_op_flag: 0x01, // GetFirstPart
        request_count: LeU16::new(request_count),
        record_change_number: LeU16::new(0),
    };
    body.write_to_prefix(&mut out[PldmHeader::LEN..])
        .map_err(|_| Error::BufferTooSmall)?;
    Ok(total)
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
                let Ok(req) = GetSensorReadingReq::ref_from_bytes(data) else {
                    return write_completion(hdr, CompletionCode::InvalidLength as u8, resp);
                };
                let sensor_id = req.sensor_id.get();
                let Some(sensor) = self
                    .sensors
                    .iter()
                    .flatten()
                    .find(|s| s.sensor_id == sensor_id)
                else {
                    return write_completion(hdr, 0x80, resp); // INVALID_SENSOR_ID
                };
                let total = 4 + core::mem::size_of::<GetSensorReadingResp>();
                if resp.len() < total {
                    return Err(Error::BufferTooSmall);
                }
                write_completion(hdr, CompletionCode::Success as u8, resp)?;
                let body = GetSensorReadingResp {
                    data_size: 4,        // uint32
                    op_state: 0,         // enabled
                    event_msg_enable: 0, // noEventGeneration
                    present_state: 1,    // normal
                    prev_state: 1,
                    event_state: 1,
                    present_reading: LeU32::new(sensor.reading),
                };
                body.write_to_prefix(&mut resp[4..])
                    .map_err(|_| Error::BufferTooSmall)?;
                Ok(total)
            }
            x if x == PlatformCommand::SetStateEffecterStates as u8 => {
                let Ok(req) = SetStateEffecterReq::ref_from_bytes(data) else {
                    return write_completion(hdr, CompletionCode::InvalidLength as u8, resp);
                };
                if req.composite_count != 1 || (req.set_request != 0 && req.set_request != 1) {
                    return write_completion(hdr, CompletionCode::InvalidData as u8, resp);
                }
                if resp.len() < 4 {
                    return Err(Error::BufferTooSmall);
                }
                let effecter_id = req.effecter_id.get();
                let Some(eff) = self
                    .effecters
                    .iter_mut()
                    .flatten()
                    .find(|e| e.effecter_id == effecter_id)
                else {
                    return write_completion(hdr, 0x80, resp); // INVALID_EFFECTER_ID
                };
                if req.set_request == 1 {
                    eff.state = req.state;
                }
                write_completion(hdr, CompletionCode::Success as u8, resp)
            }
            x if x == PlatformCommand::GetPdrRepositoryInfo as u8 => {
                if !data.is_empty() {
                    return write_completion(hdr, CompletionCode::InvalidLength as u8, resp);
                }
                let total = 4 + core::mem::size_of::<GetPdrRepoInfoResp>();
                if resp.len() < total {
                    return Err(Error::BufferTooSmall);
                }
                let record_count = self.sensors.iter().flatten().count() as u32
                    + self.effecters.iter().flatten().count() as u32;
                let rec_size = core::mem::size_of::<CompactPdrRecord>() as u32;
                // Valid DSP0248 timestamp104: 2026-01-01 00:00:00 UTC.
                const TS_2026: [u8; 13] = [0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 0xEA, 0x07, 0];
                write_completion(hdr, CompletionCode::Success as u8, resp)?;
                let body = GetPdrRepoInfoResp {
                    repo_state: 0x00, // available
                    update_time: TS_2026,
                    oem_update_time: TS_2026,
                    record_count: LeU32::new(record_count),
                    repo_size: LeU32::new(record_count.saturating_mul(rec_size)),
                    largest_record_size: LeU32::new(rec_size),
                    timeout: 0x00,
                };
                body.write_to_prefix(&mut resp[4..])
                    .map_err(|_| Error::BufferTooSmall)?;
                Ok(total)
            }
            x if x == PlatformCommand::GetPdr as u8 => {
                let Ok(req) = GetPdrReq::ref_from_bytes(data) else {
                    return write_completion(hdr, CompletionCode::InvalidLength as u8, resp);
                };
                let record_handle = req.record_handle.get();
                let request_count = req.request_count.get();
                if req.transfer_op_flag != 0x01 {
                    return write_completion(hdr, CompletionCode::InvalidData as u8, resp);
                }
                let sensor_count = self.sensors.iter().flatten().count();
                let effecter_count = self.effecters.iter().flatten().count();
                let total = sensor_count + effecter_count;
                let hdr_end = 4 + core::mem::size_of::<GetPdrRespHeader>();
                if total == 0 {
                    if record_handle != 0 {
                        return write_completion(hdr, 0x82, resp); // INVALID_RECORD_HANDLE
                    }
                    if resp.len() < hdr_end {
                        return Err(Error::BufferTooSmall);
                    }
                    write_completion(hdr, CompletionCode::Success as u8, resp)?;
                    let rhdr = GetPdrRespHeader {
                        next_record_handle: LeU32::new(0),
                        next_data_transfer_handle: LeU32::new(0),
                        transfer_flag: 0x05, // StartAndEnd
                        response_count: LeU16::new(0),
                    };
                    rhdr.write_to_prefix(&mut resp[4..])
                        .map_err(|_| Error::BufferTooSmall)?;
                    return Ok(hdr_end);
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
                let record = CompactPdrRecord {
                    record_handle: LeU32::new(handle),
                    hdr_version: 0x01,
                    pdr_type,
                    record_change_number: LeU16::new(0),
                    data_length: LeU16::new(6),
                    terminus_handle: LeU16::new(0),
                    entity_id: LeU16::new(entity_id),
                    entity_type: LeU16::new(1),
                };
                let record_bytes = record.as_bytes();
                let copy_len = (request_count as usize).min(record_bytes.len());
                if resp.len() < hdr_end + copy_len {
                    return Err(Error::BufferTooSmall);
                }
                write_completion(hdr, CompletionCode::Success as u8, resp)?;
                let rhdr = GetPdrRespHeader {
                    next_record_handle: LeU32::new(next_handle),
                    next_data_transfer_handle: LeU32::new(0),
                    transfer_flag: 0x05, // StartAndEnd
                    response_count: LeU16::new(copy_len as u16),
                };
                rhdr.write_to_prefix(&mut resp[4..])
                    .map_err(|_| Error::BufferTooSmall)?;
                resp[hdr_end..hdr_end + copy_len].copy_from_slice(&record_bytes[..copy_len]);
                Ok(hdr_end + copy_len)
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
