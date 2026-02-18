//! FFS reader — no_std, no alloc, operates on a `&[u8]` flash image.
//!
//! The reader is designed for use in the bootblock and later stages.
//! Because the bootblock has the anchor block embedded in its own binary,
//! the typical flow is:
//!
//! 1. The bootblock references the anchor as a static (known at link time).
//! 2. It reads `manifest_offset` / `manifest_size` from the anchor.
//! 3. It slices the image at those offsets and deserializes the `SignedManifest`.
//! 4. It verifies the manifest signature using keys from the anchor.
//! 5. It looks up regions by name, then entries by name, then loads segments.
//!
//! For tools (running on a host with `std`), the `scan_for_anchor` function
//! finds the anchor by searching for `FFS_MAGIC` in an arbitrary binary.

use fstart_types::ffs::{
    AnchorBlock, EntryContent, ImageManifest, Region, RegionContent, RegionEntry, Segment,
    SignedManifest, FFS_MAGIC,
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
    /// File/entry not found in the region.
    FileNotFound,
    /// Digest verification failed for a file's segments.
    DigestMismatch,
    /// The requested region was not found in the manifest.
    RegionNotFound,
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

    /// Read an `AnchorBlock` from a known offset in the image.
    ///
    /// This is used when the bootblock knows the anchor's offset (because
    /// codegen placed it at a link-time-known address). The caller passes
    /// the offset relative to the start of `image`.
    ///
    /// The anchor is `#[repr(C)]` — read via pointer cast, no deserialization.
    pub fn read_anchor(&self, offset: usize) -> Result<AnchorBlock, ReaderError> {
        let data = self.image.get(offset..).ok_or(ReaderError::OutOfBounds)?;
        // SAFETY: the image is a contiguous byte slice; we check length
        // and AnchorBlock validates magic + version internally.
        unsafe { AnchorBlock::from_bytes(data) }
            .ok_or(ReaderError::BadMagic)
            .cloned()
    }

    /// Deserialize an anchor from raw bytes (e.g., the `FSTART_ANCHOR` static).
    ///
    /// Uses volatile read to see post-build patched values.
    ///
    /// # Safety
    ///
    /// The data must be at least `ANCHOR_SIZE` bytes and properly aligned.
    pub unsafe fn read_anchor_volatile(data: &[u8]) -> Result<AnchorBlock, ReaderError> {
        AnchorBlock::read_volatile(data).ok_or(ReaderError::BadMagic)
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

    /// Read and verify the signed image manifest referenced by the anchor.
    ///
    /// 1. Reads the `SignedManifest` from the offset/size in the anchor.
    /// 2. Verifies the signature using the keys embedded in the anchor.
    /// 3. Deserializes and returns the `ImageManifest`.
    pub fn read_manifest(&self, anchor: &AnchorBlock) -> Result<ImageManifest, ReaderError> {
        self.read_verified_manifest(
            anchor.manifest_offset as usize,
            anchor.manifest_size as usize,
            anchor.valid_keys(),
        )
    }

    /// Find a region by name in the manifest.
    pub fn find_region<'m>(
        manifest: &'m ImageManifest,
        name: &str,
    ) -> Result<&'m Region, ReaderError> {
        manifest
            .regions
            .iter()
            .find(|r| r.name.as_str() == name)
            .ok_or(ReaderError::RegionNotFound)
    }

    /// Find a file/entry by name within a container region.
    ///
    /// Returns an error if the region is not a Container or the entry is
    /// not found.
    pub fn find_entry<'r>(region: &'r Region, name: &str) -> Result<&'r RegionEntry, ReaderError> {
        match &region.content {
            RegionContent::Container { children } => children
                .iter()
                .find(|e| e.name.as_str() == name)
                .ok_or(ReaderError::FileNotFound),
            RegionContent::Raw { .. } => Err(ReaderError::FileNotFound),
        }
    }

    /// Read raw segment data from the image.
    ///
    /// Resolves the absolute offset from the region's base offset plus the
    /// entry's offset plus the segment's offset:
    /// `absolute = region.offset + entry.offset + segment.offset`
    ///
    /// Returns a slice of the compressed (or uncompressed if `compression ==
    /// None`) segment data.
    pub fn read_segment_data(
        &self,
        segment: &Segment,
        region: &Region,
        entry: &RegionEntry,
    ) -> Result<&'a [u8], ReaderError> {
        let start = (region.offset + entry.offset + segment.offset) as usize;
        let end = start + segment.stored_size as usize;
        self.image.get(start..end).ok_or(ReaderError::OutOfBounds)
    }

    /// Verify a file entry's digests against the actual segment data in the image.
    ///
    /// For single-segment uncompressed files, verifies in-place.
    /// For multi-segment or compressed files, returns `CannotVerifyInPlace`.
    pub fn verify_entry_digests(
        &self,
        entry: &RegionEntry,
        region: &Region,
    ) -> Result<(), ReaderError> {
        let (segments, digests) = match &entry.content {
            EntryContent::File {
                segments, digests, ..
            } => (segments, digests),
            EntryContent::Raw { .. } => return Ok(()), // nothing to verify
        };

        if segments.len() == 1 {
            let seg = &segments[0];
            if seg.compression == fstart_types::ffs::Compression::None {
                let data = self.read_segment_data(seg, region, entry)?;
                digest::verify_digest_set(data, digests)
                    .map_err(|_| ReaderError::DigestMismatch)?;
                return Ok(());
            }
        }

        // Multi-segment or compressed: cannot verify in-place without a buffer.
        Err(ReaderError::CannotVerifyInPlace)
    }

    // ---- Internal helpers ----

    /// Read a SignedManifest, verify its signature, and deserialize the ImageManifest.
    fn read_verified_manifest(
        &self,
        offset: usize,
        size: usize,
        keys: &[fstart_types::ffs::VerificationKey],
    ) -> Result<ImageManifest, ReaderError> {
        let end = offset + size;
        let data = self
            .image
            .get(offset..end)
            .ok_or(ReaderError::OutOfBounds)?;

        verify_and_parse_manifest(data, keys)
    }
}

/// Verify a signed manifest and deserialize the inner [`ImageManifest`].
///
/// This is the core manifest verification logic, factored out of
/// [`FfsReader`] so it can be reused by boot-media-aware code paths
/// that read the manifest into a stack buffer (e.g., when the boot
/// medium is a block device rather than memory-mapped flash).
///
/// # Arguments
///
/// - `data`: The raw bytes of the serialized [`SignedManifest`].
/// - `keys`: Verification keys from the anchor block.
///
/// # Errors
///
/// Returns [`ReaderError::DeserializeError`] if postcard deserialization fails,
/// [`ReaderError::SignatureInvalid`] if the signature doesn't verify, or
/// [`ReaderError::KeyNotFound`] if no matching key is found.
pub fn verify_and_parse_manifest(
    data: &[u8],
    keys: &[fstart_types::ffs::VerificationKey],
) -> Result<ImageManifest, ReaderError> {
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

    // Deserialize the inner ImageManifest
    let manifest: ImageManifest =
        postcard::from_bytes(&signed.manifest_bytes).map_err(|_| ReaderError::DeserializeError)?;

    Ok(manifest)
}
