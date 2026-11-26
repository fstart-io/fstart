//! PLDM for Firmware Update (Type 5, DSP0267).
//!
//! Provides both the `no_std` **Firmware Device (FD)** state machine and the
//! `no_std` **Update Agent (UA)** request/response codecs so either the BMC or
//! the Host can act as UA or FD.

use crate::{CompletionCode, Error, PldmHeader, PldmType, write_completion};

/// PLDM Firmware Update command codes (DSP0267 Table 7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum FwUpdateCommand {
    QueryDeviceIdentifiers = 0x01,
    GetFirmwareParameters = 0x02,
    RequestUpdate = 0x10,
    PassComponentTable = 0x13,
    UpdateComponent = 0x14,
    RequestFirmwareData = 0x15,
    TransferComplete = 0x16,
    VerifyComplete = 0x17,
    ApplyComplete = 0x18,
    ActivateFirmware = 0x1A,
    GetStatus = 0x1B,
    CancelUpdate = 0x1D,
}

/// Firmware Device state machine states (DSP0267 Table 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum FdState {
    Idle = 0,
    LearnComponents = 1,
    ReadyXfer = 2,
    Download = 3,
    Verify = 4,
    Apply = 5,
    Activate = 6,
}

/// Encode `RequestUpdate` (UA -> FD).
pub fn encode_request_update_req(
    instance_id: u8,
    max_transfer_size: u32,
    num_components: u16,
    max_outstanding: u8,
    pkg_data_len: u16,
    version_str: &[u8],
    out: &mut [u8],
) -> Result<usize, Error> {
    let total = 3 + 4 + 2 + 1 + 2 + 1 + 1 + version_str.len();
    if out.len() < total || version_str.len() > 255 {
        return Err(Error::BufferTooSmall);
    }
    let hdr = PldmHeader::request(
        instance_id,
        PldmType::FirmwareUpdate,
        FwUpdateCommand::RequestUpdate as u8,
    );
    out[..3].copy_from_slice(&hdr.to_bytes());
    out[3..7].copy_from_slice(&max_transfer_size.to_le_bytes());
    out[7..9].copy_from_slice(&num_components.to_le_bytes());
    out[9] = max_outstanding;
    out[10..12].copy_from_slice(&pkg_data_len.to_le_bytes());
    out[12] = 0x01; // ASCII
    out[13] = version_str.len() as u8;
    out[14..total].copy_from_slice(version_str);
    Ok(total)
}

/// Encode `PassComponentTable` (UA -> FD) for a single component (`StartAndEnd`).
pub fn encode_pass_component_req(
    instance_id: u8,
    classification: u16,
    identifier: u16,
    stamp: u32,
    version_str: &[u8],
    out: &mut [u8],
) -> Result<usize, Error> {
    let total = 3 + 1 + 2 + 2 + 1 + 4 + 1 + 1 + version_str.len();
    if out.len() < total || version_str.len() > 255 {
        return Err(Error::BufferTooSmall);
    }
    let hdr = PldmHeader::request(
        instance_id,
        PldmType::FirmwareUpdate,
        FwUpdateCommand::PassComponentTable as u8,
    );
    out[..3].copy_from_slice(&hdr.to_bytes());
    out[3] = 0x05; // StartAndEnd
    out[4..6].copy_from_slice(&classification.to_le_bytes());
    out[6..8].copy_from_slice(&identifier.to_le_bytes());
    out[8] = 0; // classification_index
    out[9..13].copy_from_slice(&stamp.to_le_bytes());
    out[13] = 0x01; // ASCII
    out[14] = version_str.len() as u8;
    out[15..total].copy_from_slice(version_str);
    Ok(total)
}

/// Encode `UpdateComponent` (UA -> FD).
pub fn encode_update_component_req(
    instance_id: u8,
    classification: u16,
    identifier: u16,
    stamp: u32,
    image_size: u32,
    version_str: &[u8],
    out: &mut [u8],
) -> Result<usize, Error> {
    let total = 3 + 2 + 2 + 1 + 4 + 4 + 4 + 1 + 1 + version_str.len();
    if out.len() < total || version_str.len() > 255 {
        return Err(Error::BufferTooSmall);
    }
    let hdr = PldmHeader::request(
        instance_id,
        PldmType::FirmwareUpdate,
        FwUpdateCommand::UpdateComponent as u8,
    );
    out[..3].copy_from_slice(&hdr.to_bytes());
    out[3..5].copy_from_slice(&classification.to_le_bytes());
    out[5..7].copy_from_slice(&identifier.to_le_bytes());
    out[7] = 0; // classification_index
    out[8..12].copy_from_slice(&stamp.to_le_bytes());
    out[12..16].copy_from_slice(&image_size.to_le_bytes());
    out[16..20].copy_from_slice(&0u32.to_le_bytes()); // update_option_flags
    out[20] = 0x01; // ASCII
    out[21] = version_str.len() as u8;
    out[22..total].copy_from_slice(version_str);
    Ok(total)
}

/// Encode `ActivateFirmware` (UA -> FD).
pub fn encode_activate_firmware_req(
    instance_id: u8,
    self_contained: bool,
    out: &mut [u8],
) -> Result<usize, Error> {
    if out.len() < 4 {
        return Err(Error::BufferTooSmall);
    }
    let hdr = PldmHeader::request(
        instance_id,
        PldmType::FirmwareUpdate,
        FwUpdateCommand::ActivateFirmware as u8,
    );
    out[..3].copy_from_slice(&hdr.to_bytes());
    out[3] = u8::from(self_contained);
    Ok(4)
}

/// Encode `GetStatus` (UA -> FD).
pub fn encode_get_status_req(instance_id: u8, out: &mut [u8]) -> Result<usize, Error> {
    if out.len() < 3 {
        return Err(Error::BufferTooSmall);
    }
    let hdr = PldmHeader::request(
        instance_id,
        PldmType::FirmwareUpdate,
        FwUpdateCommand::GetStatus as u8,
    );
    out[..3].copy_from_slice(&hdr.to_bytes());
    Ok(3)
}

/// Allocation-free PLDM Firmware Device (FD) responder state machine (DSP0267).
pub struct FirmwareDevice<const MAX_IMAGE: usize = 4096> {
    state: FdState,
    prev_state: FdState,
    vendor_iana: u32,
    max_transfer_size: u32,
    expected_image_size: usize,
    received_bytes: usize,
    image_buf: [u8; MAX_IMAGE],
    activated: bool,
}

impl<const MAX_IMAGE: usize> FirmwareDevice<MAX_IMAGE> {
    pub const fn new(vendor_iana: u32) -> Self {
        Self {
            state: FdState::Idle,
            prev_state: FdState::Idle,
            vendor_iana,
            max_transfer_size: 256,
            expected_image_size: 0,
            received_bytes: 0,
            image_buf: [0; MAX_IMAGE],
            activated: false,
        }
    }

    pub const fn state(&self) -> FdState {
        self.state
    }

    pub const fn activated(&self) -> bool {
        self.activated
    }

    pub fn downloaded_image(&self) -> &[u8] {
        &self.image_buf[..self.received_bytes]
    }

    fn transition(&mut self, next: FdState) {
        self.prev_state = self.state;
        self.state = next;
    }

    /// Handle an incoming UA -> FD command.
    pub fn handle_request(
        &mut self,
        hdr: PldmHeader,
        data: &[u8],
        resp: &mut [u8],
    ) -> Result<usize, Error> {
        match hdr.command_code {
            x if x == FwUpdateCommand::QueryDeviceIdentifiers as u8 => {
                if !data.is_empty() {
                    return write_completion(hdr, CompletionCode::InvalidLength as u8, resp);
                }
                // CC(1) + device_identifiers_len(4) + descriptor_count(1) + IANA descriptor(2+2+4)
                if resp.len() < 17 {
                    return Err(Error::BufferTooSmall);
                }
                write_completion(hdr, CompletionCode::Success as u8, resp)?;
                resp[4..8].copy_from_slice(&8u32.to_le_bytes()); // descriptor bytes
                resp[8] = 1; // 1 descriptor
                resp[9..11].copy_from_slice(&0x0001u16.to_le_bytes()); // IANA Enterprise ID
                resp[11..13].copy_from_slice(&4u16.to_le_bytes());
                resp[13..17].copy_from_slice(&self.vendor_iana.to_le_bytes());
                Ok(17)
            }
            x if x == FwUpdateCommand::RequestUpdate as u8 => {
                if self.state != FdState::Idle {
                    return write_completion(hdr, CompletionCode::AlreadyInUpdateMode as u8, resp);
                }
                if data.len() < 11 {
                    return write_completion(hdr, CompletionCode::InvalidLength as u8, resp);
                }
                let req_max = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
                self.max_transfer_size = req_max.clamp(32, 512);
                self.transition(FdState::LearnComponents);
                if resp.len() < 7 {
                    return Err(Error::BufferTooSmall);
                }
                write_completion(hdr, CompletionCode::Success as u8, resp)?;
                resp[4..6].copy_from_slice(&0u16.to_le_bytes()); // fd_metadata_len = 0
                resp[6] = 0; // fd_will_send_pkg_data = 0
                Ok(7)
            }
            x if x == FwUpdateCommand::PassComponentTable as u8 => {
                if self.state != FdState::LearnComponents {
                    return write_completion(
                        hdr,
                        CompletionCode::InvalidStateForCommand as u8,
                        resp,
                    );
                }
                if data.len() < 12 {
                    return write_completion(hdr, CompletionCode::InvalidLength as u8, resp);
                }
                let transfer_flag = data[0];
                if transfer_flag == 0x04 || transfer_flag == 0x05 {
                    self.transition(FdState::ReadyXfer);
                }
                if resp.len() < 6 {
                    return Err(Error::BufferTooSmall);
                }
                write_completion(hdr, CompletionCode::Success as u8, resp)?;
                resp[4] = 0x00; // component_response = can be updated
                resp[5] = 0x00; // component_response_code = OK
                Ok(6)
            }
            x if x == FwUpdateCommand::UpdateComponent as u8 => {
                if self.state != FdState::ReadyXfer {
                    return write_completion(
                        hdr,
                        CompletionCode::InvalidStateForCommand as u8,
                        resp,
                    );
                }
                if data.len() < 19 {
                    return write_completion(hdr, CompletionCode::InvalidLength as u8, resp);
                }
                let size = u32::from_le_bytes([data[9], data[10], data[11], data[12]]) as usize;
                if size == 0 || size > MAX_IMAGE {
                    return write_completion(hdr, CompletionCode::InvalidData as u8, resp);
                }
                self.expected_image_size = size;
                self.received_bytes = 0;
                self.transition(FdState::Download);
                if resp.len() < 12 {
                    return Err(Error::BufferTooSmall);
                }
                write_completion(hdr, CompletionCode::Success as u8, resp)?;
                resp[4] = 0x00; // component_compat_response = can be updated
                resp[5] = 0x00; // compat_response_code = OK
                resp[6..10].copy_from_slice(&0u32.to_le_bytes()); // update_option_flags_enabled
                resp[10..12].copy_from_slice(&1u16.to_le_bytes()); // estimated_time_before_req_fw_data
                Ok(12)
            }
            x if x == FwUpdateCommand::ActivateFirmware as u8 => {
                if self.state != FdState::ReadyXfer {
                    return write_completion(
                        hdr,
                        CompletionCode::InvalidStateForCommand as u8,
                        resp,
                    );
                }
                self.transition(FdState::Activate);
                self.activated = true;
                self.transition(FdState::Idle);
                if resp.len() < 6 {
                    return Err(Error::BufferTooSmall);
                }
                write_completion(hdr, CompletionCode::Success as u8, resp)?;
                resp[4..6].copy_from_slice(&0u16.to_le_bytes()); // estimated_time_for_self_contained
                Ok(6)
            }
            x if x == FwUpdateCommand::GetStatus as u8 => {
                if resp.len() < 14 {
                    return Err(Error::BufferTooSmall);
                }
                write_completion(hdr, CompletionCode::Success as u8, resp)?;
                resp[4] = self.state as u8;
                resp[5] = self.prev_state as u8;
                resp[6] = 0; // aux_state
                resp[7] = 0; // aux_state_status
                resp[8] = self
                    .received_bytes
                    .saturating_mul(100)
                    .checked_div(self.expected_image_size)
                    .map_or(101, |pct| pct.min(100) as u8);
                resp[9] = 0; // reason_code
                resp[10..14].copy_from_slice(&0u32.to_le_bytes()); // update_option_flags_enabled
                Ok(14)
            }
            x if x == FwUpdateCommand::CancelUpdate as u8 => {
                self.transition(FdState::Idle);
                self.received_bytes = 0;
                self.expected_image_size = 0;
                if resp.len() < 13 {
                    return Err(Error::BufferTooSmall);
                }
                write_completion(hdr, CompletionCode::Success as u8, resp)?;
                resp[4] = 0; // non_functioning_component_indication = false
                resp[5..13].fill(0); // non_functioning_component_bitmap
                Ok(13)
            }
            _ => write_completion(hdr, CompletionCode::UnsupportedCmd as u8, resp),
        }
    }

    /// While in [`FdState::Download`], build the next `RequestFirmwareData`
    /// (FD -> UA) request. Returns `Ok(None)` if all bytes have been downloaded.
    pub fn next_fw_data_request(
        &self,
        instance_id: u8,
        out: &mut [u8],
    ) -> Result<Option<(usize, usize)>, Error> {
        if self.state != FdState::Download || self.received_bytes >= self.expected_image_size {
            return Ok(None);
        }
        if out.len() < 11 {
            return Err(Error::BufferTooSmall);
        }
        let remaining = self.expected_image_size - self.received_bytes;
        let req_len = remaining.min(self.max_transfer_size as usize).max(32);
        let hdr = PldmHeader::request(
            instance_id,
            PldmType::FirmwareUpdate,
            FwUpdateCommand::RequestFirmwareData as u8,
        );
        out[..3].copy_from_slice(&hdr.to_bytes());
        out[3..7].copy_from_slice(&(self.received_bytes as u32).to_le_bytes());
        out[7..11].copy_from_slice(&(req_len as u32).to_le_bytes());
        Ok(Some((11, req_len)))
    }

    /// Feed a chunk returned by the UA in response to `RequestFirmwareData`.
    /// When the full image is received, advances through Verify -> Apply -> ReadyXfer.
    pub fn push_fw_data_chunk(&mut self, chunk: &[u8]) -> Result<bool, Error> {
        if self.state != FdState::Download {
            return Err(Error::InvalidData);
        }
        let remaining = self.expected_image_size - self.received_bytes;
        let take = chunk.len().min(remaining);
        self.image_buf[self.received_bytes..self.received_bytes + take]
            .copy_from_slice(&chunk[..take]);
        self.received_bytes += take;
        if self.received_bytes >= self.expected_image_size {
            self.transition(FdState::Verify);
            self.transition(FdState::Apply);
            self.transition(FdState::ReadyXfer);
            return Ok(true);
        }
        Ok(false)
    }
}
