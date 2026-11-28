//! PLDM for Firmware Update (Type 5, DSP0267).
//!
//! Provides both the `no_std` **Firmware Device (FD)** state machine and the
//! `no_std` **Update Agent (UA)** request/response codecs so either the BMC or
//! the Host can act as UA or FD.

use crate::{CompletionCode, Error, PldmHeader, PldmType, write_completion};
use zerocopy::little_endian::{U16 as LeU16, U32 as LeU32};
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned};

#[derive(FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct RequestUpdateReqHeader {
    max_transfer_size: LeU32,
    num_components: LeU16,
    max_outstanding: u8,
    pkg_data_len: LeU16,
    ver_str_type: u8,
    ver_str_len: u8,
}

#[derive(FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct PassComponentReqHeader {
    transfer_flag: u8,
    classification: LeU16,
    identifier: LeU16,
    classification_index: u8,
    stamp: LeU32,
    ver_str_type: u8,
    ver_str_len: u8,
}

#[derive(FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct UpdateComponentReqHeader {
    classification: LeU16,
    identifier: LeU16,
    classification_index: u8,
    stamp: LeU32,
    image_size: LeU32,
    update_option_flags: LeU32,
    ver_str_type: u8,
    ver_str_len: u8,
}

#[derive(FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct GetFwParamsResp {
    capabilities_during_update: LeU32,
    component_count: LeU16,
    active_ver_str_type: u8,
    active_ver_str_len: u8,
    pending_ver_str_type: u8,
    pending_ver_str_len: u8,
}

#[derive(FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct UpdateComponentResp {
    compatibility_response: u8,
    compatibility_response_code: u8,
    update_option_flags_enabled: LeU32,
    estimated_time: LeU16,
}

#[derive(FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct GetStatusResp {
    current_state: u8,
    previous_state: u8,
    aux_state: u8,
    aux_state_status: u8,
    progress_percent: u8,
    reason_code: u8,
    update_option_flags_enabled: LeU32,
}

#[derive(FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct RequestFwDataReq {
    offset: LeU32,
    length: LeU32,
}

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

/// Encode `GetFirmwareParameters` (UA -> FD).
pub fn encode_get_fw_params_req(instance_id: u8, out: &mut [u8]) -> Result<usize, Error> {
    if out.len() < 3 {
        return Err(Error::BufferTooSmall);
    }
    let hdr = PldmHeader::request(
        instance_id,
        PldmType::FirmwareUpdate,
        FwUpdateCommand::GetFirmwareParameters as u8,
    );
    out[..3].copy_from_slice(&hdr.to_bytes());
    Ok(3)
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
    let fixed = PldmHeader::LEN + core::mem::size_of::<RequestUpdateReqHeader>();
    let total = fixed + version_str.len();
    if out.len() < total || version_str.len() > 255 {
        return Err(Error::BufferTooSmall);
    }
    let hdr = PldmHeader::request(
        instance_id,
        PldmType::FirmwareUpdate,
        FwUpdateCommand::RequestUpdate as u8,
    );
    out[..PldmHeader::LEN].copy_from_slice(&hdr.to_bytes());
    let body = RequestUpdateReqHeader {
        max_transfer_size: LeU32::new(max_transfer_size),
        num_components: LeU16::new(num_components),
        max_outstanding,
        pkg_data_len: LeU16::new(pkg_data_len),
        ver_str_type: 0x01, // ASCII
        ver_str_len: version_str.len() as u8,
    };
    body.write_to_prefix(&mut out[PldmHeader::LEN..])
        .map_err(|_| Error::BufferTooSmall)?;
    out[fixed..total].copy_from_slice(version_str);
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
    let fixed = PldmHeader::LEN + core::mem::size_of::<PassComponentReqHeader>();
    let total = fixed + version_str.len();
    if out.len() < total || version_str.len() > 255 {
        return Err(Error::BufferTooSmall);
    }
    let hdr = PldmHeader::request(
        instance_id,
        PldmType::FirmwareUpdate,
        FwUpdateCommand::PassComponentTable as u8,
    );
    out[..PldmHeader::LEN].copy_from_slice(&hdr.to_bytes());
    let body = PassComponentReqHeader {
        transfer_flag: 0x05, // StartAndEnd
        classification: LeU16::new(classification),
        identifier: LeU16::new(identifier),
        classification_index: 0,
        stamp: LeU32::new(stamp),
        ver_str_type: 0x01, // ASCII
        ver_str_len: version_str.len() as u8,
    };
    body.write_to_prefix(&mut out[PldmHeader::LEN..])
        .map_err(|_| Error::BufferTooSmall)?;
    out[fixed..total].copy_from_slice(version_str);
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
    let fixed = PldmHeader::LEN + core::mem::size_of::<UpdateComponentReqHeader>();
    let total = fixed + version_str.len();
    if out.len() < total || version_str.len() > 255 {
        return Err(Error::BufferTooSmall);
    }
    let hdr = PldmHeader::request(
        instance_id,
        PldmType::FirmwareUpdate,
        FwUpdateCommand::UpdateComponent as u8,
    );
    out[..PldmHeader::LEN].copy_from_slice(&hdr.to_bytes());
    let body = UpdateComponentReqHeader {
        classification: LeU16::new(classification),
        identifier: LeU16::new(identifier),
        classification_index: 0,
        stamp: LeU32::new(stamp),
        image_size: LeU32::new(image_size),
        update_option_flags: LeU32::new(0),
        ver_str_type: 0x01, // ASCII
        ver_str_len: version_str.len() as u8,
    };
    body.write_to_prefix(&mut out[PldmHeader::LEN..])
        .map_err(|_| Error::BufferTooSmall)?;
    out[fixed..total].copy_from_slice(version_str);
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
    reason_code: u8,
    vendor_iana: u32,
    max_transfer_size: u32,
    expected_component: Option<(u16, u16)>,
    applied_components: u16,
    expected_image_size: usize,
    received_bytes: usize,
    pending_req_len: usize,
    image_buf: [u8; MAX_IMAGE],
    activated: bool,
}

impl<const MAX_IMAGE: usize> FirmwareDevice<MAX_IMAGE> {
    pub const fn new(vendor_iana: u32) -> Self {
        Self {
            state: FdState::Idle,
            prev_state: FdState::Idle,
            reason_code: 0,
            vendor_iana,
            max_transfer_size: 256,
            expected_component: None,
            applied_components: 0,
            expected_image_size: 0,
            received_bytes: 0,
            pending_req_len: 0,
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
    ///
    /// Always checks response buffer capacity before mutating FD state.
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
                if resp.len() < 17 {
                    return Err(Error::BufferTooSmall);
                }
                write_completion(hdr, CompletionCode::Success as u8, resp)?;
                resp[4..8].copy_from_slice(&8u32.to_le_bytes());
                resp[8] = 1;
                resp[9..11].copy_from_slice(&0x0001u16.to_le_bytes()); // IANA Enterprise ID
                resp[11..13].copy_from_slice(&4u16.to_le_bytes());
                resp[13..17].copy_from_slice(&self.vendor_iana.to_le_bytes());
                Ok(17)
            }
            x if x == FwUpdateCommand::GetFirmwareParameters as u8 => {
                if !data.is_empty() {
                    return write_completion(hdr, CompletionCode::InvalidLength as u8, resp);
                }
                let total = 4 + core::mem::size_of::<GetFwParamsResp>();
                if resp.len() < total {
                    return Err(Error::BufferTooSmall);
                }
                write_completion(hdr, CompletionCode::Success as u8, resp)?;
                let body = GetFwParamsResp {
                    capabilities_during_update: LeU32::new(0),
                    component_count: LeU16::new(0),
                    active_ver_str_type: 0x01, // ASCII
                    active_ver_str_len: 0,
                    pending_ver_str_type: 0x00, // Unknown (no pending image, DSP0267 §10.2)
                    pending_ver_str_len: 0,
                };
                body.write_to_prefix(&mut resp[4..])
                    .map_err(|_| Error::BufferTooSmall)?;
                Ok(total)
            }
            x if x == FwUpdateCommand::RequestUpdate as u8 => {
                if self.state != FdState::Idle {
                    return write_completion(hdr, CompletionCode::AlreadyInUpdateMode as u8, resp);
                }
                let Ok((req, rest)) = RequestUpdateReqHeader::ref_from_prefix(data) else {
                    return write_completion(hdr, CompletionCode::InvalidLength as u8, resp);
                };
                if rest.len() != req.ver_str_len as usize {
                    return write_completion(hdr, CompletionCode::InvalidLength as u8, resp);
                }
                let req_max = req.max_transfer_size.get();
                // Single-component FirmwareDevice contract: require num_components == 1.
                if req_max < 32 || req.num_components.get() != 1 {
                    return write_completion(hdr, CompletionCode::InvalidData as u8, resp);
                }
                if resp.len() < 7 {
                    return Err(Error::BufferTooSmall);
                }
                self.max_transfer_size = req_max.min(256);
                self.expected_component = None;
                self.applied_components = 0;
                self.transition(FdState::LearnComponents);
                write_completion(hdr, CompletionCode::Success as u8, resp)?;
                resp[4..6].copy_from_slice(&0u16.to_le_bytes());
                resp[6] = 0;
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
                let Ok((req, rest)) = PassComponentReqHeader::ref_from_prefix(data) else {
                    return write_completion(hdr, CompletionCode::InvalidLength as u8, resp);
                };
                if rest.len() != req.ver_str_len as usize {
                    return write_completion(hdr, CompletionCode::InvalidLength as u8, resp);
                }
                if !matches!(req.transfer_flag, 0x01 | 0x02 | 0x04 | 0x05) {
                    return write_completion(hdr, CompletionCode::InvalidData as u8, resp);
                }
                if resp.len() < 6 {
                    return Err(Error::BufferTooSmall);
                }
                self.expected_component = Some((req.classification.get(), req.identifier.get()));
                if req.transfer_flag == 0x04 || req.transfer_flag == 0x05 {
                    self.transition(FdState::ReadyXfer);
                }
                write_completion(hdr, CompletionCode::Success as u8, resp)?;
                resp[4] = 0x00;
                resp[5] = 0x00;
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
                let Ok((req, rest)) = UpdateComponentReqHeader::ref_from_prefix(data) else {
                    return write_completion(hdr, CompletionCode::InvalidLength as u8, resp);
                };
                if rest.len() != req.ver_str_len as usize {
                    return write_completion(hdr, CompletionCode::InvalidLength as u8, resp);
                }
                if self.expected_component != Some((req.classification.get(), req.identifier.get()))
                {
                    return write_completion(hdr, CompletionCode::InvalidData as u8, resp);
                }
                let size = req.image_size.get() as usize;
                if size == 0 || size > MAX_IMAGE {
                    return write_completion(hdr, CompletionCode::InvalidData as u8, resp);
                }
                let total = 4 + core::mem::size_of::<UpdateComponentResp>();
                if resp.len() < total {
                    return Err(Error::BufferTooSmall);
                }
                self.expected_image_size = size;
                self.received_bytes = 0;
                self.pending_req_len = 0;
                self.transition(FdState::Download);
                write_completion(hdr, CompletionCode::Success as u8, resp)?;
                let body = UpdateComponentResp {
                    compatibility_response: 0x00,
                    compatibility_response_code: 0x00,
                    update_option_flags_enabled: LeU32::new(0),
                    estimated_time: LeU16::new(1),
                };
                body.write_to_prefix(&mut resp[4..])
                    .map_err(|_| Error::BufferTooSmall)?;
                Ok(total)
            }
            x if x == FwUpdateCommand::ActivateFirmware as u8 => {
                if self.state != FdState::ReadyXfer {
                    return write_completion(
                        hdr,
                        CompletionCode::InvalidStateForCommand as u8,
                        resp,
                    );
                }
                if data.len() != 1 {
                    return write_completion(hdr, CompletionCode::InvalidLength as u8, resp);
                }
                if data[0] > 1 {
                    return write_completion(hdr, CompletionCode::InvalidData as u8, resp);
                }
                if self.applied_components == 0 {
                    return write_completion(hdr, CompletionCode::IncompleteUpdate as u8, resp);
                }
                if resp.len() < 6 {
                    return Err(Error::BufferTooSmall);
                }
                self.transition(FdState::Activate);
                self.activated = true;
                self.reason_code = 1; // ActivateFirmware received
                self.transition(FdState::Idle);
                write_completion(hdr, CompletionCode::Success as u8, resp)?;
                resp[4..6].copy_from_slice(&0u16.to_le_bytes());
                Ok(6)
            }
            x if x == FwUpdateCommand::GetStatus as u8 => {
                if !data.is_empty() {
                    return write_completion(hdr, CompletionCode::InvalidLength as u8, resp);
                }
                let total = 4 + core::mem::size_of::<GetStatusResp>();
                if resp.len() < total {
                    return Err(Error::BufferTooSmall);
                }
                write_completion(hdr, CompletionCode::Success as u8, resp)?;
                let body = GetStatusResp {
                    current_state: self.state as u8,
                    previous_state: self.prev_state as u8,
                    // DSP0267 §11.12: AuxState = 3 (Idle/Learn/Ready) when not in Download/Verify/Apply.
                    aux_state: match self.state {
                        FdState::Idle | FdState::LearnComponents | FdState::ReadyXfer => 3,
                        _ => 0,
                    },
                    aux_state_status: 0,
                    progress_percent: self
                        .received_bytes
                        .saturating_mul(100)
                        .checked_div(self.expected_image_size)
                        .map_or(101, |pct| pct.min(100) as u8),
                    reason_code: if self.state == FdState::Idle {
                        self.reason_code
                    } else {
                        0
                    },
                    update_option_flags_enabled: LeU32::new(0),
                };
                body.write_to_prefix(&mut resp[4..])
                    .map_err(|_| Error::BufferTooSmall)?;
                Ok(total)
            }
            x if x == FwUpdateCommand::CancelUpdate as u8 => {
                if !data.is_empty() {
                    return write_completion(hdr, CompletionCode::InvalidLength as u8, resp);
                }
                if self.state == FdState::Idle {
                    return write_completion(hdr, CompletionCode::NotInUpdateMode as u8, resp);
                }
                if resp.len() < 13 {
                    return Err(Error::BufferTooSmall);
                }
                self.transition(FdState::Idle);
                self.reason_code = 2; // CancelUpdate received
                self.received_bytes = 0;
                self.expected_image_size = 0;
                self.pending_req_len = 0;
                self.applied_components = 0;
                write_completion(hdr, CompletionCode::Success as u8, resp)?;
                resp[4] = 0;
                resp[5..13].fill(0);
                Ok(13)
            }
            _ => write_completion(hdr, CompletionCode::UnsupportedCmd as u8, resp),
        }
    }

    /// While in [`FdState::Download`], build the next `RequestFirmwareData`
    /// (FD -> UA) request. Returns `Ok(None)` if all bytes have been downloaded.
    pub fn next_fw_data_request(
        &mut self,
        instance_id: u8,
        out: &mut [u8],
    ) -> Result<Option<(usize, usize)>, Error> {
        if self.state != FdState::Download || self.received_bytes >= self.expected_image_size {
            return Ok(None);
        }
        let total = PldmHeader::LEN + core::mem::size_of::<RequestFwDataReq>();
        if out.len() < total {
            return Err(Error::BufferTooSmall);
        }
        let remaining = self.expected_image_size - self.received_bytes;
        let req_len = remaining.min(self.max_transfer_size as usize).max(32);
        let hdr = PldmHeader::request(
            instance_id,
            PldmType::FirmwareUpdate,
            FwUpdateCommand::RequestFirmwareData as u8,
        );
        out[..PldmHeader::LEN].copy_from_slice(&hdr.to_bytes());
        let body = RequestFwDataReq {
            offset: LeU32::new(self.received_bytes as u32),
            length: LeU32::new(req_len as u32),
        };
        body.write_to_prefix(&mut out[PldmHeader::LEN..])
            .map_err(|_| Error::BufferTooSmall)?;
        self.pending_req_len = req_len;
        Ok(Some((total, req_len)))
    }

    /// Feed a chunk returned by the UA in response to `RequestFirmwareData`.
    ///
    /// Per DSP0267 §11.6, `chunk.len()` must match the exact requested length
    /// (zero-padded by the UA past the end of the image). Returns `Ok(true)`
    /// when all image bytes have been received and the FD is ready to emit
    /// `TransferComplete`.
    pub fn push_fw_data_chunk(&mut self, chunk: &[u8]) -> Result<bool, Error> {
        if self.state != FdState::Download || self.pending_req_len == 0 {
            return Err(Error::InvalidData);
        }
        if chunk.len() != self.pending_req_len {
            return Err(Error::InvalidData);
        }
        self.pending_req_len = 0;
        let remaining = self.expected_image_size - self.received_bytes;
        let take = chunk.len().min(remaining);
        self.image_buf[self.received_bytes..self.received_bytes + take]
            .copy_from_slice(&chunk[..take]);
        self.received_bytes += take;
        Ok(self.received_bytes >= self.expected_image_size)
    }

    /// Build `TransferComplete` (FD -> UA) and transition `Download -> Verify`.
    pub fn build_transfer_complete(
        &mut self,
        instance_id: u8,
        out: &mut [u8],
    ) -> Result<usize, Error> {
        if self.state != FdState::Download || self.received_bytes < self.expected_image_size {
            return Err(Error::InvalidData);
        }
        if out.len() < 4 {
            return Err(Error::BufferTooSmall);
        }
        let hdr = PldmHeader::request(
            instance_id,
            PldmType::FirmwareUpdate,
            FwUpdateCommand::TransferComplete as u8,
        );
        out[..3].copy_from_slice(&hdr.to_bytes());
        out[3] = 0x00; // TransferSuccess
        self.transition(FdState::Verify);
        Ok(4)
    }

    /// Build `VerifyComplete` (FD -> UA). Per DSP0267 §11.8, only transitions
    /// `Verify -> Apply` when `verify_result == 0x00` (`VerifySuccess`); a
    /// failed verification stays in `Verify` until `CancelUpdate`.
    pub fn build_verify_complete(
        &mut self,
        instance_id: u8,
        verify_result: u8,
        out: &mut [u8],
    ) -> Result<usize, Error> {
        if self.state != FdState::Verify {
            return Err(Error::InvalidData);
        }
        if out.len() < 4 {
            return Err(Error::BufferTooSmall);
        }
        let hdr = PldmHeader::request(
            instance_id,
            PldmType::FirmwareUpdate,
            FwUpdateCommand::VerifyComplete as u8,
        );
        out[..3].copy_from_slice(&hdr.to_bytes());
        out[3] = verify_result;
        if verify_result == 0x00 {
            self.transition(FdState::Apply);
        }
        Ok(4)
    }

    /// Build `ApplyComplete` (FD -> UA). Only records an applied component and
    /// transitions `Apply -> ReadyXfer` when `apply_result == 0x00` (`ApplySuccess`).
    pub fn build_apply_complete(
        &mut self,
        instance_id: u8,
        apply_result: u8,
        out: &mut [u8],
    ) -> Result<usize, Error> {
        if self.state != FdState::Apply {
            return Err(Error::InvalidData);
        }
        if out.len() < 6 {
            return Err(Error::BufferTooSmall);
        }
        let hdr = PldmHeader::request(
            instance_id,
            PldmType::FirmwareUpdate,
            FwUpdateCommand::ApplyComplete as u8,
        );
        out[..3].copy_from_slice(&hdr.to_bytes());
        out[3] = apply_result;
        out[4..6].copy_from_slice(&0u16.to_le_bytes()); // activation methods modification
        if apply_result == 0x00 {
            self.applied_components = self.applied_components.saturating_add(1);
            self.transition(FdState::ReadyXfer);
        }
        Ok(6)
    }
}
