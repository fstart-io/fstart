//! Shared `zerocopy` SPDM 1.2 wire-format structures (DSP0274).
//!
//! Every struct here is alignment-one `#[repr(C)]` with explicit little-endian
//! fields so both [`crate::SpdmRequester`] and [`crate::SpdmResponder`] share
//! the exact same on-wire definitions.

use crate::crypto::{HASH_LEN, P256_PUB_LEN};
use zerocopy::little_endian::{U16 as LeU16, U32 as LeU32};
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned};

/// Fixed prefix of `VERSION` (`0x04`) body before the `VersionNumberEntry` array.
#[derive(Debug, Clone, Copy, FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
pub struct VersionResponsePrefix {
    pub reserved: u8,
    pub version_number_entry_count: u8,
}

/// Shared body of `GET_CAPABILITIES` (`0xE1`) and `CAPABILITIES` (`0x61`).
#[derive(Debug, Clone, Copy, FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
pub struct CapabilitiesBody {
    pub reserved: u8,
    pub ct_exponent: u8,
    pub reserved2: LeU16,
    pub flags: LeU32,
    pub data_transfer_size: LeU32,
    pub max_spdm_msg_size: LeU32,
}

/// 4-byte `AlgStruct` entry in `NEGOTIATE_ALGORITHMS` / `ALGORITHMS`.
#[derive(Debug, Clone, Copy, FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
pub struct AlgStructEntry {
    pub alg_type: u8,
    pub alg_count: u8,
    pub alg_supported: LeU16,
}

/// Fixed body prefix of `NEGOTIATE_ALGORITHMS` (`0xE3`) before `AlgStructEntry`s.
#[derive(Debug, Clone, Copy, FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
pub struct NegotiateAlgorithmsBody {
    pub length: LeU16,
    pub measurement_spec: u8,
    pub other_params_support: u8,
    pub base_asym_algo: LeU32,
    pub base_hash_algo: LeU32,
    pub reserved: [u8; 12],
    pub ext_asym_count: u8,
    pub ext_hash_count: u8,
    pub reserved2: LeU16,
}

/// Fixed body prefix of `ALGORITHMS` (`0x63`) before `AlgStructEntry`s.
#[derive(Debug, Clone, Copy, FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
pub struct AlgorithmsResponseBody {
    pub length: LeU16,
    pub measurement_spec_sel: u8,
    pub other_params_sel: u8,
    pub measurement_hash_algo: LeU32,
    pub base_asym_sel: LeU32,
    pub base_hash_sel: LeU32,
    pub reserved: [u8; 12],
    pub ext_asym_sel_count: u8,
    pub ext_hash_sel_count: u8,
    pub reserved2: LeU16,
}

/// DSP0274 §10.7 certificate chain prefix (`Length(2) + Reserved(2) + RootHash(32)`).
#[derive(Debug, Clone, Copy, FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
pub struct CertificateChainPrefix {
    pub length: LeU16,
    pub reserved: LeU16,
    pub root_hash: [u8; HASH_LEN],
}

/// Body of `GET_CERTIFICATE` (`0x82`).
#[derive(Debug, Clone, Copy, FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
pub struct GetCertificateBody {
    pub offset: LeU16,
    pub length: LeU16,
}

/// Fixed body prefix of `CERTIFICATE` (`0x02`) before certificate bytes.
#[derive(Debug, Clone, Copy, FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
pub struct CertificateResponsePrefix {
    pub portion_length: LeU16,
    pub remainder_length: LeU16,
}

/// Fixed body of `CHALLENGE_AUTH` (`0x03`, no measurement summary hash) before signature.
#[derive(Debug, Clone, Copy, FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
pub struct ChallengeAuthPrefix {
    pub cert_chain_hash: [u8; HASH_LEN],
    pub nonce: [u8; 32],
    pub opaque_length: LeU16,
}

/// Body of signed `GET_MEASUREMENTS` (`0xE0`).
#[derive(Debug, Clone, Copy, FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
pub struct GetMeasurementsBody {
    pub nonce: [u8; 32],
    pub slot_id_param: u8,
}

/// Fixed body prefix of `MEASUREMENTS` (`0x60`) before measurement blocks.
#[derive(Debug, Clone, Copy, FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
pub struct MeasurementsPrefix {
    pub num_blocks: u8,
    /// 24-bit little-endian `MeasurementRecordLength`.
    pub record_len: [u8; 3],
}

impl MeasurementsPrefix {
    pub const fn new(num_blocks: u8, record_len: usize) -> Self {
        let b = (record_len as u32).to_le_bytes();
        Self {
            num_blocks,
            record_len: [b[0], b[1], b[2]],
        }
    }

    pub const fn record_len(&self) -> usize {
        (self.record_len[0] as usize)
            | ((self.record_len[1] as usize) << 8)
            | ((self.record_len[2] as usize) << 16)
    }
}

/// Single SHA-256 DMTF measurement block (39 bytes, DSP0274 Table 47).
#[derive(Debug, Clone, Copy, FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
pub struct DmtfMeasurementBlock {
    pub index: u8,
    pub spec: u8,
    pub size: LeU16,
    pub value_type: u8,
    pub value_size: LeU16,
    pub digest: [u8; HASH_LEN],
}

/// Fixed suffix after measurement blocks in `MEASUREMENTS` (`0x60`) before opaque/signature.
#[derive(Debug, Clone, Copy, FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
pub struct MeasurementsSuffix {
    pub nonce: [u8; 32],
    pub opaque_length: LeU16,
}

/// Fixed body prefix of `KEY_EXCHANGE` (`0xE4`) before opaque data.
#[derive(Debug, Clone, Copy, FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
pub struct KeyExchangeReqPrefix {
    pub req_session_id: LeU16,
    pub session_policy: u8,
    pub reserved: u8,
    pub random_data: [u8; 32],
    pub exchange_data: [u8; P256_PUB_LEN],
    pub opaque_length: LeU16,
}

/// Fixed body prefix of `KEY_EXCHANGE_RSP` (`0x64`) before opaque data, signature, and verify data.
#[derive(Debug, Clone, Copy, FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
pub struct KeyExchangeRspPrefix {
    pub rsp_session_id: LeU16,
    pub mut_auth_requested: u8,
    pub slot_id_param: u8,
    pub random_data: [u8; 32],
    pub exchange_data: [u8; P256_PUB_LEN],
    pub opaque_length: LeU16,
}

/// DSP0274 `bin_str` fixed prefix (`Length(2) + "spdmX.Y "(8)`) for HKDF-Expand.
#[derive(Debug, Clone, Copy, FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
pub struct SpdmBinStrPrefix {
    pub length: LeU16,
    pub version: [u8; 8],
}
