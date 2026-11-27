//! Sans-IO SPDM 1.2 Requester state machine (DSP0274).

use crate::crypto::{
    EphemeralP256, HASH_LEN, P256_PUB_LEN, Rng, SIG_LEN, Sha256Transcript, ct_eq, hmac_sha256,
    sha256, spdm12_signing_digest, verify_p256_prehash,
};
use crate::secured::{OPAQUE_SUPPORTED_VERSION_1_1, SecuredSession, SessionTransport};
use crate::wire::{
    AlgStructEntry, AlgorithmsResponseBody, CapabilitiesBody, CertificateResponsePrefix,
    ChallengeAuthPrefix, GetCertificateBody, GetMeasurementsBody, KeyExchangeReqPrefix,
    KeyExchangeRspPrefix, MeasurementsPrefix, MeasurementsSuffix, NegotiateAlgorithmsBody,
    VersionResponsePrefix,
};
use crate::{
    AEAD_AES_256_GCM, BASE_ASYM_ECDSA_ECC_NIST_P256, BASE_HASH_SHA_256, CAP_CERT, CAP_CHAL,
    CAP_ENCRYPT, CAP_HBEAT, CAP_KEY_EX, CAP_KEY_UPD, CAP_MAC, DHE_SECP256R1, Error,
    KEY_SCHEDULE_SPDM, SPDM_VERSION_1_0, SPDM_VERSION_1_2, SpdmCode, SpdmHeader,
};
use zerocopy::little_endian::{U16 as LeU16, U32 as LeU32};
use zerocopy::{FromBytes, IntoBytes};

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
        self.vca = Sha256Transcript::new();
        self.m_transcript = Sha256Transcript::new();
        self.peer_cert_len = 0;
        self.peer_cert_hash = [0; HASH_LEN];
        self.eph = None;
        self.kx_transcript = None;
        self.session = None;
        SpdmHeader::new(SPDM_VERSION_1_0, SpdmCode::GetVersion, 0, 0)
            .write_to_prefix(out)
            .map_err(|_| Error::BufferTooSmall)?;
        self.vca.update(&out[..SpdmHeader::LEN]);
        Ok(SpdmHeader::LEN)
    }

    pub fn handle_version(&mut self, resp: &[u8]) -> Result<u8, Error> {
        let (hdr, body) = SpdmHeader::parse(resp)?;
        if hdr.code == SpdmCode::Error as u8 {
            return Err(Error::RemoteError(hdr.param1));
        }
        if hdr.code != SpdmCode::Version as u8 {
            return Err(Error::InvalidMessage);
        }
        let (pfx, rest) =
            VersionResponsePrefix::ref_from_prefix(body).map_err(|_| Error::InvalidMessage)?;
        let count = pfx.version_number_entry_count as usize;
        let (entries, _) =
            <[LeU16]>::ref_from_prefix_with_elems(rest, count).map_err(|_| Error::ShortBuffer)?;
        if !entries
            .iter()
            .any(|e| ((e.get() >> 8) as u8) == SPDM_VERSION_1_2)
        {
            return Err(Error::VersionMismatch);
        }
        let exact_len = SpdmHeader::LEN + core::mem::size_of::<VersionResponsePrefix>() + count * 2;
        self.version = SPDM_VERSION_1_2;
        self.vca.update(&resp[..exact_len]);
        Ok(self.version)
    }

    pub fn build_get_capabilities(&mut self, out: &mut [u8]) -> Result<usize, Error> {
        let total = SpdmHeader::LEN + core::mem::size_of::<CapabilitiesBody>();
        if out.len() < total {
            return Err(Error::BufferTooSmall);
        }
        let caps =
            CAP_CERT | CAP_CHAL | CAP_ENCRYPT | CAP_MAC | CAP_KEY_EX | CAP_HBEAT | CAP_KEY_UPD;
        SpdmHeader::new(self.version, SpdmCode::GetCapabilities, 0, 0)
            .write_to_prefix(out)
            .map_err(|_| Error::BufferTooSmall)?;
        let body = CapabilitiesBody {
            reserved: 0,
            ct_exponent: 20,
            reserved2: LeU16::new(0),
            flags: LeU32::new(caps),
            data_transfer_size: LeU32::new(4096),
            max_spdm_msg_size: LeU32::new(4096),
        };
        body.write_to_prefix(&mut out[SpdmHeader::LEN..])
            .map_err(|_| Error::BufferTooSmall)?;
        self.vca.update(&out[..total]);
        Ok(total)
    }

    pub fn handle_capabilities(&mut self, resp: &[u8]) -> Result<u32, Error> {
        let (hdr, body) = SpdmHeader::parse(resp)?;
        if hdr.code == SpdmCode::Error as u8 {
            return Err(Error::RemoteError(hdr.param1));
        }
        if hdr.code != SpdmCode::Capabilities as u8 {
            return Err(Error::InvalidMessage);
        }
        let (caps, _) =
            CapabilitiesBody::ref_from_prefix(body).map_err(|_| Error::InvalidMessage)?;
        let exact_len = SpdmHeader::LEN + core::mem::size_of::<CapabilitiesBody>();
        self.vca.update(&resp[..exact_len]);
        Ok(caps.flags.get())
    }

    pub fn build_negotiate_algorithms(&mut self, out: &mut [u8]) -> Result<usize, Error> {
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
        let body_off = SpdmHeader::LEN;
        let algs_off = body_off + core::mem::size_of::<NegotiateAlgorithmsBody>();
        let total = algs_off + core::mem::size_of_val(&algs);
        if out.len() < total {
            return Err(Error::BufferTooSmall);
        }
        SpdmHeader::new(
            self.version,
            SpdmCode::NegotiateAlgorithms,
            algs.len() as u8,
            0,
        )
        .write_to_prefix(out)
        .map_err(|_| Error::BufferTooSmall)?;
        let body = NegotiateAlgorithmsBody {
            length: LeU16::new(total as u16),
            measurement_spec: 0x01,     // DMTF
            other_params_support: 0x02, // OpaqueDataFmt1
            base_asym_algo: LeU32::new(BASE_ASYM_ECDSA_ECC_NIST_P256),
            base_hash_algo: LeU32::new(BASE_HASH_SHA_256),
            reserved: [0; 12],
            ext_asym_count: 0,
            ext_hash_count: 0,
            reserved2: LeU16::new(0),
        };
        body.write_to_prefix(&mut out[body_off..])
            .map_err(|_| Error::BufferTooSmall)?;
        algs.write_to_prefix(&mut out[algs_off..])
            .map_err(|_| Error::BufferTooSmall)?;
        self.vca.update(&out[..total]);
        Ok(total)
    }

    pub fn handle_algorithms(&mut self, resp: &[u8]) -> Result<(), Error> {
        let (hdr, body) = SpdmHeader::parse(resp)?;
        if hdr.code == SpdmCode::Error as u8 {
            return Err(Error::RemoteError(hdr.param1));
        }
        if hdr.code != SpdmCode::Algorithms as u8 {
            return Err(Error::InvalidMessage);
        }
        let (alg, _) =
            AlgorithmsResponseBody::ref_from_prefix(body).map_err(|_| Error::InvalidMessage)?;
        if alg.base_asym_sel.get() != BASE_ASYM_ECDSA_ECC_NIST_P256
            || alg.base_hash_sel.get() != BASE_HASH_SHA_256
        {
            return Err(Error::UnsupportedAlgorithm);
        }
        let min_len = SpdmHeader::LEN + core::mem::size_of::<AlgorithmsResponseBody>();
        let alg_len = alg.length.get() as usize;
        if alg_len < min_len || resp.len() < alg_len {
            return Err(Error::InvalidMessage);
        }
        self.vca.update(&resp[..alg_len]);
        self.m_transcript = self.vca.clone();
        Ok(())
    }

    pub fn build_get_digests(&mut self, out: &mut [u8]) -> Result<usize, Error> {
        SpdmHeader::new(self.version, SpdmCode::GetDigests, 0, 0)
            .write_to_prefix(out)
            .map_err(|_| Error::BufferTooSmall)?;
        self.m_transcript.update(&out[..SpdmHeader::LEN]);
        Ok(SpdmHeader::LEN)
    }

    pub fn handle_digests(&mut self, resp: &[u8]) -> Result<[u8; HASH_LEN], Error> {
        let (hdr, body) = SpdmHeader::parse(resp)?;
        let slot_count = hdr.param2.count_ones() as usize;
        // Require slot 0 to be populated and hash all advertised slot digests into M1.
        if hdr.code != SpdmCode::Digests as u8 || (hdr.param2 & 0x01) == 0 {
            return Err(Error::InvalidMessage);
        }
        let (digests, _) = <[[u8; HASH_LEN]]>::ref_from_prefix_with_elems(body, slot_count)
            .map_err(|_| Error::InvalidMessage)?;
        self.peer_cert_hash = digests[0];
        self.m_transcript
            .update(&resp[..SpdmHeader::LEN + slot_count * HASH_LEN]);
        Ok(self.peer_cert_hash)
    }

    pub fn build_get_certificate(
        &mut self,
        offset: u16,
        length: u16,
        out: &mut [u8],
    ) -> Result<usize, Error> {
        let total = SpdmHeader::LEN + core::mem::size_of::<GetCertificateBody>();
        if out.len() < total {
            return Err(Error::BufferTooSmall);
        }
        SpdmHeader::new(self.version, SpdmCode::GetCertificate, 0, 0)
            .write_to_prefix(out)
            .map_err(|_| Error::BufferTooSmall)?;
        let body = GetCertificateBody {
            offset: LeU16::new(offset),
            length: LeU16::new(length),
        };
        body.write_to_prefix(&mut out[SpdmHeader::LEN..])
            .map_err(|_| Error::BufferTooSmall)?;
        self.m_transcript.update(&out[..total]);
        Ok(total)
    }

    /// Append a certificate chunk; returns `remainder_length`. When `0`, verifies
    /// the assembled certificate chain hash matches `GET_DIGESTS`.
    pub fn handle_certificate(&mut self, resp: &[u8]) -> Result<u16, Error> {
        let (hdr, body) = SpdmHeader::parse(resp)?;
        if hdr.code != SpdmCode::Certificate as u8 {
            return Err(Error::InvalidMessage);
        }
        let (pfx, cert_data) =
            CertificateResponsePrefix::ref_from_prefix(body).map_err(|_| Error::InvalidMessage)?;
        let portion = pfx.portion_length.get() as usize;
        let remainder = pfx.remainder_length.get();
        if cert_data.len() < portion || self.peer_cert_len + portion > MAX_CERT {
            return Err(Error::BufferTooSmall);
        }
        self.peer_cert_chain[self.peer_cert_len..self.peer_cert_len + portion]
            .copy_from_slice(&cert_data[..portion]);
        self.peer_cert_len += portion;
        let exact_len =
            SpdmHeader::LEN + core::mem::size_of::<CertificateResponsePrefix>() + portion;
        self.m_transcript.update(&resp[..exact_len]);
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
        let total = SpdmHeader::LEN + 32;
        if out.len() < total {
            return Err(Error::BufferTooSmall);
        }
        SpdmHeader::new(self.version, SpdmCode::Challenge, 0, 0)
            .write_to_prefix(out)
            .map_err(|_| Error::BufferTooSmall)?;
        rng.fill_bytes(&mut out[SpdmHeader::LEN..total]);
        Ok(total)
    }

    pub fn verify_challenge_auth(&self, req: &[u8], resp: &[u8]) -> Result<(), Error> {
        let (hdr, body) = SpdmHeader::parse(resp)?;
        if hdr.code != SpdmCode::ChallengeAuth as u8 {
            return Err(Error::InvalidMessage);
        }
        let (pfx, rest) =
            ChallengeAuthPrefix::ref_from_prefix(body).map_err(|_| Error::InvalidMessage)?;
        let opaque_len = pfx.opaque_length.get() as usize;
        if rest.len() < opaque_len + SIG_LEN {
            return Err(Error::ShortBuffer);
        }
        let sig_off = SpdmHeader::LEN + core::mem::size_of::<ChallengeAuthPrefix>() + opaque_len;
        let mut t = self.m_transcript.clone();
        t.update(req);
        t.update(&resp[..sig_off]);
        let m1_hash = t.finalize_clone();
        let digest =
            spdm12_signing_digest(self.version, b"responder-challenge_auth signing", &m1_hash);
        let mut sig = [0u8; SIG_LEN];
        sig.copy_from_slice(&rest[opaque_len..opaque_len + SIG_LEN]);
        verify_p256_prehash(&self.peer_pub_xy, &digest, &sig)
    }

    pub fn build_get_measurements<R: Rng>(
        &self,
        rng: &mut R,
        out: &mut [u8],
    ) -> Result<usize, Error> {
        let total = SpdmHeader::LEN + core::mem::size_of::<GetMeasurementsBody>();
        if out.len() < total {
            return Err(Error::BufferTooSmall);
        }
        // param1 = 0x01 (request signature), param2 = 0xFF (all measurements)
        SpdmHeader::new(self.version, SpdmCode::GetMeasurements, 0x01, 0xFF)
            .write_to_prefix(out)
            .map_err(|_| Error::BufferTooSmall)?;
        let mut body = GetMeasurementsBody {
            nonce: [0; 32],
            slot_id_param: 0,
        };
        rng.fill_bytes(&mut body.nonce);
        body.write_to_prefix(&mut out[SpdmHeader::LEN..])
            .map_err(|_| Error::BufferTooSmall)?;
        Ok(total)
    }

    pub fn verify_measurements(&self, req: &[u8], resp: &[u8]) -> Result<u8, Error> {
        let (hdr, body) = SpdmHeader::parse(resp)?;
        if hdr.code != SpdmCode::Measurements as u8 {
            return Err(Error::InvalidMessage);
        }
        let (pfx, rest) =
            MeasurementsPrefix::ref_from_prefix(body).map_err(|_| Error::InvalidMessage)?;
        let record_len = pfx.record_len();
        if rest.len() < record_len {
            return Err(Error::ShortBuffer);
        }
        let (suffix, tail) = MeasurementsSuffix::ref_from_prefix(&rest[record_len..])
            .map_err(|_| Error::ShortBuffer)?;
        let opaque_len = suffix.opaque_length.get() as usize;
        if tail.len() < opaque_len + SIG_LEN {
            return Err(Error::ShortBuffer);
        }
        let sig_off = SpdmHeader::LEN
            + core::mem::size_of::<MeasurementsPrefix>()
            + record_len
            + core::mem::size_of::<MeasurementsSuffix>()
            + opaque_len;
        let mut t = self.vca.clone();
        t.update(req);
        t.update(&resp[..sig_off]);
        let l1_hash = t.finalize_clone();
        let digest =
            spdm12_signing_digest(self.version, b"responder-measurements signing", &l1_hash);
        let mut sig = [0u8; SIG_LEN];
        sig.copy_from_slice(&tail[opaque_len..opaque_len + SIG_LEN]);
        verify_p256_prehash(&self.peer_pub_xy, &digest, &sig)?;
        Ok(pfx.num_blocks)
    }

    pub fn build_key_exchange<R: Rng>(
        &mut self,
        rng: &mut R,
        out: &mut [u8],
    ) -> Result<usize, Error> {
        let opaque = &OPAQUE_SUPPORTED_VERSION_1_1;
        let fixed = SpdmHeader::LEN + core::mem::size_of::<KeyExchangeReqPrefix>();
        let total = fixed + opaque.len();
        if out.len() < total {
            return Err(Error::BufferTooSmall);
        }
        let eph = EphemeralP256::generate(rng)?;
        let req_sid = self.pending_req_session_id;
        self.pending_req_session_id = req_sid.wrapping_add(1).max(1);

        SpdmHeader::new(self.version, SpdmCode::KeyExchange, 0, 0)
            .write_to_prefix(out)
            .map_err(|_| Error::BufferTooSmall)?;
        let mut pfx = KeyExchangeReqPrefix {
            req_session_id: LeU16::new(req_sid),
            session_policy: 0,
            reserved: 0,
            random_data: [0; 32],
            exchange_data: *eph.public_xy(),
            opaque_length: LeU16::new(opaque.len() as u16),
        };
        rng.fill_bytes(&mut pfx.random_data);
        pfx.write_to_prefix(&mut out[SpdmHeader::LEN..])
            .map_err(|_| Error::BufferTooSmall)?;
        out[fixed..total].copy_from_slice(opaque);

        self.eph = Some(eph);
        Ok(total)
    }

    pub fn handle_key_exchange_rsp(&mut self, req: &[u8], resp: &[u8]) -> Result<u32, Error> {
        let (_, req_body) = SpdmHeader::parse(req)?;
        let (req_pfx, _) =
            KeyExchangeReqPrefix::ref_from_prefix(req_body).map_err(|_| Error::ShortBuffer)?;
        let (hdr, body) = SpdmHeader::parse(resp)?;
        if hdr.code == SpdmCode::Error as u8 {
            return Err(Error::RemoteError(hdr.param1));
        }
        if hdr.code != SpdmCode::KeyExchangeRsp as u8 {
            return Err(Error::InvalidMessage);
        }
        let (rsp_pfx, rest) =
            KeyExchangeRspPrefix::ref_from_prefix(body).map_err(|_| Error::InvalidMessage)?;
        let opaque_len = rsp_pfx.opaque_length.get() as usize;
        let fixed = SpdmHeader::LEN + core::mem::size_of::<KeyExchangeRspPrefix>();
        let before_sig = fixed + opaque_len;
        let before_verify = before_sig + SIG_LEN;
        let total = before_verify + HASH_LEN;
        if rest.len() < opaque_len + SIG_LEN + HASH_LEN {
            return Err(Error::ShortBuffer);
        }
        if !crate::secured::opaque_selects_version_1_1(&rest[..opaque_len]) {
            return Err(Error::VersionMismatch);
        }
        let session_id = (u32::from(rsp_pfx.rsp_session_id.get()) << 16)
            | u32::from(req_pfx.req_session_id.get());

        let eph = self.eph.take().ok_or(Error::SessionInactive)?;
        let shared_secret = eph.diffie_hellman(&rsp_pfx.exchange_data)?;

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
