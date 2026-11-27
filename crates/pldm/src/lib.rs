//! Platform Level Data Model (PLDM) `no_std` implementation over MCTP.
//!
//! Supports:
//! - Type 0: PLDM Base Messaging Control and Discovery (DSP0240)
//! - Type 2: PLDM Platform Monitoring and Control (DSP0248)
//! - Type 5: PLDM for Firmware Update (DSP0267) — both Update Agent (requester)
//!   and Firmware Device (responder) roles.

#![no_std]

pub mod base;
pub mod fw_update;
pub mod platform;

use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned};

/// Wire-format 3-byte PLDM message header (DSP0240 §7).
#[derive(Debug, Clone, Copy, FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
pub struct RawPldmHeader {
    pub rq_d_inst: u8,
    pub ver_type: u8,
    pub command_code: u8,
}

/// Wire-format 4-byte PLDM response prefix (header + completion code).
#[derive(Debug, Clone, Copy, FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
pub struct PldmResponsePrefix {
    pub header: RawPldmHeader,
    pub completion_code: u8,
}

/// Standard PLDM message types (DSP0245).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PldmType {
    Base = 0x00,
    Smbios = 0x01,
    Platform = 0x02,
    Bios = 0x03,
    Fru = 0x04,
    FirmwareUpdate = 0x05,
    Redfish = 0x06,
}

impl PldmType {
    pub const fn from_u8(val: u8) -> Option<Self> {
        match val & 0x3F {
            0x00 => Some(Self::Base),
            0x01 => Some(Self::Smbios),
            0x02 => Some(Self::Platform),
            0x03 => Some(Self::Bios),
            0x04 => Some(Self::Fru),
            0x05 => Some(Self::FirmwareUpdate),
            0x06 => Some(Self::Redfish),
            _ => None,
        }
    }
}

/// Generic PLDM completion codes (DSP0240 Table 5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum CompletionCode {
    Success = 0x00,
    Error = 0x01,
    InvalidData = 0x02,
    InvalidLength = 0x03,
    NotReady = 0x04,
    UnsupportedCmd = 0x05,
    InvalidPldmType = 0x20,
    /// Firmware Update: Not in update mode (DSP0267).
    NotInUpdateMode = 0x80,
    /// Firmware Update: Already in update mode (DSP0267).
    AlreadyInUpdateMode = 0x81,
    /// Base: Invalid PLDM type in request data (DSP0240).
    InvalidPldmTypeInRequestData = 0x83,
    /// Base / FW Update: Invalid PLDM version (DSP0240) / Invalid state for command (DSP0267).
    InvalidStateForCommand = 0x84,
    /// Firmware Update: Incomplete update on ActivateFirmware (DSP0267 = 0x85).
    IncompleteUpdate = 0x85,
}

/// Errors returned by PLDM encoding or decoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    ShortBuffer,
    BufferTooSmall,
    InvalidHeader,
    InvalidData,
    UnexpectedResponse,
    Completion(u8),
}

/// 3-byte PLDM message header (DSP0240 §7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PldmHeader {
    pub rq: bool,
    pub datagram: bool,
    pub instance_id: u8,
    pub hdr_ver: u8,
    pub pldm_type: u8,
    pub command_code: u8,
}

impl PldmHeader {
    pub const LEN: usize = 3;

    pub const fn request(instance_id: u8, pldm_type: PldmType, command_code: u8) -> Self {
        Self {
            rq: true,
            datagram: false,
            instance_id: instance_id & 0x1F,
            hdr_ver: 0,
            pldm_type: pldm_type as u8,
            command_code,
        }
    }

    pub const fn response(instance_id: u8, pldm_type: u8, command_code: u8) -> Self {
        Self {
            rq: false,
            datagram: false,
            instance_id: instance_id & 0x1F,
            hdr_ver: 0,
            pldm_type: pldm_type & 0x3F,
            command_code,
        }
    }

    pub const fn to_raw(self) -> RawPldmHeader {
        let rq_d_inst =
            ((self.rq as u8) << 7) | ((self.datagram as u8) << 6) | (self.instance_id & 0x1F);
        let ver_type = ((self.hdr_ver & 0x03) << 6) | (self.pldm_type & 0x3F);
        RawPldmHeader {
            rq_d_inst,
            ver_type,
            command_code: self.command_code,
        }
    }

    pub const fn to_bytes(self) -> [u8; 3] {
        let raw = self.to_raw();
        [raw.rq_d_inst, raw.ver_type, raw.command_code]
    }

    pub const fn from_raw(raw: RawPldmHeader) -> Result<Self, Error> {
        let hdr_ver = raw.ver_type >> 6;
        if hdr_ver != 0 {
            return Err(Error::InvalidHeader);
        }
        Ok(Self {
            rq: (raw.rq_d_inst & 0x80) != 0,
            datagram: (raw.rq_d_inst & 0x40) != 0,
            instance_id: raw.rq_d_inst & 0x1F,
            hdr_ver,
            pldm_type: raw.ver_type & 0x3F,
            command_code: raw.command_code,
        })
    }

    pub const fn from_bytes(bytes: [u8; 3]) -> Result<Self, Error> {
        Self::from_raw(RawPldmHeader {
            rq_d_inst: bytes[0],
            ver_type: bytes[1],
            command_code: bytes[2],
        })
    }

    pub fn parse(msg: &[u8]) -> Result<(Self, &[u8]), Error> {
        let (raw, rest) = RawPldmHeader::read_from_prefix(msg).map_err(|_| Error::ShortBuffer)?;
        Ok((Self::from_raw(raw)?, rest))
    }
}

/// Write a minimal PLDM error/completion response (header + completion code).
pub fn write_completion(req_hdr: PldmHeader, cc: u8, resp: &mut [u8]) -> Result<usize, Error> {
    let pfx = PldmResponsePrefix {
        header: PldmHeader::response(req_hdr.instance_id, req_hdr.pldm_type, req_hdr.command_code)
            .to_raw(),
        completion_code: cc,
    };
    pfx.write_to_prefix(resp)
        .map_err(|_| Error::BufferTooSmall)?;
    Ok(core::mem::size_of::<PldmResponsePrefix>())
}

/// Combined PLDM responder dispatching Type 0 (Base), Type 2 (Platform), and
/// Type 5 (Firmware Update) requests.
pub struct PldmResponder<
    const SENSORS: usize = 8,
    const EFFECTERS: usize = 4,
    const MAX_IMAGE: usize = 4096,
> {
    pub base: base::BaseResponder,
    pub platform: platform::PlatformResponder<SENSORS, EFFECTERS>,
    pub fw_update: fw_update::FirmwareDevice<MAX_IMAGE>,
}

impl<const SENSORS: usize, const EFFECTERS: usize, const MAX_IMAGE: usize>
    PldmResponder<SENSORS, EFFECTERS, MAX_IMAGE>
{
    pub fn new(tid: u8, vendor_iana: u32) -> Self {
        Self {
            base: base::BaseResponder::new(
                tid,
                &[PldmType::Base, PldmType::Platform, PldmType::FirmwareUpdate],
            ),
            platform: platform::PlatformResponder::new(),
            fw_update: fw_update::FirmwareDevice::new(vendor_iana),
        }
    }

    /// Dispatch a raw PLDM request payload and write the PLDM response payload
    /// into `resp`.
    pub fn handle_request(&mut self, req: &[u8], resp: &mut [u8]) -> Result<usize, Error> {
        let (hdr, data) = PldmHeader::parse(req)?;
        if !hdr.rq {
            return Err(Error::InvalidHeader);
        }
        // Unacknowledged datagrams (Rq=1, D=1) do not generate a response.
        if hdr.datagram {
            return Ok(0);
        }
        match PldmType::from_u8(hdr.pldm_type) {
            Some(PldmType::Base) => self.base.handle_request(hdr, data, resp),
            Some(PldmType::Platform) => self.platform.handle_request(hdr, data, resp),
            Some(PldmType::FirmwareUpdate) => self.fw_update.handle_request(hdr, data, resp),
            _ => write_completion(hdr, CompletionCode::InvalidPldmType as u8, resp),
        }
    }
}
