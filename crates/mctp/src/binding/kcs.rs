//! MCTP over KCS / LPC Transport Binding (DSP0254).
//!
//! Wire format inside a raw KCS transaction (host write or BMC read):
//! `[0xB0 (Binding ID)][0x01 (Revision)][byte_count][mctp_packet...][PEC]`
//!
//! PEC is CRC-8 (`0x07`) over `[0xB0, 0x01, byte_count, mctp_packet...]`.
//!
//! Binding-specific control datagrams (DSP0254 §6.3) use the same 2-byte
//! prefix `[0xB0, 0x01]` followed by the binding command code (`0xF0..0xF2`)
//! and PEC.

use crate::binding::crc8_smbus;
use crate::{Error, HEADER_LEN};

/// DSP0254 MCTP-over-KCS defining body / binding ID byte.
pub const BINDING_ID: u8 = 0xB0;
/// DSP0254 binding revision 1.0.
pub const REVISION: u8 = 0x01;
/// Maximum raw MCTP packet length carried in one KCS frame.
pub const MAX_PACKET_LEN: usize = 255;

/// KCS binding-specific control commands (DSP0254 §6.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum KcsBindingCommand {
    /// Initialize KCS MCTP binding.
    Init = 0xF0,
    /// Host polls BMC for a pending BMC-to-Host MCTP packet.
    GetMctpPacket = 0xF1,
    /// Enable/disable SMS_ATN signaling.
    SetSmsAtn = 0xF2,
}

/// Encode a raw MCTP packet into a DSP0254 KCS frame in `out`.
pub fn encode(mctp_packet: &[u8], out: &mut [u8]) -> Result<usize, Error> {
    if mctp_packet.len() < HEADER_LEN || mctp_packet.len() > MAX_PACKET_LEN {
        return Err(Error::InvalidFrame);
    }
    let total = 3 + mctp_packet.len() + 1;
    if out.len() < total {
        return Err(Error::BufferTooSmall);
    }
    out[0] = BINDING_ID;
    out[1] = REVISION;
    out[2] = mctp_packet.len() as u8;
    out[3..3 + mctp_packet.len()].copy_from_slice(mctp_packet);
    out[total - 1] = crc8_smbus(&out[..total - 1]);
    Ok(total)
}

/// Decode and PEC-verify a DSP0254 KCS MCTP packet frame.
pub fn decode(frame: &[u8]) -> Result<&[u8], Error> {
    if frame.len() < 3 + HEADER_LEN + 1 {
        return Err(Error::ShortBuffer);
    }
    if frame[0] != BINDING_ID || frame[1] != REVISION {
        return Err(Error::InvalidFrame);
    }
    let byte_count = frame[2] as usize;
    if byte_count < HEADER_LEN || frame.len() != 3 + byte_count + 1 {
        return Err(Error::InvalidFrame);
    }
    let expected = crc8_smbus(&frame[..frame.len() - 1]);
    if frame[frame.len() - 1] != expected {
        return Err(Error::BadChecksum);
    }
    Ok(&frame[3..3 + byte_count])
}

/// Encode a DSP0254 KCS binding control frame (e.g., `GetMctpPacket` `0xF1`).
pub fn encode_binding_cmd(
    cmd: KcsBindingCommand,
    arg: Option<u8>,
    out: &mut [u8],
) -> Result<usize, Error> {
    let body_len = 3 + usize::from(arg.is_some());
    let total = body_len + 1;
    if out.len() < total {
        return Err(Error::BufferTooSmall);
    }
    out[0] = BINDING_ID;
    out[1] = REVISION;
    out[2] = cmd as u8;
    if let Some(a) = arg {
        out[3] = a;
    }
    out[body_len] = crc8_smbus(&out[..body_len]);
    Ok(total)
}
