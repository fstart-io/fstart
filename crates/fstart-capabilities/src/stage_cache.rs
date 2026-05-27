//! Persistent compressed-stage cache helpers.
//!
//! The cache stores the already-compressed FFS entry bytes. On resume, firmware
//! can copy this entry back into a scratch/loader path and use the normal FFS
//! segment decompressor, avoiding SPI reads for the ramstage body.

use fstart_services::{BootMedia, ServiceError, StageCacheProvider};
use fstart_types::ffs::EntryContent;

/// Stage-cache header magic: `FSC1`.
pub const STAGE_CACHE_MAGIC: u32 = u32::from_le_bytes(*b"FSC1");
/// Stage-cache format version.
pub const STAGE_CACHE_VERSION: u16 = 1;

/// Header stored before cached compressed FFS entry bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C, packed)]
pub struct StageCacheHeader {
    /// Magic signature.
    pub magic: u32,
    /// Format version.
    pub version: u16,
    /// Header size in bytes.
    pub header_size: u16,
    /// Cached FFS entry size in bytes.
    pub entry_size: u32,
    /// FNV-1a hash of cached entry bytes.
    pub entry_hash: u32,
    /// Original FFS image offset of the cached entry.
    pub source_offset: u32,
}

impl StageCacheHeader {
    /// Serialized header size.
    pub const SIZE: usize = 20;

    /// Build a header for cached bytes.
    pub fn new(source_offset: u32, entry: &[u8]) -> Self {
        Self {
            magic: STAGE_CACHE_MAGIC,
            version: STAGE_CACHE_VERSION,
            header_size: Self::SIZE as u16,
            entry_size: entry.len() as u32,
            entry_hash: fnv1a32(entry),
            source_offset,
        }
    }

    /// Write the header to a byte slice.
    pub fn write_to(self, out: &mut [u8]) -> Result<(), ServiceError> {
        if out.len() < Self::SIZE {
            return Err(ServiceError::InvalidParam);
        }
        out[0..4].copy_from_slice(&self.magic.to_le_bytes());
        out[4..6].copy_from_slice(&self.version.to_le_bytes());
        out[6..8].copy_from_slice(&self.header_size.to_le_bytes());
        out[8..12].copy_from_slice(&self.entry_size.to_le_bytes());
        out[12..16].copy_from_slice(&self.entry_hash.to_le_bytes());
        out[16..20].copy_from_slice(&self.source_offset.to_le_bytes());
        Ok(())
    }
}

/// Save one named stage into a normal-RAM cache and reserve it from E820.
#[cfg(feature = "ffs")]
pub fn save_stage_cache_memory(
    anchor_data: &[u8],
    media: &impl BootMedia,
    stage_name: &str,
    cache_base: u64,
    cache_size: u64,
) -> Result<(), ServiceError> {
    save_compressed_stage_to_cache(
        anchor_data,
        media,
        stage_name,
        cache_base,
        cache_size as usize,
    )?;
    unsafe {
        fstart_services::memory_detect::e820_state_mut().reserve_range(cache_base, cache_size);
    }
    Ok(())
}

/// Save one named stage into a provider-owned TSEG/SMRAM cache.
#[cfg(feature = "ffs")]
pub fn save_stage_cache_tseg(
    anchor_data: &[u8],
    media: &impl BootMedia,
    stage_name: &str,
    requested_size: u64,
    provider: &impl StageCacheProvider,
) -> Result<(), ServiceError> {
    let (cache_base, cache_size) = provider
        .stage_cache_region(requested_size)
        .ok_or(ServiceError::InvalidParam)?;
    provider.stage_cache_open()?;
    let result = save_compressed_stage_to_cache(
        anchor_data,
        media,
        stage_name,
        cache_base,
        cache_size as usize,
    );
    let close_result = provider.stage_cache_close();
    result?;
    close_result?;
    Ok(())
}

/// Open a provider-owned stage cache and return its physical range.
///
/// The caller owns the matching close. This is used by x86 post-CAR stage load,
/// which intentionally keeps SMRAM open across a non-returning jump and lets the
/// next stage close it before MP/SMM initialization.
pub fn open_stage_cache_provider(
    provider: &impl StageCacheProvider,
    requested_size: u64,
) -> Result<(u64, u64), ServiceError> {
    let (base, size) = provider
        .stage_cache_region(requested_size)
        .ok_or(ServiceError::InvalidParam)?;
    provider.stage_cache_open()?;
    Ok((base, size))
}

/// Close a provider-owned stage cache before SMM/MP initialization claims SMRAM.
pub fn close_stage_cache_provider(provider: &impl StageCacheProvider) -> Result<(), ServiceError> {
    provider.stage_cache_close()
}

/// Copy one named stage's compressed FFS entry bytes to a firmware-owned cache.
///
/// The destination should be reserved memory, preferably SMRAM/TSEG while open.
#[cfg(feature = "ffs")]
fn save_compressed_stage_to_cache(
    anchor_data: &[u8],
    media: &impl BootMedia,
    stage_name: &str,
    cache_base: u64,
    cache_size: usize,
) -> Result<(), ServiceError> {
    if cache_size < StageCacheHeader::SIZE {
        return Err(ServiceError::InvalidParam);
    }
    let anchor = unsafe { fstart_ffs::FfsReader::read_anchor_volatile(anchor_data) }
        .map_err(|_| ServiceError::IoError)?;
    let manifest =
        crate::read_manifest_from_media(media, &anchor).map_err(|_| ServiceError::IoError)?;
    let image_size = if anchor.total_image_size == 0 {
        media.size()
    } else {
        (anchor.total_image_size as usize).min(media.size())
    };

    for region in &manifest.regions {
        let Ok(entry) = fstart_ffs::FfsReader::find_entry(region, stage_name) else {
            continue;
        };
        let EntryContent::File { .. } = &entry.content else {
            continue;
        };
        let source_offset = (region.offset + entry.offset) as usize;
        let entry_size = entry.size as usize;
        let total = StageCacheHeader::SIZE
            .checked_add(entry_size)
            .ok_or(ServiceError::InvalidParam)?;
        if total > cache_size || source_offset.saturating_add(entry_size) > image_size {
            return Err(ServiceError::InvalidParam);
        }

        let cache = unsafe { core::slice::from_raw_parts_mut(cache_base as *mut u8, total) };
        media
            .read_at(source_offset, &mut cache[StageCacheHeader::SIZE..])
            .map_err(|_| ServiceError::IoError)?;
        StageCacheHeader::new(source_offset as u32, &cache[StageCacheHeader::SIZE..])
            .write_to(&mut cache[..StageCacheHeader::SIZE])?;
        fstart_log::info!(
            "stage_cache: saved '{}' entry offset={:#x} size={:#x} cache={:#x}",
            stage_name,
            source_offset as u64,
            entry_size as u64,
            cache_base,
        );
        return Ok(());
    }

    Err(ServiceError::InvalidParam)
}

fn fnv1a32(bytes: &[u8]) -> u32 {
    let mut hash = 0x811c_9dc5u32;
    for byte in bytes {
        hash ^= u32::from(*byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash
}
