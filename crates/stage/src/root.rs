//! Small early root reader. Reads exactly 512 bytes on mmap or block media and
//! never opens/allocates the general directory. Policy is build/platform-owned.
use fstart_core::services::BootMedia;
use fstart_ffs::root::{AuthenticatedRoot, ROOT_SIZE, RootError, RootPolicy};

/// Authenticate the root at a protected, image-relative offset.
pub fn read_boot_root(
    media: &(impl BootMedia + ?Sized),
    offset: u64,
    policy: &RootPolicy<'_>,
) -> Result<AuthenticatedRoot, RootError> {
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
        fstart_ffs::root::authenticate_root(policy, &bytes)
    }
    #[cfg(not(feature = "ffs-signature"))]
    {
        Err(RootError::UnsupportedAlgorithm)
    }
}

/// Development-integrity policy from the stage's embedded, build-patched
/// anchor. Products claiming authenticated updates must protect this initial
/// anchor/code, or supply their own protected RootPolicy to read_boot_root.
/// A zero minimum does not enforce rollback protection.
pub fn authenticate_boot_root(
    anchor_data: &[u8],
    media: &(impl BootMedia + ?Sized),
) -> Result<AuthenticatedRoot, RootError> {
    // SAFETY: read_volatile validates length/alignment before borrowing.
    let anchor = unsafe { fstart_core::ffs::AnchorRef::read_volatile(anchor_data) }
        .ok_or(RootError::InvalidFormat)?;
    if anchor.manifest_size() as usize != ROOT_SIZE || anchor.total_image_size() == 0 {
        return Err(RootError::InvalidFormat);
    }
    let policy = RootPolicy {
        image_family: anchor.image_family(),
        minimum_security_version: 0,
        image_size: anchor.total_image_size() as u64,
        max_directory_size: 64 * 1024,
        keys: anchor.valid_keys(),
    };
    read_boot_root(media, anchor.manifest_offset() as u64, &policy)
}
