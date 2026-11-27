//! Sans-IO SPDM 1.2 Responder state machine (DSP0274).

use crate::crypto::{
    EphemeralP256, HASH_LEN, Rng, SIG_LEN, Sha256Transcript, SpdmKeyPair, ct_eq, hmac_sha256,
    sha256, spdm12_signing_digest,
};
use crate::secured::{OPAQUE_SELECTED_VERSION_1_1, SecuredSession, SessionPhase, SessionTransport};
use crate::wire::{
    AlgStructEntry, AlgorithmsResponseBody, CapabilitiesBody, CertificateChainPrefix,
    CertificateResponsePrefix, ChallengeAuthPrefix, DmtfMeasurementBlock, GetCertificateBody,
    KeyExchangeReqPrefix, KeyExchangeRspPrefix, MeasurementsPrefix, MeasurementsSuffix,
    VersionResponsePrefix,
};
use crate::{
    AEAD_AES_256_GCM, BASE_ASYM_ECDSA_ECC_NIST_P256, BASE_HASH_SHA_256, CAP_CERT, CAP_CHAL,
    CAP_ENCRYPT, CAP_HBEAT, CAP_KEY_EX, CAP_KEY_UPD, CAP_MAC, CAP_MEAS_SIG, DHE_SECP256R1, Error,
    KEY_SCHEDULE_SPDM, MEAS_HASH_SHA_256, SPDM_VERSION_1_0, SPDM_VERSION_1_2, SpdmCode,
    SpdmErrorCode, SpdmHeader, write_error,
};
use zerocopy::little_endian::{U16 as LeU16, U32 as LeU32};
use zerocopy::{FromBytes, IntoBytes};

/// A single DMTF measurement block (index 1..=254, SHA-256 digest).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MeasurementBlock {
    pub index: u8,
    pub value_type: u8,
    pub digest: [u8; HASH_LEN],
}

/// Sans-IO SPDM 1.2 Responder.
pub struct SpdmResponder<const MAX_CERT: usize = 512, const MAX_MEAS: usize = 4> {
    version: u8,
    transport: SessionTransport,
    keypair: SpdmKeyPair,
    /// DSP0274 certificate chain buffer: `[length_le16, 0x0000, root_hash(32), cert_der...]`.
    cert_chain: [u8; MAX_CERT],
    cert_chain_len: usize,
    cert_chain_hash: [u8; HASH_LEN],
    measurements: [Option<MeasurementBlock>; MAX_MEAS],
    /// VCA transcript (`GET_VERSION..ALGORITHMS`), cloned into measurement and
    /// key-exchange transcripts per SPDM 1.2.
    vca: Sha256Transcript,
    /// M1/M2 base transcript: VCA || GET_DIGESTS/DIGESTS || GET_CERTIFICATE/CERTIFICATE
    /// per DSP0274 Table 36.
    m_transcript: Sha256Transcript,
    /// Active key-exchange transcript (VCA || cert_hash || KEY_EXCHANGE || KEY_EXCHANGE_RSP).
    kx_transcript: Option<Sha256Transcript>,
    /// Active secured session (created at KEY_EXCHANGE, promoted to data keys at FINISH).
    pub session: Option<SecuredSession>,
    next_rsp_session_id: u16,
}

impl<const MAX_CERT: usize, const MAX_MEAS: usize> SpdmResponder<MAX_CERT, MAX_MEAS> {
    /// Construct a responder with identity `keypair` and a raw certificate blob
    /// (wrapped automatically in the DSP0274 certificate-chain header).
    pub fn new(
        transport: SessionTransport,
        keypair: SpdmKeyPair,
        cert_der: &[u8],
    ) -> Result<Self, Error> {
        let pfx_len = core::mem::size_of::<CertificateChainPrefix>();
        let total = pfx_len + cert_der.len();
        if total > MAX_CERT || total > u16::MAX as usize {
            return Err(Error::BufferTooSmall);
        }
        let mut cert_chain = [0u8; MAX_CERT];
        let pfx = CertificateChainPrefix {
            length: LeU16::new(total as u16),
            reserved: LeU16::new(0),
            root_hash: sha256(cert_der),
        };
        pfx.write_to_prefix(&mut cert_chain)
            .map_err(|_| Error::BufferTooSmall)?;
        cert_chain[pfx_len..total].copy_from_slice(cert_der);
        let cert_chain_hash = sha256(&cert_chain[..total]);

        Ok(Self {
            version: SPDM_VERSION_1_2,
            transport,
            keypair,
            cert_chain,
            cert_chain_len: total,
            cert_chain_hash,
            measurements: [None; MAX_MEAS],
            vca: Sha256Transcript::new(),
            m_transcript: Sha256Transcript::new(),
            kx_transcript: None,
            session: None,
            next_rsp_session_id: 0x0001,
        })
    }

    pub fn add_measurement(&mut self, block: MeasurementBlock) -> Result<(), Error> {
        for slot in &mut self.measurements {
            if slot.is_none() {
                *slot = Some(block);
                return Ok(());
            }
        }
        Err(Error::BufferTooSmall)
    }

    pub fn cert_chain(&self) -> &[u8] {
        &self.cert_chain[..self.cert_chain_len]
    }

    /// Process a plaintext SPDM request (`MessageType::Spdm`) and write the
    /// response into `resp`.
    pub fn handle_request<R: Rng>(
        &mut self,
        req: &[u8],
        resp: &mut [u8],
        rng: &mut R,
    ) -> Result<usize, Error> {
        let (hdr, body) = SpdmHeader::parse(req)?;
        match hdr.code {
            x if x == SpdmCode::GetVersion as u8 => self.on_get_version(req, hdr, resp),
            x if x == SpdmCode::GetCapabilities as u8 => {
                self.on_get_capabilities(req, hdr, body, resp)
            }
            x if x == SpdmCode::NegotiateAlgorithms as u8 => {
                self.on_negotiate_algorithms(req, hdr, body, resp)
            }
            x if x == SpdmCode::GetDigests as u8 => self.on_get_digests(req, hdr, resp),
            x if x == SpdmCode::GetCertificate as u8 => {
                self.on_get_certificate(req, hdr, body, resp)
            }
            x if x == SpdmCode::Challenge as u8 => self.on_challenge(req, hdr, body, resp, rng),
            x if x == SpdmCode::GetMeasurements as u8 => {
                self.on_get_measurements(req, hdr, body, resp, rng)
            }
            x if x == SpdmCode::KeyExchange as u8 => {
                self.on_key_exchange(req, hdr, body, resp, rng)
            }
            _ => write_error(self.version, SpdmErrorCode::UnsupportedRequest, 0, resp),
        }
    }

    /// Process an encrypted SPDM session message (e.g. `FINISH`, `HEARTBEAT`,
    /// `KEY_UPDATE`, `END_SESSION`) carried inside a DSP0277 secured record.
    ///
    /// For `SessionTransport::Mctp`, the inner decrypted plaintext starts with
    /// the MCTP message type byte (`0x05` for SPDM); for `SessionTransport::Doe`,
    /// it starts directly with the SPDM header.
    pub fn handle_secured_spdm(
        &mut self,
        secured_req: &[u8],
        secured_resp: &mut [u8],
    ) -> Result<usize, Error> {
        let mut pt_in = [0u8; 512];
        let (pt_len, used_pending_req) = {
            let sess = self.session.as_mut().ok_or(Error::SessionInactive)?;
            sess.decrypt_trying_pending(secured_req, &mut pt_in)?
        };
        if used_pending_req && let Some(sess) = self.session.as_mut() {
            sess.commit_req_key()?;
            sess.commit_rsp_key_if_pending();
        }
        let prefix_len = usize::from(self.transport == SessionTransport::Mctp);
        if pt_len < prefix_len + SpdmHeader::LEN {
            return Err(Error::ShortBuffer);
        }
        if prefix_len == 1 && (pt_in[0] & 0x7F) != crate::MCTP_MSG_TYPE_SPDM {
            return Err(Error::InvalidMessage);
        }
        let spdm_req = &pt_in[prefix_len..pt_len];
        let (hdr, body) = SpdmHeader::parse(spdm_req)?;
        let phase = self.session.as_ref().ok_or(Error::SessionInactive)?.phase;

        let mut pt_out = [0u8; 512];
        if prefix_len == 1 {
            pt_out[0] = crate::MCTP_MSG_TYPE_SPDM;
        }
        let spdm_out = &mut pt_out[prefix_len..];

        let mut stage_key_update: Option<bool> = None;
        let mut encrypt_with_pending_rsp = false;
        let mut promote_th2: Option<[u8; HASH_LEN]> = None;
        let mut close_session = false;

        let rsp_len = match hdr.code {
            x if x == SpdmCode::Finish as u8 => {
                if phase != SessionPhase::Handshake {
                    return Err(Error::InvalidMessage);
                }
                let (n, th2) = self.on_finish(spdm_req, hdr, body, spdm_out)?;
                promote_th2 = Some(th2);
                n
            }
            x if x == SpdmCode::Heartbeat as u8 => {
                if phase != SessionPhase::Application {
                    return Err(Error::SessionInactive);
                }
                if spdm_out.len() < 4 {
                    return Err(Error::BufferTooSmall);
                }
                spdm_out[..4].copy_from_slice(&[self.version, SpdmCode::HeartbeatAck as u8, 0, 0]);
                4
            }
            x if x == SpdmCode::KeyUpdate as u8 => {
                if phase != SessionPhase::Application {
                    return Err(Error::SessionInactive);
                }
                // DSP0274 §10.21.1:
                // param1: 1 = UpdateKey (stage req), 2 = UpdateAllKeys (stage req + rsp,
                // encrypt ACK with pending rsp), 3 = VerifyNewKey (must decrypt under pending req).
                match hdr.param1 {
                    1 => {
                        stage_key_update = Some(false);
                        if spdm_out.len() < 4 {
                            return Err(Error::BufferTooSmall);
                        }
                        spdm_out[..4].copy_from_slice(&[
                            self.version,
                            SpdmCode::KeyUpdateAck as u8,
                            hdr.param1,
                            hdr.param2,
                        ]);
                        4
                    }
                    2 => {
                        stage_key_update = Some(true);
                        encrypt_with_pending_rsp = true;
                        if spdm_out.len() < 4 {
                            return Err(Error::BufferTooSmall);
                        }
                        spdm_out[..4].copy_from_slice(&[
                            self.version,
                            SpdmCode::KeyUpdateAck as u8,
                            hdr.param1,
                            hdr.param2,
                        ]);
                        4
                    }
                    3 => {
                        if !used_pending_req {
                            write_error(self.version, SpdmErrorCode::InvalidRequest, 0, spdm_out)?
                        } else {
                            if spdm_out.len() < 4 {
                                return Err(Error::BufferTooSmall);
                            }
                            spdm_out[..4].copy_from_slice(&[
                                self.version,
                                SpdmCode::KeyUpdateAck as u8,
                                hdr.param1,
                                hdr.param2,
                            ]);
                            4
                        }
                    }
                    _ => write_error(self.version, SpdmErrorCode::InvalidRequest, 0, spdm_out)?,
                }
            }
            x if x == SpdmCode::EndSession as u8 => {
                if phase != SessionPhase::Application {
                    return Err(Error::SessionInactive);
                }
                close_session = true;
                if spdm_out.len() < 4 {
                    return Err(Error::BufferTooSmall);
                }
                spdm_out[..4].copy_from_slice(&[self.version, SpdmCode::EndSessionAck as u8, 0, 0]);
                4
            }
            _ => write_error(self.version, SpdmErrorCode::UnsupportedRequest, 0, spdm_out)?,
        };

        let enc_len = {
            let sess = self.session.as_mut().ok_or(Error::SessionInactive)?;
            if let Some(update_all) = stage_key_update {
                sess.stage_key_update(update_all)?;
            }
            // DSP0274 §10.21.1: UpdateAllKeys encrypts KEY_UPDATE_ACK with the staged new rsp key.
            let n = if encrypt_with_pending_rsp {
                sess.encrypt_pending(&pt_out[..prefix_len + rsp_len], secured_resp)?
            } else {
                sess.encrypt(&pt_out[..prefix_len + rsp_len], secured_resp)?
            };
            if let Some(th2) = promote_th2 {
                sess.derive_data_keys(&th2)?;
            }
            if close_session {
                sess.active = false;
            }
            n
        };
        if close_session {
            self.session = None;
        }
        Ok(enc_len)
    }

    fn on_get_version(
        &mut self,
        req: &[u8],
        hdr: SpdmHeader,
        resp: &mut [u8],
    ) -> Result<usize, Error> {
        if hdr.version != SPDM_VERSION_1_0 {
            return write_error(SPDM_VERSION_1_0, SpdmErrorCode::VersionMismatch, 0, resp);
        }
        let pfx_off = SpdmHeader::LEN;
        let ver_off = pfx_off + core::mem::size_of::<VersionResponsePrefix>();
        let total = ver_off + 2;
        if resp.len() < total {
            return Err(Error::BufferTooSmall);
        }
        // Reset VCA, M1/M2 transcript, and any active session on GET_VERSION (DSP0274 §10.2).
        self.vca = Sha256Transcript::new();
        self.vca.update(req);
        self.kx_transcript = None;
        self.session = None;

        SpdmHeader::new(SPDM_VERSION_1_0, SpdmCode::Version, 0, 0)
            .write_to_prefix(resp)
            .map_err(|_| Error::BufferTooSmall)?;
        let pfx = VersionResponsePrefix {
            reserved: 0,
            version_number_entry_count: 1,
        };
        pfx.write_to_prefix(&mut resp[pfx_off..])
            .map_err(|_| Error::BufferTooSmall)?;
        // Version 1.2.0.0 encoded as u16 LE: 0x1200 -> [0x00, 0x12]
        LeU16::new(0x1200)
            .write_to_prefix(&mut resp[ver_off..])
            .map_err(|_| Error::BufferTooSmall)?;
        self.vca.update(&resp[..total]);
        self.m_transcript = self.vca.clone();
        Ok(total)
    }

    fn on_get_capabilities(
        &mut self,
        req: &[u8],
        hdr: SpdmHeader,
        body: &[u8],
        resp: &mut [u8],
    ) -> Result<usize, Error> {
        if hdr.version != self.version || body.len() < core::mem::size_of::<CapabilitiesBody>() {
            return write_error(self.version, SpdmErrorCode::InvalidRequest, 0, resp);
        }
        let total = SpdmHeader::LEN + core::mem::size_of::<CapabilitiesBody>();
        if resp.len() < total {
            return Err(Error::BufferTooSmall);
        }
        self.vca.update(req);

        let caps = CAP_CERT
            | CAP_CHAL
            | CAP_MEAS_SIG
            | CAP_ENCRYPT
            | CAP_MAC
            | CAP_KEY_EX
            | CAP_HBEAT
            | CAP_KEY_UPD;
        SpdmHeader::new(self.version, SpdmCode::Capabilities, 0, 0)
            .write_to_prefix(resp)
            .map_err(|_| Error::BufferTooSmall)?;
        let out_body = CapabilitiesBody {
            reserved: 0,
            ct_exponent: 20, // 2^20 us
            reserved2: LeU16::new(0),
            flags: LeU32::new(caps),
            data_transfer_size: LeU32::new(4096),
            max_spdm_msg_size: LeU32::new(4096),
        };
        out_body
            .write_to_prefix(&mut resp[SpdmHeader::LEN..])
            .map_err(|_| Error::BufferTooSmall)?;
        self.vca.update(&resp[..total]);
        Ok(total)
    }

    fn on_negotiate_algorithms(
        &mut self,
        req: &[u8],
        hdr: SpdmHeader,
        body: &[u8],
        resp: &mut [u8],
    ) -> Result<usize, Error> {
        if hdr.version != self.version || body.len() < 28 {
            return write_error(self.version, SpdmErrorCode::InvalidRequest, 0, resp);
        }
        let algs = [
            AlgStructEntry {
                alg_type: 0x02,
                alg_count: 0x20,
                alg_supported: LeU16::new(DHE_SECP256R1),
            },
            AlgStructEntry {
                alg_type: 0x03,
                alg_count: 0x20,
                alg_supported: LeU16::new(AEAD_AES_256_GCM),
            },
            AlgStructEntry {
                alg_type: 0x05,
                alg_count: 0x20,
                alg_supported: LeU16::new(KEY_SCHEDULE_SPDM),
            },
        ];
        let algs_off = SpdmHeader::LEN + core::mem::size_of::<AlgorithmsResponseBody>();
        let total = algs_off + core::mem::size_of_val(&algs);
        if resp.len() < total {
            return Err(Error::BufferTooSmall);
        }
        self.vca.update(req);

        SpdmHeader::new(self.version, SpdmCode::Algorithms, algs.len() as u8, 0)
            .write_to_prefix(resp)
            .map_err(|_| Error::BufferTooSmall)?;
        let out_body = AlgorithmsResponseBody {
            length: LeU16::new(total as u16),
            measurement_spec_sel: 0x01, // DMTF
            other_params_sel: 0x02,     // OpaqueDataFmt1
            measurement_hash_algo: LeU32::new(MEAS_HASH_SHA_256),
            base_asym_sel: LeU32::new(BASE_ASYM_ECDSA_ECC_NIST_P256),
            base_hash_sel: LeU32::new(BASE_HASH_SHA_256),
            reserved: [0; 12],
            ext_asym_sel_count: 0,
            ext_hash_sel_count: 0,
            reserved2: LeU16::new(0),
        };
        out_body
            .write_to_prefix(&mut resp[SpdmHeader::LEN..])
            .map_err(|_| Error::BufferTooSmall)?;
        algs.write_to_prefix(&mut resp[algs_off..])
            .map_err(|_| Error::BufferTooSmall)?;

        self.vca.update(&resp[..total]);
        self.m_transcript = self.vca.clone();
        Ok(total)
    }

    fn on_get_digests(
        &mut self,
        req: &[u8],
        hdr: SpdmHeader,
        resp: &mut [u8],
    ) -> Result<usize, Error> {
        if hdr.version != self.version {
            return write_error(self.version, SpdmErrorCode::InvalidRequest, 0, resp);
        }
        let total = SpdmHeader::LEN + HASH_LEN;
        if resp.len() < total {
            return Err(Error::BufferTooSmall);
        }
        // param2 = slot mask (slot 0 populated = 0x01)
        SpdmHeader::new(self.version, SpdmCode::Digests, 0, 0x01)
            .write_to_prefix(resp)
            .map_err(|_| Error::BufferTooSmall)?;
        resp[SpdmHeader::LEN..total].copy_from_slice(&self.cert_chain_hash);
        // DSP0274 Table 36: M1/M2 includes GET_DIGESTS and DIGESTS.
        self.m_transcript.update(req);
        self.m_transcript.update(&resp[..total]);
        Ok(total)
    }

    fn on_get_certificate(
        &mut self,
        req: &[u8],
        hdr: SpdmHeader,
        body: &[u8],
        resp: &mut [u8],
    ) -> Result<usize, Error> {
        let Ok((req_body, _)) = GetCertificateBody::ref_from_prefix(body) else {
            return write_error(self.version, SpdmErrorCode::InvalidRequest, 0, resp);
        };
        if hdr.version != self.version || hdr.param1 != 0 {
            return write_error(self.version, SpdmErrorCode::InvalidRequest, 0, resp);
        }
        let offset = req_body.offset.get() as usize;
        let length = req_body.length.get() as usize;
        if offset > self.cert_chain_len {
            return write_error(self.version, SpdmErrorCode::InvalidRequest, 0, resp);
        }
        let remaining = self.cert_chain_len - offset;
        let portion = remaining.min(length);
        let remainder = remaining - portion;
        let data_off = SpdmHeader::LEN + core::mem::size_of::<CertificateResponsePrefix>();
        let total = data_off + portion;
        if resp.len() < total {
            return Err(Error::BufferTooSmall);
        }
        SpdmHeader::new(self.version, SpdmCode::Certificate, 0, 0)
            .write_to_prefix(resp)
            .map_err(|_| Error::BufferTooSmall)?;
        let pfx = CertificateResponsePrefix {
            portion_length: LeU16::new(portion as u16),
            remainder_length: LeU16::new(remainder as u16),
        };
        pfx.write_to_prefix(&mut resp[SpdmHeader::LEN..])
            .map_err(|_| Error::BufferTooSmall)?;
        resp[data_off..total].copy_from_slice(&self.cert_chain[offset..offset + portion]);
        // DSP0274 Table 36: M1/M2 includes every GET_CERTIFICATE / CERTIFICATE pair.
        self.m_transcript.update(req);
        self.m_transcript.update(&resp[..total]);
        Ok(total)
    }

    fn on_challenge<R: Rng>(
        &self,
        req: &[u8],
        hdr: SpdmHeader,
        body: &[u8],
        resp: &mut [u8],
        rng: &mut R,
    ) -> Result<usize, Error> {
        if hdr.version != self.version || body.len() < 32 || hdr.param1 != 0 {
            return write_error(self.version, SpdmErrorCode::InvalidRequest, 0, resp);
        }
        let unsigned_len = SpdmHeader::LEN + core::mem::size_of::<ChallengeAuthPrefix>();
        let total = unsigned_len + SIG_LEN;
        if resp.len() < total {
            return Err(Error::BufferTooSmall);
        }
        SpdmHeader::new(self.version, SpdmCode::ChallengeAuth, 0x00, 0x01)
            .write_to_prefix(resp)
            .map_err(|_| Error::BufferTooSmall)?;
        let mut pfx = ChallengeAuthPrefix {
            cert_chain_hash: self.cert_chain_hash,
            nonce: [0; 32],
            opaque_length: LeU16::new(0),
        };
        rng.fill_bytes(&mut pfx.nonce);
        pfx.write_to_prefix(&mut resp[SpdmHeader::LEN..])
            .map_err(|_| Error::BufferTooSmall)?;

        let mut t = self.m_transcript.clone();
        t.update(req);
        t.update(&resp[..unsigned_len]);
        let m1_hash = t.finalize_clone();
        let sign_digest =
            spdm12_signing_digest(self.version, b"responder-challenge_auth signing", &m1_hash);
        let sig = self.keypair.sign_prehash(&sign_digest)?;
        resp[unsigned_len..total].copy_from_slice(&sig);
        Ok(total)
    }

    fn on_get_measurements<R: Rng>(
        &self,
        req: &[u8],
        hdr: SpdmHeader,
        _body: &[u8],
        resp: &mut [u8],
        rng: &mut R,
    ) -> Result<usize, Error> {
        if hdr.version != self.version {
            return write_error(self.version, SpdmErrorCode::InvalidRequest, 0, resp);
        }
        let signed = (hdr.param1 & 0x01) != 0;
        let count = self.measurements.iter().flatten().count();
        let block_len = core::mem::size_of::<DmtfMeasurementBlock>();
        let record_len = count * block_len;
        let blocks_off = SpdmHeader::LEN + core::mem::size_of::<MeasurementsPrefix>();
        let unsigned_len = blocks_off + record_len + core::mem::size_of::<MeasurementsSuffix>();
        let total = unsigned_len + if signed { SIG_LEN } else { 0 };
        if resp.len() < total {
            return Err(Error::BufferTooSmall);
        }

        SpdmHeader::new(self.version, SpdmCode::Measurements, 0, 0)
            .write_to_prefix(resp)
            .map_err(|_| Error::BufferTooSmall)?;
        let pfx = MeasurementsPrefix::new(count as u8, record_len);
        pfx.write_to_prefix(&mut resp[SpdmHeader::LEN..])
            .map_err(|_| Error::BufferTooSmall)?;

        let mut pos = blocks_off;
        for block in self.measurements.iter().flatten() {
            let wire = DmtfMeasurementBlock {
                index: block.index,
                spec: 0x01, // DMTF measurement spec
                size: LeU16::new((3 + HASH_LEN) as u16),
                value_type: block.value_type,
                value_size: LeU16::new(HASH_LEN as u16),
                digest: block.digest,
            };
            wire.write_to_prefix(&mut resp[pos..])
                .map_err(|_| Error::BufferTooSmall)?;
            pos += block_len;
        }
        let mut suffix = MeasurementsSuffix {
            nonce: [0; 32],
            opaque_length: LeU16::new(0),
        };
        rng.fill_bytes(&mut suffix.nonce);
        suffix
            .write_to_prefix(&mut resp[pos..])
            .map_err(|_| Error::BufferTooSmall)?;
        pos += core::mem::size_of::<MeasurementsSuffix>();

        if signed {
            if req.len() < 37 {
                return write_error(self.version, SpdmErrorCode::InvalidRequest, 0, resp);
            }
            // SPDM 1.2 includes VCA in L1/L2 measurement transcript!
            // Strip any 1..3 trailing PCIe DOE DW-alignment pad bytes from req (37B).
            let mut t = self.vca.clone();
            t.update(&req[..37]);
            t.update(&resp[..pos]);
            let l1_hash = t.finalize_clone();
            let sign_digest =
                spdm12_signing_digest(self.version, b"responder-measurements signing", &l1_hash);
            let sig = self.keypair.sign_prehash(&sign_digest)?;
            resp[pos..pos + SIG_LEN].copy_from_slice(&sig);
            pos += SIG_LEN;
        }
        Ok(pos)
    }

    fn on_key_exchange<R: Rng>(
        &mut self,
        req: &[u8],
        hdr: SpdmHeader,
        body: &[u8],
        resp: &mut [u8],
        rng: &mut R,
    ) -> Result<usize, Error> {
        // Enforce single-session limit.
        if self.session.is_some() {
            return write_error(self.version, SpdmErrorCode::Busy, 0, resp);
        }
        let Ok((req_pfx, req_rest)) = KeyExchangeReqPrefix::ref_from_prefix(body) else {
            return write_error(self.version, SpdmErrorCode::InvalidRequest, 0, resp);
        };
        if hdr.version != self.version {
            return write_error(self.version, SpdmErrorCode::InvalidRequest, 0, resp);
        }
        let req_opaque_len = req_pfx.opaque_length.get() as usize;
        if req_rest.len() < req_opaque_len
            || !crate::secured::opaque_offers_version_1_1(&req_rest[..req_opaque_len])
        {
            return write_error(self.version, SpdmErrorCode::InvalidRequest, 0, resp);
        }

        let eph = EphemeralP256::generate(rng)?;
        let shared_secret = eph.diffie_hellman(&req_pfx.exchange_data)?;

        let rsp_session_id = self.next_rsp_session_id;
        self.next_rsp_session_id = self.next_rsp_session_id.wrapping_add(1).max(1);
        // DSP0274 §10.12: SessionID = (rsp_session_id << 16) | req_session_id
        let session_id =
            (u32::from(rsp_session_id) << 16) | u32::from(req_pfx.req_session_id.get());

        let opaque = &OPAQUE_SELECTED_VERSION_1_1;
        let opaque_off = SpdmHeader::LEN + core::mem::size_of::<KeyExchangeRspPrefix>();
        let before_sig = opaque_off + opaque.len();
        let before_verify = before_sig + SIG_LEN;
        let total = before_verify + HASH_LEN;
        if resp.len() < total {
            return Err(Error::BufferTooSmall);
        }

        SpdmHeader::new(self.version, SpdmCode::KeyExchangeRsp, 0, 0)
            .write_to_prefix(resp)
            .map_err(|_| Error::BufferTooSmall)?;
        let mut rsp_pfx = KeyExchangeRspPrefix {
            rsp_session_id: LeU16::new(rsp_session_id),
            mut_auth_requested: 0,
            slot_id_param: 0,
            random_data: [0; 32],
            exchange_data: *eph.public_xy(),
            opaque_length: LeU16::new(opaque.len() as u16),
        };
        rng.fill_bytes(&mut rsp_pfx.random_data);
        rsp_pfx
            .write_to_prefix(&mut resp[SpdmHeader::LEN..])
            .map_err(|_| Error::BufferTooSmall)?;
        resp[opaque_off..before_sig].copy_from_slice(opaque);

        // Transcript up to signature: VCA || cert_chain_hash || KEY_EXCHANGE || KEY_EXCHANGE_RSP[..before_sig]
        // Strip any 1..3 trailing PCIe DOE DW-alignment pad bytes from req.
        let req_exact =
            SpdmHeader::LEN + core::mem::size_of::<KeyExchangeReqPrefix>() + req_opaque_len;
        let mut t = self.vca.clone();
        t.update(&self.cert_chain_hash);
        t.update(&req[..req_exact]);
        t.update(&resp[..before_sig]);
        let th1_pre_sig = t.finalize_clone();
        let sign_digest = spdm12_signing_digest(
            self.version,
            b"responder-key_exchange_rsp signing",
            &th1_pre_sig,
        );
        let sig = self.keypair.sign_prehash(&sign_digest)?;
        resp[before_sig..before_verify].copy_from_slice(&sig);

        t.update(&sig);
        let th1 = t.finalize_clone();
        let sess = SecuredSession::from_handshake(
            session_id,
            self.version,
            self.transport,
            false,
            &shared_secret,
            &th1,
        )?;
        let verify_data = hmac_sha256(&sess.rsp_finished_key, &th1)?;
        resp[before_verify..total].copy_from_slice(&verify_data);
        t.update(&verify_data);

        self.kx_transcript = Some(t);
        self.session = Some(sess);
        Ok(total)
    }

    fn on_finish(
        &mut self,
        req: &[u8],
        hdr: SpdmHeader,
        body: &[u8],
        resp: &mut [u8],
    ) -> Result<(usize, [u8; HASH_LEN]), Error> {
        if hdr.version != self.version || body.len() < HASH_LEN {
            return Err(Error::InvalidMessage);
        }
        let mut t = self.kx_transcript.take().ok_or(Error::SessionInactive)?;
        // FINISH request = header(4) + VerifyData(32)
        t.update(&req[..4]);
        let th_req_fin = t.finalize_clone();

        let sess = self.session.as_ref().ok_or(Error::SessionInactive)?;
        let expected_req_verify = hmac_sha256(&sess.req_finished_key, &th_req_fin)?;
        if !ct_eq(&body[..HASH_LEN], &expected_req_verify) {
            return Err(Error::VerifyDataMismatch);
        }
        t.update(&body[..HASH_LEN]);

        // DSP0274 Table 60: when HANDSHAKE_IN_THE_CLEAR_CAP is not set,
        // encrypted FINISH_RSP is 4 bytes only (no ResponderVerifyData).
        if resp.len() < 4 {
            return Err(Error::BufferTooSmall);
        }
        resp[0..4].copy_from_slice(&[self.version, SpdmCode::FinishRsp as u8, 0, 0]);
        t.update(&resp[..4]);

        let th2 = t.finalize_clone();
        Ok((4, th2))
    }
}
