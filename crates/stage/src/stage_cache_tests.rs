use super::*;
use fstart_core::ffs::Compression;
use fstart_core::services::ServiceError;
use fstart_crypto::digest::hash_sha256;
use fstart_ffs::root::BootstrapRole;

use crate::boot::MemoryWindow;

struct SliceMedia<'a>(&'a [u8]);

impl BootMedia for SliceMedia<'_> {
    fn read_at(&self, offset: usize, buf: &mut [u8]) -> Result<usize, ServiceError> {
        let end = offset
            .checked_add(buf.len())
            .ok_or(ServiceError::InvalidParam)?;
        buf.copy_from_slice(self.0.get(offset..end).ok_or(ServiceError::InvalidParam)?);
        Ok(buf.len())
    }

    fn size(&self) -> usize {
        self.0.len()
    }
}

const BODY: &[u8] = b"hello";

fn descriptor() -> BootstrapDescriptor {
    BootstrapDescriptor {
        role: BootstrapRole::Mainstage,
        compression: Compression::None,
        offset: 0,
        stored_size: BODY.len() as u64,
        loaded_size: BODY.len() as u64,
        load_addr: 0,
        entry_offset: 0,
        scratch_size: 0,
        stored_digest: hash_sha256(BODY),
        loaded_digest: hash_sha256(BODY),
    }
}

/// Load into a real writable buffer and return (output, policy-free result).
fn load(
    slot: &[u8],
    stage: CachedStage,
    descriptor: &BootstrapDescriptor,
    output: &mut [u8],
) -> Option<u64> {
    let mut descriptor = *descriptor;
    descriptor.load_addr = output.as_ptr() as u64;
    let writable = [MemoryWindow {
        start: descriptor.load_addr,
        size: output.len() as u64,
    }];
    let policy = MemoryPolicy {
        writable: &writable,
        reserved: &[],
        entry_alignment: 1,
    };
    load_from_slot(&SliceMedia(slot), stage, &descriptor, &policy).map(|v| v.entry())
}

#[test]
fn store_then_load_round_trip() {
    let descriptor = descriptor();
    let mut slot = [0u8; 64];
    store(
        &mut slot,
        CachedStage::Mainstage,
        &SliceMedia(BODY),
        &descriptor,
    )
    .unwrap();
    // Header fields are the descriptor's, body follows at the fixed offset.
    let (header, _) = CacheHeader::read_from_prefix(&slot).unwrap();
    assert_eq!(header.magic.get(), STAGE_CACHE_MAGIC);
    assert_eq!(header.stored_size.get(), BODY.len() as u64);
    assert_eq!(
        &slot[STAGE_CACHE_HEADER_LEN..STAGE_CACHE_HEADER_LEN + 5],
        BODY
    );

    let mut output = [0u8; 5];
    let entry = load(&slot, CachedStage::Mainstage, &descriptor, &mut output);
    assert_eq!(entry, Some(output.as_ptr() as u64));
    assert_eq!(&output, BODY);
}

#[test]
fn invalid_magic_is_rejected() {
    let descriptor = descriptor();
    let mut slot = [0u8; 64];
    store(
        &mut slot,
        CachedStage::Mainstage,
        &SliceMedia(BODY),
        &descriptor,
    )
    .unwrap();
    slot[0] = 0;
    let mut output = [0u8; 5];
    assert_eq!(
        load(&slot, CachedStage::Mainstage, &descriptor, &mut output),
        None
    );
}

#[test]
fn nonzero_reserved_header_bytes_are_rejected() {
    let descriptor = descriptor();
    let mut slot = [0u8; 64];
    store(
        &mut slot,
        CachedStage::Mainstage,
        &SliceMedia(BODY),
        &descriptor,
    )
    .unwrap();
    slot[STAGE_CACHE_HEADER_LEN - 1] = 1;
    let mut output = [0u8; 5];
    assert_eq!(
        load(&slot, CachedStage::Mainstage, &descriptor, &mut output),
        None
    );
}

#[test]
fn wrong_role_is_rejected() {
    let descriptor = descriptor();
    let mut slot = [0u8; 64];
    store(
        &mut slot,
        CachedStage::Postcar,
        &SliceMedia(BODY),
        &descriptor,
    )
    .unwrap();
    let mut output = [0u8; 5];
    assert_eq!(
        load(&slot, CachedStage::Mainstage, &descriptor, &mut output),
        None
    );
}

#[test]
fn descriptor_mismatch_is_rejected() {
    let descriptor = descriptor();
    let mut slot = [0u8; 64];
    store(
        &mut slot,
        CachedStage::Mainstage,
        &SliceMedia(BODY),
        &descriptor,
    )
    .unwrap();
    // A flash update while suspended changes the authenticated sizes/digests.
    let mut changed = descriptor;
    changed.stored_size += 1;
    changed.stored_digest = hash_sha256(b"other");
    let mut output = [0u8; 5];
    assert_eq!(
        load(&slot, CachedStage::Mainstage, &changed, &mut output),
        None
    );
}

#[test]
fn corrupted_body_fails_verification() {
    let descriptor = descriptor();
    let mut slot = [0u8; 64];
    store(
        &mut slot,
        CachedStage::Mainstage,
        &SliceMedia(BODY),
        &descriptor,
    )
    .unwrap();
    slot[STAGE_CACHE_HEADER_LEN] ^= 0xFF;
    let mut output = [0u8; 5];
    assert_eq!(
        load(&slot, CachedStage::Mainstage, &descriptor, &mut output),
        None
    );
}

#[test]
fn oversized_body_is_rejected_and_invalidates_the_slot() {
    let descriptor = descriptor();
    let mut slot = [0u8; 64];
    store(
        &mut slot,
        CachedStage::Mainstage,
        &SliceMedia(BODY),
        &descriptor,
    )
    .unwrap();

    let mut oversized = descriptor;
    oversized.stored_size = 100;
    assert_eq!(
        store(
            &mut slot,
            CachedStage::Mainstage,
            &SliceMedia(BODY),
            &oversized
        ),
        Err(CacheError::SlotTooSmall)
    );
    let mut output = [0u8; 5];
    assert_eq!(
        load(&slot, CachedStage::Mainstage, &descriptor, &mut output),
        None
    );
}

#[test]
fn short_source_read_fails_closed() {
    let descriptor = descriptor();
    let mut slot = [0u8; 64];
    // Prime a valid slot, then re-store from a truncated source.
    store(
        &mut slot,
        CachedStage::Mainstage,
        &SliceMedia(BODY),
        &descriptor,
    )
    .unwrap();
    assert_eq!(
        store(
            &mut slot,
            CachedStage::Mainstage,
            &SliceMedia(b"hel"),
            &descriptor
        ),
        Err(CacheError::Media)
    );
    // The half-written body must not be usable.
    let mut output = [0u8; 5];
    assert_eq!(
        load(&slot, CachedStage::Mainstage, &descriptor, &mut output),
        None
    );
}
