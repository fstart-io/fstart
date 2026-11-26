//! High-level MCTP endpoint combining a packet transport, reassembler, tag
//! allocator, and Control Protocol responder.

use crate::control::ControlResponder;
use crate::packet::{MessageMeta, Packetizer, Reassembler};
use crate::{Error, HEADER_LEN, MctpChannel, MessageType, ReceivedMessage};

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
pub struct Endpoint<T, const MAX_MSG: usize = 4096, const SLOTS: usize = 4> {
    transport: T,
    control: ControlResponder,
    reassembler: Reassembler<MAX_MSG, SLOTS>,
    next_tag: u8,
}

impl<T: PacketTransport, const MAX_MSG: usize, const SLOTS: usize> Endpoint<T, MAX_MSG, SLOTS> {
    pub fn new(transport: T, control: ControlResponder) -> Self {
        Self {
            transport,
            control,
            reassembler: Reassembler::new(),
            next_tag: 0,
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
            let now = self.transport.now_ms();
            if let Some(meta) = self.reassembler.push(&pkt_buf[..n], now, buf)? {
                if meta.msg_type == MessageType::Control as u8 && meta.tag_owner {
                    let rlen = self
                        .control
                        .handle_request(&buf[..meta.len], &mut ctrl_resp)?;
                    let _ = self.send_message(
                        meta.src_eid,
                        MessageType::Control as u8,
                        Some(meta.msg_tag),
                        &ctrl_resp[..rlen],
                    )?;
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
        let (tag_owner, msg_tag) = match reply_tag {
            Some(t) => (false, t & 0x07),
            None => {
                let t = self.next_tag & 0x07;
                self.next_tag = self.next_tag.wrapping_add(1) & 0x07;
                (true, t)
            }
        };
        let mtu = self.transport.mtu();
        let mut pkt_buf = [0u8; 512];
        if HEADER_LEN + mtu > pkt_buf.len() {
            return Err(Error::BufferTooSmall);
        }
        let mut packetizer = Packetizer::new(
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
        )?;
        while let Some(len) = packetizer.next_packet(&mut pkt_buf)? {
            self.transport.send_packet(&pkt_buf[..len])?;
        }
        Ok(msg_tag)
    }

    fn recv_message<'a>(&mut self, buf: &'a mut [u8]) -> Result<ReceivedMessage<'a>, Error> {
        let mut pkt_buf = [0u8; 512];
        loop {
            let n = self.transport.recv_packet(&mut pkt_buf)?;
            let now = self.transport.now_ms();
            if let Some(meta) = self.reassembler.push(&pkt_buf[..n], now, buf)? {
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
