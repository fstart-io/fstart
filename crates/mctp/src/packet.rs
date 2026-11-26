//! MCTP transport header, packetization, and multi-packet reassembly (DSP0236).

use crate::{BASELINE_MTU, Error, HEADER_LEN, HEADER_VERSION};

/// Decoded 4-byte MCTP transport header (DSP0236 §8.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MctpHeader {
    pub hdr_ver: u8,
    pub dst_eid: u8,
    pub src_eid: u8,
    pub som: bool,
    pub eom: bool,
    pub pkt_seq: u8,
    pub tag_owner: bool,
    pub msg_tag: u8,
}

impl MctpHeader {
    /// Encode into 4 wire bytes.
    pub const fn to_bytes(self) -> [u8; HEADER_LEN] {
        let flags = ((self.som as u8) << 7)
            | ((self.eom as u8) << 6)
            | ((self.pkt_seq & 0x03) << 4)
            | ((self.tag_owner as u8) << 3)
            | (self.msg_tag & 0x07);
        [self.hdr_ver & 0x0F, self.dst_eid, self.src_eid, flags]
    }

    /// Parse from 4 wire bytes, validating header version (`0x01`) and
    /// reserved upper nibble (`0x0`).
    pub const fn from_bytes(bytes: [u8; HEADER_LEN]) -> Result<Self, Error> {
        if bytes[0] != HEADER_VERSION {
            return Err(Error::InvalidHeader);
        }
        let flags = bytes[3];
        Ok(Self {
            hdr_ver: bytes[0] & 0x0F,
            dst_eid: bytes[1],
            src_eid: bytes[2],
            som: (flags & 0x80) != 0,
            eom: (flags & 0x40) != 0,
            pkt_seq: (flags >> 4) & 0x03,
            tag_owner: (flags & 0x08) != 0,
            msg_tag: flags & 0x07,
        })
    }

    /// Parse from the front of a raw MCTP packet slice.
    pub fn parse(packet: &[u8]) -> Result<(Self, &[u8]), Error> {
        if packet.len() < HEADER_LEN {
            return Err(Error::ShortBuffer);
        }
        let hdr = Self::from_bytes([packet[0], packet[1], packet[2], packet[3]])?;
        Ok((hdr, &packet[HEADER_LEN..]))
    }
}

/// Addressing and tagging metadata for an outbound MCTP message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MessageMeta {
    pub dst_eid: u8,
    pub src_eid: u8,
    pub msg_type: u8,
    pub integrity_check: bool,
    pub tag_owner: bool,
    pub msg_tag: u8,
}

/// Splits a single MCTP message (message type byte + body) into one or more
/// MTU-bounded MCTP packets.
pub struct Packetizer<'a> {
    dst_eid: u8,
    src_eid: u8,
    msg_type_byte: u8,
    tag_owner: bool,
    msg_tag: u8,
    mtu: usize,
    payload: &'a [u8],
    offset: usize,
    seq: u8,
    done: bool,
}

impl<'a> Packetizer<'a> {
    /// Create a packetizer for `payload` described by `meta`.
    pub fn new(meta: MessageMeta, mtu: usize, payload: &'a [u8]) -> Result<Self, Error> {
        if mtu < BASELINE_MTU {
            return Err(Error::InvalidMtu);
        }
        let msg_type_byte = ((meta.integrity_check as u8) << 7) | (meta.msg_type & 0x7F);
        Ok(Self {
            dst_eid: meta.dst_eid,
            src_eid: meta.src_eid,
            msg_type_byte,
            tag_owner: meta.tag_owner,
            msg_tag: meta.msg_tag & 0x07,
            mtu,
            payload,
            offset: 0,
            seq: 0,
            done: false,
        })
    }

    /// Write the next MCTP packet (4-byte header + payload slice) into `out`.
    /// Returns `Ok(None)` when all packets have been emitted.
    pub fn next_packet(&mut self, out: &mut [u8]) -> Result<Option<usize>, Error> {
        if self.done {
            return Ok(None);
        }
        let som = self.seq == 0 && self.offset == 0;
        let max_body = if som { self.mtu - 1 } else { self.mtu };
        let remaining = self.payload.len() - self.offset;
        let take = remaining.min(max_body);
        let eom = (self.offset + take) == self.payload.len();

        let pkt_payload_len = take + usize::from(som);
        let total_len = HEADER_LEN + pkt_payload_len;
        if out.len() < total_len {
            return Err(Error::BufferTooSmall);
        }

        let hdr = MctpHeader {
            hdr_ver: HEADER_VERSION,
            dst_eid: self.dst_eid,
            src_eid: self.src_eid,
            som,
            eom,
            pkt_seq: self.seq & 0x03,
            tag_owner: self.tag_owner,
            msg_tag: self.msg_tag,
        };
        out[..HEADER_LEN].copy_from_slice(&hdr.to_bytes());
        let mut cursor = HEADER_LEN;
        if som {
            out[cursor] = self.msg_type_byte;
            cursor += 1;
        }
        out[cursor..cursor + take].copy_from_slice(&self.payload[self.offset..self.offset + take]);

        self.offset += take;
        self.seq = self.seq.wrapping_add(1) & 0x03;
        if eom {
            self.done = true;
        }
        Ok(Some(total_len))
    }
}

/// Completed message metadata returned by [`Reassembler::push`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReassembledMessage {
    pub src_eid: u8,
    pub dst_eid: u8,
    pub msg_type: u8,
    pub integrity_check: bool,
    pub tag_owner: bool,
    pub msg_tag: u8,
    pub len: usize,
}

#[derive(Clone, Copy)]
struct Slot<const MAX_MSG: usize> {
    active: bool,
    src_eid: u8,
    dst_eid: u8,
    tag_owner: bool,
    msg_tag: u8,
    expected_seq: u8,
    msg_type: u8,
    integrity_check: bool,
    last_tick_ms: u64,
    len: usize,
    buf: [u8; MAX_MSG],
}

impl<const MAX_MSG: usize> Slot<MAX_MSG> {
    const fn empty() -> Self {
        Self {
            active: false,
            src_eid: 0,
            dst_eid: 0,
            tag_owner: false,
            msg_tag: 0,
            expected_seq: 0,
            msg_type: 0,
            integrity_check: false,
            last_tick_ms: 0,
            len: 0,
            buf: [0; MAX_MSG],
        }
    }
}

/// Fixed-capacity, allocation-free MCTP multi-packet reassembler.
///
/// `MAX_MSG` is the maximum message body length (excluding the MCTP message
/// type byte); `SLOTS` is the number of concurrent in-flight multi-packet
/// messages tracked.
pub struct Reassembler<const MAX_MSG: usize, const SLOTS: usize> {
    slots: [Slot<MAX_MSG>; SLOTS],
    timeout_ms: u64,
}

impl<const MAX_MSG: usize, const SLOTS: usize> Reassembler<MAX_MSG, SLOTS> {
    /// Default DSP0236 `MT2` message reassembly timeout (6 seconds).
    pub const DEFAULT_TIMEOUT_MS: u64 = 6_000;

    /// Create a new empty reassembler with the default 6-second timeout.
    pub const fn new() -> Self {
        Self {
            slots: [Slot::empty(); SLOTS],
            timeout_ms: Self::DEFAULT_TIMEOUT_MS,
        }
    }

    /// Override the reassembly expiry timeout (in milliseconds).
    pub const fn with_timeout_ms(mut self, timeout_ms: u64) -> Self {
        self.timeout_ms = timeout_ms;
        self
    }

    /// Expire any in-flight assembly older than `timeout_ms`.
    pub fn expire(&mut self, now_ms: u64) {
        for slot in &mut self.slots {
            if slot.active && now_ms.saturating_sub(slot.last_tick_ms) >= self.timeout_ms {
                slot.active = false;
            }
        }
    }

    /// Feed a single raw MCTP packet (4-byte header + payload).
    ///
    /// When a complete message is ready (either single-packet or final `EOM`
    /// packet), copies the message body (after the message-type byte) into
    /// `out` and returns `Ok(Some(ReassembledMessage))`.
    pub fn push(
        &mut self,
        packet: &[u8],
        now_ms: u64,
        out: &mut [u8],
    ) -> Result<Option<ReassembledMessage>, Error> {
        self.expire(now_ms);
        let (hdr, body) = MctpHeader::parse(packet)?;

        // Fast path: single-packet message (SOM=1, EOM=1).
        if hdr.som && hdr.eom {
            let (&type_byte, payload) = body.split_first().ok_or(Error::ShortBuffer)?;
            if out.len() < payload.len() {
                return Err(Error::BufferTooSmall);
            }
            // Drop any stale partial assembly with the same key.
            if let Some(idx) = self.find_slot(hdr.src_eid, hdr.tag_owner, hdr.msg_tag) {
                self.slots[idx].active = false;
            }
            out[..payload.len()].copy_from_slice(payload);
            return Ok(Some(ReassembledMessage {
                src_eid: hdr.src_eid,
                dst_eid: hdr.dst_eid,
                msg_type: type_byte & 0x7F,
                integrity_check: (type_byte & 0x80) != 0,
                tag_owner: hdr.tag_owner,
                msg_tag: hdr.msg_tag,
                len: payload.len(),
            }));
        }

        // Start of multi-packet message (SOM=1, EOM=0).
        if hdr.som {
            let (&type_byte, payload) = body.split_first().ok_or(Error::ShortBuffer)?;
            if payload.len() > MAX_MSG {
                return Err(Error::MessageTooLarge);
            }
            // Reuse existing slot for same key (per DSP0236 §8.6.2, new SOM
            // aborts the prior incomplete message) or claim a free slot.
            let idx = self
                .find_slot(hdr.src_eid, hdr.tag_owner, hdr.msg_tag)
                .or_else(|| self.slots.iter().position(|s| !s.active))
                .ok_or(Error::NoReassemblySlot)?;

            let slot = &mut self.slots[idx];
            slot.active = true;
            slot.src_eid = hdr.src_eid;
            slot.dst_eid = hdr.dst_eid;
            slot.tag_owner = hdr.tag_owner;
            slot.msg_tag = hdr.msg_tag;
            slot.expected_seq = hdr.pkt_seq.wrapping_add(1) & 0x03;
            slot.msg_type = type_byte & 0x7F;
            slot.integrity_check = (type_byte & 0x80) != 0;
            slot.last_tick_ms = now_ms;
            slot.len = payload.len();
            slot.buf[..payload.len()].copy_from_slice(payload);
            return Ok(None);
        }

        // Continuation or final packet (SOM=0).
        let Some(idx) = self.find_slot(hdr.src_eid, hdr.tag_owner, hdr.msg_tag) else {
            return Err(Error::BadSequence);
        };
        let slot = &mut self.slots[idx];
        if hdr.pkt_seq != slot.expected_seq {
            // DSP0236 §8.6.2: sequence mismatch drops the partial assembly.
            slot.active = false;
            return Err(Error::BadSequence);
        }
        let new_len = slot.len.checked_add(body.len()).ok_or(Error::MessageTooLarge)?;
        if new_len > MAX_MSG {
            slot.active = false;
            return Err(Error::MessageTooLarge);
        }
        slot.buf[slot.len..new_len].copy_from_slice(body);
        slot.len = new_len;
        slot.expected_seq = hdr.pkt_seq.wrapping_add(1) & 0x03;
        slot.last_tick_ms = now_ms;

        if hdr.eom {
            slot.active = false;
            if out.len() < slot.len {
                return Err(Error::BufferTooSmall);
            }
            out[..slot.len].copy_from_slice(&slot.buf[..slot.len]);
            Ok(Some(ReassembledMessage {
                src_eid: slot.src_eid,
                dst_eid: slot.dst_eid,
                msg_type: slot.msg_type,
                integrity_check: slot.integrity_check,
                tag_owner: slot.tag_owner,
                msg_tag: slot.msg_tag,
                len: slot.len,
            }))
        } else {
            Ok(None)
        }
    }

    fn find_slot(&self, src_eid: u8, tag_owner: bool, msg_tag: u8) -> Option<usize> {
        self.slots.iter().position(|s| {
            s.active && s.src_eid == src_eid && s.tag_owner == tag_owner && s.msg_tag == msg_tag
        })
    }
}

impl<const MAX_MSG: usize, const SLOTS: usize> Default for Reassembler<MAX_MSG, SLOTS> {
    fn default() -> Self {
        Self::new()
    }
}
