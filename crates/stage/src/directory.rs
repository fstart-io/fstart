//! Mainstage-owned, immutable directory cache. Never initialized in CAR/SPL.
//! Only its small pointer resides in static storage; the bytes are allocated
//! once in DRAM, authenticated before parsing, and never reread from media.
use crate::boot::{LoadError, MemoryPolicy, MemoryWindow, read_exact};
use crate::heap::{boxed::Box, vec::Vec};
use core::sync::atomic::{AtomicPtr, Ordering};
use fstart_core::services::BootMedia;
use fstart_ffs::root::DirectoryRef;
use fstart_ffs::{ManifestView, ReaderError};

pub const MAX_DIRECTORY_SIZE: u64 = 64 * 1024;
const MAX_WINDOWS: usize = 32;

#[cfg(test)]
#[path = "directory_tests.rs"]
mod tests;

pub struct VerifiedDirectory {
    bytes: Vec<u8>,
    reference: DirectoryRef,
    image_size: usize,
}

impl VerifiedDirectory {
    /// Read once, authenticate the exact owned bytes, then parse those bytes.
    /// The reference must originate in the verified predecessor's handoff or
    /// an authenticated root. This function cannot establish that provenance.
    pub fn open(
        media: &(impl BootMedia + ?Sized),
        reference: DirectoryRef,
    ) -> Result<Self, ReaderError> {
        reference
            .validate(media.size() as u64, MAX_DIRECTORY_SIZE)
            .map_err(|_| ReaderError::OutOfBounds)?;
        let size = usize::try_from(reference.size).map_err(|_| ReaderError::OutOfBounds)?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(size)
            .map_err(|_| ReaderError::OutOfBounds)?;
        bytes.resize(size, 0);
        read_exact(media, reference.offset, &mut bytes).map_err(|_| ReaderError::OutOfBounds)?;
        reference
            .verify_bytes(&bytes)
            .map_err(|_| ReaderError::DigestMismatch)?;
        ManifestView::parse(&bytes)?;
        Ok(Self {
            bytes,
            reference,
            image_size: media.size(),
        })
    }

    pub fn view(&self) -> Result<ManifestView<'_>, ReaderError> {
        ManifestView::parse(&self.bytes)
    }
}

static DIRECTORY: AtomicPtr<VerifiedDirectory> = AtomicPtr::new(core::ptr::null_mut());

/// Cross-check a linked writable reservation against the authenticated,
/// predecessor-imported directory. This never supplies a writer's bounds.
pub fn matches_raw_reservation(name: &str, offset: u32, size: u32, fill: u8) -> bool {
    let pointer = DIRECTORY.load(Ordering::Acquire);
    // SAFETY: imported directories are immutable and live through OS handoff.
    let Some(directory) = (unsafe { pointer.as_ref() }) else {
        return false;
    };
    directory
        .view()
        .and_then(|view| view.raw_region(name))
        .ok()
        .flatten()
        == Some((offset, size, fill))
}

/// Import a trusted predecessor's directory reference into stable mainstage RAM.
/// No root signature or self-check is performed. Repeat installation is only
/// accepted for the same image size/reference, and never rereads directory bytes.
///
/// # Safety
/// Called only after DRAM/allocator initialization, with a reference received
/// through a protected execution/handoff chain. The heap and directory must
/// remain protected from DMA and all subsequent loader writes. A new slot/image
/// attempt requires a fresh stage context, not silently replacing this cache.
pub unsafe fn install_directory_context(
    media: &(impl BootMedia + ?Sized),
    reference: DirectoryRef,
) -> Result<(), ReaderError> {
    if let Some(existing) = context() {
        return if existing.reference == reference && existing.image_size == media.size() {
            Ok(())
        } else {
            Err(ReaderError::DigestMismatch)
        };
    }
    let directory = Box::new(VerifiedDirectory::open(media, reference)?);
    let pointer = Box::into_raw(directory);
    if DIRECTORY
        .compare_exchange(
            core::ptr::null_mut(),
            pointer,
            Ordering::Release,
            Ordering::Acquire,
        )
        .is_err()
    {
        // SAFETY: publication failed; this allocation remains exclusively ours.
        unsafe {
            drop(Box::from_raw(pointer));
        }
        return Err(ReaderError::CannotVerifyInPlace);
    }
    fstart_core::services::ffs_context::install_verified_asset_reader(read_verified_asset);
    Ok(())
}

/// Mainstage service compatibility: reuse predecessor context, or authenticate
/// one root when this stage owns the initial verification boundary. Must only
/// be called after DRAM and allocator initialization, never in CAR/SPL.
pub(crate) fn ensure_context(
    anchor_data: &[u8],
    media: &(impl BootMedia + ?Sized),
) -> Result<(), ReaderError> {
    if context().is_some() {
        return view(media).map(|_| ());
    }
    let root = crate::root::authenticate_boot_root(anchor_data, media)
        .map_err(|_| ReaderError::SignatureInvalid)?;
    fstart_log::info!("boot profile: development integrity; rollback not enforced");
    // SAFETY: fixed-flow mainstage services run after DRAM initialization; the
    // reference was just authenticated against this stage's embedded anchor.
    unsafe { install_directory_context(media, *root.directory()) }
}

fn read_verified_asset(name: &str) -> Option<&'static [u8]> {
    let context = fstart_core::services::ffs_context::memory_mapped()?;
    let size = usize::try_from(context.image_size).ok()?;
    // SAFETY: platform mount publishes its lifetime-long mapped firmware window.
    // The returned asset will own RAM bytes, not borrow that mutable medium.
    let media = unsafe {
        fstart_core::services::boot_media::MemoryMapped::from_raw_addr(context.image_base, size)
    };
    crate::ffs_helpers::read_asset(&media, name)
}

fn context() -> Option<&'static VerifiedDirectory> {
    // SAFETY: pointers are published only after initialization, never freed or
    // mutated, and installations require permanently reserved DRAM ownership.
    unsafe { DIRECTORY.load(Ordering::Acquire).as_ref() }
}

pub(crate) fn view(
    media: &(impl BootMedia + ?Sized),
) -> Result<ManifestView<'static>, ReaderError> {
    let directory = context().ok_or(ReaderError::CannotVerifyInPlace)?;
    if directory.image_size != media.size() {
        return Err(ReaderError::OutOfBounds);
    }
    directory.view()
}

struct OwnedPolicy {
    writable: Vec<MemoryWindow>,
    reserved: Vec<MemoryWindow>,
    entry_alignment: u64,
}
static POLICY: AtomicPtr<OwnedPolicy> = AtomicPtr::new(core::ptr::null_mut());

/// Copy a platform memory policy into retained mainstage ownership. Can be
/// installed after memory discovery independently of directory initialization.
/// Replacement retains prior allocations so readers cannot hold freed policy.
///
/// # Safety
/// The policy must satisfy `boot::load_bootstrap`'s physical mapping contract.
/// In particular reserve the complete running stage/stack, allocator arena,
/// handoff, temporary RAM arenas and tables, not only currently used bytes.
/// This is a platform assertion; authenticated directory destinations provide
/// no authority to write RAM. Invoke only after allocator initialization.
pub unsafe fn set_load_policy(policy: &MemoryPolicy<'_>) -> Result<(), LoadError> {
    if policy.writable.len() > MAX_WINDOWS
        || policy.reserved.len() > MAX_WINDOWS
        || !policy.entry_alignment.is_power_of_two()
        || policy
            .writable
            .iter()
            .chain(policy.reserved)
            .any(|w| w.start.checked_add(w.size).is_none())
    {
        return Err(LoadError::MemoryPolicy);
    }
    let mut writable = Vec::new();
    let mut reserved = Vec::new();
    writable
        .try_reserve_exact(policy.writable.len())
        .map_err(|_| LoadError::MemoryPolicy)?;
    reserved
        .try_reserve_exact(policy.reserved.len())
        .map_err(|_| LoadError::MemoryPolicy)?;
    writable.extend_from_slice(policy.writable);
    reserved.extend_from_slice(policy.reserved);
    let owned = Box::new(OwnedPolicy {
        writable,
        reserved,
        entry_alignment: policy.entry_alignment,
    });
    crate::loaded::initialize();
    POLICY.store(Box::into_raw(owned), Ordering::Release);
    Ok(())
}

/// Add `window` to the installed load policy, removing it from any reserved
/// range it cuts (keeping their parts outside it). Writable windows that
/// touch it merge with it, so a load may span the seam.
///
/// # Safety
/// As [`set_load_policy`]: `window` must be RAM the caller may hand to loads,
/// overlapping nothing that is still live.
pub unsafe fn grant_load_window(window: MemoryWindow) -> Result<(), LoadError> {
    let policy = load_policy().ok_or(LoadError::MemoryPolicy)?;
    let end = window
        .start
        .checked_add(window.size)
        .ok_or(LoadError::MemoryPolicy)?;
    let mut all = Vec::new();
    all.extend_from_slice(policy.writable);
    all.push(window);
    all.sort_unstable_by_key(|w| w.start);
    let mut writable: Vec<MemoryWindow> = Vec::new();
    for w in all {
        match writable.last_mut() {
            Some(last) if w.start <= last.start.saturating_add(last.size) => {
                let end = last
                    .start
                    .saturating_add(last.size)
                    .max(w.start.saturating_add(w.size));
                last.size = end - last.start;
            }
            _ => writable.push(w),
        }
    }
    let reserved: Vec<MemoryWindow> = policy
        .reserved
        .iter()
        .flat_map(|r| {
            let r_end = r.start.saturating_add(r.size);
            [
                (r.start, r_end.min(window.start)),
                (r.start.max(end), r_end),
            ]
        })
        .filter(|&(start, stop)| stop > start)
        .map(|(start, stop)| MemoryWindow {
            start,
            size: stop - start,
        })
        .collect();
    // SAFETY: the existing policy plus the caller's window contract.
    unsafe {
        set_load_policy(&MemoryPolicy {
            writable: &writable,
            reserved: &reserved,
            entry_alignment: policy.entry_alignment,
        })
    }
}

/// Platform-published writable RAM windows before exclusions. This is a
/// read-only topology view, not permission to load: callers must still apply
/// the reserved ranges through the normal load policy.
pub fn writable_memory() -> Option<&'static [MemoryWindow]> {
    load_policy().map(|policy| policy.writable)
}

pub(crate) fn load_policy() -> Option<MemoryPolicy<'static>> {
    // SAFETY: immutable allocations remain owned for the entire stage lifetime.
    let owned = unsafe { POLICY.load(Ordering::Acquire).as_ref() }?;
    Some(MemoryPolicy {
        writable: &owned.writable,
        reserved: &owned.reserved,
        entry_alignment: owned.entry_alignment,
    })
}
