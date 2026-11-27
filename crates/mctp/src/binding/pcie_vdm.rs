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
use zerocopy::big_endian::U16 as BeU16;
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned};

/// DMTF PCI-SIG Vendor ID for MCTP VDMs (DSP0238 §6.1).
pub const DMTF_VENDOR_ID: u16 = 0x1AB4;
/// PCIe Vendor Defined Message Type 1 code (silent drop if unsupported).
pub const VDM_TYPE1_MSG_CODE: u8 = 0x7F;
/// Size of the 4-DW PCIe VDM header (DW0..DW3, including the MCTP header in DW3).
pub const VDM_HEADER_LEN: usize = core::mem::size_of::<PcieVdmHeader>();

/// 4-DW big-endian PCIe VDM Type 1 TLP header (DSP0238 §6.1).
#[derive(Debug, Clone, Copy, FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct PcieVdmHeader {
    /// Bits 7:5 = Fmt (`0b011`), bits 4:0 = Type (`0b10_000 | routing`).
    fmt_type: u8,
    /// TC / Attr / TH (0 for MCTP VDMs).
    tc_flags: u8,
    /// Bit 15 = TD (digest), bits 9:0 = Length in DWs (0 encodes 1024 DWs).
    td_ep_attr_len: BeU16,
    /// 16-bit Requester BDF.
    requester_id: BeU16,
    /// Bits 5:4 = Pad Len (0..3), bits 3:0 = MCTP VDM Code (0).
    pad_vdm_code: u8,
    /// PCIe Message Code (`0x7F` = Vendor Defined Type 1).
    msg_code: u8,
    /// 16-bit Target BDF (0 unless `PcieRouting::RouteById`).
    target_id: BeU16,
    /// PCI-SIG Vendor ID (`0x1AB4` = DMTF).
    vendor_id: BeU16,
    /// DW3: 4-byte MCTP transport header verbatim.
    mctp_hdr: [u8; HEADER_LEN],
}

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
    let tgt = if routing == PcieRouting::RouteById {
        target_bdf
    } else {
        0
    };
    let mut mctp_hdr = [0u8; HEADER_LEN];
    mctp_hdr.copy_from_slice(&mctp_packet[..HEADER_LEN]);

    let hdr = PcieVdmHeader {
        fmt_type: (0b011 << 5) | (0b10_000 | (routing as u8)),
        tc_flags: 0,
        td_ep_attr_len: BeU16::new(length_field & 0x03FF),
        requester_id: BeU16::new(requester_bdf),
        pad_vdm_code: ((pad_len as u8) & 0x03) << 4,
        msg_code: VDM_TYPE1_MSG_CODE,
        target_id: BeU16::new(tgt),
        vendor_id: BeU16::new(DMTF_VENDOR_ID),
        mctp_hdr,
    };
    hdr.write_to_prefix(out)
        .map_err(|_| Error::BufferTooSmall)?;

    // Payload + zero padding.
    out[VDM_HEADER_LEN..VDM_HEADER_LEN + payload.len()].copy_from_slice(payload);
    out[VDM_HEADER_LEN + payload.len()..total].fill(0);
    Ok(total)
}

/// Decode a DSP0238 PCIe VDM TLP, returning `(PcieVdmMeta, mctp_packet_slice)`.
/// Because DW3 is immediately followed by the MCTP payload, the returned slice
/// starts at byte 12 (the 4-byte MCTP header) and excludes trailing pad bytes.
pub fn decode(tlp: &[u8]) -> Result<(PcieVdmMeta, &[u8]), Error> {
    if tlp.len() < VDM_HEADER_LEN + 4 || (tlp.len() & 0x03) != 0 {
        return Err(Error::ShortBuffer);
    }
    let (hdr, _) = PcieVdmHeader::ref_from_prefix(tlp).map_err(|_| Error::ShortBuffer)?;
    let fmt = hdr.fmt_type >> 5;
    let tlp_type = hdr.fmt_type & 0x1F;
    if fmt != 0b011 || (tlp_type & 0x18) != 0b10_000 {
        return Err(Error::InvalidFrame);
    }
    let routing = PcieRouting::from_u8(tlp_type).ok_or(Error::InvalidFrame)?;
    let td_len = hdr.td_ep_attr_len.get();
    let has_digest = (td_len & 0x8000) != 0;
    let raw_len = (td_len & 0x03FF) as usize;
    let dws = if raw_len == 0 { 1024 } else { raw_len };
    let expected_total = VDM_HEADER_LEN + dws * 4 + if has_digest { 4 } else { 0 };
    if tlp.len() != expected_total {
        return Err(Error::InvalidFrame);
    }

    let vdm_code = hdr.pad_vdm_code & 0x0F;
    let pad_len = ((hdr.pad_vdm_code >> 4) & 0x03) as usize;
    if vdm_code != 0 || hdr.msg_code != VDM_TYPE1_MSG_CODE || hdr.vendor_id.get() != DMTF_VENDOR_ID
    {
        return Err(Error::InvalidFrame);
    }

    let eom = (hdr.mctp_hdr[3] & 0x40) != 0;
    if !eom && pad_len != 0 {
        return Err(Error::InvalidFrame);
    }

    let payload_bytes = dws * 4;
    if pad_len >= payload_bytes {
        return Err(Error::InvalidFrame);
    }
    let meta = PcieVdmMeta {
        routing,
        requester_bdf: hdr.requester_id.get(),
        target_bdf: hdr.target_id.get(),
    };
    let pkt_end = VDM_HEADER_LEN + payload_bytes - pad_len;
    Ok((meta, &tlp[12..pkt_end]))
}
