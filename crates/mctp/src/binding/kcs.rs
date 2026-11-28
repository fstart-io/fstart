//! MCTP over KCS / LPC Transport Binding (DSP0254).
//!
//! Wire format inside a raw KCS transaction (host write or BMC read):
//! `[0xB0 (Binding ID)][0x01 (Revision)][byte_count][mctp_packet...][PEC]`
//!
//! PEC is CRC-8 (`0x07`) over `[0xB0, 0x01, byte_count, mctp_packet...]`.
//!
//! A read response with `byte_count == 0` (`[0xB0, 0x01, 0x00, 0xFF]`, DSP0254
//! §8) indicates no MCTP packet is currently pending on the BMC.
//!
//! Binding-specific control messages (`0xF0` Register Endpoint, `0xF1` Get
//! MCTP Packet datagram, `0xF2` Set SMS_ATN, DSP0254 §7) are carried as normal
//! MCTP Control messages inside standard KCS packet framing.

use crate::binding::crc8_smbus;
use crate::{Error, HEADER_LEN};
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned};

/// DSP0254 MCTP-over-KCS defining body / binding ID byte.
pub const BINDING_ID: u8 = 0xB0;
/// DSP0254 binding revision 1.0.
pub const REVISION: u8 = 0x01;
/// Maximum raw MCTP packet length carried in one KCS frame.
pub const MAX_PACKET_LEN: usize = 255;

#[derive(FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct KcsFrameHeader {
    binding_id: u8,
    revision: u8,
    byte_count: u8,
}

/// Encode a raw MCTP packet into a DSP0254 KCS frame in `out`.
pub fn encode(mctp_packet: &[u8], out: &mut [u8]) -> Result<usize, Error> {
    if mctp_packet.len() < HEADER_LEN || mctp_packet.len() > MAX_PACKET_LEN {
        return Err(Error::InvalidFrame);
    }
    let hdr_len = core::mem::size_of::<KcsFrameHeader>();
    let total = hdr_len + mctp_packet.len() + 1;
    if out.len() < total {
        return Err(Error::BufferTooSmall);
    }
    let hdr = KcsFrameHeader {
        binding_id: BINDING_ID,
        revision: REVISION,
        byte_count: mctp_packet.len() as u8,
    };
    hdr.write_to_prefix(out)
        .map_err(|_| Error::BufferTooSmall)?;
    out[hdr_len..hdr_len + mctp_packet.len()].copy_from_slice(mctp_packet);
    out[total - 1] = crc8_smbus(&out[..total - 1]);
    Ok(total)
}

/// Encode the DSP0254 §8 no-packet read response (`[0xB0, 0x01, 0x00, 0xFF]`).
pub fn encode_empty_response(out: &mut [u8]) -> Result<usize, Error> {
    let hdr_len = core::mem::size_of::<KcsFrameHeader>();
    if out.len() < hdr_len + 1 {
        return Err(Error::BufferTooSmall);
    }
    let hdr = KcsFrameHeader {
        binding_id: BINDING_ID,
        revision: REVISION,
        byte_count: 0,
    };
    hdr.write_to_prefix(out)
        .map_err(|_| Error::BufferTooSmall)?;
    out[hdr_len] = crc8_smbus(hdr.as_bytes());
    Ok(hdr_len + 1)
}

/// Decode and PEC-verify a DSP0254 KCS frame.
///
/// Returns `Ok(None)` for the valid no-packet read response
/// (`[0xB0, 0x01, 0x00, 0xFF]`), or `Ok(Some(mctp_packet))` otherwise.
pub fn decode(frame: &[u8]) -> Result<Option<&[u8]>, Error> {
    let (hdr, rest) = KcsFrameHeader::ref_from_prefix(frame).map_err(|_| Error::ShortBuffer)?;
    if rest.is_empty() {
        return Err(Error::ShortBuffer);
    }
    if hdr.binding_id != BINDING_ID || hdr.revision != REVISION {
        return Err(Error::InvalidFrame);
    }
    let byte_count = hdr.byte_count as usize;
    if rest.len() != byte_count + 1 {
        return Err(Error::InvalidFrame);
    }
    let expected = crc8_smbus(&frame[..frame.len() - 1]);
    if frame[frame.len() - 1] != expected {
        return Err(Error::BadChecksum);
    }
    if byte_count == 0 {
        return Ok(None);
    }
    if byte_count < HEADER_LEN {
        return Err(Error::InvalidFrame);
    }
    Ok(Some(&rest[..byte_count]))
}
