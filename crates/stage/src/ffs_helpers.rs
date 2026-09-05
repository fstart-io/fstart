//! Directory-backed loading. Unsupported multi-segment files fail before writes.
use crate::boot::MemoryPolicy;
use crate::boot::{load_bootstrap, read_exact};
use crate::heap::vec::Vec;
use fstart_core::ffs::{Compression, FileType, SegmentKind};
use fstart_core::services::{BootMedia, TempRamArena};
use fstart_crypto::digest::hash_sha256;
use fstart_ffs::root::{BootstrapDescriptor, BootstrapRole};
use fstart_ffs::{FileView, ManifestView, ReaderError};

pub(crate) fn read_manifest_from_media(
    media: &(impl BootMedia + ?Sized),
    _anchor: fstart_core::ffs::AnchorRef<'_>,
) -> Result<ManifestView<'static>, ReaderError> {
    crate::directory::view(media)
}

fn descriptor(file: &FileView<'_>) -> Option<BootstrapDescriptor> {
    let [segment] = file.segments() else {
        return None;
    };
    if segment.kind().ok()? == SegmentKind::Bss {
        return None;
    }
    let offset = u64::from(file.region_offset())
        .checked_add(u64::from(file.entry_offset()))?
        .checked_add(u64::from(segment.offset()))?;
    let loaded_size = u64::from(segment.initialized_size());
    let stored_size = u64::from(segment.stored_size());
    let compression = segment.compression().ok()?;
    Some(BootstrapDescriptor {
        role: BootstrapRole::Mainstage,
        compression,
        offset,
        stored_size,
        loaded_size,
        load_addr: segment.load_addr(),
        entry_offset: 0,
        scratch_size: if compression == Compression::Lz4 {
            loaded_size.checked_add(stored_size)?
        } else {
            0
        },
        stored_digest: segment.stored_digest(),
        loaded_digest: segment.loaded_digest(),
    })
}

pub(crate) fn load_file_segments_from_media(
    media: &(impl BootMedia + ?Sized),
    file: &FileView<'_>,
    image_size: usize,
) -> Option<u64> {
    let mut descriptor = descriptor(file)?;
    let [segment] = file.segments() else {
        return None;
    };
    let is_fdt = file.file_type().ok()? == FileType::Fdt;
    let fdt_window = if is_fdt {
        if segment.kind().ok()? == SegmentKind::Code || segment.flags().execute {
            return None;
        }
        Some(crate::fdt_workspace::destination()?)
    } else {
        None
    };
    let fdt_writable = [fdt_window.unwrap_or(crate::boot::MemoryWindow { start: 0, size: 0 })];
    let policy = if let Some(window) = fdt_window {
        descriptor.load_addr = window.start;
        // Dedicated FDT data may be relocated from its packaged address. The
        // registered workspace is already checked against the trusted policy.
        MemoryPolicy {
            writable: &fdt_writable,
            reserved: &[],
            entry_alignment: 1,
        }
    } else {
        crate::directory::load_policy()?
    };
    // Preflight the entire live footprint, including BSS and separate compressed
    // scratch, before performing the first write or read into the destination.
    let footprint = crate::boot::bootstrap_footprint(&descriptor)
        .ok()?
        .max(u64::from(segment.loaded_size()));
    crate::boot::validate_destination(media, &policy, descriptor.load_addr, footprint).ok()?;
    if descriptor.offset.checked_add(descriptor.stored_size)? > image_size as u64 {
        return None;
    }
    let pending = if let Some(window) = fdt_window {
        crate::loaded::begin_fdt(window)
    } else {
        crate::loaded::begin(
            &[crate::boot::MemoryWindow {
                start: descriptor.load_addr,
                size: footprint,
            }],
            &[crate::boot::MemoryWindow {
                start: descriptor.load_addr,
                size: u64::from(segment.loaded_size()),
            }],
        )
    }
    .ok()?;
    // SAFETY: set_load_policy's platform contract supplies exclusive mappings.
    let verified = unsafe { load_bootstrap(media, &descriptor, &policy) }.ok()?;
    let tail = u64::from(segment.loaded_size()).checked_sub(descriptor.loaded_size)?;
    if tail != 0 {
        // SAFETY: full initialized+BSS footprint was checked above; scratch is
        // no longer borrowed after load_bootstrap, and may be overwritten.
        unsafe {
            core::ptr::write_bytes(
                (descriptor.load_addr + descriptor.loaded_size) as *mut u8,
                0,
                tail as usize,
            );
        }
    }
    if !is_fdt {
        pending.commit();
    }
    Some(verified.entry())
}

pub fn load_ffs_file_by_type(
    _anchor_data: &[u8],
    media: &(impl BootMedia + ?Sized),
    file_type: FileType,
) -> bool {
    load_ffs_file_entry_by_type(media, file_type).is_some()
}

/// Return the entry actually loaded and verified, not merely a success flag.
pub fn load_ffs_file_entry_by_type(
    media: &(impl BootMedia + ?Sized),
    file_type: FileType,
) -> Option<u64> {
    let directory = crate::directory::view(media).ok()?;
    let file = directory.find_file_by_type(file_type).ok()?;
    load_file_segments_from_media(media, &file, media.size())
}

pub fn load_ffs_file_by_name(
    _anchor_data: &[u8],
    media: &(impl BootMedia + ?Sized),
    name: &str,
) -> bool {
    let Ok(directory) = crate::directory::view(media) else {
        return false;
    };
    let Ok(file) = directory.find_file_by_name(name) else {
        return false;
    };
    load_file_segments_from_media(media, &file, media.size()).is_some()
}

fn stored_file(directory: ManifestView<'_>, file_type: FileType) -> Option<BootstrapDescriptor> {
    let file = directory.find_file_by_type(file_type).ok()?;
    let descriptor = descriptor(&file)?;
    // Parser consumers receive stored bytes only, not an implicitly compressed
    // or partially selected segment. A container is a single uncompressed file.
    if descriptor.compression != Compression::None {
        return None;
    }
    Some(descriptor)
}

/// Return authenticated owned data, never a view into mutable boot media.
/// The allocation is retained for stage lifetime to preserve the legacy slice
/// API. Callers with a temporary arena should prefer the scratch variant.
pub fn find_ffs_file_data<'a>(
    _anchor_data: &[u8],
    media: &'a (impl BootMedia + ?Sized),
    file_type: FileType,
) -> Option<&'a [u8]> {
    let descriptor = stored_file(crate::directory::view(media).ok()?, file_type)?;
    let size = usize::try_from(descriptor.stored_size).ok()?;
    if descriptor.offset.checked_add(descriptor.stored_size)? > media.size() as u64 {
        return None;
    }
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(size).ok()?;
    bytes.resize(size, 0);
    read_exact(media, descriptor.offset, &mut bytes).ok()?;
    if hash_sha256(&bytes) != descriptor.stored_digest {
        return None;
    }
    Some(bytes.leak())
}

/// Authenticate the exact stable scratch bytes before returning them to parsers
/// on both block and mmap media. `as_slice()` never grants media immutability.
pub fn find_ffs_file_data_with_scratch<'a>(
    anchor_data: &[u8],
    media: &'a (impl BootMedia + ?Sized),
    file_type: FileType,
    scratch: Option<&'a mut TempRamArena>,
) -> Option<&'a [u8]> {
    let Some(scratch) = scratch else {
        return find_ffs_file_data(anchor_data, media, file_type);
    };
    let descriptor = stored_file(crate::directory::view(media).ok()?, file_type)?;
    descriptor.validate(media.size() as u64).ok()?;
    let bytes = fstart_core::services::boot_media::read_to_temp(
        media,
        usize::try_from(descriptor.offset).ok()?,
        usize::try_from(descriptor.stored_size).ok()?,
        scratch,
    )
    .ok()?;
    if hash_sha256(bytes) != descriptor.stored_digest {
        return None;
    }
    Some(bytes)
}

/// Named driver assets are allocated once per request and retained immutably.
/// This path ignores signed load addresses: it owns its heap destination.
pub(crate) fn read_asset(media: &(impl BootMedia + ?Sized), name: &str) -> Option<&'static [u8]> {
    let directory = crate::directory::view(media).ok()?;
    let file = directory.find_file_by_name(name).ok()?;
    if file.file_type().ok()? != FileType::Data {
        return None;
    }
    let descriptor = descriptor(&file)?;
    let footprint = usize::try_from(crate::boot::bootstrap_footprint(&descriptor).ok()?).ok()?;
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(footprint).ok()?;
    bytes.resize(footprint, 0);
    crate::boot::load_bootstrap_into(media, &descriptor, &mut bytes).ok()?;
    bytes.truncate(usize::try_from(descriptor.loaded_size).ok()?);
    // Avoid shrinking/reallocating with the stage's non-reclaiming allocator.
    Some(bytes.leak())
}

pub(crate) fn reader_error_str(_err: ReaderError) -> &'static str {
    "FFS authentication, format, or bounds failure"
}

/// Early compatibility entry, now selects a bounded root descriptor, never a
/// directory record. Family code should use root + role selection explicitly.
#[cfg(feature = "ffs-signature")]
pub fn ffs_file_extent(
    anchor_data: &[u8],
    media: &(impl BootMedia + ?Sized),
    name: &str,
) -> Option<BootstrapDescriptor> {
    let role = match name {
        "postcar" => BootstrapRole::Postcar,
        "ramstage" | "mainstage" | "main" => BootstrapRole::Mainstage,
        _ => return None,
    };
    let root = crate::root::authenticate_boot_root(anchor_data, media).ok()?;
    let mut matches = root
        .descriptors()
        .iter()
        .flatten()
        .filter(|d| d.role == role);
    let descriptor = *matches.next()?;
    if matches.next().is_some() {
        return None;
    }
    Some(descriptor)
}

/// Early explicit-policy API. No directory and no allocator are required.
///
/// # Safety
/// The caller must satisfy `boot::load_bootstrap`'s exclusive physical mapping,
/// reservation, and protected working-memory contract.
#[cfg(feature = "ffs-signature")]
pub unsafe fn stage_load_with_policy(
    anchor_data: &[u8],
    media: &(impl BootMedia + ?Sized),
    role: BootstrapRole,
    policy: &MemoryPolicy<'_>,
) -> Option<crate::boot::VerifiedExecutable> {
    let root = crate::root::authenticate_boot_root(anchor_data, media).ok()?;
    let mut descriptors = root
        .descriptors()
        .iter()
        .flatten()
        .filter(|d| d.role == role);
    let descriptor = descriptors.next()?;
    if descriptors.next().is_some() {
        return None;
    }
    // SAFETY: caller supplies the same physical mapping contract as load_bootstrap.
    unsafe { load_bootstrap(media, descriptor, policy) }.ok()
}
