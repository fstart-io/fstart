//! Digest (hash) computation for file integrity verification.
//!
//! Provides SHA-256 and SHA3-256 implementations via the RustCrypto crates.
//! SHA-256 uses RustCrypto's compact rolled software backend. Each algorithm
//! is behind a feature flag (`sha2-digest`, `sha3-digest`).

use fstart_core::ffs::DigestSet;

/// Error returned by digest operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DigestError {
    /// No digest algorithm is available (no feature flags enabled).
    NoAlgorithmAvailable,
    /// SHA-256 digest mismatch.
    Sha256Mismatch,
    /// SHA3-256 digest mismatch.
    Sha3Mismatch,
}

/// Compute a SHA-256 digest of `data`.
///
/// Returns a 32-byte digest.
#[cfg(any(feature = "sha2-digest", test))]
pub fn hash_sha256(data: &[u8]) -> [u8; 32] {
    use sha2::Digest;
    let mut hasher = sha2::Sha256::new();
    hasher.update(data);
    let result = hasher.finalize();
    let mut out = [0u8; 32];
    out.copy_from_slice(&result);
    out
}

/// Compute a SHA3-256 digest of `data`.
///
/// Returns a 32-byte digest.
#[cfg(feature = "sha3-digest")]
pub fn hash_sha3_256(data: &[u8]) -> [u8; 32] {
    use sha3::Digest;
    let mut hasher = sha3::Sha3_256::new();
    hasher.update(data);
    let result = hasher.finalize();
    let mut out = [0u8; 32];
    out.copy_from_slice(&result);
    out
}

/// Compute a `DigestSet` containing all available digests for `data`.
///
/// Populates whichever digests are enabled by feature flags.
/// Returns `DigestError::NoAlgorithmAvailable` if no hash feature is enabled.
pub fn hash_digest_set(data: &[u8]) -> Result<DigestSet, DigestError> {
    let set = DigestSet {
        #[cfg(feature = "sha2-digest")]
        sha256: Some(hash_sha256(data)),
        #[cfg(not(feature = "sha2-digest"))]
        sha256: None,

        #[cfg(feature = "sha3-digest")]
        sha3_256: Some(hash_sha3_256(data)),
        #[cfg(not(feature = "sha3-digest"))]
        sha3_256: None,
    };

    // At least one algorithm must be available
    if set.sha256.is_none() && set.sha3_256.is_none() {
        return Err(DigestError::NoAlgorithmAvailable);
    }

    // Suppress unused-variable warning when no features are enabled
    let _ = data;

    Ok(set)
}

/// Verify that `data` matches the digests in `expected`.
///
/// Checks every digest present in `expected`. Empty sets and unsupported
/// advertised algorithms fail closed, as does any digest mismatch.
///
/// Returns `NoAlgorithmAvailable` if digests are present in `expected` but
/// no digest feature flag is enabled — this prevents silent verification
/// bypass when feature flags are misconfigured.
pub fn verify_digest_set(data: &[u8], expected: &DigestSet) -> Result<(), DigestError> {
    // Suppress unused-variable warnings when no features are enabled
    let _ = data;

    #[allow(unused_mut)]
    let mut verified_count: u32 = 0;

    // Every advertised algorithm is mandatory. Unsupported is never success.
    #[cfg(not(feature = "sha2-digest"))]
    if expected.sha256.is_some() {
        return Err(DigestError::NoAlgorithmAvailable);
    }
    #[cfg(not(feature = "sha3-digest"))]
    if expected.sha3_256.is_some() {
        return Err(DigestError::NoAlgorithmAvailable);
    }

    #[cfg(feature = "sha2-digest")]
    if let Some(ref expected_sha256) = expected.sha256 {
        let actual = hash_sha256(data);
        if actual != *expected_sha256 {
            return Err(DigestError::Sha256Mismatch);
        }
        verified_count += 1;
    }

    #[cfg(feature = "sha3-digest")]
    if let Some(ref expected_sha3) = expected.sha3_256 {
        let actual = hash_sha3_256(data);
        if actual != *expected_sha3 {
            return Err(DigestError::Sha3Mismatch);
        }
        verified_count += 1;
    }

    // If digests were expected but none could be verified (no features enabled
    // for the algorithms present in the digest set), fail rather than silently
    // passing — this catches feature flag misconfiguration.
    let has_expected = expected.sha256.is_some() || expected.sha3_256.is_some();
    if !has_expected || verified_count == 0 {
        return Err(DigestError::NoAlgorithmAvailable);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::hash_sha256;

    #[test]
    fn empty_and_unsupported_digest_sets_fail_closed() {
        use super::{DigestError, verify_digest_set};
        use fstart_core::ffs::DigestSet;
        assert_eq!(
            verify_digest_set(
                b"",
                &DigestSet {
                    sha256: None,
                    sha3_256: None
                }
            ),
            Err(DigestError::NoAlgorithmAvailable)
        );
        #[cfg(not(feature = "sha3-digest"))]
        assert_eq!(
            verify_digest_set(
                b"abc",
                &DigestSet {
                    sha256: Some(hash_sha256(b"abc")),
                    sha3_256: Some([0; 32])
                }
            ),
            Err(DigestError::NoAlgorithmAvailable)
        );
    }

    #[test]
    fn compact_sha256_matches_known_answers() {
        let cases: &[(&[u8], [u8; 32])] = &[
            (
                b"",
                [
                    0xe3, 0xb0, 0xc4, 0x42, 0x98, 0xfc, 0x1c, 0x14, 0x9a, 0xfb, 0xf4, 0xc8, 0x99,
                    0x6f, 0xb9, 0x24, 0x27, 0xae, 0x41, 0xe4, 0x64, 0x9b, 0x93, 0x4c, 0xa4, 0x95,
                    0x99, 0x1b, 0x78, 0x52, 0xb8, 0x55,
                ],
            ),
            (
                b"abc",
                [
                    0xba, 0x78, 0x16, 0xbf, 0x8f, 0x01, 0xcf, 0xea, 0x41, 0x41, 0x40, 0xde, 0x5d,
                    0xae, 0x22, 0x23, 0xb0, 0x03, 0x61, 0xa3, 0x96, 0x17, 0x7a, 0x9c, 0xb4, 0x10,
                    0xff, 0x61, 0xf2, 0x00, 0x15, 0xad,
                ],
            ),
            (
                b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq",
                [
                    0x24, 0x8d, 0x6a, 0x61, 0xd2, 0x06, 0x38, 0xb8, 0xe5, 0xc0, 0x26, 0x93, 0x0c,
                    0x3e, 0x60, 0x39, 0xa3, 0x3c, 0xe4, 0x59, 0x64, 0xff, 0x21, 0x67, 0xf6, 0xec,
                    0xed, 0xd4, 0x19, 0xdb, 0x06, 0xc1,
                ],
            ),
        ];

        for (input, expected) in cases {
            assert_eq!(hash_sha256(input), *expected);
        }

        assert_eq!(
            hash_sha256(&[0xa5; 4097]),
            [
                0xce, 0x38, 0x85, 0x9d, 0x38, 0x7e, 0x37, 0xa0, 0x65, 0x97, 0xde, 0xb8, 0x07, 0x2e,
                0x12, 0x4e, 0xfe, 0xa0, 0x55, 0x53, 0xa6, 0x84, 0x2c, 0xa0, 0x31, 0x26, 0xf6, 0x22,
                0x25, 0xe5, 0x88, 0x19,
            ]
        );
    }
}
