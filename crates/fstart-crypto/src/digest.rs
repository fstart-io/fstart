//! Digest (hash) computation for file integrity verification.
//!
//! Provides SHA-256 and SHA3-256 implementations via the RustCrypto crates.
//! Each is behind a feature flag (`sha2-digest`, `sha3-digest`).

use fstart_types::ffs::DigestSet;

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
#[cfg(feature = "sha2-digest")]
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
/// Checks all digests that are present in `expected` AND supported by the
/// enabled feature flags. Returns an error if any enabled digest mismatches.
///
/// Returns `NoAlgorithmAvailable` if digests are present in `expected` but
/// no digest feature flag is enabled — this prevents silent verification
/// bypass when feature flags are misconfigured.
pub fn verify_digest_set(data: &[u8], expected: &DigestSet) -> Result<(), DigestError> {
    // Suppress unused-variable warnings when no features are enabled
    let _ = data;

    let mut verified_count: u32 = 0;

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
    if has_expected && verified_count == 0 {
        return Err(DigestError::NoAlgorithmAvailable);
    }

    Ok(())
}
