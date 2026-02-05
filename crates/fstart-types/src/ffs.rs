//! Firmware Filesystem (FFS) types.
//!
//! The FFS is a flat filesystem stored in ROM. It consists of a header,
//! a signed manifest (table of contents with digests), and file data blobs.
//! All types are serde-compatible for postcard serialization.

use heapless::String as HString;
use serde::{Deserialize, Serialize};

/// Magic bytes for FFS identification.
pub const FFS_MAGIC: [u8; 8] = *b"FSTART01";

/// FFS header — located at a known offset in the firmware image.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FfsHeader {
    /// Magic bytes
    pub magic: [u8; 8],
    /// Format version
    pub version: u16,
    /// Offset of the signed manifest from image start
    pub manifest_offset: u32,
    /// Size of the signed manifest in bytes
    pub manifest_size: u32,
    /// Total image size
    pub total_size: u32,
}

/// The manifest — a file table listing all files in the FFS.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub entries: heapless::Vec<FileEntry, 32>,
}

/// A single file entry in the manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileEntry {
    /// File name / identifier
    pub name: HString<64>,
    /// Type of file
    pub file_type: FileType,
    /// Offset of file data from image start
    pub offset: u32,
    /// Size after compression
    pub compressed_size: u32,
    /// Original size before compression
    pub original_size: u32,
    /// Compression algorithm
    pub compression: Compression,
    /// Integrity digests (over *uncompressed* data)
    pub digests: DigestSet,
}

/// File type in the FFS.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FileType {
    /// Executable stage binary (raw, not ELF)
    StageCode,
    /// Board configuration (postcard-serialized BoardConfig)
    BoardConfig,
    /// OS kernel or other payload
    Payload,
    /// Flattened Device Tree blob
    Fdt,
    /// Generic data
    Data,
}

/// Compression algorithm.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Compression {
    None,
    Lz4,
}

/// Set of digests for integrity verification (algorithm agility).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DigestSet {
    pub sha256: Option<[u8; 32]>,
    pub sha3_256: Option<[u8; 32]>,
}

/// A manifest bundled with its cryptographic signature.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignedManifest {
    /// Raw postcard-encoded Manifest bytes
    pub manifest_bytes: heapless::Vec<u8, 4096>,
    /// Signature over manifest_bytes
    pub signature: Signature,
}

/// Cryptographic signature (algorithm-agile).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Signature {
    Ed25519 {
        /// First 32 bytes of the Ed25519 signature
        sig_lo: [u8; 32],
        /// Last 32 bytes of the Ed25519 signature
        sig_hi: [u8; 32],
    },
    EcdsaP256 {
        r: [u8; 32],
        s: [u8; 32],
    },
}

impl Signature {
    /// Create an Ed25519 signature from a 64-byte array.
    pub fn ed25519(bytes: [u8; 64]) -> Self {
        let mut lo = [0u8; 32];
        let mut hi = [0u8; 32];
        lo.copy_from_slice(&bytes[..32]);
        hi.copy_from_slice(&bytes[32..]);
        Self::Ed25519 {
            sig_lo: lo,
            sig_hi: hi,
        }
    }

    /// For Ed25519 signatures, reconstruct the 64-byte signature.
    pub fn ed25519_bytes(&self) -> Option<[u8; 64]> {
        match self {
            Self::Ed25519 { sig_lo, sig_hi } => {
                let mut out = [0u8; 64];
                out[..32].copy_from_slice(sig_lo);
                out[32..].copy_from_slice(sig_hi);
                Some(out)
            }
            _ => None,
        }
    }
}
