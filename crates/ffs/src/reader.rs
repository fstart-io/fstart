//! FFS reader — no_std, no alloc, operates on a `&[u8]` flash image.
//!
//! Its std-only consistency helpers inspect a self-contained image's root and
//! directory using the separate locator and constant policy record. Scanning
//! untrusted media does not establish protected expected verification keys.
//! Firmware boot boundaries use `root::authenticate_root` with protected policy,
//! then retain one verified directory buffer rather than borrowing mutable media.

use fstart_core::ffs::{
    EntryContent, ImageManifest, LOCATOR_MAGIC, LOCATOR_SIZE, LocatorBlock, LocatorRef, Region,
    RegionContent, RegionEntry, Segment,
};

#[cfg(feature = "std")]
use fstart_core::ffs::trust::{TRUST_SIZE, TrustBlock};
use fstart_crypto::digest;

/// Errors returned by the FFS reader.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReaderError {
    /// Image is too small to contain the referenced data.
    OutOfBounds,
    /// Magic bytes don't match `LOCATOR_MAGIC`.
    BadMagic,
    /// Unsupported FFS version.
    UnsupportedVersion,
    /// Failed to parse a fixed-format structure.
    DeserializeError,
    /// Manifest signature verification failed.
    SignatureInvalid,
    /// No matching key/policy record found for host consistency inspection.
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

    /// Read a `LocatorBlock` from a known offset in the image.
    ///
    /// This is used when the bootblock knows the locator's link-time address.
    /// The caller passes the offset relative to the start of `image`.
    pub fn read_anchor(&self, offset: usize) -> Result<LocatorBlock, ReaderError> {
        let data = self.image.get(offset..).ok_or(ReaderError::OutOfBounds)?;
        LocatorBlock::parse(data.get(..LOCATOR_SIZE).ok_or(ReaderError::OutOfBounds)?)
            .ok_or(ReaderError::BadMagic)
    }

    /// Scan the image for `LOCATOR_MAGIC` at 8-byte-aligned offsets.
    ///
    /// Returns the offset of the first locator found. This is for host-side
    /// tools that don't know the locator's link-time address.
    pub fn scan_for_anchor(&self) -> Result<usize, ReaderError> {
        let magic = &LOCATOR_MAGIC;
        let mut offset = 0;
        while offset + LOCATOR_SIZE <= self.image.len() {
            if &self.image[offset..offset + magic.len()] == magic
                && self.locator_header_is_plausible(offset)
            {
                return Ok(offset);
            }
            offset += 8; // 8-byte aligned scan
        }
        Err(ReaderError::BadMagic)
    }

    /// Borrowed concatenated Intel microcode blob recorded in `locator`.
    ///
    /// The builder also records this file in the signed directory, but this
    /// early/AP access path does NOT verify its directory digest. The initial
    /// locator (or retained predecessor copy) duplicates its bounded span so
    /// vendor validation can run without changing pre-Rust/AP sequencing.
    pub fn intel_microcode(&self, locator: LocatorRef<'_>) -> Option<&'a [u8]> {
        let start = locator.microcode_offset() as usize;
        let size = locator.microcode_size() as usize;
        if start == 0 || size == 0 {
            return None;
        }
        let end = start.checked_add(size)?;
        self.image.get(start..end)
    }

    fn locator_header_is_plausible(&self, offset: usize) -> bool {
        let Some(header) = self.image.get(offset..offset + LOCATOR_SIZE) else {
            return false;
        };
        let Some(locator) = LocatorBlock::parse(header) else {
            return false;
        };
        let root_offset = locator.root_offset.get() as usize;
        let root_size = locator.root_size.get() as usize;
        let total_image_size = locator.total_image_size.get() as usize;
        if root_size == 0 {
            return false;
        }
        let Some(root_end) = root_offset.checked_add(root_size) else {
            return false;
        };
        root_end <= self.image.len()
            && (total_image_size == 0 || total_image_size <= self.image.len())
    }

    /// Find a constant policy block for HOST inspection of a self-contained
    /// image. This does not establish protected expected keys. Firmware callers
    /// must use their linked TrustRef with root::authenticate_root instead.
    #[cfg(feature = "std")]
    pub fn read_trust(&self) -> Result<TrustBlock, ReaderError> {
        for offset in (0..self.image.len().saturating_sub(TRUST_SIZE - 1)).step_by(8) {
            if let Some(trust) = TrustBlock::parse(&self.image[offset..offset + TRUST_SIZE]) {
                return Ok(trust);
            }
        }
        Err(ReaderError::KeyNotFound)
    }

    /// Authenticate the root and directory for host consistency inspection.
    /// This inspection helper imposes no persistent rollback minimum. Firmware
    /// must supply its own protected RootPolicy and stable-directory lifetime.
    #[cfg(feature = "std")]
    pub fn read_manifest(&self, locator: &LocatorBlock) -> Result<ImageManifest, ReaderError> {
        self.read_verified_manifest(
            locator.root_offset.get() as usize,
            locator.root_size.get() as usize,
            self.read_trust()?,
        )
    }

    /// Read and verify the directory referenced by a post-build-patched locator.
    #[cfg(feature = "std")]
    pub fn read_manifest_volatile(
        &self,
        locator: LocatorRef<'_>,
    ) -> Result<ImageManifest, ReaderError> {
        self.read_verified_manifest(
            locator.root_offset() as usize,
            locator.root_size() as usize,
            self.read_trust()?,
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
    #[cfg(feature = "std")]
    fn read_verified_manifest(
        &self,
        offset: usize,
        size: usize,
        trust: TrustBlock,
    ) -> Result<ImageManifest, ReaderError> {
        let end = offset.checked_add(size).ok_or(ReaderError::OutOfBounds)?;
        let data = self
            .image
            .get(offset..end)
            .ok_or(ReaderError::OutOfBounds)?;

        let policy = crate::root::RootPolicy {
            image_family: trust.image_family,
            minimum_security_version: trust.minimum_security_version.get(),
            image_size: self.image.len() as u64,
            max_directory_size: self.image.len() as u64,
            keys: trust.valid_keys(),
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
        crate::directory::DirectoryView::parse(directory)?.to_owned_manifest()
    }
}
