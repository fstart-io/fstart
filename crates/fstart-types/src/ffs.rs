//! Firmware Filesystem (FFS) types.
//!
//! The FFS describes the layout of a firmware flash image. The design is
//! driven by these constraints:
//!
//! - The **anchor block** is embedded directly in the bootblock binary.
//!   It is NOT loaded from flash via a driver — the bootblock executes in
//!   place (XIP) from memory-mapped flash, so the anchor is just part of
//!   the code image. The CPU sees it as ordinary memory at reset. External
//!   tools scan the binary for the `FFS_MAGIC` to locate the anchor.
//!
//! - The anchor contains a pointer to the **RO manifest** (also in the same
//!   flash region) and **embedded public keys** used to verify all manifests.
//!
//! ## Flash layout
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────┐
//! │ Bootblock (XIP from flash)                              │
//! │  ┌────────────────────────────────────────────────────┐  │
//! │  │ Anchor Block (embedded in bootblock binary)        │  │
//! │  │  • MAGIC: "FSTART01"                               │  │
//! │  │  • pointer → RO manifest                           │  │
//! │  │  • embedded verification keys                      │  │
//! │  └────────────────────────────────────────────────────┘  │
//! │  … bootblock code …                                     │
//! ├─────────────────────────────────────────────────────────┤
//! │ RO Region (immutable firmware)                          │
//! │  • RO Manifest (signed)  ← anchor points here           │
//! │  • immutable files (stages, data, …)                    │
//! │  • optional pointers → RW-A, RW-B manifests             │
//! ├─────────────────────────────────────────────────────────┤
//! │ RW-A Region (optional, updatable)                       │
//! │  • RW Manifest (signed by keys in anchor)               │
//! │  • stage code, payloads, data                           │
//! ├─────────────────────────────────────────────────────────┤
//! │ RW-B Region (optional, A/B safe update)                 │
//! │  • RW Manifest (signed by keys in anchor)               │
//! │  • stage code, payloads, data                           │
//! ├─────────────────────────────────────────────────────────┤
//! │ NVS Region (optional, plain storage)                    │
//! └─────────────────────────────────────────────────────────┘
//! ```
//!
//! The RW regions are optional:
//! - **RO-only**: no RW slots, everything is immutable.
//! - **RO + RW**: single updatable slot (no A/B redundancy).
//! - **RO + RW-A + RW-B**: A/B scheme for safe firmware updates.
//!
//! ## Files and segments
//!
//! Each file in a manifest can have multiple **segments** (e.g., `.text`
//! and `.data` for a stage binary). Each segment may be independently
//! compressed and carries metadata for future paging (code vs data,
//! read-only vs read-write).
//!
//! ## Monolithic images
//!
//! For simple monolithic builds (single stage, no bootblock separation),
//! the anchor is still embedded in the binary. The RO manifest just lists
//! all files. There are no RW slots. This keeps the format uniform — the
//! same reader code works for both monolithic and multi-stage images.
//!
//! All types are serde-compatible for postcard serialization.

use heapless::String as HString;
use serde::{Deserialize, Serialize};

// ============================================================================
// Magic and version
// ============================================================================

/// Magic bytes for anchor block identification.
///
/// Tools scanning a binary for the fstart anchor search for these 8 bytes
/// at 8-byte-aligned offsets. The anchor is embedded in the bootblock
/// binary, so this is found by scanning the XIP code image.
pub const FFS_MAGIC: [u8; 8] = *b"FSTART01";

/// Current FFS format version.
pub const FFS_VERSION: u16 = 2;

// ============================================================================
// Anchor block — embedded in the bootblock binary
// ============================================================================

/// The anchor block is embedded in the bootblock binary at build time.
///
/// Because the bootblock executes in place (XIP) from memory-mapped flash,
/// the anchor is accessible as plain memory — no SPI driver, no flash
/// reads, just a pointer dereference from the bootblock's own address space.
///
/// **How it gets there**: codegen emits the anchor as a `#[link_section]`
/// static in the bootblock. The linker places it at a known offset. The
/// `xtask assemble` step patches the `ro_manifest_offset` and sizes after
/// the full image is laid out.
///
/// **How tools find it**: scan the firmware binary for `FFS_MAGIC` at
/// 8-byte-aligned offsets. Once found, deserialize the anchor to get the
/// RO manifest pointer and verification keys.
///
/// **How the bootblock uses it**: the bootblock code references the static
/// directly (no scanning needed — it knows its own symbol). It reads the
/// manifest pointer, walks to the RO manifest in flash, and verifies it
/// using the embedded keys.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnchorBlock {
    /// Magic bytes — must equal `FFS_MAGIC`.
    pub magic: [u8; 8],
    /// Format version — must equal `FFS_VERSION`.
    pub version: u16,
    /// Offset of the RO signed manifest from the image base (bytes).
    ///
    /// The bootblock adds this to the flash base address to get a pointer.
    /// For monolithic images this is relative to the start of the binary.
    pub ro_manifest_offset: u32,
    /// Size of the serialized `SignedManifest` in bytes.
    pub ro_manifest_size: u32,
    /// Total firmware image size in bytes (all regions combined).
    pub total_image_size: u32,
    /// Base offset of the RO region's file data from the image base.
    ///
    /// The reader needs this to compute absolute offsets for RO segments
    /// (segment offsets are relative to the region base, not image base).
    pub ro_region_base: u32,
    /// Verification keys for manifest signatures.
    ///
    /// These keys verify the RO manifest. The RO manifest in turn contains
    /// pointers to RW regions whose manifests are verified by the same keys.
    ///
    /// Up to 4 keys for key rotation / algorithm agility.
    pub keys: heapless::Vec<VerificationKey, 4>,
}

/// A public key embedded in the anchor for manifest verification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationKey {
    /// Key identifier (matches `key_id` in `Signature` for key selection).
    pub key_id: u8,
    /// Algorithm this key is for.
    pub algorithm: SignatureKind,
    /// Key material — split for serde compatibility (no `[u8; 64]`).
    ///
    /// Ed25519 public keys are 32 bytes (only `key_lo` used).
    /// ECDSA P-256 public keys are 64 bytes (uncompressed x||y).
    pub key_lo: [u8; 32],
    /// Upper 32 bytes of key material (ECDSA P-256 y coordinate).
    /// Zeroed for Ed25519.
    pub key_hi: [u8; 32],
}

impl VerificationKey {
    /// Create an Ed25519 verification key.
    pub fn ed25519(key_id: u8, pubkey: [u8; 32]) -> Self {
        Self {
            key_id,
            algorithm: SignatureKind::Ed25519,
            key_lo: pubkey,
            key_hi: [0u8; 32],
        }
    }

    /// Create an ECDSA P-256 verification key from 64-byte uncompressed point.
    pub fn ecdsa_p256(key_id: u8, pubkey: [u8; 64]) -> Self {
        let mut lo = [0u8; 32];
        let mut hi = [0u8; 32];
        lo.copy_from_slice(&pubkey[..32]);
        hi.copy_from_slice(&pubkey[32..]);
        Self {
            key_id,
            algorithm: SignatureKind::EcdsaP256,
            key_lo: lo,
            key_hi: hi,
        }
    }

    /// Reconstruct the full public key bytes.
    pub fn key_bytes(&self) -> KeyBytes {
        match self.algorithm {
            SignatureKind::Ed25519 => KeyBytes::Ed25519(self.key_lo),
            SignatureKind::EcdsaP256 => {
                let mut out = [0u8; 64];
                out[..32].copy_from_slice(&self.key_lo);
                out[32..].copy_from_slice(&self.key_hi);
                KeyBytes::EcdsaP256(out)
            }
        }
    }
}

/// Reconstructed public key bytes.
#[derive(Debug, Clone)]
pub enum KeyBytes {
    /// 32-byte Ed25519 public key.
    Ed25519([u8; 32]),
    /// 64-byte ECDSA P-256 uncompressed public key (x || y).
    EcdsaP256([u8; 64]),
}

// ============================================================================
// Signed manifest
// ============================================================================

/// A manifest bundled with its cryptographic signature.
///
/// The `manifest_bytes` field contains the raw postcard-encoded `Manifest`.
/// The signature covers exactly those bytes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignedManifest {
    /// Raw postcard-encoded `Manifest` bytes.
    pub manifest_bytes: heapless::Vec<u8, 8192>,
    /// Cryptographic signature over `manifest_bytes`.
    pub signature: Signature,
}

// ============================================================================
// Manifest — the file table for a region
// ============================================================================

/// A manifest — the file table for one region (RO or RW).
///
/// The RO manifest lists immutable files and optionally points to RW
/// regions. RW manifests list only their own region's files.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    /// Which region this manifest describes.
    pub region: RegionRole,
    /// Files in this region.
    pub entries: heapless::Vec<FileEntry, 32>,
    /// Pointers to RW regions (RO manifests only).
    ///
    /// - Empty: RO-only image (no updatable firmware).
    /// - 1 entry (`Rw`): single updatable slot, no A/B redundancy.
    /// - 2 entries (`RwA` + `RwB`): A/B scheme for safe updates.
    pub rw_slots: heapless::Vec<RwSlotPointer, 2>,
    /// NVS region pointer (RO manifests only, optional).
    pub nvs: Option<NvsPointer>,
}

/// Role of the region a manifest describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RegionRole {
    /// Read-only region (immutable firmware).
    Ro,
    /// Read-write slot A (updatable, A/B scheme).
    RwA,
    /// Read-write slot B (updatable, A/B scheme).
    RwB,
    /// Single read-write slot (updatable, no A/B).
    Rw,
}

/// Pointer from an RO manifest to an RW slot's signed manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RwSlotPointer {
    /// Which slot this points to.
    pub role: RegionRole,
    /// Offset of the RW `SignedManifest` from image base.
    pub manifest_offset: u32,
    /// Size of the RW `SignedManifest` in bytes.
    pub manifest_size: u32,
    /// Base offset of the RW region's file data from image base.
    pub region_base: u32,
    /// Total size of the RW region in bytes (for bounds checking).
    pub region_size: u32,
}

/// Pointer to a non-volatile storage region.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NvsPointer {
    /// Offset of the NVS region from image base.
    pub offset: u32,
    /// Size of the NVS region in bytes.
    pub size: u32,
}

// ============================================================================
// File entries
// ============================================================================

/// A single file entry in the manifest.
///
/// A file may have multiple segments (e.g., `.text` code + `.data`
/// initialized data + `.bss` zero-fill). Each segment can be independently
/// compressed and carries load metadata for future paging support.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileEntry {
    /// File name / identifier (e.g., `"bootblock"`, `"main-stage"`,
    /// `"vmlinux"`).
    pub name: HString<64>,
    /// Type of file.
    pub file_type: FileType,
    /// Segments that make up this file.
    pub segments: heapless::Vec<Segment, 8>,
    /// Integrity digests over the concatenated uncompressed segment data.
    ///
    /// Computed as: `digest(seg0_uncompressed || seg1_uncompressed || ...)`.
    /// This provides a single whole-file digest for verification.
    pub digests: DigestSet,
}

/// Type of file in the FFS.
///
/// File type influences how the loader treats the file (e.g., code files
/// may need I-cache invalidation after loading, NVS regions are plain
/// read/write storage not covered by code-integrity checks).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FileType {
    /// Executable stage binary — loaded and jumped to.
    StageCode,
    /// Board configuration (postcard-serialized `BoardConfig`).
    BoardConfig,
    /// OS kernel or other payload.
    Payload,
    /// Flattened Device Tree blob.
    Fdt,
    /// Generic data blob (read-only).
    Data,
    /// Non-volatile storage region — plain read/write, not executable.
    /// Used for runtime settings, boot counters, etc.
    Nvs,
    /// Raw region — a plain area with no special semantics.
    Raw,
}

// ============================================================================
// Segments — sub-file load units
// ============================================================================

/// A segment within a file — one contiguous load unit.
///
/// Binary files (stages, payloads) are composed of segments analogous to
/// ELF program headers. Each segment has a content kind (code or data),
/// a load address, independent compression, and permission flags for
/// future paging / MPU support.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Segment {
    /// Segment name (e.g., `".text"`, `".data"`, `".rodata"`, `".bss"`).
    pub name: HString<32>,
    /// What kind of content this segment contains.
    pub kind: SegmentKind,
    /// Offset of the segment's (possibly compressed) data from the start
    /// of the **region** (not from image base — the region base is added
    /// by the reader).
    pub offset: u32,
    /// Size of segment data as stored in flash (after compression).
    pub stored_size: u32,
    /// Size of segment data after decompression (original size).
    /// For BSS segments this is the zero-fill size; `stored_size` is 0.
    pub loaded_size: u32,
    /// Load address — where this segment should be placed in memory.
    pub load_addr: u64,
    /// Compression algorithm used on this segment's data.
    pub compression: Compression,
    /// Memory permission flags — hints for paging / MPU setup.
    pub flags: SegmentFlags,
}

/// Segment content kind — influences cache management and paging.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SegmentKind {
    /// Executable code — may need I-cache invalidation after loading.
    Code,
    /// Initialized read-only data.
    ReadOnlyData,
    /// Initialized read-write data.
    ReadWriteData,
    /// Uninitialized data (`.bss`) — zero-filled, not stored in flash.
    /// `stored_size` should be 0 for BSS segments.
    Bss,
}

/// Memory permission flags for a segment (hints for paging / MPU).
///
/// These are used by stages that set up page tables or MPU regions.
/// Firmware running without memory protection can ignore them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SegmentFlags {
    /// Segment contains executable code.
    pub execute: bool,
    /// Segment is writable.
    pub write: bool,
    /// Segment is readable (always true in practice).
    pub read: bool,
}

impl SegmentFlags {
    /// Code segment: read + execute, no write.
    pub const CODE: Self = Self {
        execute: true,
        write: false,
        read: true,
    };

    /// Read-only data: read only, no write, no execute.
    pub const RODATA: Self = Self {
        execute: false,
        write: false,
        read: true,
    };

    /// Read-write data: read + write, no execute.
    pub const DATA: Self = Self {
        execute: false,
        write: true,
        read: true,
    };
}

impl Default for SegmentFlags {
    fn default() -> Self {
        Self::DATA
    }
}

/// Compression algorithm.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Compression {
    /// No compression.
    None,
    /// LZ4 block compression (fast decompression, moderate ratio).
    Lz4,
}

// ============================================================================
// Digests
// ============================================================================

/// Set of digests for integrity verification (algorithm agility).
///
/// Both digests may be present simultaneously for dual-digest verification.
/// At least one must be present for any file in a verified manifest.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DigestSet {
    /// SHA-256 digest (32 bytes).
    pub sha256: Option<[u8; 32]>,
    /// SHA3-256 digest (32 bytes).
    pub sha3_256: Option<[u8; 32]>,
}

// ============================================================================
// Signatures
// ============================================================================

/// Which signature algorithm is used.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SignatureKind {
    /// Ed25519 (64-byte signature, 32-byte public key).
    Ed25519,
    /// ECDSA over P-256 / secp256r1 (r,s each 32 bytes).
    EcdsaP256,
}

/// Cryptographic signature over a manifest (algorithm-agile).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Signature {
    /// Which key was used to produce this signature (matches anchor key_id).
    pub key_id: u8,
    /// Algorithm used.
    pub kind: SignatureKind,
    /// First 32 bytes of the signature.
    ///
    /// Ed25519: first half of the 64-byte signature.
    /// ECDSA P-256: the `r` component.
    pub sig_lo: [u8; 32],
    /// Last 32 bytes of the signature.
    ///
    /// Ed25519: second half of the 64-byte signature.
    /// ECDSA P-256: the `s` component.
    pub sig_hi: [u8; 32],
}

impl Signature {
    /// Create an Ed25519 signature from a 64-byte array.
    pub fn ed25519(key_id: u8, bytes: [u8; 64]) -> Self {
        let mut lo = [0u8; 32];
        let mut hi = [0u8; 32];
        lo.copy_from_slice(&bytes[..32]);
        hi.copy_from_slice(&bytes[32..]);
        Self {
            key_id,
            kind: SignatureKind::Ed25519,
            sig_lo: lo,
            sig_hi: hi,
        }
    }

    /// Create an ECDSA P-256 signature from r and s components.
    pub fn ecdsa_p256(key_id: u8, r: [u8; 32], s: [u8; 32]) -> Self {
        Self {
            key_id,
            kind: SignatureKind::EcdsaP256,
            sig_lo: r,
            sig_hi: s,
        }
    }

    /// Reconstruct the 64-byte signature (Ed25519) or r||s (ECDSA).
    pub fn signature_bytes(&self) -> [u8; 64] {
        let mut out = [0u8; 64];
        out[..32].copy_from_slice(&self.sig_lo);
        out[32..].copy_from_slice(&self.sig_hi);
        out
    }
}

// ============================================================================
// Legacy compat — simple single-region header
// ============================================================================

/// Simple FFS header for single-region (RO-only) images without keys.
///
/// **Prefer `AnchorBlock` for new designs.** This type exists for simple
/// test images where the full anchor + key machinery isn't needed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FfsHeader {
    /// Magic bytes.
    pub magic: [u8; 8],
    /// Format version.
    pub version: u16,
    /// Offset of the signed manifest from image start.
    pub manifest_offset: u32,
    /// Size of the signed manifest in bytes.
    pub manifest_size: u32,
    /// Total image size.
    pub total_size: u32,
}
