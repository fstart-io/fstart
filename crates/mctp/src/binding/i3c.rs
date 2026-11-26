//! MCTP over I3C Transport Binding (DSP0233).
//!
//! Wire format:
//! - Private Write (Controller -> Target): `[mctp_packet...][PEC]`
//!   where PEC = CRC-8 (`0x07`) over `[(target_addr << 1) | 0, mctp_packet...]`.
//! - Private Read (Target -> Controller): `[mctp_packet...][PEC]`
//!   where PEC = CRC-8 (`0x07`) over `[(target_addr << 1) | 1, mctp_packet...]`.
//! - Target-initiated In-Band Interrupt (IBI) Mandatory Data Byte: `0xAE`
//!   (MCTP pending read, DSP0233 Table 2).

use crate::binding::crc8_smbus_update;
use crate::{Error, HEADER_LEN};

/// I3C In-Band Interrupt Mandatory Data Byte for MCTP (DSP0233 Table 2).
pub const IBI_MDB_MCTP: u8 = 0xAE;

/// Encode an MCTP-over-I3C payload (`[mctp_packet..., PEC]`) for a private
/// write (`is_read = false`) or private read (`is_read = true`) to/from
/// 7-bit `target_addr`.
pub fn encode(
    target_addr: u8,
    is_read: bool,
    mctp_packet: &[u8],
    out: &mut [u8],
) -> Result<usize, Error> {
    if target_addr > 0x7F || mctp_packet.len() < HEADER_LEN {
        return Err(Error::InvalidFrame);
    }
    let total = mctp_packet.len() + 1;
    if out.len() < total {
        return Err(Error::BufferTooSmall);
    }
    let addr_byte = (target_addr << 1) | u8::from(is_read);
    let pec = crc8_smbus_update(crc8_smbus_update(0x00, &[addr_byte]), mctp_packet);
    out[..mctp_packet.len()].copy_from_slice(mctp_packet);
    out[mctp_packet.len()] = pec;
    Ok(total)
}

/// Decode and PEC-verify an MCTP-over-I3C payload (`[mctp_packet..., PEC]`).
/// Returns the raw MCTP packet slice.
pub fn decode(target_addr: u8, is_read: bool, frame: &[u8]) -> Result<&[u8], Error> {
    if target_addr > 0x7F {
        return Err(Error::InvalidFrame);
    }
    if frame.len() < HEADER_LEN + 1 {
        return Err(Error::ShortBuffer);
    }
    let (pkt, pec_slice) = frame.split_at(frame.len() - 1);
    let addr_byte = (target_addr << 1) | u8::from(is_read);
    let expected = crc8_smbus_update(crc8_smbus_update(0x00, &[addr_byte]), pkt);
    if pec_slice[0] != expected {
        return Err(Error::BadChecksum);
    }
    Ok(pkt)
}
