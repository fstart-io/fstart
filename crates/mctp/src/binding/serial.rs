//! MCTP over Serial Transport Binding (DSP0253).
//!
//! Wire format:
//! `[0x7E][0x01][byte_count][escaped mctp_packet...][fcs_msb][fcs_lsb][0x7E]`
//!
//! Byte stuffing (`0x7E -> 0x7D 0x5E`, `0x7D -> 0x7D 0x5D`) applies to the
//! packet body; FCS-16 is computed over `[0x01, byte_count, raw_mctp_packet...]`
//! before escaping.

use crate::binding::fcs16_serial_update;
use crate::{BASELINE_MTU, Error, HEADER_LEN};
use zerocopy::big_endian::U16 as BeU16;
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned};

pub const FRAME_FLAG: u8 = 0x7E;
pub const FRAME_ESCAPE: u8 = 0x7D;
pub const PROTOCOL_REVISION: u8 = 0x01;
/// DSP0253 §6.1: baseline serial MTU is 64 bytes (+ 4-byte MCTP header = 68),
/// which also guarantees `byte_count` never collides with `0x7D` / `0x7E`.
pub const MAX_PACKET_LEN: usize = HEADER_LEN + BASELINE_MTU;

#[derive(FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct SerialFrameHeader {
    flag: u8,
    revision: u8,
    byte_count: u8,
}

#[derive(FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct SerialFrameTrailer {
    fcs: BeU16,
    flag: u8,
}

/// Encode a single raw MCTP packet (4-byte header + payload) into a DSP0253
/// serial frame in `out`. Returns the total encoded frame length.
pub fn encode(mctp_packet: &[u8], out: &mut [u8]) -> Result<usize, Error> {
    if mctp_packet.len() < HEADER_LEN || mctp_packet.len() > MAX_PACKET_LEN {
        return Err(Error::InvalidFrame);
    }
    let hdr = SerialFrameHeader {
        flag: FRAME_FLAG,
        revision: PROTOCOL_REVISION,
        byte_count: mctp_packet.len() as u8,
    };
    let fcs = fcs16_serial_update(
        fcs16_serial_update(0xFFFF, &hdr.as_bytes()[1..]),
        mctp_packet,
    );
    let trailer = SerialFrameTrailer {
        fcs: BeU16::new(fcs),
        flag: FRAME_FLAG,
    };

    hdr.write_to_prefix(out)
        .map_err(|_| Error::BufferTooSmall)?;
    let mut pos = core::mem::size_of::<SerialFrameHeader>();
    for &b in mctp_packet {
        if b == FRAME_FLAG || b == FRAME_ESCAPE {
            if pos + 2 > out.len() {
                return Err(Error::BufferTooSmall);
            }
            out[pos] = FRAME_ESCAPE;
            out[pos + 1] = b ^ 0x20;
            pos += 2;
        } else {
            if pos >= out.len() {
                return Err(Error::BufferTooSmall);
            }
            out[pos] = b;
            pos += 1;
        }
    }
    trailer
        .write_to_prefix(&mut out[pos..])
        .map_err(|_| Error::BufferTooSmall)?;
    Ok(pos + core::mem::size_of::<SerialFrameTrailer>())
}

/// Decode a single complete DSP0253 serial frame into `out`. Returns the raw
/// MCTP packet length written to `out`.
pub fn decode(frame: &[u8], out: &mut [u8]) -> Result<usize, Error> {
    let mut dec = SerialDecoder::new();
    for &b in frame {
        if let Some(pkt) = dec.push_byte(b)? {
            if out.len() < pkt.len() {
                return Err(Error::BufferTooSmall);
            }
            out[..pkt.len()].copy_from_slice(pkt);
            return Ok(pkt.len());
        }
    }
    Err(Error::ShortBuffer)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    WaitSync,
    WaitRev,
    WaitLen,
    Body,
    BodyEscape,
    FcsMsb,
    FcsLsb,
    WaitEndFlag,
}

/// Streaming byte-by-byte DSP0253 serial frame decoder (allocation-free).
pub struct SerialDecoder {
    state: State,
    expected_len: usize,
    pos: usize,
    fcs_msb: u8,
    fcs_lsb: u8,
    buf: [u8; MAX_PACKET_LEN],
}

impl SerialDecoder {
    pub const fn new() -> Self {
        Self {
            state: State::WaitSync,
            expected_len: 0,
            pos: 0,
            fcs_msb: 0,
            fcs_lsb: 0,
            buf: [0; MAX_PACKET_LEN],
        }
    }

    /// Reset the decoder to await the next `0x7E` sync flag.
    pub fn reset(&mut self) {
        self.state = State::WaitSync;
        self.expected_len = 0;
        self.pos = 0;
    }

    /// Feed one received UART byte. Returns `Ok(Some(raw_mctp_packet))` when a
    /// complete, FCS-verified packet has been received.
    pub fn push_byte(&mut self, byte: u8) -> Result<Option<&[u8]>, Error> {
        match self.state {
            State::WaitSync => {
                if byte == FRAME_FLAG {
                    self.state = State::WaitRev;
                }
                Ok(None)
            }
            State::WaitRev => {
                if byte == FRAME_FLAG {
                    // Back-to-back sync flags are allowed between frames.
                    return Ok(None);
                }
                if byte != PROTOCOL_REVISION {
                    self.reset();
                    return Err(Error::InvalidFrame);
                }
                self.state = State::WaitLen;
                Ok(None)
            }
            State::WaitLen => {
                let len = byte as usize;
                if !(HEADER_LEN..=MAX_PACKET_LEN).contains(&len) {
                    self.reset();
                    return Err(Error::InvalidFrame);
                }
                self.expected_len = len;
                self.pos = 0;
                self.state = State::Body;
                Ok(None)
            }
            State::Body => {
                if byte == FRAME_FLAG {
                    // Unexpected sync flag: treat as start of a new frame.
                    self.state = State::WaitRev;
                    return Err(Error::InvalidFrame);
                }
                if byte == FRAME_ESCAPE {
                    self.state = State::BodyEscape;
                    return Ok(None);
                }
                self.buf[self.pos] = byte;
                self.pos += 1;
                if self.pos == self.expected_len {
                    self.state = State::FcsMsb;
                }
                Ok(None)
            }
            State::BodyEscape => {
                if byte == FRAME_FLAG {
                    // Resynchronize on the opening flag of the next frame.
                    self.state = State::WaitRev;
                    return Err(Error::InvalidFrame);
                }
                let unescaped = byte ^ 0x20;
                if unescaped != FRAME_FLAG && unescaped != FRAME_ESCAPE {
                    self.reset();
                    return Err(Error::InvalidFrame);
                }
                self.buf[self.pos] = unescaped;
                self.pos += 1;
                self.state = if self.pos == self.expected_len {
                    State::FcsMsb
                } else {
                    State::Body
                };
                Ok(None)
            }
            State::FcsMsb => {
                self.fcs_msb = byte;
                self.state = State::FcsLsb;
                Ok(None)
            }
            State::FcsLsb => {
                self.fcs_lsb = byte;
                self.state = State::WaitEndFlag;
                Ok(None)
            }
            State::WaitEndFlag => {
                let len = self.expected_len;
                let rx_fcs = u16::from_be_bytes([self.fcs_msb, self.fcs_lsb]);
                self.state = State::WaitSync;
                if byte != FRAME_FLAG {
                    return Err(Error::InvalidFrame);
                }
                let calc = fcs16_serial_update(
                    fcs16_serial_update(0xFFFF, &[PROTOCOL_REVISION, len as u8]),
                    &self.buf[..len],
                );
                if calc != rx_fcs {
                    return Err(Error::BadChecksum);
                }
                Ok(Some(&self.buf[..len]))
            }
        }
    }
}

impl Default for SerialDecoder {
    fn default() -> Self {
        Self::new()
    }
}
