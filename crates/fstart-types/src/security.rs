//! Security configuration types.
//!
//! These types are used in the board RON to declare the signing/verification
//! setup. At build time, codegen reads the public key file and embeds the
//! key material into the anchor block inside the bootblock binary.

use heapless::String as HString;
use serde::{Deserialize, Serialize};

/// Security configuration for a board.
///
/// Declares which signature algorithm and digest algorithms are used,
/// and where to find the key material. The public key is embedded in
/// the anchor block at build time; the private key is used by `xtask
/// assemble` to sign manifests.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityConfig {
    /// Signature algorithm for manifest signing.
    pub signing_algorithm: SignatureAlgorithm,
    /// Path to the public key file (relative to board directory).
    ///
    /// At build time the key is read from this file and embedded in the
    /// anchor block (inside the bootblock binary). The bootblock uses
    /// this embedded key to verify manifests without needing any driver.
    pub pubkey_file: HString<128>,
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
