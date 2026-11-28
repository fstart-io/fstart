//! Security Protocol and Data Model (SPDM 1.2, DSP0274) and Secured Messages
//! using SPDM (DSP0277) for `no_std` firmware.
//!
//! Designed as allocation-free, sans-IO requester and responder state machines
//! over caller buffers, supporting both MCTP (DSP0275/DSP0276) and PCIe DOE
//! encapsulation.

#![no_std]

pub mod crypto;
pub mod requester;
pub mod responder;
pub mod secured;
pub mod wire;

pub use crypto::{Rng, Sha256Transcript, SpdmKeyPair};
pub use requester::SpdmRequester;
pub use responder::{MeasurementBlock, SpdmResponder};
pub use secured::{SecuredSession, SessionPhase, SessionTransport};

/// DSP0276 inner MCTP message type byte for SPDM messages inside a secured
/// MCTP record (defined here so `fstart-spdm` does not depend on `fstart-mctp`).
pub(crate) const MCTP_MSG_TYPE_SPDM: u8 = 0x05;

/// SPDM version byte encoding (`major << 4 | minor`).
pub const SPDM_VERSION_1_0: u8 = 0x10;
pub const SPDM_VERSION_1_1: u8 = 0x11;
pub const SPDM_VERSION_1_2: u8 = 0x12;

/// SPDM Request/Response codes (DSP0274 Table 13).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum SpdmCode {
    // Responses
    Digests = 0x01,
    Certificate = 0x02,
    ChallengeAuth = 0x03,
    Version = 0x04,
    Measurements = 0x60,
    Capabilities = 0x61,
    Algorithms = 0x63,
    KeyExchangeRsp = 0x64,
    FinishRsp = 0x65,
    HeartbeatAck = 0x68,
    KeyUpdateAck = 0x69,
    EndSessionAck = 0x6C,
    Error = 0x7F,

    // Requests
    GetDigests = 0x81,
    GetCertificate = 0x82,
    Challenge = 0x83,
    GetVersion = 0x84,
    GetMeasurements = 0xE0,
    GetCapabilities = 0xE1,
    NegotiateAlgorithms = 0xE3,
    KeyExchange = 0xE4,
    Finish = 0xE5,
    Heartbeat = 0xE8,
    KeyUpdate = 0xE9,
    EndSession = 0xEC,
}

/// SPDM error codes carried in `param1` of an `ERROR (0x7F)` response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum SpdmErrorCode {
    InvalidRequest = 0x01,
    Busy = 0x03,
    UnexpectedRequest = 0x04,
    Unspecified = 0x05,
    DecryptError = 0x06,
    UnsupportedRequest = 0x07,
    InvalidSession = 0x09,
    VersionMismatch = 0x41,
}

/// Standard SPDM algorithm bitmasks (DSP0274 Table 21/23).
pub const BASE_ASYM_ECDSA_ECC_NIST_P256: u32 = 1 << 4;
pub const BASE_HASH_SHA_256: u32 = 1 << 0;
pub const DHE_SECP256R1: u16 = 1 << 3;
pub const AEAD_AES_256_GCM: u16 = 1 << 1;
pub const KEY_SCHEDULE_SPDM: u16 = 1 << 0;
pub const MEAS_HASH_SHA_256: u32 = 1 << 1;

/// SPDM capability flags (subset used by requester & responder).
pub const CAP_CERT: u32 = 1 << 1;
pub const CAP_CHAL: u32 = 1 << 2;
pub const CAP_MEAS_SIG: u32 = 1 << 4;
pub const CAP_ENCRYPT: u32 = 1 << 6;
pub const CAP_MAC: u32 = 1 << 7;
pub const CAP_KEY_EX: u32 = 1 << 9;
pub const CAP_HBEAT: u32 = 1 << 13;
pub const CAP_KEY_UPD: u32 = 1 << 14;
pub const CAP_HANDSHAKE_IN_THE_CLEAR: u32 = 0;

use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned};

/// Common 4-byte SPDM message header.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned,
)]
#[repr(C)]
pub struct SpdmHeader {
    pub version: u8,
    pub code: u8,
    pub param1: u8,
    pub param2: u8,
}

impl SpdmHeader {
    pub const LEN: usize = core::mem::size_of::<Self>();

    pub const fn new(version: u8, code: SpdmCode, param1: u8, param2: u8) -> Self {
        Self {
            version,
            code: code as u8,
            param1,
            param2,
        }
    }

    pub const fn to_bytes(self) -> [u8; 4] {
        [self.version, self.code, self.param1, self.param2]
    }

    pub fn parse(msg: &[u8]) -> Result<(Self, &[u8]), Error> {
        let (hdr, rest) = Self::read_from_prefix(msg).map_err(|_| Error::ShortBuffer)?;
        Ok((hdr, rest))
    }
}

/// Errors returned by SPDM protocol, crypto, or secured-session operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    ShortBuffer,
    BufferTooSmall,
    InvalidMessage,
    VersionMismatch,
    UnsupportedAlgorithm,
    Crypto,
    SignatureInvalid,
    VerifyDataMismatch,
    DecryptFailed,
    SequenceOverflow,
    SessionInactive,
    RemoteError(u8),
}

/// Write a 4-byte SPDM `ERROR` response into `out`.
pub fn write_error(
    version: u8,
    err: SpdmErrorCode,
    data: u8,
    out: &mut [u8],
) -> Result<usize, Error> {
    SpdmHeader::new(version, SpdmCode::Error, err as u8, data)
        .write_to_prefix(out)
        .map_err(|_| Error::BufferTooSmall)?;
    Ok(SpdmHeader::LEN)
}
