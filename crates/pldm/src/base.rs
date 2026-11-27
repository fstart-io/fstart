//! PLDM Base Messaging Control and Discovery (Type 0, DSP0240).

use crate::{CompletionCode, Error, PldmHeader, PldmType, write_completion};

/// PLDM Base command codes (DSP0240 Table 6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum BaseCommand {
    SetTid = 0x01,
    GetTid = 0x02,
    GetPldmVersion = 0x03,
    GetPldmTypes = 0x04,
    GetPldmCommands = 0x05,
}

/// Encode `SetTID` request.
pub fn encode_set_tid_req(instance_id: u8, tid: u8, out: &mut [u8]) -> Result<usize, Error> {
    if out.len() < 4 {
        return Err(Error::BufferTooSmall);
    }
    let hdr = PldmHeader::request(instance_id, PldmType::Base, BaseCommand::SetTid as u8);
    out[..3].copy_from_slice(&hdr.to_bytes());
    out[3] = tid;
    Ok(4)
}

/// Encode `GetTID` request.
pub fn encode_get_tid_req(instance_id: u8, out: &mut [u8]) -> Result<usize, Error> {
    if out.len() < 3 {
        return Err(Error::BufferTooSmall);
    }
    let hdr = PldmHeader::request(instance_id, PldmType::Base, BaseCommand::GetTid as u8);
    out[..3].copy_from_slice(&hdr.to_bytes());
    Ok(3)
}

/// Encode `GetPLDMTypes` request.
pub fn encode_get_pldm_types_req(instance_id: u8, out: &mut [u8]) -> Result<usize, Error> {
    if out.len() < 3 {
        return Err(Error::BufferTooSmall);
    }
    let hdr = PldmHeader::request(instance_id, PldmType::Base, BaseCommand::GetPldmTypes as u8);
    out[..3].copy_from_slice(&hdr.to_bytes());
    Ok(3)
}

/// Encode `GetPLDMVersion` request (`transfer_op_flag = 0x01` GetFirstPart).
pub fn encode_get_pldm_version_req(
    instance_id: u8,
    pldm_type: PldmType,
    out: &mut [u8],
) -> Result<usize, Error> {
    if out.len() < 9 {
        return Err(Error::BufferTooSmall);
    }
    let hdr = PldmHeader::request(
        instance_id,
        PldmType::Base,
        BaseCommand::GetPldmVersion as u8,
    );
    out[..3].copy_from_slice(&hdr.to_bytes());
    out[3..7].copy_from_slice(&0u32.to_le_bytes()); // data_transfer_handle
    out[7] = 0x01; // GetFirstPart
    out[8] = pldm_type as u8;
    Ok(9)
}

/// Stateful PLDM Base (Type 0) responder.
pub struct BaseResponder {
    tid: u8,
    supported_types_bitmap: [u8; 8],
}

impl BaseResponder {
    /// Create a Base responder with initial `tid` and list of supported [`PldmType`]s.
    pub fn new(tid: u8, types: &[PldmType]) -> Self {
        let mut bitmap = [0u8; 8];
        // Base (Type 0) is always supported.
        bitmap[0] |= 1 << 0;
        for &t in types {
            let v = t as usize;
            if v < 64 {
                bitmap[v / 8] |= 1 << (v % 8);
            }
        }
        Self {
            tid,
            supported_types_bitmap: bitmap,
        }
    }

    pub const fn tid(&self) -> u8 {
        self.tid
    }

    /// Handle a PLDM Base request.
    pub fn handle_request(
        &mut self,
        hdr: PldmHeader,
        data: &[u8],
        resp: &mut [u8],
    ) -> Result<usize, Error> {
        match hdr.command_code {
            x if x == BaseCommand::SetTid as u8 => {
                if data.len() != 1 {
                    return write_completion(hdr, CompletionCode::InvalidLength as u8, resp);
                }
                if data[0] == 0x00 || data[0] == 0xFF {
                    return write_completion(hdr, CompletionCode::InvalidData as u8, resp);
                }
                if resp.len() < 4 {
                    return Err(Error::BufferTooSmall);
                }
                self.tid = data[0];
                write_completion(hdr, CompletionCode::Success as u8, resp)
            }
            x if x == BaseCommand::GetTid as u8 => {
                if !data.is_empty() {
                    return write_completion(hdr, CompletionCode::InvalidLength as u8, resp);
                }
                if resp.len() < 5 {
                    return Err(Error::BufferTooSmall);
                }
                write_completion(hdr, CompletionCode::Success as u8, resp)?;
                resp[4] = self.tid;
                Ok(5)
            }
            x if x == BaseCommand::GetPldmTypes as u8 => {
                if !data.is_empty() {
                    return write_completion(hdr, CompletionCode::InvalidLength as u8, resp);
                }
                if resp.len() < 12 {
                    return Err(Error::BufferTooSmall);
                }
                write_completion(hdr, CompletionCode::Success as u8, resp)?;
                resp[4..12].copy_from_slice(&self.supported_types_bitmap);
                Ok(12)
            }
            x if x == BaseCommand::GetPldmVersion as u8 => {
                if data.len() != 6 {
                    return write_completion(hdr, CompletionCode::InvalidLength as u8, resp);
                }
                let op_flag = data[4];
                // DSP0240 §8.4: GetFirstPart (0x01) ignores data_transfer_handle.
                if op_flag != 0x01 {
                    return write_completion(hdr, CompletionCode::InvalidData as u8, resp);
                }
                let pldm_type = data[5];
                if (pldm_type as usize) >= 64
                    || (self.supported_types_bitmap[pldm_type as usize / 8]
                        & (1 << (pldm_type as usize % 8)))
                        == 0
                {
                    return write_completion(
                        hdr,
                        CompletionCode::InvalidPldmTypeInRequestData as u8,
                        resp,
                    );
                }
                if resp.len() < 17 {
                    return Err(Error::BufferTooSmall);
                }
                write_completion(hdr, CompletionCode::Success as u8, resp)?;
                resp[4..8].copy_from_slice(&0u32.to_le_bytes()); // next_transfer_handle = 0
                resp[8] = 0x05; // StartAndEnd
                // Ver32 for v1.0.0: [alpha=0x00, update=0xF0, minor=0xF0, major=0xF1]
                let ver: [u8; 4] = [0x00, 0xF0, 0xF0, 0xF1];
                resp[9..13].copy_from_slice(&ver);
                let crc = crc32_ieee(&ver);
                resp[13..17].copy_from_slice(&crc.to_le_bytes());
                Ok(17)
            }
            x if x == BaseCommand::GetPldmCommands as u8 => {
                if data.len() != 5 {
                    return write_completion(hdr, CompletionCode::InvalidLength as u8, resp);
                }
                let pldm_type = data[0];
                if (pldm_type as usize) >= 64
                    || (self.supported_types_bitmap[pldm_type as usize / 8]
                        & (1 << (pldm_type as usize % 8)))
                        == 0
                {
                    return write_completion(
                        hdr,
                        CompletionCode::InvalidPldmTypeInRequestData as u8,
                        resp,
                    );
                }
                // Only v1.0.0 ([0x00, 0xF0, 0xF0, 0xF1]) is supported.
                if data[1..5] != [0x00, 0xF0, 0xF0, 0xF1] {
                    return write_completion(hdr, 0x84, resp); // INVALID_PLDM_VERSION_IN_REQUEST_DATA
                }
                if resp.len() < 36 {
                    return Err(Error::BufferTooSmall);
                }
                let mut cmds = [0u8; 32];
                match pldm_type {
                    0 => {
                        // SetTID(1), GetTID(2), GetPLDMVersion(3), GetPLDMTypes(4), GetPLDMCommands(5)
                        cmds[0] = 0b0011_1110;
                    }
                    2 => {
                        // GetSensorReading(0x11), SetStateEffecterStates(0x39), GetPDRRepoInfo(0x50)
                        cmds[0x11 / 8] |= 1 << (0x11 % 8);
                        cmds[0x39 / 8] |= 1 << (0x39 % 8);
                        cmds[0x50 / 8] |= 1 << (0x50 % 8);
                    }
                    5 => {
                        for c in [0x01usize, 0x02, 0x10, 0x13, 0x14, 0x1A, 0x1B, 0x1D] {
                            cmds[c / 8] |= 1 << (c % 8);
                        }
                    }
                    _ => {
                        return write_completion(
                            hdr,
                            CompletionCode::InvalidPldmTypeInRequestData as u8,
                            resp,
                        );
                    }
                }
                write_completion(hdr, CompletionCode::Success as u8, resp)?;
                resp[4..36].copy_from_slice(&cmds);
                Ok(36)
            }
            _ => write_completion(hdr, CompletionCode::UnsupportedCmd as u8, resp),
        }
    }
}

/// Standard IEEE 802.3 CRC-32 (polynomial `0xEDB88320`) used by PLDM `GetPLDMVersion`.
pub fn crc32_ieee(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}
