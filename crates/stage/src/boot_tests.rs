use super::*;
use core::sync::atomic::{AtomicUsize, Ordering};
use fstart_core::services::ServiceError;
use fstart_ffs::root::BootstrapRole;

struct ChangingMedia {
    first: &'static [u8],
    later: &'static [u8],
    reads: AtomicUsize,
    short: bool,
}
impl BootMedia for ChangingMedia {
    fn read_at(&self, offset: usize, buf: &mut [u8]) -> Result<usize, ServiceError> {
        let bytes = if self.reads.fetch_add(1, Ordering::Relaxed) == 0 {
            self.first
        } else {
            self.later
        };
        let end = offset
            .checked_add(buf.len())
            .ok_or(ServiceError::InvalidParam)?;
        buf.copy_from_slice(bytes.get(offset..end).ok_or(ServiceError::InvalidParam)?);
        Ok(buf.len() - usize::from(self.short))
    }
    fn size(&self) -> usize {
        self.first.len()
    }
}
fn medium(first: &'static [u8]) -> ChangingMedia {
    ChangingMedia {
        first,
        later: first,
        reads: AtomicUsize::new(0),
        short: false,
    }
}
fn descriptor(stored: &[u8], loaded: &[u8], compression: Compression) -> BootstrapDescriptor {
    BootstrapDescriptor {
        role: BootstrapRole::Mainstage,
        compression,
        offset: 0,
        stored_size: stored.len() as u64,
        loaded_size: loaded.len() as u64,
        load_addr: 0x1000,
        entry_offset: 0,
        scratch_size: if compression == Compression::Lz4 {
            (stored.len() + loaded.len()) as u64
        } else {
            0
        },
        stored_digest: hash_sha256(stored),
        loaded_digest: hash_sha256(loaded),
    }
}

#[test]
fn short_reads_and_truncation_fail() {
    let descriptor = descriptor(b"hello", b"hello", Compression::None);
    let mut workspace = [0; 5];
    let mut media = medium(b"hello");
    media.short = true;
    assert_eq!(
        load_bootstrap_into(&media, &descriptor, &mut workspace),
        Err(LoadError::Io)
    );
    assert_eq!(
        load_bootstrap_into(&medium(b"hell"), &descriptor, &mut workspace),
        Err(LoadError::Bounds)
    );
}

#[test]
fn bounds_and_reservations_fail_before_writes() {
    let media = medium(b"hello");
    let mut output = [0x77; 5];
    let mut descriptor = descriptor(b"hello", b"hello", Compression::None);
    descriptor.load_addr = output.as_mut_ptr() as u64;
    let writable = [MemoryWindow {
        start: descriptor.load_addr,
        size: 5,
    }];
    let policy = MemoryPolicy {
        writable: &writable,
        reserved: &writable,
        entry_alignment: 1,
    };
    // SAFETY: a real owned mapped buffer is supplied, but the reservation must
    // reject it before the loader forms a destination borrow or reads media.
    assert!(matches!(
        unsafe { load_bootstrap(&media, &descriptor, &policy) },
        Err(LoadError::MemoryPolicy)
    ));
    assert_eq!(output, [0x77; 5]);
    assert_eq!(media.reads.load(Ordering::Relaxed), 0);
    assert!(!policy.permits(u64::MAX - 2, 5));
    descriptor.entry_offset = descriptor.loaded_size;
    assert!(bootstrap_footprint(&descriptor).is_err());
}

#[test]
fn uncompressed_bytes_are_checked_at_destination() {
    let descriptor = descriptor(b"hello", b"hello", Compression::None);
    let mut workspace = [0; 5];
    assert_eq!(
        load_bootstrap_into(&medium(b"HELLO"), &descriptor, &mut workspace),
        Err(LoadError::StoredDigest)
    );
    assert_eq!(
        load_bootstrap_into(&medium(b"hello"), &descriptor, &mut workspace),
        Ok(())
    );
    assert_eq!(&workspace, b"hello");
}

#[cfg(feature = "lz4")]
#[test]
fn decoder_consumes_the_single_verified_media_read() {
    let media = ChangingMedia {
        first: b"\x50hello",
        later: b"\x50EVIL!",
        reads: AtomicUsize::new(0),
        short: false,
    };
    let descriptor = descriptor(b"\x50hello", b"hello", Compression::Lz4);
    let mut workspace = [0; 11];
    assert_eq!(
        load_bootstrap_into(&media, &descriptor, &mut workspace),
        Ok(())
    );
    assert_eq!(&workspace[..5], b"hello");
    assert_eq!(media.reads.load(Ordering::Relaxed), 1);
}

#[cfg(feature = "lz4")]
#[test]
fn bad_stored_hash_prevents_decompression_and_bad_output_prevents_entry() {
    let media = medium(b"\x50hello");
    let mut descriptor = descriptor(b"\x50hello", b"hello", Compression::Lz4);
    descriptor.stored_digest[0] ^= 1;
    let mut workspace = [0x77; 11];
    assert_eq!(
        load_bootstrap_into(&media, &descriptor, &mut workspace),
        Err(LoadError::StoredDigest)
    );
    assert_eq!(&workspace[..5], &[0x77; 5]);
    descriptor.stored_digest[0] ^= 1;
    descriptor.loaded_digest[0] ^= 1;
    descriptor.load_addr = workspace.as_mut_ptr() as u64;
    let writable = [MemoryWindow {
        start: descriptor.load_addr,
        size: 11,
    }];
    let policy = MemoryPolicy {
        writable: &writable,
        reserved: &[],
        entry_alignment: 1,
    };
    // SAFETY: the entire workspace is exclusively writable for this call.
    assert!(matches!(
        unsafe { load_bootstrap(&media, &descriptor, &policy) },
        Err(LoadError::LoadedDigest)
    ));
}

#[cfg(feature = "lz4")]
#[test]
fn scratch_and_exact_decoded_length_are_enforced() {
    let mut descriptor = descriptor(b"\x50hello", b"hello", Compression::Lz4);
    let media = medium(b"\x50hello");
    assert_eq!(
        load_bootstrap_into(&media, &descriptor, &mut [0; 10]),
        Err(LoadError::Bounds)
    );
    descriptor.scratch_size = 10;
    assert_eq!(
        load_bootstrap_into(&media, &descriptor, &mut [0; 11]),
        Err(LoadError::Bounds)
    );
    descriptor.loaded_size = 6;
    descriptor.scratch_size = 12;
    assert_eq!(
        load_bootstrap_into(&media, &descriptor, &mut [0; 12]),
        Err(LoadError::Decompression)
    );
}
