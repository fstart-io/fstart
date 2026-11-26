//! Cryptographic primitives for SPDM 1.2 (SHA-256, HMAC, HKDF, ECDSA-P256,
//! ECDHE-P256, AES-256-GCM) implemented in pure Rust `no_std`.

use crate::Error;
use aes_gcm::aead::{AeadInPlace, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce, Tag};
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use p256::ecdh::diffie_hellman;
use p256::ecdsa::signature::hazmat::{PrehashSigner, PrehashVerifier};
use p256::ecdsa::{Signature, SigningKey, VerifyingKey};
use p256::elliptic_curve::sec1::{FromEncodedPoint, ToEncodedPoint};
use p256::{EncodedPoint, PublicKey, SecretKey};
use sha2::{Digest, Sha256};

/// SHA-256 digest length in bytes.
pub const HASH_LEN: usize = 32;
/// Raw ECDSA P-256 signature length (`r || s`, big-endian).
pub const SIG_LEN: usize = 64;
/// Raw P-256 uncompressed public key coordinates (`x || y`, big-endian).
pub const P256_PUB_LEN: usize = 64;
/// AES-256-GCM key length.
pub const AEAD_KEY_LEN: usize = 32;
/// AES-256-GCM IV/nonce length.
pub const AEAD_IV_LEN: usize = 12;
/// AES-256-GCM authentication tag length.
pub const AEAD_TAG_LEN: usize = 16;

type HmacSha256 = Hmac<Sha256>;

/// Caller-provided entropy source for SPDM nonces and ephemeral ECDHE keys.
pub trait Rng {
    fn fill_bytes(&mut self, dest: &mut [u8]);
}

/// Deterministic PRNG for host tests / QEMU bringup (never for production keys).
pub struct DeterministicRng {
    state: u64,
}

impl DeterministicRng {
    pub const fn new(seed: u64) -> Self {
        Self {
            state: if seed == 0 {
                0x9E37_79B9_7F4A_7C15
            } else {
                seed
            },
        }
    }

    fn next_u64(&mut self) -> u64 {
        // xorshift64*
        let mut x = self.state;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.state = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
}

impl Rng for DeterministicRng {
    fn fill_bytes(&mut self, dest: &mut [u8]) {
        let mut i = 0;
        while i < dest.len() {
            let v = self.next_u64().to_le_bytes();
            let take = (dest.len() - i).min(8);
            dest[i..i + take].copy_from_slice(&v[..take]);
            i += take;
        }
    }
}

/// Incremental SHA-256 transcript hasher (Clone-able for TH1/TH2 forks).
#[derive(Clone)]
pub struct Sha256Transcript {
    hasher: Sha256,
}

impl Sha256Transcript {
    pub fn new() -> Self {
        Self {
            hasher: Sha256::new(),
        }
    }

    pub fn update(&mut self, data: &[u8]) {
        self.hasher.update(data);
    }

    pub fn finalize_clone(&self) -> [u8; HASH_LEN] {
        let out = self.hasher.clone().finalize();
        let mut buf = [0u8; HASH_LEN];
        buf.copy_from_slice(&out);
        buf
    }
}

impl Default for Sha256Transcript {
    fn default() -> Self {
        Self::new()
    }
}

/// Constant-time slice equality for MAC / `verify_data` checks.
pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (&x, &y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// One-shot SHA-256 hash.
pub fn sha256(data: &[u8]) -> [u8; HASH_LEN] {
    let out = Sha256::digest(data);
    let mut buf = [0u8; HASH_LEN];
    buf.copy_from_slice(&out);
    buf
}

/// HMAC-SHA-256.
pub fn hmac_sha256(key: &[u8], data: &[u8]) -> Result<[u8; HASH_LEN], Error> {
    let mut mac = <HmacSha256 as Mac>::new_from_slice(key).map_err(|_| Error::Crypto)?;
    mac.update(data);
    let out = mac.finalize().into_bytes();
    let mut buf = [0u8; HASH_LEN];
    buf.copy_from_slice(&out);
    Ok(buf)
}

/// HKDF-Extract using SHA-256.
pub fn hkdf_extract_sha256(salt: &[u8], ikm: &[u8]) -> [u8; HASH_LEN] {
    let (prk, _) = Hkdf::<Sha256>::extract(Some(salt), ikm);
    let mut out = [0u8; HASH_LEN];
    out.copy_from_slice(&prk);
    out
}

/// HKDF-Expand using SHA-256 with DSP0277 `BinConcat` info encoding.
///
/// `BinConcat(Length, Version, Label, Context)`:
/// `[len_le16][version_str: "spdm1.2 " (8B)][label][optional context]`
pub fn hkdf_expand_spdm(
    prk: &[u8; HASH_LEN],
    version: u8,
    label: &[u8],
    context: Option<&[u8]>,
    out: &mut [u8],
) -> Result<(), Error> {
    let hk = Hkdf::<Sha256>::from_prk(prk).map_err(|_| Error::Crypto)?;
    let major = (version >> 4) + b'0';
    let minor = (version & 0x0F) + b'0';
    let ver_str: [u8; 8] = [b's', b'p', b'd', b'm', major, b'.', minor, b' '];

    let mut info = [0u8; 128];
    let len_bytes = (out.len() as u16).to_le_bytes();
    let ctx_len = context.map_or(0, <[u8]>::len);
    let total = 2 + ver_str.len() + label.len() + ctx_len;
    if total > info.len() {
        return Err(Error::BufferTooSmall);
    }
    info[0..2].copy_from_slice(&len_bytes);
    info[2..10].copy_from_slice(&ver_str);
    info[10..10 + label.len()].copy_from_slice(label);
    if let Some(ctx) = context {
        info[10 + label.len()..total].copy_from_slice(ctx);
    }
    hk.expand(&info[..total], out).map_err(|_| Error::Crypto)
}

/// Build the SPDM 1.2 signing digest (DSP0274 §15):
/// `SHA256( ("dmtf-spdm-v1.2.*" * 4) || zero_pad_36(context) || transcript_hash )`
pub fn spdm12_signing_digest(
    version: u8,
    context: &[u8],
    transcript_hash: &[u8; HASH_LEN],
) -> [u8; HASH_LEN] {
    let major = (version >> 4) + b'0';
    let minor = (version & 0x0F) + b'0';
    let prefix: [u8; 16] = [
        b'd', b'm', b't', b'f', b'-', b's', b'p', b'd', b'm', b'-', b'v', major, b'.', minor, b'.',
        b'*',
    ];
    let mut ctx_block = [0u8; 36];
    let clen = context.len().min(36);
    // DSP0274 §15: zero-padded on the left (leading zeros) to 36 bytes.
    ctx_block[36 - clen..].copy_from_slice(&context[..clen]);

    let mut h = Sha256::new();
    for _ in 0..4 {
        h.update(prefix);
    }
    h.update(ctx_block);
    h.update(transcript_hash);
    let out = h.finalize();
    let mut buf = [0u8; HASH_LEN];
    buf.copy_from_slice(&out);
    buf
}

/// Static P-256 identity keypair (signing + raw 64-byte public key).
pub struct SpdmKeyPair {
    signing: SigningKey,
    pub_xy: [u8; P256_PUB_LEN],
}

impl SpdmKeyPair {
    /// Construct from a 32-byte non-zero P-256 private scalar.
    pub fn from_secret_bytes(secret: &[u8; 32]) -> Result<Self, Error> {
        let signing = SigningKey::from_slice(secret).map_err(|_| Error::Crypto)?;
        let verifying = signing.verifying_key();
        let point = verifying.to_encoded_point(false);
        let bytes = point.as_bytes();
        if bytes.len() != 65 || bytes[0] != 0x04 {
            return Err(Error::Crypto);
        }
        let mut pub_xy = [0u8; P256_PUB_LEN];
        pub_xy.copy_from_slice(&bytes[1..65]);
        Ok(Self { signing, pub_xy })
    }

    pub fn public_xy(&self) -> &[u8; P256_PUB_LEN] {
        &self.pub_xy
    }

    /// Sign a 32-byte pre-hashed digest, returning raw 64-byte `r || s`.
    pub fn sign_prehash(&self, digest: &[u8; HASH_LEN]) -> Result<[u8; SIG_LEN], Error> {
        let sig: Signature = self
            .signing
            .sign_prehash(digest)
            .map_err(|_| Error::Crypto)?;
        let mut out = [0u8; SIG_LEN];
        out.copy_from_slice(&sig.to_bytes());
        Ok(out)
    }
}

/// Verify a raw 64-byte `r || s` ECDSA P-256 signature against a raw 64-byte
/// uncompressed `x || y` public key and 32-byte pre-hashed digest.
pub fn verify_p256_prehash(
    pub_xy: &[u8; P256_PUB_LEN],
    digest: &[u8; HASH_LEN],
    sig_rs: &[u8; SIG_LEN],
) -> Result<(), Error> {
    let mut sec1 = [0u8; 65];
    sec1[0] = 0x04;
    sec1[1..65].copy_from_slice(pub_xy);
    let vk = VerifyingKey::from_sec1_bytes(&sec1).map_err(|_| Error::Crypto)?;
    let sig = Signature::from_slice(sig_rs).map_err(|_| Error::SignatureInvalid)?;
    vk.verify_prehash(digest, &sig)
        .map_err(|_| Error::SignatureInvalid)
}

/// Ephemeral P-256 ECDHE keypair for SPDM `KEY_EXCHANGE`.
pub struct EphemeralP256 {
    secret: SecretKey,
    pub_xy: [u8; P256_PUB_LEN],
}

impl EphemeralP256 {
    pub fn generate<R: Rng>(rng: &mut R) -> Result<Self, Error> {
        let mut raw = [0u8; 32];
        for _ in 0..16 {
            rng.fill_bytes(&mut raw);
            if let Ok(secret) = SecretKey::from_slice(&raw) {
                let pk = secret.public_key();
                let pt = pk.to_encoded_point(false);
                let bytes = pt.as_bytes();
                if bytes.len() == 65 && bytes[0] == 0x04 {
                    let mut pub_xy = [0u8; P256_PUB_LEN];
                    pub_xy.copy_from_slice(&bytes[1..65]);
                    return Ok(Self { secret, pub_xy });
                }
            }
        }
        Err(Error::Crypto)
    }

    pub fn public_xy(&self) -> &[u8; P256_PUB_LEN] {
        &self.pub_xy
    }

    /// Compute the 32-byte X-coordinate shared secret with peer's raw 64-byte `x || y`.
    pub fn diffie_hellman(&self, peer_xy: &[u8; P256_PUB_LEN]) -> Result<[u8; 32], Error> {
        let mut sec1 = [0u8; 65];
        sec1[0] = 0x04;
        sec1[1..65].copy_from_slice(peer_xy);
        let point = EncodedPoint::from_bytes(sec1).map_err(|_| Error::Crypto)?;
        let peer_pk = Option::<PublicKey>::from(PublicKey::from_encoded_point(&point))
            .ok_or(Error::Crypto)?;
        let shared = diffie_hellman(self.secret.to_nonzero_scalar(), peer_pk.as_affine());
        let mut out = [0u8; 32];
        out.copy_from_slice(shared.raw_secret_bytes());
        Ok(out)
    }
}

/// Compute DSP0277 per-record AES-GCM IV: `base_iv XOR seq_num_le64` (in low 8 bytes).
pub fn record_iv(base_iv: &[u8; AEAD_IV_LEN], seq_num: u64) -> [u8; AEAD_IV_LEN] {
    let mut iv = *base_iv;
    let seq_bytes = seq_num.to_le_bytes();
    for i in 0..8 {
        iv[i] ^= seq_bytes[i];
    }
    iv
}

/// In-place AES-256-GCM encryption returning the 16-byte authentication tag.
pub fn aes256_gcm_encrypt_in_place(
    key: &[u8; AEAD_KEY_LEN],
    iv: &[u8; AEAD_IV_LEN],
    aad: &[u8],
    buf: &mut [u8],
) -> Result<[u8; AEAD_TAG_LEN], Error> {
    let cipher = Aes256Gcm::new_from_slice(key).map_err(|_| Error::Crypto)?;
    let nonce = Nonce::from(*iv);
    let tag = cipher
        .encrypt_in_place_detached(&nonce, aad, buf)
        .map_err(|_| Error::Crypto)?;
    let mut out = [0u8; AEAD_TAG_LEN];
    out.copy_from_slice(&tag);
    Ok(out)
}

/// In-place AES-256-GCM decryption & authentication tag verification.
pub fn aes256_gcm_decrypt_in_place(
    key: &[u8; AEAD_KEY_LEN],
    iv: &[u8; AEAD_IV_LEN],
    aad: &[u8],
    buf: &mut [u8],
    tag: &[u8; AEAD_TAG_LEN],
) -> Result<(), Error> {
    let cipher = Aes256Gcm::new_from_slice(key).map_err(|_| Error::Crypto)?;
    let nonce = Nonce::from(*iv);
    let t = Tag::from(*tag);
    cipher
        .decrypt_in_place_detached(&nonce, aad, buf, &t)
        .map_err(|_| Error::DecryptFailed)
}
