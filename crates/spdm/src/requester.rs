//! Sans-IO SPDM 1.2 Requester state machine (DSP0274).

use crate::crypto::{
    EphemeralP256, HASH_LEN, P256_PUB_LEN, Rng, SIG_LEN, Sha256Transcript, ct_eq, hmac_sha256,
    sha256, spdm12_signing_digest, verify_p256_prehash,
};
use crate::secured::{OPAQUE_SUPPORTED_VERSION_1_1, SecuredSession, SessionTransport};
use crate::{
    AEAD_AES_256_GCM, BASE_ASYM_ECDSA_ECC_NIST_P256, BASE_HASH_SHA_256, CAP_CERT, CAP_CHAL,
    CAP_ENCRYPT, CAP_HBEAT, CAP_KEY_EX, CAP_KEY_UPD, CAP_MAC, DHE_SECP256R1, Error,
    KEY_SCHEDULE_SPDM, SPDM_VERSION_1_0, SPDM_VERSION_1_2, SpdmCode, SpdmHeader,
};

/// Sans-IO SPDM 1.2 Requester supporting discovery, certificate retrieval,
/// challenge authentication, signed measurements, and secured sessions.
pub struct SpdmRequester<const MAX_CERT: usize = 512> {
    version: u8,
    transport: SessionTransport,
    vca: Sha256Transcript,
    m_transcript: Sha256Transcript,
    peer_cert_chain: [u8; MAX_CERT],
    peer_cert_len: usize,
    peer_cert_hash: [u8; HASH_LEN],
    peer_pub_xy: [u8; P256_PUB_LEN],
    eph: Option<EphemeralP256>,
    kx_transcript: Option<Sha256Transcript>,
    pending_req_session_id: u16,
    pub session: Option<SecuredSession>,
}

impl<const MAX_CERT: usize> SpdmRequester<MAX_CERT> {
    /// Construct a requester knowing the peer's P-256 public key `peer_pub_xy`
    /// (e.g. extracted from the peer's leaf certificate or pinned root).
    pub fn new(transport: SessionTransport, peer_pub_xy: [u8; P256_PUB_LEN]) -> Self {
        Self {
            version: SPDM_VERSION_1_2,
            transport,
            vca: Sha256Transcript::new(),
            m_transcript: Sha256Transcript::new(),
            peer_cert_chain: [0; MAX_CERT],
            peer_cert_len: 0,
            peer_cert_hash: [0; HASH_LEN],
            peer_pub_xy,
            eph: None,
            kx_transcript: None,
            pending_req_session_id: 0x0001,
            session: None,
        }
    }

    pub fn build_get_version(&mut self, out: &mut [u8]) -> Result<usize, Error> {
        if out.len() < 4 {
            return Err(Error::BufferTooSmall);
        }
        self.vca = Sha256Transcript::new();
        self.m_transcript = Sha256Transcript::new();
        self.peer_cert_len = 0;
        self.peer_cert_hash = [0; HASH_LEN];
        self.eph = None;
        self.kx_transcript = None;
        self.session = None;
        out[..4].copy_from_slice(&[SPDM_VERSION_1_0, SpdmCode::GetVersion as u8, 0, 0]);
        self.vca.update(&out[..4]);
        Ok(4)
    }

    pub fn handle_version(&mut self, resp: &[u8]) -> Result<u8, Error> {
        let (hdr, body) = SpdmHeader::parse(resp)?;
        if hdr.code == SpdmCode::Error as u8 {
            return Err(Error::RemoteError(hdr.param1));
        }
        if hdr.code != SpdmCode::Version as u8 || body.len() < 4 {
            return Err(Error::InvalidMessage);
        }
        let count = body[1] as usize;
        if body.len() < 2 + count * 2 {
            return Err(Error::ShortBuffer);
        }
        let mut found_12 = false;
        for i in 0..count {
            let entry = u16::from_le_bytes([body[2 + i * 2], body[3 + i * 2]]);
            if (entry >> 8) as u8 == SPDM_VERSION_1_2 {
                found_12 = true;
            }
        }
        if !found_12 {
            return Err(Error::VersionMismatch);
        }
        let exact_len = 6 + count * 2;
        self.version = SPDM_VERSION_1_2;
        self.vca.update(&resp[..exact_len]);
        Ok(self.version)
    }

    pub fn build_get_capabilities(&mut self, out: &mut [u8]) -> Result<usize, Error> {
        if out.len() < 20 {
            return Err(Error::BufferTooSmall);
        }
        let caps =
            CAP_CERT | CAP_CHAL | CAP_ENCRYPT | CAP_MAC | CAP_KEY_EX | CAP_HBEAT | CAP_KEY_UPD;
        out[0..4].copy_from_slice(&[self.version, SpdmCode::GetCapabilities as u8, 0, 0]);
        out[4] = 0;
        out[5] = 20;
        out[6..8].copy_from_slice(&0u16.to_le_bytes());
        out[8..12].copy_from_slice(&caps.to_le_bytes());
        out[12..16].copy_from_slice(&4096u32.to_le_bytes());
        out[16..20].copy_from_slice(&4096u32.to_le_bytes());
        self.vca.update(&out[..20]);
        Ok(20)
    }

    pub fn handle_capabilities(&mut self, resp: &[u8]) -> Result<u32, Error> {
        let (hdr, body) = SpdmHeader::parse(resp)?;
        if hdr.code == SpdmCode::Error as u8 {
            return Err(Error::RemoteError(hdr.param1));
        }
        if hdr.code != SpdmCode::Capabilities as u8 || body.len() < 16 {
            return Err(Error::InvalidMessage);
        }
        let flags = u32::from_le_bytes([body[4], body[5], body[6], body[7]]);
        self.vca.update(&resp[..20]);
        Ok(flags)
    }

    pub fn build_negotiate_algorithms(&mut self, out: &mut [u8]) -> Result<usize, Error> {
        if out.len() < 44 {
            return Err(Error::BufferTooSmall);
        }
        out[0..4].copy_from_slice(&[self.version, SpdmCode::NegotiateAlgorithms as u8, 3, 0]);
        out[4..6].copy_from_slice(&44u16.to_le_bytes());
        out[6] = 0x01; // DMTF measurement spec
        out[7] = 0x02; // OpaqueDataFmt1
        out[8..12].copy_from_slice(&BASE_ASYM_ECDSA_ECC_NIST_P256.to_le_bytes());
        out[12..16].copy_from_slice(&BASE_HASH_SHA_256.to_le_bytes());
        out[16..28].fill(0);
        out[28] = 0; // ExtAsymCount
        out[29] = 0; // ExtHashCount
        out[30..32].copy_from_slice(&0u16.to_le_bytes());
        out[32..36].copy_from_slice(&[
            0x02,
            0x20,
            (DHE_SECP256R1 & 0xFF) as u8,
            (DHE_SECP256R1 >> 8) as u8,
        ]);
        out[36..40].copy_from_slice(&[
            0x03,
            0x20,
            (AEAD_AES_256_GCM & 0xFF) as u8,
            (AEAD_AES_256_GCM >> 8) as u8,
        ]);
        out[40..44].copy_from_slice(&[
            0x05,
            0x20,
            (KEY_SCHEDULE_SPDM & 0xFF) as u8,
            (KEY_SCHEDULE_SPDM >> 8) as u8,
        ]);
        self.vca.update(&out[..44]);
        Ok(44)
    }

    pub fn handle_algorithms(&mut self, resp: &[u8]) -> Result<(), Error> {
        let (hdr, body) = SpdmHeader::parse(resp)?;
        if hdr.code == SpdmCode::Error as u8 {
            return Err(Error::RemoteError(hdr.param1));
        }
        if hdr.code != SpdmCode::Algorithms as u8 || body.len() < 32 {
            return Err(Error::InvalidMessage);
        }
        let asym = u32::from_le_bytes([body[8], body[9], body[10], body[11]]);
        let hash = u32::from_le_bytes([body[12], body[13], body[14], body[15]]);
        if asym != BASE_ASYM_ECDSA_ECC_NIST_P256 || hash != BASE_HASH_SHA_256 {
            return Err(Error::UnsupportedAlgorithm);
        }
        let alg_len = u16::from_le_bytes([resp[4], resp[5]]) as usize;
        if alg_len < 36 || resp.len() < alg_len {
            return Err(Error::InvalidMessage);
        }
        self.vca.update(&resp[..alg_len]);
        self.m_transcript = self.vca.clone();
        Ok(())
    }

    pub fn build_get_digests(&mut self, out: &mut [u8]) -> Result<usize, Error> {
        if out.len() < 4 {
            return Err(Error::BufferTooSmall);
        }
        out[..4].copy_from_slice(&[self.version, SpdmCode::GetDigests as u8, 0, 0]);
        self.m_transcript.update(&out[..4]);
        Ok(4)
    }

    pub fn handle_digests(&mut self, resp: &[u8]) -> Result<[u8; HASH_LEN], Error> {
        let (hdr, body) = SpdmHeader::parse(resp)?;
        let slot_count = hdr.param2.count_ones() as usize;
        // Require slot 0 to be populated and hash all advertised slot digests into M1.
        if hdr.code != SpdmCode::Digests as u8
            || (hdr.param2 & 0x01) == 0
            || body.len() < slot_count * HASH_LEN
        {
            return Err(Error::InvalidMessage);
        }
        self.peer_cert_hash.copy_from_slice(&body[..HASH_LEN]);
        self.m_transcript.update(&resp[..4 + slot_count * HASH_LEN]);
        Ok(self.peer_cert_hash)
    }

    pub fn build_get_certificate(
        &mut self,
        offset: u16,
        length: u16,
        out: &mut [u8],
    ) -> Result<usize, Error> {
        if out.len() < 8 {
            return Err(Error::BufferTooSmall);
        }
        out[0..4].copy_from_slice(&[self.version, SpdmCode::GetCertificate as u8, 0, 0]);
        out[4..6].copy_from_slice(&offset.to_le_bytes());
        out[6..8].copy_from_slice(&length.to_le_bytes());
        self.m_transcript.update(&out[..8]);
        Ok(8)
    }

    /// Append a certificate chunk; returns `remainder_length`. When `0`, verifies
    /// the assembled certificate chain hash matches `GET_DIGESTS`.
    pub fn handle_certificate(&mut self, resp: &[u8]) -> Result<u16, Error> {
        let (hdr, body) = SpdmHeader::parse(resp)?;
        if hdr.code != SpdmCode::Certificate as u8 || body.len() < 4 {
            return Err(Error::InvalidMessage);
        }
        let portion = u16::from_le_bytes([body[0], body[1]]) as usize;
        let remainder = u16::from_le_bytes([body[2], body[3]]);
        if body.len() < 4 + portion || self.peer_cert_len + portion > MAX_CERT {
            return Err(Error::BufferTooSmall);
        }
        self.peer_cert_chain[self.peer_cert_len..self.peer_cert_len + portion]
            .copy_from_slice(&body[4..4 + portion]);
        self.peer_cert_len += portion;
        self.m_transcript.update(&resp[..8 + portion]);
        if remainder == 0 {
            let calc = sha256(&self.peer_cert_chain[..self.peer_cert_len]);
            if self.peer_cert_hash != [0; HASH_LEN] && !ct_eq(&calc, &self.peer_cert_hash) {
                return Err(Error::VerifyDataMismatch);
            }
            self.peer_cert_hash = calc;
        }
        Ok(remainder)
    }

    pub fn build_challenge<R: Rng>(&self, rng: &mut R, out: &mut [u8]) -> Result<usize, Error> {
        if out.len() < 36 {
            return Err(Error::BufferTooSmall);
        }
        out[0..4].copy_from_slice(&[self.version, SpdmCode::Challenge as u8, 0, 0]);
        rng.fill_bytes(&mut out[4..36]);
        Ok(36)
    }

    pub fn verify_challenge_auth(&self, req: &[u8], resp: &[u8]) -> Result<(), Error> {
        let (hdr, _) = SpdmHeader::parse(resp)?;
        if hdr.code != SpdmCode::ChallengeAuth as u8 || resp.len() < 70 + SIG_LEN {
            return Err(Error::InvalidMessage);
        }
        let opaque_len = u16::from_le_bytes([resp[68], resp[69]]) as usize;
        let sig_off = 70 + opaque_len;
        if resp.len() < sig_off + SIG_LEN {
            return Err(Error::ShortBuffer);
        }
        let mut t = self.m_transcript.clone();
        t.update(req);
        t.update(&resp[..sig_off]);
        let m1_hash = t.finalize_clone();
        let digest =
            spdm12_signing_digest(self.version, b"responder-challenge_auth signing", &m1_hash);
        let mut sig = [0u8; SIG_LEN];
        sig.copy_from_slice(&resp[sig_off..sig_off + SIG_LEN]);
        verify_p256_prehash(&self.peer_pub_xy, &digest, &sig)
    }

    pub fn build_get_measurements<R: Rng>(
        &self,
        rng: &mut R,
        out: &mut [u8],
    ) -> Result<usize, Error> {
        if out.len() < 37 {
            return Err(Error::BufferTooSmall);
        }
        // param1 = 0x01 (request signature), param2 = 0xFF (all measurements)
        out[0..4].copy_from_slice(&[self.version, SpdmCode::GetMeasurements as u8, 0x01, 0xFF]);
        rng.fill_bytes(&mut out[4..36]);
        out[36] = 0; // slot 0
        Ok(37)
    }

    pub fn verify_measurements(&self, req: &[u8], resp: &[u8]) -> Result<u8, Error> {
        let (hdr, _) = SpdmHeader::parse(resp)?;
        if hdr.code != SpdmCode::Measurements as u8 || resp.len() < 8 + 32 + 2 + SIG_LEN {
            return Err(Error::InvalidMessage);
        }
        let record_len = u32::from_le_bytes([resp[5], resp[6], resp[7], 0]) as usize;
        let opaque_off = 8 + record_len + 32;
        if resp.len() < opaque_off + 2 {
            return Err(Error::ShortBuffer);
        }
        let opaque_len = u16::from_le_bytes([resp[opaque_off], resp[opaque_off + 1]]) as usize;
        let sig_off = opaque_off + 2 + opaque_len;
        if resp.len() < sig_off + SIG_LEN {
            return Err(Error::ShortBuffer);
        }
        let mut t = self.vca.clone();
        t.update(req);
        t.update(&resp[..sig_off]);
        let l1_hash = t.finalize_clone();
        let digest =
            spdm12_signing_digest(self.version, b"responder-measurements signing", &l1_hash);
        let mut sig = [0u8; SIG_LEN];
        sig.copy_from_slice(&resp[sig_off..sig_off + SIG_LEN]);
        verify_p256_prehash(&self.peer_pub_xy, &digest, &sig)?;
        Ok(resp[4]) // number of measurement blocks
    }

    pub fn build_key_exchange<R: Rng>(
        &mut self,
        rng: &mut R,
        out: &mut [u8],
    ) -> Result<usize, Error> {
        let opaque = &OPAQUE_SUPPORTED_VERSION_1_1;
        let total = 4 + 2 + 1 + 1 + 32 + P256_PUB_LEN + 2 + opaque.len();
        if out.len() < total {
            return Err(Error::BufferTooSmall);
        }
        let eph = EphemeralP256::generate(rng)?;
        let req_sid = self.pending_req_session_id;
        self.pending_req_session_id = req_sid.wrapping_add(1).max(1);

        out[0..4].copy_from_slice(&[self.version, SpdmCode::KeyExchange as u8, 0, 0]);
        out[4..6].copy_from_slice(&req_sid.to_le_bytes());
        out[6] = 0; // SessionPolicy
        out[7] = 0; // Reserved
        rng.fill_bytes(&mut out[8..40]);
        out[40..104].copy_from_slice(eph.public_xy());
        out[104..106].copy_from_slice(&(opaque.len() as u16).to_le_bytes());
        out[106..total].copy_from_slice(opaque);

        self.eph = Some(eph);
        Ok(total)
    }

    pub fn handle_key_exchange_rsp(&mut self, req: &[u8], resp: &[u8]) -> Result<u32, Error> {
        if req.len() < 106 {
            return Err(Error::ShortBuffer);
        }
        let (hdr, body) = SpdmHeader::parse(resp)?;
        if hdr.code == SpdmCode::Error as u8 {
            return Err(Error::RemoteError(hdr.param1));
        }
        if hdr.code != SpdmCode::KeyExchangeRsp as u8 || resp.len() < 106 {
            return Err(Error::InvalidMessage);
        }
        let opaque_len = u16::from_le_bytes([resp[104], resp[105]]) as usize;
        let before_sig = 106 + opaque_len;
        let before_verify = before_sig + SIG_LEN;
        let total = before_verify + HASH_LEN;
        if resp.len() < total {
            return Err(Error::ShortBuffer);
        }
        if !crate::secured::opaque_selects_version_1_1(&resp[106..before_sig]) {
            return Err(Error::VersionMismatch);
        }
        let req_sid = u16::from_le_bytes([req[4], req[5]]);
        let rsp_sid = u16::from_le_bytes([body[0], body[1]]);
        let session_id = (u32::from(rsp_sid) << 16) | u32::from(req_sid);

        let mut peer_eph_xy = [0u8; P256_PUB_LEN];
        peer_eph_xy.copy_from_slice(&resp[40..104]);
        let eph = self.eph.take().ok_or(Error::SessionInactive)?;
        let shared_secret = eph.diffie_hellman(&peer_eph_xy)?;

        let mut t = self.vca.clone();
        t.update(&self.peer_cert_hash);
        t.update(req);
        t.update(&resp[..before_sig]);
        let th1_pre_sig = t.finalize_clone();
        let sign_digest = spdm12_signing_digest(
            self.version,
            b"responder-key_exchange_rsp signing",
            &th1_pre_sig,
        );
        let mut sig = [0u8; SIG_LEN];
        sig.copy_from_slice(&resp[before_sig..before_verify]);
        verify_p256_prehash(&self.peer_pub_xy, &sign_digest, &sig)?;

        t.update(&sig);
        let th1 = t.finalize_clone();
        let sess = SecuredSession::from_handshake(
            session_id,
            self.version,
            self.transport,
            true,
            &shared_secret,
            &th1,
        )?;
        let expected_verify = hmac_sha256(&sess.rsp_finished_key, &th1)?;
        if !ct_eq(&resp[before_verify..total], &expected_verify) {
            return Err(Error::VerifyDataMismatch);
        }
        t.update(&resp[before_verify..total]);

        self.kx_transcript = Some(t);
        self.session = Some(sess);
        Ok(session_id)
    }

    /// Build an encrypted `FINISH` secured record into `secured_out`.
    pub fn build_secured_finish(&mut self, secured_out: &mut [u8]) -> Result<usize, Error> {
        let t_orig = self.kx_transcript.as_ref().ok_or(Error::SessionInactive)?;
        let sess = self.session.as_mut().ok_or(Error::SessionInactive)?;

        let prefix_len = usize::from(self.transport == SessionTransport::Mctp);
        let mut t = t_orig.clone();
        let mut pt = [0u8; 40];
        if prefix_len == 1 {
            pt[0] = crate::MCTP_MSG_TYPE_SPDM;
        }
        let spdm = &mut pt[prefix_len..prefix_len + 4 + HASH_LEN];
        spdm[0..4].copy_from_slice(&[self.version, SpdmCode::Finish as u8, 0, 0]);
        t.update(&spdm[0..4]);
        let th_req_fin = t.finalize_clone();
        let verify_data = hmac_sha256(&sess.req_finished_key, &th_req_fin)?;
        spdm[4..4 + HASH_LEN].copy_from_slice(&verify_data);
        t.update(&verify_data);

        let n = sess.encrypt(&pt[..prefix_len + 4 + HASH_LEN], secured_out)?;
        self.kx_transcript = Some(t);
        Ok(n)
    }

    /// Decrypt and verify `FINISH_RSP` (4 bytes when `HANDSHAKE_IN_THE_CLEAR_CAP`
    /// is not negotiated, DSP0274 Table 60), transitioning the session to
    /// application data keys.
    pub fn handle_secured_finish_rsp(&mut self, secured_rsp: &[u8]) -> Result<(), Error> {
        let mut t = self.kx_transcript.take().ok_or(Error::SessionInactive)?;
        let sess = self.session.as_mut().ok_or(Error::SessionInactive)?;

        let mut pt = [0u8; 128];
        let pt_len = sess.decrypt(secured_rsp, &mut pt)?;
        let prefix_len = usize::from(self.transport == SessionTransport::Mctp);
        if pt_len < prefix_len + 4 {
            return Err(Error::ShortBuffer);
        }
        if prefix_len == 1 && (pt[0] & 0x7F) != crate::MCTP_MSG_TYPE_SPDM {
            return Err(Error::InvalidMessage);
        }
        let spdm = &pt[prefix_len..pt_len];
        let (hdr, _body) = SpdmHeader::parse(spdm)?;
        if hdr.code != SpdmCode::FinishRsp as u8 {
            return Err(Error::InvalidMessage);
        }
        t.update(&spdm[..4]);
        let th2 = t.finalize_clone();
        sess.derive_data_keys(&th2)?;
        Ok(())
    }

    /// Build an encrypted in-session SPDM command (`HEARTBEAT`, `KEY_UPDATE`,
    /// `END_SESSION`).
    pub fn build_secured_cmd(
        &mut self,
        code: SpdmCode,
        param1: u8,
        param2: u8,
        secured_out: &mut [u8],
    ) -> Result<usize, Error> {
        let sess = self.session.as_mut().ok_or(Error::SessionInactive)?;
        let prefix_len = usize::from(self.transport == SessionTransport::Mctp);
        let mut pt = [0u8; 8];
        if prefix_len == 1 {
            pt[0] = crate::MCTP_MSG_TYPE_SPDM;
        }
        pt[prefix_len..prefix_len + 4].copy_from_slice(&[self.version, code as u8, param1, param2]);
        // DSP0274 §10.21.1:
        // - VerifyNewKey (param1 == 3) is encrypted with the staged pending req key.
        // - UpdateKey (1) / UpdateAllKeys (2) are encrypted with the active req key and
        //   stage the new key(s) so rollback remains possible until VerifyNewKey ACKs.
        let n = if code == SpdmCode::KeyUpdate && param1 == 3 {
            sess.encrypt_pending(&pt[..prefix_len + 4], secured_out)?
        } else {
            let n = sess.encrypt(&pt[..prefix_len + 4], secured_out)?;
            if code == SpdmCode::KeyUpdate {
                match param1 {
                    1 => sess.stage_key_update(false)?,
                    2 => sess.stage_key_update(true)?,
                    _ => {}
                }
            }
            n
        };
        Ok(n)
    }

    /// Decrypt and validate an in-session ACK (`HEARTBEAT_ACK`, `KEY_UPDATE_ACK`,
    /// `END_SESSION_ACK`).
    pub fn handle_secured_ack(
        &mut self,
        expected_code: SpdmCode,
        secured_rsp: &[u8],
    ) -> Result<(u8, u8), Error> {
        let sess = self.session.as_mut().ok_or(Error::SessionInactive)?;
        let mut pt = [0u8; 64];
        let (pt_len, used_pending_rsp) = sess.decrypt_trying_pending(secured_rsp, &mut pt)?;
        let prefix_len = usize::from(self.transport == SessionTransport::Mctp);
        if pt_len < prefix_len + 4 {
            return Err(Error::ShortBuffer);
        }
        if prefix_len == 1 && (pt[0] & 0x7F) != crate::MCTP_MSG_TYPE_SPDM {
            return Err(Error::InvalidMessage);
        }
        let (hdr, _) = SpdmHeader::parse(&pt[prefix_len..pt_len])?;
        if hdr.code != expected_code as u8 {
            return Err(Error::InvalidMessage);
        }
        if used_pending_rsp {
            sess.commit_rsp_key_if_pending();
        }
        if expected_code == SpdmCode::KeyUpdateAck && hdr.param1 == 3 {
            sess.commit_req_key()?;
        }
        if expected_code == SpdmCode::EndSessionAck {
            sess.active = false;
            self.session = None;
        }
        Ok((hdr.param1, hdr.param2))
    }
}
