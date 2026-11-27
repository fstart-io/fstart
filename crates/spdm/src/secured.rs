//! Secured Messages using SPDM (DSP0277) over MCTP (DSP0276) and PCIe DOE.
//!
//! Wire record layout:
//! - `session_id`: 4 bytes LE
//! - `seq_num`: 2 bytes LE on MCTP (`SessionTransport::Mctp`), 0 bytes on DOE
//!   (`SessionTransport::Doe`)
//! - `length`: 2 bytes LE = `ciphertext.len() + 16 (tag)`
//! - `ciphertext`: encrypted `[app_data_len_le16, app_data...]`
//! - `tag`: 16-byte AES-256-GCM authentication tag
//!
//! AAD covers `[session_id, (optional seq_num), length]`.

use crate::Error;
use crate::crypto::{
    AEAD_IV_LEN, AEAD_KEY_LEN, AEAD_TAG_LEN, HASH_LEN, aes256_gcm_decrypt_in_place,
    aes256_gcm_encrypt_in_place, hkdf_expand_spdm, hkdf_extract_sha256, record_iv,
};
use zeroize::Zeroize;

/// DSP0277 §6.1 `OpaqueDataFmt1` SupportedVersion list (advertising DSP0277 1.1)
/// sent in `KEY_EXCHANGE`.
pub const OPAQUE_SUPPORTED_VERSION_1_1: [u8; 16] = [
    0x01, 0x00, 0x00, 0x00, // TotalElements=1, Reserved[3]=0
    0x00, 0x00, // ID=DMTF(0x00), VendorLen=0
    0x06, 0x00, // OpaqueElementDataLen=6
    0x01, 0x01, // SMDataVersion=1, SMDataID=SupportedVersion(1)
    0x01, 0x00, 0x11, 0x00, // VersionCount=1, Version=1.1 (0x1100 LE) + 1B pad
    0x00, 0x00, // 4-byte alignment padding
];

/// DSP0277 §6.1 `OpaqueDataFmt1` VersionSelection (selecting DSP0277 1.1)
/// sent in `KEY_EXCHANGE_RSP`.
pub const OPAQUE_SELECTED_VERSION_1_1: [u8; 12] = [
    0x01, 0x00, 0x00, 0x00, // TotalElements=1, Reserved[3]=0
    0x00, 0x00, // ID=DMTF(0x00), VendorLen=0
    0x04, 0x00, // OpaqueElementDataLen=4
    0x01, 0x00, // SMDataVersion=1, SMDataID=VersionSelection(0)
    0x00, 0x11, // SelectedVersion=1.1 (0x1100 LE)
];

/// Encapsulation rules that differ between MCTP (DSP0276) and PCIe DOE.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionTransport {
    /// MCTP (DSP0276): 2-byte LE sequence number transmitted in record header.
    Mctp,
    /// PCIe DOE: 0-byte sequence number transmitted (implicit counter only).
    Doe,
}

impl SessionTransport {
    pub const fn seq_num_wire_len(self) -> usize {
        match self {
            Self::Mctp => 2,
            Self::Doe => 0,
        }
    }

    pub const fn header_len(self) -> usize {
        4 + self.seq_num_wire_len() + 2
    }
}

/// Lifecycle phase of a [`SecuredSession`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionPhase {
    /// Handshake traffic keys active (only `FINISH` / `FINISH_RSP` permitted).
    Handshake,
    /// Application data traffic keys active (PLDM, HEARTBEAT, KEY_UPDATE, END_SESSION).
    Application,
}

/// Directional traffic key, base IV, and 64-bit monotonic sequence counter.
#[derive(Clone)]
pub struct DirectionKeys {
    pub secret: [u8; HASH_LEN],
    pub key: [u8; AEAD_KEY_LEN],
    pub iv: [u8; AEAD_IV_LEN],
    pub seq_num: u64,
}

impl Drop for DirectionKeys {
    fn drop(&mut self) {
        self.secret.zeroize();
        self.key.zeroize();
        self.iv.zeroize();
    }
}

impl DirectionKeys {
    pub fn derive(version: u8, secret: [u8; HASH_LEN]) -> Result<Self, Error> {
        let mut key = [0u8; AEAD_KEY_LEN];
        let mut iv = [0u8; AEAD_IV_LEN];
        hkdf_expand_spdm(&secret, version, b"key", None, &mut key)?;
        hkdf_expand_spdm(&secret, version, b"iv", None, &mut iv)?;
        Ok(Self {
            secret,
            key,
            iv,
            seq_num: 0,
        })
    }

    /// Ratchet traffic keys for SPDM `KEY_UPDATE` (DSP0277 §6.4).
    pub fn ratchet(&mut self, version: u8) -> Result<(), Error> {
        let mut next_secret = [0u8; HASH_LEN];
        hkdf_expand_spdm(
            &self.secret,
            version,
            b"traffic upd",
            None,
            &mut next_secret,
        )?;
        *self = Self::derive(version, next_secret)?;
        next_secret.zeroize();
        Ok(())
    }
}

/// Active SPDM secured session (handshake or application data phase).
pub struct SecuredSession {
    pub session_id: u32,
    pub version: u8,
    pub transport: SessionTransport,
    pub is_requester: bool,
    pub phase: SessionPhase,
    pub handshake_secret: [u8; HASH_LEN],
    pub req_finished_key: [u8; HASH_LEN],
    pub rsp_finished_key: [u8; HASH_LEN],
    pub req_keys: DirectionKeys,
    pub rsp_keys: DirectionKeys,
    pub active: bool,
}

impl Drop for SecuredSession {
    fn drop(&mut self) {
        self.handshake_secret.zeroize();
        self.req_finished_key.zeroize();
        self.rsp_finished_key.zeroize();
    }
}

impl SecuredSession {
    /// Derive handshake traffic keys and finished keys after `KEY_EXCHANGE_RSP`
    /// (DSP0277 §6.2).
    pub fn from_handshake(
        session_id: u32,
        version: u8,
        transport: SessionTransport,
        is_requester: bool,
        shared_secret: &[u8; 32],
        th1: &[u8; HASH_LEN],
    ) -> Result<Self, Error> {
        let zero_salt = [0u8; HASH_LEN];
        let handshake_secret = hkdf_extract_sha256(&zero_salt, shared_secret);

        let mut req_hs = [0u8; HASH_LEN];
        let mut rsp_hs = [0u8; HASH_LEN];
        hkdf_expand_spdm(
            &handshake_secret,
            version,
            b"req hs data",
            Some(th1),
            &mut req_hs,
        )?;
        hkdf_expand_spdm(
            &handshake_secret,
            version,
            b"rsp hs data",
            Some(th1),
            &mut rsp_hs,
        )?;

        let mut req_finished_key = [0u8; HASH_LEN];
        let mut rsp_finished_key = [0u8; HASH_LEN];
        hkdf_expand_spdm(&req_hs, version, b"finished", None, &mut req_finished_key)?;
        hkdf_expand_spdm(&rsp_hs, version, b"finished", None, &mut rsp_finished_key)?;

        let req_keys = DirectionKeys::derive(version, req_hs)?;
        let rsp_keys = DirectionKeys::derive(version, rsp_hs)?;
        req_hs.zeroize();
        rsp_hs.zeroize();

        Ok(Self {
            session_id,
            version,
            transport,
            is_requester,
            phase: SessionPhase::Handshake,
            handshake_secret,
            req_finished_key,
            rsp_finished_key,
            req_keys,
            rsp_keys,
            active: true,
        })
    }

    /// Transition from handshake keys to application data keys after `FINISH_RSP`
    /// (DSP0277 §6.3 / DSP0274 Table 89).
    pub fn derive_data_keys(&mut self, th2: &[u8; HASH_LEN]) -> Result<(), Error> {
        let mut salt1 = [0u8; HASH_LEN];
        hkdf_expand_spdm(
            &self.handshake_secret,
            self.version,
            b"derived",
            None,
            &mut salt1,
        )?;
        let zero_ikm = [0u8; HASH_LEN];
        let mut master_secret = hkdf_extract_sha256(&salt1, &zero_ikm);
        salt1.zeroize();

        let mut req_data = [0u8; HASH_LEN];
        let mut rsp_data = [0u8; HASH_LEN];
        // DSP0274 Table 89: exact labels are "req app data" and "rsp app data".
        hkdf_expand_spdm(
            &master_secret,
            self.version,
            b"req app data",
            Some(th2),
            &mut req_data,
        )?;
        hkdf_expand_spdm(
            &master_secret,
            self.version,
            b"rsp app data",
            Some(th2),
            &mut rsp_data,
        )?;
        master_secret.zeroize();

        self.req_keys = DirectionKeys::derive(self.version, req_data)?;
        self.rsp_keys = DirectionKeys::derive(self.version, rsp_data)?;
        req_data.zeroize();
        rsp_data.zeroize();
        self.handshake_secret.zeroize();
        self.req_finished_key.zeroize();
        self.rsp_finished_key.zeroize();
        self.phase = SessionPhase::Application;
        Ok(())
    }

    /// Encrypt `app_data` into a DSP0277 secured record in `out`.
    pub fn encrypt(&mut self, app_data: &[u8], out: &mut [u8]) -> Result<usize, Error> {
        if !self.active {
            return Err(Error::SessionInactive);
        }
        let tx = if self.is_requester {
            &mut self.req_keys
        } else {
            &mut self.rsp_keys
        };
        let hdr_len = self.transport.header_len();
        let pt_len = 2 + app_data.len();
        let record_len = pt_len + AEAD_TAG_LEN;
        let total = hdr_len + record_len;
        if out.len() < total || record_len > u16::MAX as usize {
            return Err(Error::BufferTooSmall);
        }

        let seq = tx.seq_num;
        tx.seq_num = seq.checked_add(1).ok_or(Error::SequenceOverflow)?;

        out[0..4].copy_from_slice(&self.session_id.to_le_bytes());
        if self.transport == SessionTransport::Mctp {
            out[4..6].copy_from_slice(&(seq as u16).to_le_bytes());
            out[6..8].copy_from_slice(&(record_len as u16).to_le_bytes());
        } else {
            out[4..6].copy_from_slice(&(record_len as u16).to_le_bytes());
        }

        out[hdr_len..hdr_len + 2].copy_from_slice(&(app_data.len() as u16).to_le_bytes());
        out[hdr_len + 2..hdr_len + pt_len].copy_from_slice(app_data);

        let iv = record_iv(&tx.iv, seq);
        let (aad, rest) = out[..total].split_at_mut(hdr_len);
        let (ct, tag_slot) = rest.split_at_mut(pt_len);
        let tag = aes256_gcm_encrypt_in_place(&tx.key, &iv, aad, ct)?;
        tag_slot.copy_from_slice(&tag);
        Ok(total)
    }

    /// Decrypt and authenticate a DSP0277 secured record into `out`, returning
    /// the decrypted `app_data` length.
    pub fn decrypt(&mut self, record: &[u8], out: &mut [u8]) -> Result<usize, Error> {
        if !self.active {
            return Err(Error::SessionInactive);
        }
        let hdr_len = self.transport.header_len();
        if record.len() < hdr_len + 2 + AEAD_TAG_LEN {
            return Err(Error::ShortBuffer);
        }
        let sid = u32::from_le_bytes([record[0], record[1], record[2], record[3]]);
        if sid != self.session_id {
            return Err(Error::InvalidMessage);
        }
        let rx = if self.is_requester {
            &mut self.rsp_keys
        } else {
            &mut self.req_keys
        };
        let expected_seq = rx.seq_num;
        let rec_len = if self.transport == SessionTransport::Mctp {
            let wire_seq = u16::from_le_bytes([record[4], record[5]]);
            if wire_seq != (expected_seq as u16) {
                return Err(Error::DecryptFailed);
            }
            u16::from_le_bytes([record[6], record[7]]) as usize
        } else {
            u16::from_le_bytes([record[4], record[5]]) as usize
        };

        let expected_len = hdr_len + rec_len;
        let len_ok = if self.transport == SessionTransport::Doe {
            // PCIe DOE pads data objects to 4-byte DW boundaries (0..3 pad bytes).
            record.len() >= expected_len && record.len() - expected_len <= 3
        } else {
            record.len() == expected_len
        };
        if !len_ok || rec_len < 2 + AEAD_TAG_LEN {
            return Err(Error::InvalidMessage);
        }
        let pt_len = rec_len - AEAD_TAG_LEN;
        let aad = &record[..hdr_len];
        let ct_slice = &record[hdr_len..hdr_len + pt_len];
        let mut tag = [0u8; AEAD_TAG_LEN];
        tag.copy_from_slice(&record[hdr_len + pt_len..expected_len]);

        if out.len() < pt_len {
            return Err(Error::BufferTooSmall);
        }
        out[..pt_len].copy_from_slice(ct_slice);
        let iv = record_iv(&rx.iv, expected_seq);
        aes256_gcm_decrypt_in_place(&rx.key, &iv, aad, &mut out[..pt_len], &tag)?;

        rx.seq_num = expected_seq.checked_add(1).ok_or(Error::SequenceOverflow)?;
        let app_len = u16::from_le_bytes([out[0], out[1]]) as usize;
        if 2 + app_len > pt_len {
            return Err(Error::InvalidMessage);
        }
        out.copy_within(2..2 + app_len, 0);
        Ok(app_len)
    }
}
