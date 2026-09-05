use super::*;
use core::sync::atomic::{AtomicUsize, Ordering};
use fstart_core::services::ServiceError;

// Smallest revision-2 directory: no records, empty string table at offset 28.
const EMPTY: [u8; 28] = [
    b'F', b'S', b'M', b'Z', 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 28, 0, 0, 0, 0, 0, 0, 0,
];
struct MutableMedia(AtomicUsize);
impl BootMedia for MutableMedia {
    fn read_at(&self, offset: usize, buf: &mut [u8]) -> Result<usize, ServiceError> {
        if offset != 0 || buf.len() != EMPTY.len() {
            return Err(ServiceError::InvalidParam);
        }
        if self.0.fetch_add(1, Ordering::Relaxed) == 0 {
            buf.copy_from_slice(&EMPTY);
        } else {
            buf.fill(0xff);
        }
        Ok(buf.len())
    }
    fn size(&self) -> usize {
        EMPTY.len()
    }
}
fn reference() -> DirectoryRef {
    DirectoryRef {
        offset: 0,
        size: EMPTY.len() as u64,
        digest: fstart_crypto::digest::hash_sha256(&EMPTY),
    }
}

#[test]
fn owned_directory_survives_mutable_media_without_rereading() {
    let media = MutableMedia(AtomicUsize::new(0));
    let directory = VerifiedDirectory::open(&media, reference()).unwrap();
    for _ in 0..3 {
        let view = directory.view().unwrap();
        assert_eq!(view.summary().entries, 0);
    }
    assert_eq!(media.0.load(Ordering::Relaxed), 1);
    assert!(matches!(
        VerifiedDirectory::open(&media, reference()),
        Err(ReaderError::DigestMismatch)
    ));
    assert_eq!(directory.view().unwrap().summary().regions, 0);
}

#[test]
fn directory_bounds_and_digest_fail_closed() {
    let media = MutableMedia(AtomicUsize::new(0));
    let mut reference = reference();
    reference.size = MAX_DIRECTORY_SIZE + 1;
    assert!(matches!(
        VerifiedDirectory::open(&media, reference),
        Err(ReaderError::OutOfBounds)
    ));
    assert_eq!(media.0.load(Ordering::Relaxed), 0);
    reference.size = EMPTY.len() as u64;
    reference.digest[0] ^= 1;
    assert!(matches!(
        VerifiedDirectory::open(&media, reference),
        Err(ReaderError::DigestMismatch)
    ));
}
