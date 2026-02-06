//! FFS reader — no_std, no alloc, operates on a `&[u8]` flash image.
//!
//! The reader is designed for use in the bootblock and later stages.
//! Because the bootblock has the anchor block embedded in its own binary,
//! the typical flow is:
//!
//! 1. The bootblock references the anchor as a static (known at link time).
//! 2. It reads `ro_manifest_offset` / `ro_manifest_size` from the anchor.
//! 3. It slices the image at those offsets and deserializes the `SignedManifest`.
//! 4. It verifies the manifest signature using keys from the anchor.
//! 5. It looks up files by name and loads their segments.
//!
//! For tools (running on a host with `std`), the `scan_for_anchor` function
//! finds the anchor by searching for `FFS_MAGIC` in an arbitrary binary.

use fstart_types::ffs::{
    AnchorBlock, FileEntry, Manifest, RegionRole, RwSlotPointer, Segment, SignedManifest,
    FFS_MAGIC, FFS_VERSION,
};

use fstart_crypto::digest;
use fstart_crypto::verify;

/// Errors returned by the FFS reader.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReaderError {
    /// Image is too small to contain the referenced data.
    OutOfBounds,
    /// Magic bytes don't match `FFS_MAGIC`.
    BadMagic,
    /// Unsupported FFS version.
    UnsupportedVersion,
    /// Failed to deserialize a postcard-encoded structure.
    DeserializeError,
    /// Manifest signature verification failed.
    SignatureInvalid,
    /// No key with matching key_id found in the anchor.
    KeyNotFound,
    /// File not found in the manifest.
    FileNotFound,
    /// Digest verification failed for a file's segments.
    DigestMismatch,
    /// The requested RW slot was not found in the RO manifest.
    RwSlotNotFound,
    /// Algorithm not compiled in (feature flag missing).
    UnsupportedAlgorithm,
    /// In-place digest verification not possible (multi-segment or compressed).
    ///
    /// The caller must decompress/concatenate segments and verify manually.
    CannotVerifyInPlace,
}

/// FFS reader — reads from a memory-mapped firmware image.
///
/// The reader borrows the entire image as a `&[u8]`. For XIP flash this
/// is the memory-mapped region starting at the flash base address.
pub struct FfsReader<'a> {
    /// The entire firmware image as a byte slice.
    image: &'a [u8],
}

impl<'a> FfsReader<'a> {
    /// Create a reader over a firmware image byte slice.
    pub fn new(image: &'a [u8]) -> Self {
        Self { image }
    }

    /// Deserialize an `AnchorBlock` from a known offset in the image.
    ///
    /// This is used when the bootblock knows the anchor's offset (because
    /// codegen placed it at a link-time-known address). The caller passes
    /// the offset relative to the start of `image`.
    pub fn read_anchor(&self, offset: usize) -> Result<AnchorBlock, ReaderError> {
        let data = self.image.get(offset..).ok_or(ReaderError::OutOfBounds)?;

        let anchor: AnchorBlock =
            postcard::from_bytes(data).map_err(|_| ReaderError::DeserializeError)?;

        if anchor.magic != FFS_MAGIC {
            return Err(ReaderError::BadMagic);
        }
        if anchor.version != FFS_VERSION {
            return Err(ReaderError::UnsupportedVersion);
        }

        Ok(anchor)
    }

    /// Scan the image for `FFS_MAGIC` at 8-byte-aligned offsets.
    ///
    /// Returns the offset of the first anchor found. This is for host-side
    /// tools that don't know the anchor's link-time address.
    pub fn scan_for_anchor(&self) -> Result<usize, ReaderError> {
        let magic = &FFS_MAGIC;
        let mut offset = 0;
        while offset + magic.len() <= self.image.len() {
            if &self.image[offset..offset + magic.len()] == magic {
                return Ok(offset);
            }
            offset += 8; // 8-byte aligned scan
        }
        Err(ReaderError::BadMagic)
    }

    /// Read and verify the RO signed manifest referenced by the anchor.
    ///
    /// 1. Reads the `SignedManifest` from the offset/size in the anchor.
    /// 2. Verifies the signature using the keys embedded in the anchor.
    /// 3. Deserializes and returns the `Manifest`.
    pub fn read_ro_manifest(&self, anchor: &AnchorBlock) -> Result<Manifest, ReaderError> {
        self.read_verified_manifest(
            anchor.ro_manifest_offset as usize,
            anchor.ro_manifest_size as usize,
            &anchor.keys,
        )
    }

    /// Read and verify an RW signed manifest referenced by an `RwSlotPointer`.
    ///
    /// Uses the same keys from the anchor (RO and RW manifests are all
    /// signed by the same key set embedded in the bootblock).
    pub fn read_rw_manifest(
        &self,
        slot: &RwSlotPointer,
        anchor: &AnchorBlock,
    ) -> Result<Manifest, ReaderError> {
        self.read_verified_manifest(
            slot.manifest_offset as usize,
            slot.manifest_size as usize,
            &anchor.keys,
        )
    }

    /// Find an RW slot pointer in the RO manifest by role.
    pub fn find_rw_slot(
        manifest: &Manifest,
        role: RegionRole,
    ) -> Result<&RwSlotPointer, ReaderError> {
        manifest
            .rw_slots
            .iter()
            .find(|s| s.role == role)
            .ok_or(ReaderError::RwSlotNotFound)
    }

    /// Look up a file by name in a manifest.
    pub fn find_file<'m>(manifest: &'m Manifest, name: &str) -> Result<&'m FileEntry, ReaderError> {
        manifest
            .entries
            .iter()
            .find(|e| e.name.as_str() == name)
            .ok_or(ReaderError::FileNotFound)
    }

    /// Read raw segment data from the image.
    ///
    /// `region_base` is the base offset of the region. For the RO region,
    /// use `AnchorBlock::ro_region_base`. For RW regions, use
    /// `RwSlotPointer::region_base`.
    ///
    /// Returns a slice of the compressed (or uncompressed if `compression ==
    /// None`) segment data.
    pub fn read_segment_data(
        &self,
        segment: &Segment,
        region_base: u32,
    ) -> Result<&'a [u8], ReaderError> {
        let start = (region_base + segment.offset) as usize;
        let end = start + segment.stored_size as usize;
        self.image.get(start..end).ok_or(ReaderError::OutOfBounds)
    }

    /// Verify a file's digests against the actual segment data in the image.
    ///
    /// Concatenates all segment data (uncompressed) and checks against the
    /// file's `DigestSet`. For compressed segments, the caller must
    /// decompress first — this function works on the stored data directly
    /// (which for `Compression::None` is the uncompressed data).
    ///
    /// Note: for compressed or multi-segment files, the caller must
    /// decompress/concatenate segments and verify digests manually.
    pub fn verify_file_digests_raw(
        &self,
        file: &FileEntry,
        region_base: u32,
    ) -> Result<(), ReaderError> {
        // For files with a single uncompressed segment, we can verify in-place
        // For multi-segment or compressed files, this is an approximation —
        // the builder computes digests over concatenated uncompressed data.
        // In firmware (no alloc), single-segment uncompressed is the common case.
        if file.segments.len() == 1 {
            let seg = &file.segments[0];
            if seg.compression == fstart_types::ffs::Compression::None {
                let data = self.read_segment_data(seg, region_base)?;
                digest::verify_digest_set(data, &file.digests)
                    .map_err(|_| ReaderError::DigestMismatch)?;
                return Ok(());
            }
        }

        // Multi-segment or compressed: cannot verify in-place without a buffer.
        // Return an explicit error so the caller knows verification was NOT done.
        Err(ReaderError::CannotVerifyInPlace)
    }

    // ---- Internal helpers ----

    /// Read a SignedManifest, verify its signature, and deserialize the Manifest.
    fn read_verified_manifest(
        &self,
        offset: usize,
        size: usize,
        keys: &[fstart_types::ffs::VerificationKey],
    ) -> Result<Manifest, ReaderError> {
        let end = offset + size;
        let data = self
            .image
            .get(offset..end)
            .ok_or(ReaderError::OutOfBounds)?;

        // Deserialize the SignedManifest envelope
        let signed: SignedManifest =
            postcard::from_bytes(data).map_err(|_| ReaderError::DeserializeError)?;

        // Verify the signature over the raw manifest bytes
        verify::verify_with_key_lookup(&signed.manifest_bytes, &signed.signature, keys).map_err(
            |e| match e {
                verify::VerifyError::KeyNotFound => ReaderError::KeyNotFound,
                verify::VerifyError::UnsupportedAlgorithm => ReaderError::UnsupportedAlgorithm,
                _ => ReaderError::SignatureInvalid,
            },
        )?;

        // Deserialize the inner Manifest
        let manifest: Manifest = postcard::from_bytes(&signed.manifest_bytes)
            .map_err(|_| ReaderError::DeserializeError)?;

        Ok(manifest)
    }
}
