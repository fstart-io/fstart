//! FFS reader — no_std, no alloc, operates on a `&[u8]` flash image.
//!
//! This convenience reader is intended for host inspection or a stable image
//! snapshot. It authenticates the root and directory relative to the supplied
//! anchor; scanning an untrusted image does not establish a protected trust root.
//! Firmware boot boundaries use `root::authenticate_root` with protected policy,
//! then retain one verified directory buffer rather than borrowing mutable media.
//! The anchor's historical `manifest_offset`/`manifest_size` now point to the root.

use fstart_core::ffs::{
    ANCHOR_MAX_KEYS, ANCHOR_SIZE, AnchorBlock, AnchorRef, EntryContent, FFS_MAGIC, FFS_VERSION,
    ImageManifest, Region, RegionContent, RegionEntry, Segment,
};

use fstart_crypto::digest;

/// Errors returned by the FFS reader.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReaderError {
    /// Image is too small to contain the referenced data.
    OutOfBounds,
    /// Magic bytes don't match `FFS_MAGIC`.
    BadMagic,
    /// Unsupported FFS version.
    UnsupportedVersion,
    /// Failed to parse a fixed-format structure.
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
    /// This is used when the bootblock knows the anchor's link-time address.
    /// The caller passes
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

    /// Borrow an anchor from raw bytes (e.g., the `FSTART_ANCHOR` static).
    ///
    /// Scalar fields are read through volatile accessors to see post-build
    /// patched values; the key array is borrowed in place.
    ///
    /// # Safety
    ///
    /// The data must be at least `ANCHOR_SIZE` bytes and properly aligned.
    pub unsafe fn read_anchor_volatile(data: &'a [u8]) -> Result<AnchorRef<'a>, ReaderError> {
        // SAFETY: caller guarantees alignment, size, and lifetime of `data`.
        unsafe { AnchorRef::read_volatile(data) }.ok_or(ReaderError::BadMagic)
    }

    /// Scan the image for `FFS_MAGIC` at 8-byte-aligned offsets.
    ///
    /// Returns the offset of the first anchor found. This is for host-side
    /// tools that don't know the anchor's link-time address.
    pub fn scan_for_anchor(&self) -> Result<usize, ReaderError> {
        let magic = &FFS_MAGIC;
        let mut offset = 0;
        while offset + ANCHOR_SIZE <= self.image.len() {
            if &self.image[offset..offset + magic.len()] == magic
                && self.anchor_header_is_plausible(offset)
            {
                return Ok(offset);
            }
            offset += 8; // 8-byte aligned scan
        }
        Err(ReaderError::BadMagic)
    }

    /// Borrowed concatenated Intel microcode blob recorded in `anchor`.
    ///
    /// The blob deliberately lives outside the signed manifest so pre-Rust
    /// entry code can apply it without an FFS parser; the anchor duplicates
    /// its offset and size for later consumers (e.g. per-AP microcode update
    /// during MP init).
    pub fn intel_microcode(&self, anchor: AnchorRef<'_>) -> Option<&'a [u8]> {
        let start = anchor.microcode_offset() as usize;
        let size = anchor.microcode_size() as usize;
        if start == 0 || size == 0 {
            return None;
        }
        let end = start.checked_add(size)?;
        self.image.get(start..end)
    }

    fn anchor_header_is_plausible(&self, offset: usize) -> bool {
        let Some(header) = self.image.get(offset..offset + 40) else {
            return false;
        };
        let version = u32::from_le_bytes([header[8], header[9], header[10], header[11]]);
        let manifest_offset = u32::from_le_bytes([header[12], header[13], header[14], header[15]]);
        let manifest_size = u32::from_le_bytes([header[16], header[17], header[18], header[19]]);
        let total_image_size = u32::from_le_bytes([header[20], header[21], header[22], header[23]]);
        let key_count = u32::from_le_bytes([header[36], header[37], header[38], header[39]]);

        if version != FFS_VERSION || manifest_size == 0 || key_count as usize > ANCHOR_MAX_KEYS {
            return false;
        }

        let Some(manifest_end) = (manifest_offset as usize).checked_add(manifest_size as usize)
        else {
            return false;
        };
        manifest_end <= self.image.len()
            && (total_image_size == 0 || total_image_size as usize <= self.image.len())
    }

    /// Authenticate the root and directory against the supplied anchor.
    /// This inspection helper imposes no persistent rollback minimum. Firmware
    /// must supply its own protected RootPolicy and stable-directory lifetime.
    pub fn read_manifest(&self, anchor: &AnchorBlock) -> Result<ImageManifest, ReaderError> {
        self.read_verified_manifest(
            anchor.manifest_offset as usize,
            anchor.manifest_size as usize,
            anchor.valid_keys(),
            anchor.image_family,
        )
    }

    /// Read and verify the manifest referenced by a post-build-patched anchor.
    pub fn read_manifest_volatile(
        &self,
        anchor: AnchorRef<'_>,
    ) -> Result<ImageManifest, ReaderError> {
        self.read_verified_manifest(
            anchor.manifest_offset() as usize,
            anchor.manifest_size() as usize,
            anchor.valid_keys(),
            anchor.image_family(),
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
        let start = region
            .offset
            .checked_add(entry.offset)
            .and_then(|n| n.checked_add(segment.offset))
            .ok_or(ReaderError::OutOfBounds)? as usize;
        let end = start
            .checked_add(segment.stored_size as usize)
            .ok_or(ReaderError::OutOfBounds)?;
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
            EntryContent::Raw { .. } => return Err(ReaderError::CannotVerifyInPlace),
        };

        if segments.len() == 1 {
            let seg = &segments[0];
            if seg.compression == fstart_core::ffs::Compression::None {
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

    /// Authenticate a bounded root, then hash and parse its exact directory.
    fn read_verified_manifest(
        &self,
        offset: usize,
        size: usize,
        keys: &[fstart_core::ffs::VerificationKey],
        image_family: [u8; 16],
    ) -> Result<ImageManifest, ReaderError> {
        let end = offset.checked_add(size).ok_or(ReaderError::OutOfBounds)?;
        let data = self
            .image
            .get(offset..end)
            .ok_or(ReaderError::OutOfBounds)?;

        let policy = crate::root::RootPolicy {
            image_family,
            minimum_security_version: 0,
            image_size: self.image.len() as u64,
            max_directory_size: self.image.len() as u64,
            keys,
        };
        let root = crate::root::authenticate_root(&policy, data)
            .map_err(|_| ReaderError::SignatureInvalid)?;
        let reference = root.directory();
        let start = usize::try_from(reference.offset).map_err(|_| ReaderError::OutOfBounds)?;
        let size = usize::try_from(reference.size).map_err(|_| ReaderError::OutOfBounds)?;
        let end = start.checked_add(size).ok_or(ReaderError::OutOfBounds)?;
        let directory = self.image.get(start..end).ok_or(ReaderError::OutOfBounds)?;
        reference
            .verify_bytes(directory)
            .map_err(|_| ReaderError::DigestMismatch)?;
        crate::manifest::ManifestView::parse(directory)?.to_owned_manifest()
    }
}
