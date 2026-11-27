//! Sans-IO SPDM 1.2 Responder state machine (DSP0274).

use crate::crypto::{
    EphemeralP256, HASH_LEN, P256_PUB_LEN, Rng, SIG_LEN, Sha256Transcript, SpdmKeyPair, ct_eq,
    hmac_sha256, sha256, spdm12_signing_digest,
};
use crate::secured::{OPAQUE_SELECTED_VERSION_1_1, SecuredSession, SessionPhase, SessionTransport};
use crate::{
    AEAD_AES_256_GCM, BASE_ASYM_ECDSA_ECC_NIST_P256, BASE_HASH_SHA_256, CAP_CERT, CAP_CHAL,
    CAP_ENCRYPT, CAP_HBEAT, CAP_KEY_EX, CAP_KEY_UPD, CAP_MAC, CAP_MEAS_SIG, DHE_SECP256R1, Error,
    KEY_SCHEDULE_SPDM, MEAS_HASH_SHA_256, SPDM_VERSION_1_0, SPDM_VERSION_1_2, SpdmCode,
    SpdmErrorCode, SpdmHeader, write_error,
};

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
        let total = 4 + HASH_LEN + cert_der.len();
        if total > MAX_CERT || total > u16::MAX as usize {
            return Err(Error::BufferTooSmall);
        }
        let mut cert_chain = [0u8; MAX_CERT];
        cert_chain[0..2].copy_from_slice(&(total as u16).to_le_bytes());
        cert_chain[2..4].copy_from_slice(&0u16.to_le_bytes());
        let root_hash = sha256(cert_der);
        cert_chain[4..4 + HASH_LEN].copy_from_slice(&root_hash);
        cert_chain[4 + HASH_LEN..total].copy_from_slice(cert_der);
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
        let pt_len = {
            let sess = self.session.as_mut().ok_or(Error::SessionInactive)?;
            sess.decrypt(secured_req, &mut pt_in)?
        };
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

        let mut ratchet_req = false;
        let mut ratchet_rsp_before_ack = false;
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
                // param1: 1 = UpdateKey (req only), 2 = UpdateAllKeys (req + rsp BEFORE ACK),
                // 3 = VerifyNewKey.
                match hdr.param1 {
                    1 => ratchet_req = true,
                    2 => {
                        ratchet_req = true;
                        ratchet_rsp_before_ack = true;
                    }
                    3 => {}
                    _ => {
                        return write_error(
                            self.version,
                            SpdmErrorCode::InvalidRequest,
                            0,
                            secured_resp,
                        );
                    }
                }
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
            if ratchet_req {
                sess.req_keys.ratchet(self.version)?;
            }
            // DSP0274 §10.21.1: UpdateAllKeys encrypts KEY_UPDATE_ACK with the NEW rsp key.
            if ratchet_rsp_before_ack {
                sess.rsp_keys.ratchet(self.version)?;
            }
            let n = sess.encrypt(&pt_out[..prefix_len + rsp_len], secured_resp)?;
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
        if resp.len() < 8 {
            return Err(Error::BufferTooSmall);
        }
        // Reset VCA, M1/M2 transcript, and any active session on GET_VERSION (DSP0274 §10.2).
        self.vca = Sha256Transcript::new();
        self.vca.update(req);
        self.kx_transcript = None;
        self.session = None;

        resp[0..4].copy_from_slice(&[SPDM_VERSION_1_0, SpdmCode::Version as u8, 0, 0]);
        resp[4] = 0; // reserved
        resp[5] = 1; // version_number_entry_count = 1
        // Version 1.2.0.0 encoded as u16 LE: 0x1200 -> [0x00, 0x12]
        resp[6..8].copy_from_slice(&0x1200u16.to_le_bytes());
        self.vca.update(&resp[..8]);
        self.m_transcript = self.vca.clone();
        Ok(8)
    }

    fn on_get_capabilities(
        &mut self,
        req: &[u8],
        hdr: SpdmHeader,
        body: &[u8],
        resp: &mut [u8],
    ) -> Result<usize, Error> {
        if hdr.version != self.version || body.len() < 16 {
            return write_error(self.version, SpdmErrorCode::InvalidRequest, 0, resp);
        }
        if resp.len() < 20 {
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
        resp[0..4].copy_from_slice(&[self.version, SpdmCode::Capabilities as u8, 0, 0]);
        resp[4] = 0; // reserved
        resp[5] = 20; // CTExponent (2^20 us)
        resp[6..8].copy_from_slice(&0u16.to_le_bytes());
        resp[8..12].copy_from_slice(&caps.to_le_bytes());
        resp[12..16].copy_from_slice(&4096u32.to_le_bytes()); // DataTransferSize
        resp[16..20].copy_from_slice(&4096u32.to_le_bytes()); // MaxSPDMmsgSize
        self.vca.update(&resp[..20]);
        Ok(20)
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
        if resp.len() < 48 {
            return Err(Error::BufferTooSmall);
        }
        self.vca.update(req);

        // Minimal SPDM 1.2 ALGORITHMS response (36B base + 3x4B AlgStruct = 48B).
        resp[0..4].copy_from_slice(&[self.version, SpdmCode::Algorithms as u8, 3, 0]);
        resp[4..6].copy_from_slice(&48u16.to_le_bytes()); // Length
        resp[6] = 0x01; // MeasurementSpecificationSel = DMTF
        resp[7] = 0x02; // OtherParamsSupport = OpaqueDataFmt1
        resp[8..12].copy_from_slice(&MEAS_HASH_SHA_256.to_le_bytes());
        resp[12..16].copy_from_slice(&BASE_ASYM_ECDSA_ECC_NIST_P256.to_le_bytes());
        resp[16..20].copy_from_slice(&BASE_HASH_SHA_256.to_le_bytes());
        resp[20..32].fill(0); // reserved
        resp[32] = 0; // ExtAsymSelCount
        resp[33] = 0; // ExtHashSelCount
        resp[34..36].copy_from_slice(&0u16.to_le_bytes());
        // AlgStruct 1: DHE = secp256r1
        resp[36] = 0x02;
        resp[37] = 0x20;
        resp[38..40].copy_from_slice(&DHE_SECP256R1.to_le_bytes());
        // AlgStruct 2: AEAD = AES-256-GCM
        resp[40] = 0x03;
        resp[41] = 0x20;
        resp[42..44].copy_from_slice(&AEAD_AES_256_GCM.to_le_bytes());
        // AlgStruct 3: KeySchedule = SPDM
        resp[44] = 0x05;
        resp[45] = 0x20;
        resp[46..48].copy_from_slice(&KEY_SCHEDULE_SPDM.to_le_bytes());

        self.vca.update(&resp[..48]);
        self.m_transcript = self.vca.clone();
        Ok(48)
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
        let total = 4 + HASH_LEN;
        if resp.len() < total {
            return Err(Error::BufferTooSmall);
        }
        // param2 = slot mask (slot 0 populated = 0x01)
        resp[0..4].copy_from_slice(&[self.version, SpdmCode::Digests as u8, 0, 0x01]);
        resp[4..total].copy_from_slice(&self.cert_chain_hash);
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
        if hdr.version != self.version || body.len() < 4 || hdr.param1 != 0 {
            return write_error(self.version, SpdmErrorCode::InvalidRequest, 0, resp);
        }
        let offset = u16::from_le_bytes([body[0], body[1]]) as usize;
        let length = u16::from_le_bytes([body[2], body[3]]) as usize;
        if offset > self.cert_chain_len {
            return write_error(self.version, SpdmErrorCode::InvalidRequest, 0, resp);
        }
        let remaining = self.cert_chain_len - offset;
        let portion = remaining.min(length);
        let remainder = remaining - portion;
        let total = 8 + portion;
        if resp.len() < total {
            return Err(Error::BufferTooSmall);
        }
        resp[0..4].copy_from_slice(&[self.version, SpdmCode::Certificate as u8, 0, 0]);
        resp[4..6].copy_from_slice(&(portion as u16).to_le_bytes());
        resp[6..8].copy_from_slice(&(remainder as u16).to_le_bytes());
        resp[8..total].copy_from_slice(&self.cert_chain[offset..offset + portion]);
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
        // CHALLENGE_AUTH (no measurement summary hash):
        // header(4) + cert_chain_hash(32) + nonce(32) + opaque_len(2=0) + sig(64) = 134 bytes
        let unsigned_len = 4 + HASH_LEN + 32 + 2;
        let total = unsigned_len + SIG_LEN;
        if resp.len() < total {
            return Err(Error::BufferTooSmall);
        }
        resp[0..4].copy_from_slice(&[self.version, SpdmCode::ChallengeAuth as u8, 0x00, 0x01]);
        resp[4..36].copy_from_slice(&self.cert_chain_hash);
        rng.fill_bytes(&mut resp[36..68]);
        resp[68..70].copy_from_slice(&0u16.to_le_bytes()); // opaque_data_len = 0

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
        // Each DMTF block: index(1) + spec(1=1) + size(2=35) + dmtf_type(1) + dmtf_size(2=32) + digest(32) = 39B
        const BLOCK_LEN: usize = 39;
        let record_len = count * BLOCK_LEN;
        let unsigned_len = 8 + record_len + 32 + 2;
        let total = unsigned_len + if signed { SIG_LEN } else { 0 };
        if resp.len() < total {
            return Err(Error::BufferTooSmall);
        }

        resp[0..4].copy_from_slice(&[self.version, SpdmCode::Measurements as u8, 0, 0]);
        resp[4] = count as u8;
        let rlen_bytes = (record_len as u32).to_le_bytes();
        resp[5..8].copy_from_slice(&rlen_bytes[..3]);

        let mut pos = 8;
        for block in self.measurements.iter().flatten() {
            resp[pos] = block.index;
            resp[pos + 1] = 0x01; // DMTF measurement spec
            resp[pos + 2..pos + 4].copy_from_slice(&35u16.to_le_bytes());
            resp[pos + 4] = block.value_type;
            resp[pos + 5..pos + 7].copy_from_slice(&(HASH_LEN as u16).to_le_bytes());
            resp[pos + 7..pos + 39].copy_from_slice(&block.digest);
            pos += BLOCK_LEN;
        }
        rng.fill_bytes(&mut resp[pos..pos + 32]); // responder nonce
        pos += 32;
        resp[pos..pos + 2].copy_from_slice(&0u16.to_le_bytes()); // opaque_len = 0
        pos += 2;

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
        // KEY_EXCHANGE body: ReqSessionID(2) + SessionPolicy(1) + Reserved(1) +
        // RandomData(32) + ExchangeData(64) + OpaqueLen(2) + Opaque(...)
        if hdr.version != self.version || body.len() < 102 {
            return write_error(self.version, SpdmErrorCode::InvalidRequest, 0, resp);
        }
        let req_opaque_len = u16::from_le_bytes([body[100], body[101]]) as usize;
        if body.len() < 102 + req_opaque_len {
            return write_error(self.version, SpdmErrorCode::InvalidRequest, 0, resp);
        }
        let req_session_id = u16::from_le_bytes([body[0], body[1]]);
        let mut peer_xy = [0u8; P256_PUB_LEN];
        peer_xy.copy_from_slice(&body[36..100]);

        let eph = EphemeralP256::generate(rng)?;
        let shared_secret = eph.diffie_hellman(&peer_xy)?;

        let rsp_session_id = self.next_rsp_session_id;
        self.next_rsp_session_id = self.next_rsp_session_id.wrapping_add(1).max(1);
        // DSP0274 §10.12: SessionID = (rsp_session_id << 16) | req_session_id
        let session_id = (u32::from(rsp_session_id) << 16) | u32::from(req_session_id);

        // KEY_EXCHANGE_RSP: header(4) + RspSessionID(2) + MutAuth(1=0) + SlotID(1=0) +
        // RandomData(32) + ExchangeData(64) + OpaqueLen(2) + Opaque(12) + Sig(64) + VerifyData(32)
        let opaque = &OPAQUE_SELECTED_VERSION_1_1;
        let before_sig = 4 + 2 + 1 + 1 + 32 + P256_PUB_LEN + 2 + opaque.len();
        let before_verify = before_sig + SIG_LEN;
        let total = before_verify + HASH_LEN;
        if resp.len() < total {
            return Err(Error::BufferTooSmall);
        }

        resp[0..4].copy_from_slice(&[self.version, SpdmCode::KeyExchangeRsp as u8, 0, 0]);
        resp[4..6].copy_from_slice(&rsp_session_id.to_le_bytes());
        resp[6] = 0; // MutAuthRequested = 0
        resp[7] = 0; // SlotIDParam = 0
        rng.fill_bytes(&mut resp[8..40]);
        resp[40..104].copy_from_slice(eph.public_xy());
        resp[104..106].copy_from_slice(&(opaque.len() as u16).to_le_bytes());
        resp[106..before_sig].copy_from_slice(opaque);

        // Transcript up to signature: VCA || cert_chain_hash || KEY_EXCHANGE || KEY_EXCHANGE_RSP[..before_sig]
        // Strip any 1..3 trailing PCIe DOE DW-alignment pad bytes from req.
        let req_exact = 106 + req_opaque_len;
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
