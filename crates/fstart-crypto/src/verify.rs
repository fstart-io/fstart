//! Signature verification for manifest authentication.
//!
//! Supports Ed25519 and ECDSA P-256, each behind a feature flag.
//! The `verify_signature` function dispatches to the correct algorithm
//! based on the `Signature::kind` field.

use fstart_types::ffs::{Signature, SignatureKind, VerificationKey};

/// Error returned by signature verification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerifyError {
    /// Signature is invalid (verification failed).
    InvalidSignature,
    /// The key algorithm doesn't match the signature algorithm.
    AlgorithmMismatch,
    /// The requested algorithm feature is not compiled in.
    UnsupportedAlgorithm,
    /// The key material is malformed.
    InvalidKey,
    /// No key with the required `key_id` was found.
    KeyNotFound,
}

/// Verify a signature over `message` using the given key.
///
/// Dispatches to Ed25519 or ECDSA P-256 based on `signature.kind`.
/// Returns `Ok(())` if the signature is valid, or an error otherwise.
///
/// # Errors
///
/// - `AlgorithmMismatch` if `key.algorithm != signature.kind`.
/// - `UnsupportedAlgorithm` if the feature for the algorithm isn't enabled.
/// - `InvalidKey` if the key material can't be deserialized.
/// - `InvalidSignature` if the cryptographic verification fails.
pub fn verify_signature(
    message: &[u8],
    signature: &Signature,
    key: &VerificationKey,
) -> Result<(), VerifyError> {
    // Check that the key algorithm matches the signature algorithm
    let key_matches = matches!(
        (key.algorithm, signature.kind),
        (SignatureKind::Ed25519, SignatureKind::Ed25519)
            | (SignatureKind::EcdsaP256, SignatureKind::EcdsaP256)
    );
    if !key_matches {
        return Err(VerifyError::AlgorithmMismatch);
    }

    match signature.kind {
        SignatureKind::Ed25519 => verify_ed25519(message, signature, key),
        SignatureKind::EcdsaP256 => verify_ecdsa_p256(message, signature, key),
    }
}

/// Find a key by `key_id` from a slice of keys and verify the signature.
///
/// Convenience wrapper that searches the anchor's key list for a matching
/// key_id before calling `verify_signature`.
pub fn verify_with_key_lookup(
    message: &[u8],
    signature: &Signature,
    keys: &[VerificationKey],
) -> Result<(), VerifyError> {
    let key = keys
        .iter()
        .find(|k| k.key_id == signature.key_id)
        .ok_or(VerifyError::KeyNotFound)?;
    verify_signature(message, signature, key)
}

// ---- Ed25519 ----

#[cfg(feature = "ed25519")]
fn verify_ed25519(
    message: &[u8],
    signature: &Signature,
    key: &VerificationKey,
) -> Result<(), VerifyError> {
    use ed25519_dalek::{Signature as Ed25519Sig, Verifier, VerifyingKey};

    let pubkey_bytes = key.key_lo; // Ed25519 uses only the lower 32 bytes
    let verifying_key =
        VerifyingKey::from_bytes(&pubkey_bytes).map_err(|_| VerifyError::InvalidKey)?;

    let sig_bytes = signature.signature_bytes();
    let sig = Ed25519Sig::from_bytes(&sig_bytes);

    verifying_key
        .verify(message, &sig)
        .map_err(|_| VerifyError::InvalidSignature)
}

#[cfg(not(feature = "ed25519"))]
fn verify_ed25519(
    _message: &[u8],
    _signature: &Signature,
    _key: &VerificationKey,
) -> Result<(), VerifyError> {
    Err(VerifyError::UnsupportedAlgorithm)
}

// ---- ECDSA P-256 ----

#[cfg(feature = "ecdsa-p256")]
fn verify_ecdsa_p256(
    message: &[u8],
    signature: &Signature,
    key: &VerificationKey,
) -> Result<(), VerifyError> {
    use p256::ecdsa::{signature::Verifier, Signature as P256Sig, VerifyingKey};
    use p256::EncodedPoint;

    // Reconstruct 64-byte uncompressed public key (x || y)
    let mut pubkey_uncompressed = [0u8; 65]; // 0x04 prefix + 64 bytes
    pubkey_uncompressed[0] = 0x04; // uncompressed point marker
    pubkey_uncompressed[1..33].copy_from_slice(&key.key_lo);
    pubkey_uncompressed[33..65].copy_from_slice(&key.key_hi);

    let point =
        EncodedPoint::from_bytes(pubkey_uncompressed).map_err(|_| VerifyError::InvalidKey)?;
    let verifying_key =
        VerifyingKey::from_encoded_point(&point).map_err(|_| VerifyError::InvalidKey)?;

    // r and s are in sig_lo and sig_hi respectively
    let sig = P256Sig::from_scalars(signature.sig_lo, signature.sig_hi)
        .map_err(|_| VerifyError::InvalidSignature)?;

    verifying_key
        .verify(message, &sig)
        .map_err(|_| VerifyError::InvalidSignature)
}

#[cfg(not(feature = "ecdsa-p256"))]
fn verify_ecdsa_p256(
    _message: &[u8],
    _signature: &Signature,
    _key: &VerificationKey,
) -> Result<(), VerifyError> {
    Err(VerifyError::UnsupportedAlgorithm)
}
