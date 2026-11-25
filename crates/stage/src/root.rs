//! Small early root reader. Reads exactly 512 bytes on mmap or block media and
//! never opens/allocates the general directory. Policy is build/platform-owned.
use fstart_core::services::{BootMedia, ServiceError};
use fstart_ffs::root::{AuthenticatedRoot, ROOT_SIZE, RootError, RootPolicy};

/// Bounded, slot-relative view over an underlying [`BootMedia`].
///
/// Offset `0` of this view corresponds to `base` in `inner`, and reads cannot
/// escape `base..base + size`. This lets identical RW_A and RW_B FFS images
/// use slot-relative offsets starting at `0`.
pub struct SliceBootMedia<'a, M: BootMedia + ?Sized> {
    inner: &'a M,
    base: usize,
    size: usize,
}

impl<'a, M: BootMedia + ?Sized> SliceBootMedia<'a, M> {
    pub fn new(inner: &'a M, base: usize, size: usize) -> Option<Self> {
        let end = base.checked_add(size)?;
        if end > inner.size() {
            return None;
        }
        Some(Self { inner, base, size })
    }
}

impl<M: BootMedia + ?Sized> BootMedia for SliceBootMedia<'_, M> {
    fn read_at(&self, offset: usize, buf: &mut [u8]) -> Result<usize, ServiceError> {
        let end = offset
            .checked_add(buf.len())
            .ok_or(ServiceError::InvalidParam)?;
        if end > self.size {
            return Err(ServiceError::InvalidParam);
        }
        self.inner.read_at(self.base + offset, buf)
    }

    fn size(&self) -> usize {
        self.size
    }
}

/// Authenticate the root at a protected, image-relative offset, returning both
/// the authenticated root and the SHA-256 digest of the raw 512-byte root.
pub fn read_boot_root_with_digest(
    media: &(impl BootMedia + ?Sized),
    offset: u64,
    policy: &RootPolicy<'_>,
) -> Result<(AuthenticatedRoot, [u8; 32]), RootError> {
    if policy.image_size > media.size() as u64
        || offset
            .checked_add(ROOT_SIZE as u64)
            .filter(|&end| end <= policy.image_size)
            .is_none()
    {
        return Err(RootError::OutOfBounds);
    }
    let mut bytes = [0; ROOT_SIZE];
    crate::boot::read_exact(media, offset, &mut bytes).map_err(|_| RootError::OutOfBounds)?;
    #[cfg(feature = "ffs-signature")]
    {
        let root = fstart_ffs::root::authenticate_root(policy, &bytes)?;
        let digest = fstart_crypto::digest::hash_sha256(&bytes);
        Ok((root, digest))
    }
    #[cfg(not(feature = "ffs-signature"))]
    {
        Err(RootError::UnsupportedAlgorithm)
    }
}

/// Authenticate the root at a protected, image-relative offset.
pub fn read_boot_root(
    media: &(impl BootMedia + ?Sized),
    offset: u64,
    policy: &RootPolicy<'_>,
) -> Result<AuthenticatedRoot, RootError> {
    read_boot_root_with_digest(media, offset, policy).map(|(root, _)| root)
}

/// Authenticate a self-contained RW slot whose `Root` sits at offset `0` of
/// `slot_offset..slot_offset + slot_size`, enforcing `max(trust_floor,
/// external_min_security_version)`.
pub fn authenticate_slot_root(
    media: &(impl BootMedia + ?Sized),
    slot_offset: usize,
    slot_size: usize,
    external_min_security_version: u64,
) -> Result<(AuthenticatedRoot, [u8; 32]), RootError> {
    // SAFETY: the immutable build-patched static owns this policy's provenance.
    let trust =
        unsafe { fstart_core::ffs::trust::TrustRef::read_volatile(crate::fstart_trust_bytes()) }
            .ok_or(RootError::InvalidFormat)?;
    let slice = SliceBootMedia::new(media, slot_offset, slot_size).ok_or(RootError::OutOfBounds)?;
    let policy = RootPolicy {
        image_family: trust.image_family(),
        minimum_security_version: trust
            .minimum_security_version()
            .max(external_min_security_version),
        image_size: slot_size as u64,
        max_directory_size: 64 * 1024,
        keys: trust.valid_keys(),
    };
    read_boot_root_with_digest(&slice, 0, &policy)
}

/// Development-integrity policy from the stage's embedded, build-patched
/// anchor. Products claiming authenticated updates must protect this initial
/// anchor/code, or supply their own protected RootPolicy to read_boot_root.
/// A zero minimum does not enforce rollback protection.
pub fn authenticate_boot_root(
    anchor_data: &[u8],
    media: &(impl BootMedia + ?Sized),
) -> Result<AuthenticatedRoot, RootError> {
    // Expected policy is linked into this verifier and finalized before its
    // compression/hash pin. Never obtain expected keys from the locator/media.
    // SAFETY: the immutable build-patched static owns this policy's provenance.
    let trust =
        unsafe { fstart_core::ffs::trust::TrustRef::read_volatile(crate::fstart_trust_bytes()) }
            .ok_or(RootError::InvalidFormat)?;
    // SAFETY: fixed-size locator bytes are readable for this snapshot.
    let anchor = unsafe { fstart_core::ffs::LocatorRef::read_volatile(anchor_data) }
        .ok_or(RootError::InvalidFormat)?;
    if anchor.image_offset() != 0 || !anchor.media().validate(media.size() as u64) {
        return Err(RootError::InvalidFormat);
    }
    let policy = RootPolicy {
        image_family: trust.image_family(),
        minimum_security_version: trust.minimum_security_version(),
        image_size: anchor.total_image_size() as u64,
        max_directory_size: 64 * 1024,
        keys: trust.valid_keys(),
    };
    read_boot_root(media, anchor.root_offset() as u64, &policy)
}
