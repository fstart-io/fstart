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
use zerocopy::byteorder::{LE, U32, U64};
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned};

use crate::boot::{MemoryPolicy, VerifiedExecutable, load_bootstrap};

/// Slot header magic, the bytes `FSCC` read little-endian.
pub const STAGE_CACHE_MAGIC: u32 = u32::from_le_bytes(*b"FSCC");
/// Wire version of the slot header.
pub const STAGE_CACHE_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned)]
#[repr(C)]
struct CacheHeader {
    magic: U32<LE>,
    version: U32<LE>,
    stored_size: U64<LE>,
    loaded_size: U64<LE>,
    role: u8,
    reserved: [u8; 7],
}

/// Header size preceding the cached compressed body.
pub const STAGE_CACHE_HEADER_LEN: usize = core::mem::size_of::<CacheHeader>();

const _: () = assert!(STAGE_CACHE_HEADER_LEN == 32);

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

/// Validate a slot header against `stage`; returns the typed header.
fn valid_header(bytes: &[u8], stage: CachedStage) -> Option<CacheHeader> {
    let (header, _) = CacheHeader::read_from_prefix(bytes).ok()?;
    (header.magic.get() == STAGE_CACHE_MAGIC
        && header.version.get() == STAGE_CACHE_VERSION
        && header.role == stage.role_byte()
        && header.reserved == [0; 7]
        && header.stored_size.get() != 0)
        .then_some(header)
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
    // Invalidate the old header before any fallible size or media operation.
    if let Some(magic) = slot.get_mut(..core::mem::size_of::<u32>()) {
        magic.fill(0);
    }
    let stored = usize::try_from(descriptor.stored_size).map_err(|_| CacheError::SlotTooSmall)?;
    let required = STAGE_CACHE_HEADER_LEN
        .checked_add(stored)
        .ok_or(CacheError::SlotTooSmall)?;
    if slot.len() < required {
        return Err(CacheError::SlotTooSmall);
    }
    let body = &mut slot[STAGE_CACHE_HEADER_LEN..STAGE_CACHE_HEADER_LEN + stored];
    let offset = usize::try_from(descriptor.offset).map_err(|_| CacheError::Media)?;
    match source.read_at(offset, body) {
        Ok(read) if read == stored => {}
        _ => return Err(CacheError::Media),
    }
    let header = CacheHeader {
        magic: U32::new(0),
        version: U32::new(STAGE_CACHE_VERSION),
        stored_size: U64::new(descriptor.stored_size),
        loaded_size: U64::new(descriptor.loaded_size),
        role: stage.role_byte(),
        reserved: [0; 7],
    };
    slot[..STAGE_CACHE_HEADER_LEN].copy_from_slice(header.as_bytes());
    // Magic last: a valid header implies a complete body.
    slot[..core::mem::size_of::<u32>()].copy_from_slice(&STAGE_CACHE_MAGIC.to_le_bytes());
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
    let header = valid_header(&header, stage)?;
    let stored = usize::try_from(header.stored_size.get()).ok()?;
    // The slot must hold exactly the body the authenticated descriptor names:
    // this is what makes a flash update while suspended fail closed.
    if header.stored_size.get() != descriptor.stored_size
        || header.loaded_size.get() != descriptor.loaded_size
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
