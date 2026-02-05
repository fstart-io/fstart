//! Security configuration types.

use heapless::String as HString;
use serde::{Deserialize, Serialize};

/// Security configuration for a board.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityConfig {
    /// Signature algorithm for manifest signing
    pub signing_algorithm: SignatureAlgorithm,
    /// Path to the public key file (relative to board directory)
    pub pubkey_file: HString<128>,
    /// Digest algorithms required for file integrity
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
