//! Management Component Transport Protocol (MCTP, DSP0236) core, Control
//! Protocol, and transport bindings for `no_std` firmware.
//!
//! All five physical transport bindings are compiled unconditionally (no
//! per-transport Cargo features); the linker discards unused bindings.

#![no_std]

pub mod binding;
pub mod control;
pub mod endpoint;
pub mod packet;

pub use binding::{PhysAddr, TransportKind};
pub use control::{ControlCommand, ControlCompletion, ControlResponder, SetEidOperation};
pub use endpoint::{Endpoint, PacketTransport};
pub use packet::{MctpHeader, MessageMeta, Packetizer, ReassembledMessage, Reassembler};

/// MCTP transport header version defined by DSP0236.
pub const HEADER_VERSION: u8 = 0x01;

/// Baseline MCTP transmission unit (payload bytes per packet, DSP0236 §8.3.1).
pub const BASELINE_MTU: usize = 64;

/// Size in bytes of the 4-byte MCTP transport header.
pub const HEADER_LEN: usize = 4;

/// Null Endpoint ID (unassigned / physical-address-only).
pub const EID_NULL: u8 = 0x00;

/// Broadcast Endpoint ID.
pub const EID_BROADCAST: u8 = 0xFF;

/// Standard MCTP message types (DSP0239).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum MessageType {
    /// MCTP Control Protocol (DSP0236 §12).
    Control = 0x00,
    /// Platform Level Data Model (DSP0240).
    Pldm = 0x01,
    /// NC-SI over MCTP (DSP0261).
    Ncsi = 0x02,
    /// Ethernet over MCTP (DSP0261).
    Ethernet = 0x03,
    /// NVMe Management Interface over MCTP.
    NvmeMi = 0x04,
    /// Security Protocol and Data Model (DSP0275).
    Spdm = 0x05,
    /// Secured Messages using SPDM over MCTP (DSP0276).
    SecuredMsg = 0x06,
    /// CXL Fabric Manager API over MCTP.
    CxlFmApi = 0x07,
    /// CXL Component Command Interface over MCTP.
    CxlCci = 0x08,
    /// Vendor Defined - PCI.
    VendorPci = 0x7E,
    /// Vendor Defined - IANA.
    VendorIana = 0x7F,
}

impl MessageType {
    /// Decode a 7-bit message type field.
    pub const fn from_u8(val: u8) -> Option<Self> {
        match val & 0x7F {
            0x00 => Some(Self::Control),
            0x01 => Some(Self::Pldm),
            0x02 => Some(Self::Ncsi),
            0x03 => Some(Self::Ethernet),
            0x04 => Some(Self::NvmeMi),
            0x05 => Some(Self::Spdm),
            0x06 => Some(Self::SecuredMsg),
            0x07 => Some(Self::CxlFmApi),
            0x08 => Some(Self::CxlCci),
            0x7E => Some(Self::VendorPci),
            0x7F => Some(Self::VendorIana),
            _ => None,
        }
    }
}

/// Errors produced by MCTP framing, reassembly, control, or transport bindings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// Input frame or packet is shorter than required.
    ShortBuffer,
    /// Caller-provided output buffer is too small.
    BufferTooSmall,
    /// Unsupported or malformed MCTP header version or reserved bits.
    InvalidHeader,
    /// Transport-specific framing error (bad sync byte, length, vendor ID, etc.).
    InvalidFrame,
    /// Checksum mismatch (CRC-8 PEC or FCS-16).
    BadChecksum,
    /// Invalid or out-of-range MTU (must be >= [`BASELINE_MTU`]).
    InvalidMtu,
    /// Sequence number mismatch during multi-packet reassembly.
    BadSequence,
    /// Reassembly message exceeded the configured maximum message buffer size.
    MessageTooLarge,
    /// All concurrent reassembly slots are occupied.
    NoReassemblySlot,
    /// All 8 outbound requester message tags are currently in use.
    NoAvailableTag,
    /// Transport layer I/O failure.
    Transport,
    /// Operation timed out waiting for a packet or message.
    Timeout,
    /// Malformed or unsupported MCTP control message.
    InvalidControl,
}

/// A complete reassembled MCTP message delivered to upper layers.
#[derive(Debug, PartialEq, Eq)]
pub struct ReceivedMessage<'a> {
    pub src_eid: u8,
    pub dst_eid: u8,
    pub msg_type: u8,
    pub integrity_check: bool,
    pub tag_owner: bool,
    pub msg_tag: u8,
    pub payload: &'a [u8],
}

/// Transport-independent message-level MCTP channel used by PLDM and SPDM.
pub trait MctpChannel {
    /// Send a complete MCTP message.
    ///
    /// If `reply_tag` is `None`, the channel allocates a new tag and sets
    /// `TO = 1` (request). If `Some(tag)`, it sets `TO = 0` and reuses `tag`
    /// (response). Returns the 3-bit `msg_tag` used on the wire.
    fn send_message(
        &mut self,
        dst_eid: u8,
        msg_type: u8,
        reply_tag: Option<u8>,
        payload: &[u8],
    ) -> Result<u8, Error>;

    /// Poll/receive the next complete MCTP message into `buf`.
    fn recv_message<'a>(&mut self, buf: &'a mut [u8]) -> Result<ReceivedMessage<'a>, Error>;
}
