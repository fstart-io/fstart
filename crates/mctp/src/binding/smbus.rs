//! MCTP over SMBus / I2C Transport Binding (DSP0237).
//!
//! Wire format (SMBus Block Write, command code `0x0F`):
//! - Byte 0: `(dst_addr << 1) | 0` (7-bit destination target address, R/W=0)
//! - Byte 1: `0x0F` (MCTP command code)
//! - Byte 2: `byte_count` = `1 + mctp_packet.len()` (excludes PEC)
//! - Byte 3: `(src_addr << 1) | 1` (7-bit source address with bit 0 = 1)
//! - Bytes `4..4+N`: raw MCTP packet
//! - Last byte: PEC = CRC-8 (`0x07`) over bytes `0..4+N` (includes dest addr)

use crate::binding::crc8_smbus;
use crate::{Error, HEADER_LEN};

pub const COMMAND_CODE: u8 = 0x0F;
/// Maximum raw MCTP packet bytes fitting in an SMBus block write (byte_count <= 255).
pub const MAX_PACKET_LEN: usize = 254;

/// Decoded SMBus MCTP frame header metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SmbusMeta {
    pub dst_addr: u8,
    pub src_addr: u8,
}

/// Encode `mctp_packet` into a complete SMBus block-write frame (including
/// byte 0 `(dst_addr << 1)` and trailing PEC) in `out`.
pub fn encode(
    dst_addr: u8,
    src_addr: u8,
    mctp_packet: &[u8],
    out: &mut [u8],
) -> Result<usize, Error> {
    if dst_addr > 0x7F || src_addr > 0x7F {
        return Err(Error::InvalidFrame);
    }
    if mctp_packet.len() < HEADER_LEN || mctp_packet.len() > MAX_PACKET_LEN {
        return Err(Error::InvalidFrame);
    }
    let total = 4 + mctp_packet.len() + 1;
    if out.len() < total {
        return Err(Error::BufferTooSmall);
    }
    out[0] = dst_addr << 1;
    out[1] = COMMAND_CODE;
    out[2] = (1 + mctp_packet.len()) as u8;
    out[3] = (src_addr << 1) | 0x01;
    out[4..4 + mctp_packet.len()].copy_from_slice(mctp_packet);
    let pec = crc8_smbus(&out[..total - 1]);
    out[total - 1] = pec;
    Ok(total)
}

/// Decode a complete SMBus frame (starting with `(dst_addr << 1)` and ending
/// with PEC). Returns `(SmbusMeta, mctp_packet_slice)`.
pub fn decode(frame: &[u8]) -> Result<(SmbusMeta, &[u8]), Error> {
    if frame.len() < 4 + HEADER_LEN + 1 {
        return Err(Error::ShortBuffer);
    }
    if (frame[0] & 0x01) != 0 {
        return Err(Error::InvalidFrame);
    }
    if frame[1] != COMMAND_CODE {
        return Err(Error::InvalidFrame);
    }
    let byte_count = frame[2] as usize;
    if byte_count < 1 + HEADER_LEN || frame.len() != 3 + byte_count + 1 {
        return Err(Error::InvalidFrame);
    }
    if (frame[3] & 0x01) == 0 {
        // DSP0237 §6.1: bit 0 of source slave address must be 1.
        return Err(Error::InvalidFrame);
    }
    let expected_pec = crc8_smbus(&frame[..frame.len() - 1]);
    if frame[frame.len() - 1] != expected_pec {
        return Err(Error::BadChecksum);
    }
    let meta = SmbusMeta {
        dst_addr: frame[0] >> 1,
        src_addr: frame[3] >> 1,
    };
    Ok((meta, &frame[4..frame.len() - 1]))
}
