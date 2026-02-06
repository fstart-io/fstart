//! Cryptographic primitives for fstart firmware verification.
//!
//! All implementations are `no_std` compatible. Each algorithm family is
//! behind a feature flag:
//!
//! - `sha2-digest` — SHA-256 hashing
//! - `sha3-digest` — SHA3-256 hashing
//! - `ed25519` — Ed25519 signature verification
//! - `ecdsa-p256` — ECDSA P-256 signature verification
//!
//! The `all` feature enables everything.
//!
//! ## Usage
//!
//! ```ignore
//! use fstart_crypto::{hash_sha256, verify_signature};
//! use fstart_types::ffs::{Signature, VerificationKey};
//!
//! let digest = hash_sha256(data);
//! let ok = verify_signature(manifest_bytes, &signature, &key);
//! ```

#![no_std]

pub mod digest;
pub mod verify;

pub use digest::{hash_digest_set, DigestError};
pub use verify::{verify_signature, VerifyError};

// Re-export individual hash functions when available.
#[cfg(feature = "sha2-digest")]
pub use digest::hash_sha256;

#[cfg(feature = "sha3-digest")]
pub use digest::hash_sha3_256;
