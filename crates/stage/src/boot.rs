//! Bounded bootstrap loading. No directory allocation or signature dependency.
//!
//! Descriptors imported from a handoff are trusted only because their producer
//! was verified before entry and the handoff RAM remains protected. These APIs
//! do not authenticate RAM, enforce flash write protection, or prevent rollback.

use fstart_core::ffs::Compression;
use fstart_core::services::BootMedia;
use fstart_crypto::digest::hash_sha256;
use fstart_ffs::root::BootstrapDescriptor;

#[cfg(test)]
#[path = "boot_tests.rs"]
mod tests;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoadError {
    Bounds,
    MemoryPolicy,
    Unsupported,
    Io,
    StoredDigest,
    LoadedDigest,
    Decompression,
}

/// Half-open physical memory window, provided by trusted platform code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemoryWindow {
    pub start: u64,
    pub size: u64,
}

impl MemoryWindow {
    fn end(self) -> Option<u64> {
        self.start.checked_add(self.size)
    }

    fn contains(self, other: Self) -> bool {
        matches!((self.end(), other.end()), (Some(end), Some(other_end))
            if other.start >= self.start && other_end <= end)
    }

    pub(crate) fn overlaps(self, other: Self) -> bool {
        match (self.end(), other.end()) {
            (Some(end), Some(other_end)) => self.start < other_end && other.start < end,
            _ => true,
        }
    }
}

/// Writable destinations must be in an allowed window and outside *every*
/// reservation (running code, stacks, heap, handoff, tables, DMA and media).
#[derive(Clone, Copy)]
pub struct MemoryPolicy<'a> {
    pub writable: &'a [MemoryWindow],
    pub reserved: &'a [MemoryWindow],
    pub entry_alignment: u64,
}

impl MemoryPolicy<'_> {
    pub fn permits(&self, start: u64, size: u64) -> bool {
        let range = MemoryWindow { start, size };
        size != 0
            && start != 0
            && usize::try_from(start).is_ok()
            && size <= isize::MAX as u64
            && range
                .end()
                .and_then(|end| usize::try_from(end).ok())
                .is_some()
            && self.writable.iter().any(|window| window.contains(range))
            && !self.reserved.iter().any(|window| window.overlaps(range))
    }
}

/// Linker-owned running image and writable state reservations. Includes XIP
/// text/rodata/data source separately from live data/BSS/heap/stack/page tables.
/// Boards must still reserve handoff, temporary arenas, tables and DMA buffers
/// located outside these linker ranges.
pub fn running_stage_windows() -> Result<[MemoryWindow; 2], LoadError> {
    #[cfg(target_os = "none")]
    {
        unsafe extern "C" {
            static _text_start: u8;
            static _data_load: u8;
            static _data_start: u8;
            static _data_end: u8;
            static _writable_end: u8;
        }
        let text = core::ptr::addr_of!(_text_start) as u64;
        let data_load = core::ptr::addr_of!(_data_load) as u64;
        let data_start = core::ptr::addr_of!(_data_start) as u64;
        let data_end = core::ptr::addr_of!(_data_end) as u64;
        let writable_end = core::ptr::addr_of!(_writable_end) as u64;
        let initialized = data_end.checked_sub(data_start).ok_or(LoadError::Bounds)?;
        let image_end = data_load
            .checked_add(initialized)
            .ok_or(LoadError::Bounds)?;
        Ok([
            MemoryWindow {
                start: text,
                size: image_end.checked_sub(text).ok_or(LoadError::Bounds)?,
            },
            MemoryWindow {
                start: data_start,
                size: writable_end
                    .checked_sub(data_start)
                    .ok_or(LoadError::Bounds)?,
            },
        ])
    }
    #[cfg(not(target_os = "none"))]
    {
        Err(LoadError::MemoryPolicy)
    }
}

/// Only created after exact initialized destination bytes have been verified.
#[derive(Debug)]
pub struct VerifiedExecutable {
    entry: u64,
}

impl VerifiedExecutable {
    pub fn entry(&self) -> u64 {
        self.entry
    }
}

/// Read an exact range; short successful device reads are still failures.
pub(crate) fn read_exact(
    media: &(impl BootMedia + ?Sized),
    offset: u64,
    destination: &mut [u8],
) -> Result<(), LoadError> {
    let offset = usize::try_from(offset).map_err(|_| LoadError::Bounds)?;
    if offset
        .checked_add(destination.len())
        .filter(|&end| end <= media.size())
        .is_none()
    {
        return Err(LoadError::Bounds);
    }
    match media.read_at(offset, destination) {
        Ok(size) if size == destination.len() => Ok(()),
        _ => Err(LoadError::Io),
    }
}

/// Independently computed live footprint, not an allocation justified by a
/// descriptor's advertised scratch size. LZ4 source and output never overlap.
pub fn bootstrap_footprint(descriptor: &BootstrapDescriptor) -> Result<u64, LoadError> {
    descriptor
        .validate(u64::MAX)
        .map_err(|_| LoadError::Bounds)?;
    match descriptor.compression {
        Compression::None => {
            if descriptor.stored_size != descriptor.loaded_size
                || descriptor.stored_digest != descriptor.loaded_digest
            {
                return Err(LoadError::Bounds);
            }
            Ok(descriptor.loaded_size)
        }
        Compression::Lz4 => descriptor
            .loaded_size
            .checked_add(descriptor.stored_size)
            .ok_or(LoadError::Bounds),
    }
}

/// Load a trusted descriptor into a caller-owned, exclusive workspace. The
/// workspace represents the descriptor's load address; the physical wrapper
/// below checks that mapping. Useful also for block media and host tests.
pub fn load_bootstrap_into(
    media: &(impl BootMedia + ?Sized),
    descriptor: &BootstrapDescriptor,
    workspace: &mut [u8],
) -> Result<(), LoadError> {
    let required =
        usize::try_from(bootstrap_footprint(descriptor)?).map_err(|_| LoadError::Bounds)?;
    if workspace.len() < required
        || descriptor
            .offset
            .checked_add(descriptor.stored_size)
            .filter(|&end| end <= media.size() as u64)
            .is_none()
    {
        return Err(LoadError::Bounds);
    }
    let loaded = usize::try_from(descriptor.loaded_size).map_err(|_| LoadError::Bounds)?;
    let (output, scratch) = workspace[..required].split_at_mut(loaded);
    match descriptor.compression {
        Compression::None => {
            read_exact(media, descriptor.offset, output)?;
            if hash_sha256(output) != descriptor.stored_digest {
                return Err(LoadError::StoredDigest);
            }
        }
        Compression::Lz4 => {
            #[cfg(feature = "lz4")]
            {
                // Authenticate precisely the stable bytes passed to the decoder.
                read_exact(media, descriptor.offset, scratch)?;
                if hash_sha256(scratch) != descriptor.stored_digest {
                    return Err(LoadError::StoredDigest);
                }
                let count = fstart_ffs::lz4::decompress_block(scratch, output)
                    .map_err(|_| LoadError::Decompression)?;
                if count != loaded {
                    return Err(LoadError::Decompression);
                }
            }
            #[cfg(not(feature = "lz4"))]
            {
                let _ = scratch;
                return Err(LoadError::Unsupported);
            }
        }
    }
    if hash_sha256(output) != descriptor.loaded_digest {
        return Err(LoadError::LoadedDigest);
    }
    Ok(())
}

pub(crate) fn validate_destination(
    media: &(impl BootMedia + ?Sized),
    policy: &MemoryPolicy<'_>,
    start: u64,
    size: u64,
) -> Result<(), LoadError> {
    if !policy.permits(start, size) {
        return Err(LoadError::MemoryPolicy);
    }
    if let Some(source) = media.as_slice() {
        let source = MemoryWindow {
            start: source.as_ptr() as u64,
            size: source.len() as u64,
        };
        if source.overlaps(MemoryWindow { start, size }) {
            return Err(LoadError::MemoryPolicy);
        }
    }
    Ok(())
}

/// Validate and load before granting the executable entry address.
///
/// # Safety
/// The caller supplies authenticated metadata (root or protected handoff) and a
/// trusted memory policy. All permitted physical windows must map exclusively
/// writable RAM for the full operation; reservations must exclude live code,
/// stack, heap, page tables, handoff and device/DMA-owned memory. No writable
/// mapping may alias media, policy, descriptor, or other borrowed Rust objects.
/// DMA and other CPUs must not modify input scratch or output before entry.
pub unsafe fn load_bootstrap(
    media: &(impl BootMedia + ?Sized),
    descriptor: &BootstrapDescriptor,
    policy: &MemoryPolicy<'_>,
) -> Result<VerifiedExecutable, LoadError> {
    let footprint = bootstrap_footprint(descriptor)?;
    let entry = descriptor
        .load_addr
        .checked_add(descriptor.entry_offset)
        .ok_or(LoadError::Bounds)?;
    if descriptor.entry_offset >= descriptor.loaded_size
        || policy.entry_alignment == 0
        || !policy.entry_alignment.is_power_of_two()
        || entry % policy.entry_alignment != 0
    {
        return Err(LoadError::MemoryPolicy);
    }
    // Even a readable mmap view does not imply immutability. Reject aliasing
    // before making the mutable destination slice, then buffer compressed input.
    validate_destination(media, policy, descriptor.load_addr, footprint)?;
    // SAFETY: the caller's mapping contract and the complete footprint check.
    let workspace = unsafe {
        core::slice::from_raw_parts_mut(descriptor.load_addr as *mut u8, footprint as usize)
    };
    load_bootstrap_into(media, descriptor, workspace)?;
    Ok(VerifiedExecutable { entry })
}
