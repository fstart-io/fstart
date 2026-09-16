//! S3-resident compressed stage cache.
//!
//! On cold boot the bootblock and postcar verify a stage from flash and then
//! copy its compressed FFS body into a reserved DRAM slot. On S3 resume the
//! stages are re-loaded from those slots instead of flash, through the same
//! verified [`crate::boot::load_bootstrap`] path with the descriptor's
//! `offset` re-pointed at the slot. Resume therefore re-authenticates the
//! stage bytes (stored digest, LZ4, loaded digest) against the freshly
//! authenticated descriptor, and a flash update while suspended fails the
//! stored digest instead of silently resuming a mixed-revision image.
//!
//! The cache is an optimization and a coherence anchor, never a trust bypass:
//! the digests in the authenticated descriptor are what bind a slot to the
//! image. Callers treat "no valid slot" on resume as fatal and reset to a
//! clean cold boot.

use fstart_core::services::BootMedia;
use fstart_core::services::boot_media::SubRegion;

use fstart_ffs::root::BootstrapDescriptor;

use crate::boot::{MemoryPolicy, VerifiedExecutable, load_bootstrap};

/// Slot header magic, the bytes `FSCC` read little-endian.
pub const STAGE_CACHE_MAGIC: u32 = u32::from_le_bytes(*b"FSCC");
/// Wire version of the slot header.
pub const STAGE_CACHE_VERSION: u32 = 1;
/// Header size preceding the cached compressed body.
pub const STAGE_CACHE_HEADER_LEN: usize = 32;

const MAGIC_OFF: usize = 0;
const VERSION_OFF: usize = 4;
const STORED_SIZE_OFF: usize = 8;
const LOADED_SIZE_OFF: usize = 16;
const ROLE_OFF: usize = 24;

/// Which stage a slot holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CachedStage {
    Postcar,
    Mainstage,
}

impl CachedStage {
    /// Role byte stored in the slot header.
    pub const fn role_byte(self) -> u8 {
        match self {
            Self::Postcar => 1,
            Self::Mainstage => 2,
        }
    }

    /// Human-readable name for logs.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Postcar => "postcar",
            Self::Mainstage => "ramstage",
        }
    }
}

/// Why a slot could not be stored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CacheError {
    /// The slot is smaller than the header plus the stage body.
    SlotTooSmall,
    /// The source media could not supply the declared body.
    Media,
}

fn u32_at(bytes: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(bytes[off..off + 4].try_into().unwrap_or([0; 4]))
}

fn u64_at(bytes: &[u8], off: usize) -> u64 {
    u64::from_le_bytes(bytes[off..off + 8].try_into().unwrap_or([0; 8]))
}

/// Validate a slot header against `stage`; returns the cached body length.
fn valid_body_len(header: &[u8], stage: CachedStage) -> Option<usize> {
    if header.len() < STAGE_CACHE_HEADER_LEN
        || u32_at(header, MAGIC_OFF) != STAGE_CACHE_MAGIC
        || u32_at(header, VERSION_OFF) != STAGE_CACHE_VERSION
        || header[ROLE_OFF] != stage.role_byte()
    {
        return None;
    }
    let stored = usize::try_from(u64_at(header, STORED_SIZE_OFF)).ok()?;
    // A body must exist and fit; the slot's own length is the only bound the
    // caller can check here, and `load_from_slot` re-checks it against media.
    (stored != 0).then_some(stored)
}

/// Copy the compressed stage body from `source` into `slot` and seal the
/// header. On any error the slot is left invalid (magic cleared).
///
/// The caller guarantees `slot` is writable reserved RAM of at least
/// [`STAGE_CACHE_HEADER_LEN`] + `descriptor.stored_size` bytes. The bootblock
/// calls this while DRAM writes are uncached, so the copy survives the CAR
/// teardown `INVD`; the postcar calls it with caching already enabled.
///
/// No digest is checked here: the body is only consumed through
/// [`load_from_slot`], which re-verifies it against the same authenticated
/// descriptor.
pub fn store(
    slot: &mut [u8],
    stage: CachedStage,
    source: &(impl BootMedia + ?Sized),
    descriptor: &BootstrapDescriptor,
) -> Result<(), CacheError> {
    let stored = usize::try_from(descriptor.stored_size).map_err(|_| CacheError::SlotTooSmall)?;
    if slot.len() < STAGE_CACHE_HEADER_LEN + stored {
        return Err(CacheError::SlotTooSmall);
    }
    // Invalidate the old header before writing a new body.
    slot[MAGIC_OFF..MAGIC_OFF + 4].fill(0);
    let body = &mut slot[STAGE_CACHE_HEADER_LEN..STAGE_CACHE_HEADER_LEN + stored];
    let offset = usize::try_from(descriptor.offset).map_err(|_| CacheError::Media)?;
    match source.read_at(offset, body) {
        Ok(read) if read == stored => {}
        _ => return Err(CacheError::Media),
    }
    slot[STORED_SIZE_OFF..STORED_SIZE_OFF + 8]
        .copy_from_slice(&descriptor.stored_size.to_le_bytes());
    slot[LOADED_SIZE_OFF..LOADED_SIZE_OFF + 8]
        .copy_from_slice(&descriptor.loaded_size.to_le_bytes());
    slot[ROLE_OFF] = stage.role_byte();
    slot[ROLE_OFF + 1..STAGE_CACHE_HEADER_LEN].fill(0);
    slot[VERSION_OFF..VERSION_OFF + 4].copy_from_slice(&STAGE_CACHE_VERSION.to_le_bytes());
    // Magic last: a valid header implies a complete body.
    slot[MAGIC_OFF..MAGIC_OFF + 4].copy_from_slice(&STAGE_CACHE_MAGIC.to_le_bytes());
    Ok(())
}

/// Load a stage from a validated slot through the standard verified loader.
///
/// `slot_media` spans the whole slot (header included). Returns `None` when
/// the slot holds no usable body or the cached bytes fail verification; the
/// resume path treats that as fatal.
pub fn load_from_slot(
    slot_media: &impl BootMedia,
    stage: CachedStage,
    descriptor: &BootstrapDescriptor,
    policy: &MemoryPolicy<'_>,
) -> Option<VerifiedExecutable> {
    let mut header = [0u8; STAGE_CACHE_HEADER_LEN];
    if slot_media.read_at(0, &mut header).ok()? != STAGE_CACHE_HEADER_LEN {
        return None;
    }
    let stored = valid_body_len(&header, stage)?;
    // The slot must hold exactly the body the authenticated descriptor names:
    // this is what makes a flash update while suspended fail closed.
    if stored as u64 != descriptor.stored_size
        || u64_at(&header, LOADED_SIZE_OFF) != descriptor.loaded_size
    {
        return None;
    }
    let body = SubRegion::new(slot_media, STAGE_CACHE_HEADER_LEN, stored)?;
    // The cached body lives at slot offset zero; every other descriptor field
    // (digests, sizes, load address) still comes from the authenticated root.
    let mut cached = *descriptor;
    cached.offset = 0;
    // SAFETY: the caller supplies trusted slot geometry and the same memory
    // policy as the flash path: the destination window is exclusive writable
    // RAM, and reservations exclude live code, stack, heap and the slot.
    unsafe { load_bootstrap(&body, &cached, policy) }.ok()
}

#[cfg(test)]
#[path = "stage_cache_tests.rs"]
mod tests;
