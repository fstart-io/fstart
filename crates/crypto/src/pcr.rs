//! The hashes of TPM PCR banks, selected by TPM 2.0 algorithm ID
//! (`TPM_ALG_ID`): SHA-1 (the only bank of TPM 1.2), SHA-256, SHA-384 and
//! SHA-512.

use sha2::Digest;

pub const ALG_SHA1: u16 = 0x0004;
pub const ALG_SHA256: u16 = 0x000B;
pub const ALG_SHA384: u16 = 0x000C;
pub const ALG_SHA512: u16 = 0x000D;

/// Largest digest of a supported bank.
pub const MAX_DIGEST_SIZE: usize = 64;

/// Digest size of `alg`, or `None` when it is not supported.
pub const fn digest_size(alg: u16) -> Option<usize> {
    match alg {
        ALG_SHA1 => Some(20),
        ALG_SHA256 => Some(32),
        ALG_SHA384 => Some(48),
        ALG_SHA512 => Some(64),
        _ => None,
    }
}

/// A running hash for one bank.
#[derive(Clone)]
pub enum BankHasher {
    Sha1(sha1::Sha1),
    Sha256(sha2::Sha256),
    Sha384(sha2::Sha384),
    Sha512(sha2::Sha512),
}

impl BankHasher {
    pub fn new(alg: u16) -> Option<Self> {
        Some(match alg {
            ALG_SHA1 => Self::Sha1(sha1::Sha1::new()),
            ALG_SHA256 => Self::Sha256(sha2::Sha256::new()),
            ALG_SHA384 => Self::Sha384(sha2::Sha384::new()),
            ALG_SHA512 => Self::Sha512(sha2::Sha512::new()),
            _ => return None,
        })
    }

    pub fn update(&mut self, data: &[u8]) {
        match self {
            Self::Sha1(h) => h.update(data),
            Self::Sha256(h) => h.update(data),
            Self::Sha384(h) => h.update(data),
            Self::Sha512(h) => h.update(data),
        }
    }

    /// Write the digest to the front of `out`; returns its size.
    pub fn finalize(self, out: &mut [u8; MAX_DIGEST_SIZE]) -> usize {
        fn put(out: &mut [u8], digest: &[u8]) -> usize {
            out[..digest.len()].copy_from_slice(digest);
            digest.len()
        }
        match self {
            Self::Sha1(h) => put(out, &h.finalize()),
            Self::Sha256(h) => put(out, &h.finalize()),
            Self::Sha384(h) => put(out, &h.finalize()),
            Self::Sha512(h) => put(out, &h.finalize()),
        }
    }
}

/// Hash `data` with `alg` into `out`; returns the digest size.
pub fn hash(alg: u16, data: &[u8], out: &mut [u8; MAX_DIGEST_SIZE]) -> Option<usize> {
    let mut hasher = BankHasher::new(alg)?;
    hasher.update(data);
    Some(hasher.finalize(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_answers() {
        let mut out = [0; MAX_DIGEST_SIZE];
        assert_eq!(hash(ALG_SHA1, b"abc", &mut out), Some(20));
        assert_eq!(out[..4], [0xa9, 0x99, 0x3e, 0x36]);
        assert_eq!(hash(ALG_SHA384, b"abc", &mut out), Some(48));
        assert_eq!(out[..4], [0xcb, 0x00, 0x75, 0x3f]);
        assert_eq!(hash(0x0012, b"abc", &mut out), None);
    }
}
