//! MCTP Control Protocol (DSP0236 §12) codecs, requester helpers, and endpoint
//! responder.

use crate::{EID_BROADCAST, EID_NULL, Error, MessageType};

/// MCTP Control command codes (DSP0236 Table 12).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ControlCommand {
    SetEndpointId = 0x01,
    GetEndpointId = 0x02,
    GetEndpointUuid = 0x03,
    GetMctpVersionSupport = 0x04,
    GetMessageTypeSupport = 0x05,
    GetVendorDefinedMessageSupport = 0x06,
    ResolveEndpointId = 0x07,
    AllocateEndpointIds = 0x08,
    RoutingInformationUpdate = 0x09,
    GetRoutingTableEntries = 0x0A,
    PrepareForEndpointDiscovery = 0x0B,
    EndpointDiscovery = 0x0C,
    DiscoveryNotify = 0x0D,
}

impl ControlCommand {
    pub const fn from_u8(val: u8) -> Option<Self> {
        match val {
            0x01 => Some(Self::SetEndpointId),
            0x02 => Some(Self::GetEndpointId),
            0x03 => Some(Self::GetEndpointUuid),
            0x04 => Some(Self::GetMctpVersionSupport),
            0x05 => Some(Self::GetMessageTypeSupport),
            0x06 => Some(Self::GetVendorDefinedMessageSupport),
            0x07 => Some(Self::ResolveEndpointId),
            0x08 => Some(Self::AllocateEndpointIds),
            0x09 => Some(Self::RoutingInformationUpdate),
            0x0A => Some(Self::GetRoutingTableEntries),
            0x0B => Some(Self::PrepareForEndpointDiscovery),
            0x0C => Some(Self::EndpointDiscovery),
            0x0D => Some(Self::DiscoveryNotify),
            _ => None,
        }
    }
}

/// MCTP Control completion codes (DSP0236 Table 13).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ControlCompletion {
    Success = 0x00,
    Error = 0x01,
    InvalidData = 0x02,
    InvalidLength = 0x03,
    NotReady = 0x04,
    UnsupportedCmd = 0x05,
    MessageTypeNotSupported = 0x80,
}

/// `Set Endpoint ID` operation field (DSP0236 Table 14).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum SetEidOperation {
    SetEid = 0x00,
    ForceEid = 0x01,
    ResetEid = 0x02,
    SetDiscoveredFlag = 0x03,
}

impl SetEidOperation {
    pub const fn from_u8(val: u8) -> Option<Self> {
        match val & 0x03 {
            0x00 => Some(Self::SetEid),
            0x01 => Some(Self::ForceEid),
            0x02 => Some(Self::ResetEid),
            0x03 => Some(Self::SetDiscoveredFlag),
            _ => None,
        }
    }
}

/// Decoded MCTP Control message header (bytes 0..2 of the control payload).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ControlHeader {
    pub rq: bool,
    pub datagram: bool,
    pub instance_id: u8,
    pub command_code: u8,
}

impl ControlHeader {
    pub const fn to_bytes(self) -> [u8; 2] {
        let b0 = ((self.rq as u8) << 7) | ((self.datagram as u8) << 6) | (self.instance_id & 0x1F);
        [b0, self.command_code]
    }

    pub const fn from_bytes(bytes: [u8; 2]) -> Self {
        Self {
            rq: (bytes[0] & 0x80) != 0,
            datagram: (bytes[0] & 0x40) != 0,
            instance_id: bytes[0] & 0x1F,
            command_code: bytes[1],
        }
    }

    pub fn parse(msg: &[u8]) -> Result<(Self, &[u8]), Error> {
        if msg.len() < 2 {
            return Err(Error::ShortBuffer);
        }
        Ok((Self::from_bytes([msg[0], msg[1]]), &msg[2..]))
    }
}

/// Encode a `Set Endpoint ID` request into `out`.
pub fn encode_set_eid_req(
    instance_id: u8,
    op: SetEidOperation,
    eid: u8,
    out: &mut [u8],
) -> Result<usize, Error> {
    if out.len() < 4 {
        return Err(Error::BufferTooSmall);
    }
    let hdr = ControlHeader {
        rq: true,
        datagram: false,
        instance_id,
        command_code: ControlCommand::SetEndpointId as u8,
    };
    out[..2].copy_from_slice(&hdr.to_bytes());
    out[2] = op as u8;
    out[3] = eid;
    Ok(4)
}

/// Encode a `Get Endpoint ID` request into `out`.
pub fn encode_get_eid_req(instance_id: u8, out: &mut [u8]) -> Result<usize, Error> {
    if out.len() < 2 {
        return Err(Error::BufferTooSmall);
    }
    let hdr = ControlHeader {
        rq: true,
        datagram: false,
        instance_id,
        command_code: ControlCommand::GetEndpointId as u8,
    };
    out[..2].copy_from_slice(&hdr.to_bytes());
    Ok(2)
}

/// Encode a `Get Endpoint UUID` request into `out`.
pub fn encode_get_uuid_req(instance_id: u8, out: &mut [u8]) -> Result<usize, Error> {
    if out.len() < 2 {
        return Err(Error::BufferTooSmall);
    }
    let hdr = ControlHeader {
        rq: true,
        datagram: false,
        instance_id,
        command_code: ControlCommand::GetEndpointUuid as u8,
    };
    out[..2].copy_from_slice(&hdr.to_bytes());
    Ok(2)
}

/// Encode a `Get MCTP Version Support` request for `msg_type` (`0xFF` = base).
pub fn encode_get_version_req(
    instance_id: u8,
    msg_type: u8,
    out: &mut [u8],
) -> Result<usize, Error> {
    if out.len() < 3 {
        return Err(Error::BufferTooSmall);
    }
    let hdr = ControlHeader {
        rq: true,
        datagram: false,
        instance_id,
        command_code: ControlCommand::GetMctpVersionSupport as u8,
    };
    out[..2].copy_from_slice(&hdr.to_bytes());
    out[2] = msg_type;
    Ok(3)
}

/// Encode a `Get Message Type Support` request into `out`.
pub fn encode_get_msg_types_req(instance_id: u8, out: &mut [u8]) -> Result<usize, Error> {
    if out.len() < 2 {
        return Err(Error::BufferTooSmall);
    }
    let hdr = ControlHeader {
        rq: true,
        datagram: false,
        instance_id,
        command_code: ControlCommand::GetMessageTypeSupport as u8,
    };
    out[..2].copy_from_slice(&hdr.to_bytes());
    Ok(2)
}

/// Stateful MCTP Control Protocol responder for an endpoint or bus owner.
pub struct ControlResponder {
    eid: u8,
    static_eid: bool,
    is_bus_owner: bool,
    discovered: bool,
    uuid: [u8; 16],
    msg_types: [u8; 8],
    msg_type_count: usize,
}

impl ControlResponder {
    /// Create a new control responder with `initial_eid`, 16-byte `uuid`, and
    /// list of supported non-control message types (Control `0x00` is always
    /// implicit in DSP0236, though callers may pass e.g. `[Pldm, Spdm, SecuredMsg]`).
    pub fn new(initial_eid: u8, uuid: [u8; 16], supported_types: &[u8]) -> Self {
        let mut msg_types = [0u8; 8];
        let count = supported_types.len().min(msg_types.len());
        msg_types[..count].copy_from_slice(&supported_types[..count]);
        Self {
            eid: initial_eid,
            static_eid: initial_eid != EID_NULL,
            is_bus_owner: false,
            discovered: false,
            uuid,
            msg_types,
            msg_type_count: count,
        }
    }

    pub const fn with_bus_owner(mut self, bus_owner: bool) -> Self {
        self.is_bus_owner = bus_owner;
        self
    }

    pub const fn eid(&self) -> u8 {
        self.eid
    }

    pub const fn discovered(&self) -> bool {
        self.discovered
    }

    /// Process an incoming MCTP Control request payload and write the response
    /// payload into `resp`. Returns the response length.
    pub fn handle_request(&mut self, req: &[u8], resp: &mut [u8]) -> Result<usize, Error> {
        let (hdr, data) = ControlHeader::parse(req)?;
        if !hdr.rq {
            return Err(Error::InvalidControl);
        }
        if resp.len() < 3 {
            return Err(Error::BufferTooSmall);
        }
        let resp_hdr = ControlHeader {
            rq: false,
            datagram: false,
            instance_id: hdr.instance_id,
            command_code: hdr.command_code,
        };
        resp[..2].copy_from_slice(&resp_hdr.to_bytes());

        let Some(cmd) = ControlCommand::from_u8(hdr.command_code) else {
            resp[2] = ControlCompletion::UnsupportedCmd as u8;
            return Ok(3);
        };

        match cmd {
            ControlCommand::SetEndpointId => self.handle_set_eid(data, resp),
            ControlCommand::GetEndpointId => self.handle_get_eid(data, resp),
            ControlCommand::GetEndpointUuid => self.handle_get_uuid(data, resp),
            ControlCommand::GetMctpVersionSupport => self.handle_get_version(data, resp),
            ControlCommand::GetMessageTypeSupport => self.handle_get_msg_types(data, resp),
            ControlCommand::PrepareForEndpointDiscovery => {
                if !data.is_empty() {
                    resp[2] = ControlCompletion::InvalidLength as u8;
                    return Ok(3);
                }
                self.discovered = false;
                resp[2] = ControlCompletion::Success as u8;
                Ok(3)
            }
            ControlCommand::EndpointDiscovery => {
                if !data.is_empty() {
                    resp[2] = ControlCompletion::InvalidLength as u8;
                    return Ok(3);
                }
                resp[2] = ControlCompletion::Success as u8;
                Ok(3)
            }
            _ => {
                resp[2] = ControlCompletion::UnsupportedCmd as u8;
                Ok(3)
            }
        }
    }

    fn handle_set_eid(&mut self, data: &[u8], resp: &mut [u8]) -> Result<usize, Error> {
        if data.len() != 2 {
            resp[2] = ControlCompletion::InvalidLength as u8;
            return Ok(3);
        }
        if resp.len() < 6 {
            return Err(Error::BufferTooSmall);
        }
        let op = SetEidOperation::from_u8(data[0]).ok_or(Error::InvalidControl)?;
        let requested_eid = data[1];
        // DSP0236 §12.3: 0x00 and 0xFF are illegal for Set/Force EID.
        if matches!(op, SetEidOperation::SetEid | SetEidOperation::ForceEid)
            && (requested_eid == EID_NULL || requested_eid == EID_BROADCAST)
        {
            resp[2] = ControlCompletion::InvalidData as u8;
            return Ok(3);
        }

        let mut status: u8 = 0x00; // EID assignment accepted, no pool required
        match op {
            SetEidOperation::SetEid => {
                if self.discovered && self.eid != EID_NULL {
                    // Reject if already discovered; return current EID.
                    status = 0x10;
                } else {
                    self.eid = requested_eid;
                    self.discovered = true;
                }
            }
            SetEidOperation::ForceEid => {
                self.eid = requested_eid;
                self.discovered = true;
            }
            SetEidOperation::ResetEid => {
                if !self.static_eid {
                    self.eid = EID_NULL;
                }
                self.discovered = false;
            }
            SetEidOperation::SetDiscoveredFlag => {
                self.discovered = true;
            }
        }

        resp[2] = ControlCompletion::Success as u8;
        resp[3] = status;
        resp[4] = self.eid;
        resp[5] = 0x00; // EID pool size
        Ok(6)
    }

    fn handle_get_eid(&self, data: &[u8], resp: &mut [u8]) -> Result<usize, Error> {
        if !data.is_empty() {
            resp[2] = ControlCompletion::InvalidLength as u8;
            return Ok(3);
        }
        if resp.len() < 6 {
            return Err(Error::BufferTooSmall);
        }
        let ep_type = ((self.is_bus_owner as u8) << 4) | if self.static_eid { 0x01 } else { 0x00 };
        resp[2] = ControlCompletion::Success as u8;
        resp[3] = self.eid;
        resp[4] = ep_type;
        resp[5] = 0x00; // Medium-specific information
        Ok(6)
    }

    fn handle_get_uuid(&self, data: &[u8], resp: &mut [u8]) -> Result<usize, Error> {
        if !data.is_empty() {
            resp[2] = ControlCompletion::InvalidLength as u8;
            return Ok(3);
        }
        if resp.len() < 19 {
            return Err(Error::BufferTooSmall);
        }
        resp[2] = ControlCompletion::Success as u8;
        resp[3..19].copy_from_slice(&self.uuid);
        Ok(19)
    }

    fn handle_get_version(&self, data: &[u8], resp: &mut [u8]) -> Result<usize, Error> {
        if data.len() != 1 {
            resp[2] = ControlCompletion::InvalidLength as u8;
            return Ok(3);
        }
        if resp.len() < 8 {
            return Err(Error::BufferTooSmall);
        }
        let msg_type = data[0];
        // Version encoding (DSP0236 Table 17): [major, minor, update, alpha]
        // 0xF1 0xF3 0xF1 0x00 = v1.3.1 (DSP0236), 0xF1 0xF0 0xF0 0x00 = v1.0.0.
        let version: Option<[u8; 4]> = match msg_type {
            0xFF | 0x00 => Some([0xF1, 0xF3, 0xF1, 0x00]),
            t if self.supports_msg_type(t) => match MessageType::from_u8(t) {
                Some(MessageType::Pldm) => Some([0xF1, 0xF0, 0xF0, 0x00]),
                Some(MessageType::Spdm) | Some(MessageType::SecuredMsg) => {
                    Some([0xF1, 0xF0, 0xF0, 0x00])
                }
                _ => Some([0xF1, 0xF0, 0xF0, 0x00]),
            },
            _ => None,
        };
        let Some(ver) = version else {
            resp[2] = ControlCompletion::MessageTypeNotSupported as u8;
            return Ok(3);
        };
        resp[2] = ControlCompletion::Success as u8;
        resp[3] = 1; // 1 version entry
        resp[4..8].copy_from_slice(&ver);
        Ok(8)
    }

    fn handle_get_msg_types(&self, data: &[u8], resp: &mut [u8]) -> Result<usize, Error> {
        if !data.is_empty() {
            resp[2] = ControlCompletion::InvalidLength as u8;
            return Ok(3);
        }
        let needed = 4 + self.msg_type_count;
        if resp.len() < needed {
            return Err(Error::BufferTooSmall);
        }
        resp[2] = ControlCompletion::Success as u8;
        resp[3] = self.msg_type_count as u8;
        resp[4..needed].copy_from_slice(&self.msg_types[..self.msg_type_count]);
        Ok(needed)
    }

    fn supports_msg_type(&self, msg_type: u8) -> bool {
        self.msg_types[..self.msg_type_count].contains(&msg_type)
    }
}
