//! High-level MCTP endpoint combining a packet transport, reassembler,
//! outstanding-tag table, and Control Protocol responder.

use crate::control::ControlResponder;
use crate::packet::{MctpHeader, MessageMeta, Packetizer, Reassembler};
use crate::{
    BASELINE_MTU, EID_BROADCAST, EID_NULL, Error, HEADER_LEN, MctpChannel, MessageType,
    ReceivedMessage,
};

/// Physical packet-level transport abstraction implemented by UART, SMBus/I2C,
/// I3C, KCS, PCIe VDM, or host-side socket/in-memory bridges.
pub trait PacketTransport {
    /// Maximum MCTP payload bytes per packet (must be >= [`crate::BASELINE_MTU`]).
    fn mtu(&self) -> usize;

    /// Transmit one raw MCTP packet (`[4-byte header, payload...]`).
    fn send_packet(&mut self, mctp_packet: &[u8]) -> Result<(), Error>;

    /// Receive the next raw MCTP packet into `out`. Returns its length.
    fn recv_packet(&mut self, out: &mut [u8]) -> Result<usize, Error>;

    /// Monotonic timestamp in milliseconds for reassembly timeout tracking.
    fn now_ms(&self) -> u64 {
        0
    }
}

/// Complete MCTP endpoint implementing [`MctpChannel`] over any [`PacketTransport`].
///
/// Defaults (`MAX_MSG = 512`, `SLOTS = 2`) keep inline storage ~1 KiB so it
/// fits comfortably on early-stage 8 KiB stacks. Callers in DRAM stages may
/// instantiate larger const generics (e.g. `Endpoint<T, 4096, 4>`).
pub struct Endpoint<T, const MAX_MSG: usize = 512, const SLOTS: usize = 2> {
    transport: T,
    control: ControlResponder,
    reassembler: Reassembler<MAX_MSG, SLOTS>,
    next_tag: u8,
    /// Tracks active outbound requester tags (`TO=1`) -> target peer EID, so
    /// unsolicited `TO=0` responses are dropped per DSP0236 §8.6.
    live_tags: [Option<u8>; 8],
}

impl<T: PacketTransport, const MAX_MSG: usize, const SLOTS: usize> Endpoint<T, MAX_MSG, SLOTS> {
    pub fn new(transport: T, control: ControlResponder) -> Self {
        Self {
            transport,
            control,
            reassembler: Reassembler::new(),
            next_tag: 0,
            live_tags: [None; 8],
        }
    }

    pub fn transport(&self) -> &T {
        &self.transport
    }

    pub fn transport_mut(&mut self) -> &mut T {
        &mut self.transport
    }

    pub fn control(&self) -> &ControlResponder {
        &self.control
    }

    pub fn control_mut(&mut self) -> &mut ControlResponder {
        &mut self.control
    }

    pub fn eid(&self) -> u8 {
        self.control.eid()
    }

    /// Explicitly release an outstanding requester tag (e.g. after timeout).
    pub fn release_tag(&mut self, tag: u8) {
        self.live_tags[(tag & 0x07) as usize] = None;
    }

    fn accepts_packet(&self, hdr: MctpHeader) -> bool {
        // DSP0236 §8.6: accept packets addressed to our EID, null EID
        // (physical-address-only), or broadcast EID.
        let eid_ok = hdr.dst_eid == self.control.eid()
            || hdr.dst_eid == EID_NULL
            || hdr.dst_eid == EID_BROADCAST;
        if !eid_ok {
            return false;
        }
        // Drop TO=0 responses for unallocated tags or mismatched unicast peers
        // (allow null-EID discovery requests to match the peer's assigned EID).
        if !hdr.tag_owner {
            match self.live_tags[(hdr.msg_tag & 0x07) as usize] {
                Some(expected) if expected == hdr.src_eid || expected == EID_NULL => {}
                _ => return false,
            }
        }
        true
    }

    fn alloc_tag(&mut self, dst_eid: u8) -> Result<u8, Error> {
        for i in 0..8 {
            let candidate = self.next_tag.wrapping_add(i) & 0x07;
            if self.live_tags[candidate as usize].is_none() {
                self.live_tags[candidate as usize] = Some(dst_eid);
                self.next_tag = candidate.wrapping_add(1) & 0x07;
                return Ok(candidate);
            }
        }
        Err(Error::NoAvailableTag)
    }

    /// Receive the next message; if it is an incoming MCTP Control request
    /// (`msg_type == Control && tag_owner`), handle it automatically and send
    /// the reply, then continue waiting for the next non-control (or control
    /// response) message.
    pub fn recv_handling_control<'a>(
        &mut self,
        buf: &'a mut [u8],
    ) -> Result<ReceivedMessage<'a>, Error> {
        let mut pkt_buf = [0u8; 512];
        let mut ctrl_resp = [0u8; 64];
        loop {
            let n = self.transport.recv_packet(&mut pkt_buf)?;
            if n > pkt_buf.len() {
                return Err(Error::BufferTooSmall);
            }
            let pkt = &pkt_buf[..n];
            let Ok((hdr, _)) = MctpHeader::parse(pkt) else {
                continue;
            };
            if !self.accepts_packet(hdr) {
                continue;
            }
            let now = self.transport.now_ms();
            if let Some(meta) = self.reassembler.push(pkt, now, buf)? {
                if !meta.tag_owner {
                    self.live_tags[(meta.msg_tag & 0x07) as usize] = None;
                }
                if meta.msg_type == MessageType::Control as u8 && meta.tag_owner {
                    // DSP0236 §§11.2.1, 11.4: Control messages must be IC=0 and
                    // single-packet (<= 64B payload).
                    if meta.integrity_check || meta.len >= BASELINE_MTU || !(hdr.som && hdr.eom) {
                        continue;
                    }
                    let rlen = self
                        .control
                        .handle_request(&buf[..meta.len], &mut ctrl_resp)?;
                    if rlen > 0 {
                        let _ = self.send_message(
                            meta.src_eid,
                            MessageType::Control as u8,
                            Some(meta.msg_tag),
                            &ctrl_resp[..rlen],
                        )?;
                    }
                    continue;
                }
                return Ok(ReceivedMessage {
                    src_eid: meta.src_eid,
                    dst_eid: meta.dst_eid,
                    msg_type: meta.msg_type,
                    integrity_check: meta.integrity_check,
                    tag_owner: meta.tag_owner,
                    msg_tag: meta.msg_tag,
                    payload: &buf[..meta.len],
                });
            }
        }
    }
}

impl<T: PacketTransport, const MAX_MSG: usize, const SLOTS: usize> MctpChannel
    for Endpoint<T, MAX_MSG, SLOTS>
{
    fn send_message(
        &mut self,
        dst_eid: u8,
        msg_type: u8,
        reply_tag: Option<u8>,
        payload: &[u8],
    ) -> Result<u8, Error> {
        if msg_type == MessageType::Control as u8 && payload.len() >= BASELINE_MTU {
            return Err(Error::InvalidControl);
        }
        let (tag_owner, msg_tag) = match reply_tag {
            Some(t) => (false, t & 0x07),
            None => (true, self.alloc_tag(dst_eid)?),
        };
        let mtu = self.transport.mtu();
        let mut pkt_buf = [0u8; 512];
        if mtu > pkt_buf.len() - HEADER_LEN {
            if tag_owner {
                self.live_tags[msg_tag as usize] = None;
            }
            return Err(Error::BufferTooSmall);
        }
        let mut packetizer = match Packetizer::new(
            MessageMeta {
                dst_eid,
                src_eid: self.control.eid(),
                msg_type,
                integrity_check: false,
                tag_owner,
                msg_tag,
            },
            mtu,
            payload,
        ) {
            Ok(p) => p,
            Err(e) => {
                if tag_owner {
                    self.live_tags[msg_tag as usize] = None;
                }
                return Err(e);
            }
        };
        while let Some(len) = packetizer.next_packet(&mut pkt_buf)? {
            if let Err(e) = self.transport.send_packet(&pkt_buf[..len]) {
                if tag_owner {
                    self.live_tags[msg_tag as usize] = None;
                }
                return Err(e);
            }
        }
        Ok(msg_tag)
    }

    fn recv_message<'a>(&mut self, buf: &'a mut [u8]) -> Result<ReceivedMessage<'a>, Error> {
        let mut pkt_buf = [0u8; 512];
        loop {
            let n = self.transport.recv_packet(&mut pkt_buf)?;
            if n > pkt_buf.len() {
                return Err(Error::BufferTooSmall);
            }
            let pkt = &pkt_buf[..n];
            let Ok((hdr, _)) = MctpHeader::parse(pkt) else {
                continue;
            };
            if !self.accepts_packet(hdr) {
                continue;
            }
            let now = self.transport.now_ms();
            if let Some(meta) = self.reassembler.push(pkt, now, buf)? {
                if !meta.tag_owner {
                    self.live_tags[(meta.msg_tag & 0x07) as usize] = None;
                }
                return Ok(ReceivedMessage {
                    src_eid: meta.src_eid,
                    dst_eid: meta.dst_eid,
                    msg_type: meta.msg_type,
                    integrity_check: meta.integrity_check,
                    tag_owner: meta.tag_owner,
                    msg_tag: meta.msg_tag,
                    payload: &buf[..meta.len],
                });
            }
        }
    }
}
