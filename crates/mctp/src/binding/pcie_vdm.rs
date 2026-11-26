//! MCTP over PCIe Vendor Defined Message (VDM) Transport Binding (DSP0238).
//!
//! Wire format (4-DW big-endian PCIe VDM Type 1 TLP header + DW-padded payload):
//! - DW0: `Fmt = 0b011` (4DW + data), `Type = 0b10_000 | routing`, `Length` (DWs)
//! - DW1: `Requester ID` (16-bit BDF), `Pad Len` (bits 5:4 of byte 6),
//!   `MCTP VDM Code = 0x0` (bits 3:0 of byte 6), `Message Code = 0x7F` (byte 7)
//! - DW2: `Target ID` (16-bit BDF for Route-by-ID, else 0), `Vendor ID = 0x1AB4`
//! - DW3: **4-byte MCTP Transport Header**
//! - Bytes `16..16 + length_dws*4`: MCTP payload + `pad_len` (0..3) zero bytes.

use crate::{Error, HEADER_LEN};

/// DMTF PCI-SIG Vendor ID for MCTP VDMs (DSP0238 §6.1).
pub const DMTF_VENDOR_ID: u16 = 0x1AB4;
/// PCIe Vendor Defined Message Type 1 code (silent drop if unsupported).
pub const VDM_TYPE1_MSG_CODE: u8 = 0x7F;
/// Size of the 4-DW PCIe VDM header (DW0..DW3, including the MCTP header in DW3).
pub const VDM_HEADER_LEN: usize = 16;

/// PCIe VDM routing mode (low 3 bits of TLP Type field, DSP0238 Table 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PcieRouting {
    /// Route to Root Complex (`0b000`).
    RouteToRc = 0x0,
    /// Route by ID (`0b010`).
    RouteById = 0x2,
    /// Broadcast from Root Complex (`0b011`).
    BroadcastFromRc = 0x3,
}

impl PcieRouting {
    pub const fn from_u8(val: u8) -> Option<Self> {
        match val & 0x07 {
            0x0 => Some(Self::RouteToRc),
            0x2 => Some(Self::RouteById),
            0x3 => Some(Self::BroadcastFromRc),
            _ => None,
        }
    }
}

/// Decoded PCIe VDM TLP addressing metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PcieVdmMeta {
    pub routing: PcieRouting,
    pub requester_bdf: u16,
    pub target_bdf: u16,
}

/// Encode a raw MCTP packet (`[4-byte MCTP header, payload...]`) into a
/// DSP0238 PCIe VDM TLP in `out`. Returns the total TLP byte length (multiple of 4).
pub fn encode(
    routing: PcieRouting,
    requester_bdf: u16,
    target_bdf: u16,
    mctp_packet: &[u8],
    out: &mut [u8],
) -> Result<usize, Error> {
    if mctp_packet.len() < HEADER_LEN + 1 {
        // DSP0238 requires at least 1 DW of data (non-empty MCTP payload).
        return Err(Error::InvalidFrame);
    }
    let payload = &mctp_packet[HEADER_LEN..];
    let dws = payload.len().div_ceil(4);
    if dws == 0 || dws > 1024 {
        return Err(Error::InvalidFrame);
    }
    let pad_len = (dws * 4) - payload.len();
    let eom = (mctp_packet[3] & 0x40) != 0;
    // DSP0238 §6.1: non-final packets (EOM=0) must be DW-aligned (pad_len == 0).
    if !eom && pad_len != 0 {
        return Err(Error::InvalidFrame);
    }
    let total = VDM_HEADER_LEN + dws * 4;
    if out.len() < total {
        return Err(Error::BufferTooSmall);
    }

    let length_field: u16 = if dws == 1024 { 0 } else { dws as u16 };
    // DW0: Fmt=011 (bits 31:29), Type=10rrr (bits 28:24), Length (bits 9:0).
    let dw0_b0 = (0b011 << 5) | (0b10_000 | (routing as u8));
    out[0] = dw0_b0;
    out[1] = 0x00;
    out[2] = ((length_field >> 8) & 0x03) as u8;
    out[3] = (length_field & 0xFF) as u8;

    // DW1: Requester ID, Pad Len (bits 5:4), MCTP VDM Code=0 (bits 3:0), Msg Code=0x7F.
    let req_bytes = requester_bdf.to_be_bytes();
    out[4] = req_bytes[0];
    out[5] = req_bytes[1];
    out[6] = ((pad_len as u8) & 0x03) << 4;
    out[7] = VDM_TYPE1_MSG_CODE;

    // DW2: Target ID (0 unless RouteById), DMTF Vendor ID 0x1AB4.
    let tgt = if routing == PcieRouting::RouteById {
        target_bdf
    } else {
        0
    };
    let tgt_bytes = tgt.to_be_bytes();
    let vid_bytes = DMTF_VENDOR_ID.to_be_bytes();
    out[8] = tgt_bytes[0];
    out[9] = tgt_bytes[1];
    out[10] = vid_bytes[0];
    out[11] = vid_bytes[1];

    // DW3: 4-byte MCTP transport header verbatim.
    out[12..16].copy_from_slice(&mctp_packet[..HEADER_LEN]);

    // Payload + zero padding.
    out[16..16 + payload.len()].copy_from_slice(payload);
    for b in &mut out[16 + payload.len()..total] {
        *b = 0;
    }
    Ok(total)
}

/// Decode a DSP0238 PCIe VDM TLP, returning `(PcieVdmMeta, mctp_packet_slice)`.
/// Because DW3 is immediately followed by the MCTP payload, the returned slice
/// starts at byte 12 (the 4-byte MCTP header) and excludes trailing pad bytes.
pub fn decode(tlp: &[u8]) -> Result<(PcieVdmMeta, &[u8]), Error> {
    if tlp.len() < VDM_HEADER_LEN + 4 || (tlp.len() & 0x03) != 0 {
        return Err(Error::ShortBuffer);
    }
    let fmt = tlp[0] >> 5;
    let tlp_type = tlp[0] & 0x1F;
    if fmt != 0b011 || (tlp_type & 0x18) != 0b10_000 {
        return Err(Error::InvalidFrame);
    }
    let routing = PcieRouting::from_u8(tlp_type).ok_or(Error::InvalidFrame)?;
    let has_digest = (tlp[2] & 0x80) != 0;
    let raw_len = u16::from_be_bytes([tlp[2] & 0x03, tlp[3]]) as usize;
    let dws = if raw_len == 0 { 1024 } else { raw_len };
    let expected_total = VDM_HEADER_LEN + dws * 4 + if has_digest { 4 } else { 0 };
    if tlp.len() != expected_total {
        return Err(Error::InvalidFrame);
    }

    let vdm_code = tlp[6] & 0x0F;
    let pad_len = ((tlp[6] >> 4) & 0x03) as usize;
    let msg_code = tlp[7];
    let vendor_id = u16::from_be_bytes([tlp[10], tlp[11]]);
    if vdm_code != 0 || msg_code != VDM_TYPE1_MSG_CODE || vendor_id != DMTF_VENDOR_ID {
        return Err(Error::InvalidFrame);
    }

    let eom = (tlp[15] & 0x40) != 0;
    if !eom && pad_len != 0 {
        return Err(Error::InvalidFrame);
    }

    let payload_bytes = dws * 4;
    if pad_len >= payload_bytes {
        return Err(Error::InvalidFrame);
    }
    let meta = PcieVdmMeta {
        routing,
        requester_bdf: u16::from_be_bytes([tlp[4], tlp[5]]),
        target_bdf: u16::from_be_bytes([tlp[8], tlp[9]]),
    };
    let pkt_end = VDM_HEADER_LEN + payload_bytes - pad_len;
    Ok((meta, &tlp[12..pkt_end]))
}
