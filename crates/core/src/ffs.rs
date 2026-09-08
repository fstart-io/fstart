//! Firmware Filesystem (FFS) types — unified region model.
//!
//! The FFS describes the layout of a firmware flash image. Every meaningful
//! byte range is modeled as a **region**. The image is a flat list of
//! top-level regions, each of which is either:
//!
//! - A **Container** holding named file entries (stages, configs, etc.)
//! - A **Raw** reserved area (NVS, scratch pads)
//!
//! ## Flash layout
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────┐
//! │ Bootblock (XIP from flash)                              │
//! │  ┌────────────────────────────────────────────────────┐  │
//! │  │ Anchor Block (embedded in bootblock binary)        │  │
//! │  │  • MAGIC: "FSTART01"                               │  │
//! │  │  • pointer → signed 512-byte boot root              │  │
//! │  │  • location only; constant trust is separate       │  │
//! │  └────────────────────────────────────────────────────┘  │
//! │  … bootblock code …                                     │
//! ├─────────────────────────────────────────────────────────┤
//! │ Additional RO files (stages, data, …)                   │
//! ├─────────────────────────────────────────────────────────┤
//! │ RW-A files (optional, updatable)                        │
//! ├─────────────────────────────────────────────────────────┤
//! │ RW-B files (optional, A/B safe update)                  │
//! ├─────────────────────────────────────────────────────────┤
//! │ NVS region (optional, raw 0xFF-filled)                  │
//! ├─────────────────────────────────────────────────────────┤
//! │ Directory (flat tables), then signed boot root          │
//! └─────────────────────────────────────────────────────────┘
//! ```
//!
//! ## Unified region model
//!
//! Instead of separate `Manifest`, `RwSlotPointer`, and `NvsPointer` types,
//! everything is a `Region` with a `RegionContent` enum:
//!
//! - `Container { children }` — a signed collection of file entries
//! - `Raw { fill }` — reserved space filled with a constant byte
//!
//! The ergonomic manifest is encoded into revision-2 directory tables and
//! authenticated by the root's SHA-256 reference, not a second signature.
//! Exact root and directory wire definitions live in fstart-ffs.

pub mod locator;
pub mod trust;

use heapless::String as HString;
use serde::{Deserialize, Serialize};

// ============================================================================
// Magic and version
// ============================================================================

/// Location-only initial-image wire format. Version 6 combined anchors are not decoded.
pub use locator::{
    LOCATOR_MAGIC as FFS_MAGIC, LOCATOR_SIZE as ANCHOR_SIZE, LOCATOR_VERSION as FFS_VERSION,
    LocatorBlock as AnchorBlock, LocatorRef as AnchorRef,
};

/// Bounded key capacity of the independent constant trust policy.
pub const TRUST_MAX_KEYS: usize = 4;

/// A public key embedded in constant policy for root verification.
///
/// `#[repr(C)]` fixed layout — 68 bytes per key.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct VerificationKey {
    /// Key identifier (matches `key_id` in `Signature` for key selection).
    pub key_id: u8,
    /// Algorithm this key is for (see `SignatureKind` ordinals).
    /// 0 = Ed25519, 1 = EcdsaP256.
    pub algorithm: u8,
    /// Padding for alignment.
    pub _pad: [u8; 2],
    /// Key material — lower 32 bytes.
    ///
    /// Ed25519 public keys are 32 bytes (only `key_lo` used).
    /// ECDSA P-256 public keys are 64 bytes (uncompressed x||y).
    pub key_lo: [u8; 32],
    /// Upper 32 bytes of key material (ECDSA P-256 y coordinate).
    /// Zeroed for Ed25519.
    pub key_hi: [u8; 32],
}

/// Algorithm byte values for `VerificationKey::algorithm`.
impl VerificationKey {
    /// Algorithm byte for Ed25519.
    pub const ALG_ED25519: u8 = 0;
    /// Algorithm byte for ECDSA P-256.
    pub const ALG_ECDSA_P256: u8 = 1;

    /// A zeroed key (used to fill unused slots in constant policy).
    pub const ZERO: Self = Self {
        key_id: 0,
        algorithm: 0,
        _pad: [0; 2],
        key_lo: [0; 32],
        key_hi: [0; 32],
    };

    /// Create an Ed25519 verification key.
    pub fn ed25519(key_id: u8, pubkey: [u8; 32]) -> Self {
        Self {
            key_id,
            algorithm: Self::ALG_ED25519,
            _pad: [0; 2],
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
            algorithm: Self::ALG_ECDSA_P256,
            _pad: [0; 2],
            key_lo: lo,
            key_hi: hi,
        }
    }

    /// Get the algorithm as a `SignatureKind`.
    pub fn signature_kind(&self) -> Option<SignatureKind> {
        match self.algorithm {
            Self::ALG_ED25519 => Some(SignatureKind::Ed25519),
            Self::ALG_ECDSA_P256 => Some(SignatureKind::EcdsaP256),
            _ => None,
        }
    }

    /// Reconstruct the full public key bytes.
    pub fn key_bytes(&self) -> KeyBytes {
        match self.algorithm {
            Self::ALG_ED25519 => KeyBytes::Ed25519(self.key_lo),
            Self::ALG_ECDSA_P256 => {
                let mut out = [0u8; 64];
                out[..32].copy_from_slice(&self.key_lo);
                out[32..].copy_from_slice(&self.key_hi);
                KeyBytes::EcdsaP256(out)
            }
            _ => KeyBytes::Ed25519([0; 32]), // unknown algorithm
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
// Image manifest — the top-level region table
// ============================================================================

/// The image manifest — top-level table of all regions in the firmware image.
///
/// This replaces the old `Manifest` type. Instead of separate fields for
/// RO entries, RW slot pointers, and NVS pointers, everything is a `Region`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImageManifest {
    /// All top-level regions in the image.
    ///
    /// Typically: one Container "ro", optionally Container "rw-a"/"rw-b",
    /// optionally Raw "nvs". Descriptor-based x86 images may also list
    /// descriptor/GbE/ME raw regions that are outside the BIOS image.
    ///
    /// Keep the bound modest: this type is used in firmware without `alloc`,
    /// and larger `heapless` capacities directly increase stack/BSS pressure.
    pub regions: heapless::Vec<Region, 6>,
}

// ============================================================================
// Regions — the unified abstraction
// ============================================================================

/// A top-level region in the firmware image.
///
/// Every meaningful byte range is a Region. The `content` enum discriminates
/// what the region contains.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Region {
    /// Region name (e.g., `"ro"`, `"rw-a"`, `"rw-b"`, `"nvs"`).
    pub name: HString<64>,
    /// Offset from image base (bytes).
    pub offset: u32,
    /// Total size of this region in bytes.
    pub size: u32,
    /// What this region contains.
    pub content: RegionContent,
}

/// What a top-level region contains.
///
/// The `Container` variant is much larger than `Raw` because it holds a
/// `heapless::Vec` of entries. This is inherent to bounded `no_std` types —
/// we cannot use `Box` in firmware code.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(clippy::large_enum_variant)]
pub enum RegionContent {
    /// A container holding file entries (stages, configs, data blobs).
    ///
    /// This is the unit of cryptographic signing — each container's
    /// file entries are covered by the image manifest's signature.
    Container {
        /// File entries and other children within this container.
        ///
        /// Eight covers current images (QEMU/Sifive: 3, x86 full-flash: 4)
        /// while avoiding large no_std stack frames from unused capacity.
        children: heapless::Vec<RegionEntry, 8>,
    },

    /// Raw reserved space with no internal structure.
    ///
    /// Used for NVS (non-volatile storage), scratch pads, etc.
    /// Pre-filled with `fill` byte (typically 0xFF for erased flash).
    Raw {
        /// Fill byte (0xFF for erased flash convention).
        fill: u8,
    },
}

/// A child entry within a Container region.
///
/// Each entry is a named, typed byte range within its parent region.
/// Offsets are relative to the parent `Region`'s offset.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegionEntry {
    /// Entry name (e.g., `"bootblock"`, `"main"`, `"board.cfg"`).
    pub name: HString<64>,
    /// Offset from the parent region's base (bytes).
    pub offset: u32,
    /// Total size of this entry in bytes.
    pub size: u32,
    /// What this entry contains.
    pub content: EntryContent,
}

/// What a child entry within a container contains.
///
/// The `File` variant is much larger than `Raw` due to the segment vec.
/// Cannot use `Box` in `no_std`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(clippy::large_enum_variant)]
pub enum EntryContent {
    /// A loadable file composed of segments (stages, payloads, configs).
    ///
    /// Each file may have multiple segments (e.g., `.text` + `.data` + `.bss`).
    /// Digests cover the concatenated uncompressed segment data.
    File {
        /// Type of file — influences how the loader treats it.
        file_type: FileType,
        /// Segments that make up this file (max 12: text, rodata, data, bss,
        /// page tables/IDT, plus additional x86 boot/reset sections).
        segments: heapless::Vec<Segment, 12>,
        /// Integrity digests over concatenated uncompressed segment data.
        digests: DigestSet,
    },

    /// Raw reserved space within a container.
    Raw {
        /// Fill byte.
        fill: u8,
    },
}

// ============================================================================
// File types
// ============================================================================

/// Type of file in the FFS.
///
/// File type influences how the loader treats the file (e.g., code files
/// may need I-cache invalidation after loading).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FileType {
    /// Executable stage binary — loaded and jumped to.
    StageCode,
    /// Board configuration blob.
    BoardConfig,
    /// OS kernel or other payload.
    Payload,
    /// Flattened Device Tree blob.
    Fdt,
    /// Generic data blob (read-only).
    Data,
    /// Raw region — a plain area with no special semantics.
    Raw,
    /// Runtime firmware blob (SBI firmware or ATF BL31).
    Firmware,
    /// FIT (Flattened Image Tree) image for runtime parsing.
    FitImage,
    /// CPU microcode update blob. The CPU vendor authenticates update records;
    /// fstart still records normal file digests when the entry is signed.
    CpuMicrocode,
    /// Initial RAM filesystem (initramfs / initrd) — loaded to RAM and
    /// passed to the kernel via FDT `/chosen/linux,initrd-start` and
    /// `linux,initrd-end` properties.
    Initramfs,
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
///
/// ## In-place decompression
///
/// Compressed segments support in-place decompression following coreboot's
/// technique: the compressed data is loaded to the **end** of the output
/// buffer (`load_addr + in_place_size - stored_size`), then decompressed
/// from head to tail. The decompressor reads from the tail while writing
/// from the head, so the write pointer never overtakes the read pointer as
/// long as the buffer is large enough.
///
/// The builder verifies in-place safety empirically by simulating the
/// decompression at build time. `in_place_size` records the minimum buffer
/// size required (always `>= loaded_size`). For uncompressed segments
/// `in_place_size == 0` (unused).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Segment {
    /// Segment name (e.g., `".text"`, `".data"`, `".rodata"`, `".bss"`).
    pub name: HString<32>,
    /// What kind of content this segment contains.
    pub kind: SegmentKind,
    /// Offset of the segment's (possibly compressed) data from the start
    /// of the **parent region entry** (not from image base).
    pub offset: u32,
    /// Size of segment data as stored in flash (after compression).
    pub stored_size: u32,
    /// Exact initialized bytes after decompression; excludes a BSS tail.
    pub initialized_size: u32,
    /// SHA-256 of exact stored bytes (zero for pure zero-fill).
    pub stored_digest: [u8; 32],
    /// SHA-256 of exact initialized bytes (zero for pure zero-fill).
    pub loaded_digest: [u8; 32],
    /// Size of segment data after decompression (original size).
    /// For BSS segments this is the zero-fill size; `stored_size` is 0.
    pub loaded_size: u32,
    /// Disjoint workspace bytes: initialized output plus stored input for LZ4.
    /// The historical field name is retained in the ergonomic schema only;
    /// overlapping shared/mutable Rust slices are never permitted.
    /// `0` for uncompressed or BSS segments.
    pub in_place_size: u32,
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
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
pub struct Signature {
    /// Which key was used to produce this signature (matches constant-policy key_id).
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
