//! Security configuration types.
//!
//! These types are used in the board metadata to declare the signing/verification
//! setup. Key material is not board metadata: `fbuild assemble` signs with the
//! key given by `--signing-key` (or an ephemeral one) and writes its public
//! half into the trust block.

use serde::{Deserialize, Serialize};

/// Security configuration for a board.
///
/// Declares which signature algorithm and digest algorithms are used.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecurityConfig {
    /// Signature algorithm for manifest signing.
    pub signing_algorithm: SignatureAlgorithm,
    /// Digest algorithms required for file integrity verification.
    ///
    /// Both may be specified for dual-digest algorithm agility.
    pub required_digests: heapless::Vec<DigestAlgorithm, 4>,
}

/// Supported signature algorithms (algorithm agility).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SignatureAlgorithm {
    Ed25519,
    EcdsaP256,
}

/// Supported digest (hash) algorithms.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DigestAlgorithm {
    Sha256,
    Sha3_256,
}
