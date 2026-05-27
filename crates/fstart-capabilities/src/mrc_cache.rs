//! Generic MRC/training-data cache record helpers.
//!
//! This mirrors coreboot's split: chipset RAM init owns platform validation
//! (CPU id, SPD CRCs, expected struct size), while the generic layer owns a
//! small persistent-record format and integrity checks. Storage is discovered
//! through a named FFS raw region; board RON controls the region size, not a
//! hardcoded SPI offset.

use fstart_services::{BootMedia, ServiceError};
#[cfg(feature = "ffs")]
use fstart_types::ffs::RegionContent;
use zerocopy::byteorder::{LE, U16, U32};
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned};

/// Coreboot-compatible-ish training-data cache signature: `MRCD`.
pub const MRC_CACHE_SIGNATURE: u32 = u32::from_le_bytes(*b"MRCD");
/// fstart record format version.
pub const MRC_CACHE_FORMAT_VERSION: u16 = 1;
/// Default FFS raw-region name for MRC/training data.
pub const DEFAULT_MRC_CACHE_REGION: &str = "mrc-cache";

/// Persistent metadata header stored immediately before MRC payload bytes.
#[derive(Debug, Clone, Copy, FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned)]
#[repr(C)]
pub struct MrcCacheHeader {
    /// Magic signature, [`MRC_CACHE_SIGNATURE`].
    pub signature: U32<LE>,
    /// Generic fstart record format version.
    pub format_version: U16<LE>,
    /// Platform-owned cache ABI version.
    pub data_version: U16<LE>,
    /// FNV-1a hash of the rustc version string used to build the payload.
    ///
    /// This is part of the ABI guard for cached native Rust structs. The
    /// chipset owner supplies the value because it knows whether the payload is
    /// a raw Rust struct or a stable hand-serialized format.
    pub rustc_version_hash: U32<LE>,
    /// Payload size in bytes.
    pub data_size: U32<LE>,
    /// FNV-1a hash of payload bytes.
    pub data_hash: U32<LE>,
    /// FNV-1a hash of this header with `header_hash` zeroed.
    pub header_hash: U32<LE>,
}

impl MrcCacheHeader {
    /// Serialized header size.
    pub const SIZE: usize = 24;

    /// Build a header for `data` and platform `data_version`.
    pub fn new(data_version: u16, rustc_version_hash: u32, data: &[u8]) -> Self {
        let mut header = Self {
            signature: U32::new(MRC_CACHE_SIGNATURE),
            format_version: U16::new(MRC_CACHE_FORMAT_VERSION),
            data_version: U16::new(data_version),
            rustc_version_hash: U32::new(rustc_version_hash),
            data_size: U32::new(data.len() as u32),
            data_hash: U32::new(fnv1a32(data)),
            header_hash: U32::new(0),
        };
        header.header_hash = U32::new(header.compute_header_hash());
        header
    }

    /// Parse a little-endian header from bytes.
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < Self::SIZE {
            return None;
        }
        let header = Self::ref_from_bytes(&bytes[..Self::SIZE]).ok()?;
        Some(*header)
    }

    /// Write this header to `out` in little-endian form.
    pub fn write_to(self, out: &mut [u8]) -> Result<(), ServiceError> {
        if out.len() < Self::SIZE {
            return Err(ServiceError::InvalidParam);
        }
        out[..Self::SIZE].copy_from_slice(self.as_bytes());
        Ok(())
    }

    /// Validate header fields and payload integrity.
    pub fn validate(
        self,
        expected_data_version: u16,
        expected_rustc_version_hash: u32,
        data: &[u8],
    ) -> Result<(), MrcCacheError> {
        if self.signature.get() != MRC_CACHE_SIGNATURE {
            return Err(MrcCacheError::BadSignature);
        }
        if self.format_version.get() != MRC_CACHE_FORMAT_VERSION
            || self.data_version.get() != expected_data_version
        {
            return Err(MrcCacheError::VersionMismatch);
        }
        if self.rustc_version_hash.get() != expected_rustc_version_hash {
            return Err(MrcCacheError::RustcVersionMismatch);
        }
        if self.data_size.get() as usize != data.len() {
            return Err(MrcCacheError::SizeMismatch);
        }
        if self.compute_header_hash() != self.header_hash.get() {
            return Err(MrcCacheError::HeaderHashMismatch);
        }
        if fnv1a32(data) != self.data_hash.get() {
            return Err(MrcCacheError::DataHashMismatch);
        }
        Ok(())
    }

    fn compute_header_hash(self) -> u32 {
        let mut buf = [0u8; Self::SIZE];
        let mut tmp = self;
        tmp.header_hash = U32::new(0);
        let _ = tmp.write_to(&mut buf);
        fnv1a32(&buf)
    }
}

/// MRC cache validation error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MrcCacheError {
    /// Cache header magic is not `MRCD`.
    BadSignature,
    /// Generic format or platform data version mismatch.
    VersionMismatch,
    /// Cached payload was produced by a different rustc version.
    RustcVersionMismatch,
    /// Header payload size does not match supplied data.
    SizeMismatch,
    /// Header integrity hash mismatch.
    HeaderHashMismatch,
    /// Payload integrity hash mismatch.
    DataHashMismatch,
    /// Configured FFS region is not a raw cache region.
    BadRegion,
    /// Storage I/O failed.
    Io,
}

/// Locate the configured MRC cache raw region in an FFS manifest.
#[cfg(feature = "ffs")]
pub fn find_region(
    media: &impl BootMedia,
    anchor_data: &[u8],
    name: &str,
) -> Result<(usize, usize), MrcCacheError> {
    let anchor = unsafe { fstart_ffs::FfsReader::read_anchor_volatile(anchor_data) }
        .map_err(|_| MrcCacheError::Io)?;
    let manifest =
        crate::read_manifest_from_media(media, &anchor).map_err(|_| MrcCacheError::Io)?;
    let region =
        fstart_ffs::FfsReader::find_region(&manifest, name).map_err(|_| MrcCacheError::Io)?;
    if !matches!(region.content, RegionContent::Raw { .. }) {
        return Err(MrcCacheError::BadRegion);
    }
    Ok((region.offset as usize, region.size as usize))
}

/// Read and validate one MRC cache record from `media` at `offset`.
pub fn read_record(
    media: &impl BootMedia,
    offset: usize,
    expected_data_version: u16,
    expected_rustc_version_hash: u32,
    out: &mut [u8],
) -> Result<usize, MrcCacheError> {
    let mut header_buf = [0u8; MrcCacheHeader::SIZE];
    media
        .read_at(offset, &mut header_buf)
        .map_err(|_| MrcCacheError::Io)?;
    let header = MrcCacheHeader::parse(&header_buf).ok_or(MrcCacheError::BadSignature)?;
    let size = header.data_size.get() as usize;
    if size > out.len() {
        return Err(MrcCacheError::SizeMismatch);
    }
    media
        .read_at(offset + MrcCacheHeader::SIZE, &mut out[..size])
        .map_err(|_| MrcCacheError::Io)?;
    header.validate(
        expected_data_version,
        expected_rustc_version_hash,
        &out[..size],
    )?;
    Ok(size)
}

/// Build a record in `out`, returning total bytes written.
pub fn build_record(
    data_version: u16,
    rustc_version_hash: u32,
    data: &[u8],
    out: &mut [u8],
) -> Result<usize, ServiceError> {
    let total = MrcCacheHeader::SIZE
        .checked_add(data.len())
        .ok_or(ServiceError::InvalidParam)?;
    if out.len() < total {
        return Err(ServiceError::InvalidParam);
    }
    MrcCacheHeader::new(data_version, rustc_version_hash, data)
        .write_to(&mut out[..MrcCacheHeader::SIZE])?;
    out[MrcCacheHeader::SIZE..total].copy_from_slice(data);
    Ok(total)
}

fn fnv1a32(bytes: &[u8]) -> u32 {
    let mut hash = 0x811c_9dc5u32;
    for byte in bytes {
        hash ^= u32::from(*byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash
}
