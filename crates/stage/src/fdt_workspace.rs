//! One dedicated, platform-owned FDT workspace. Not a general memory manager.
//! Its entire capacity is excluded from executable/data loads from registration
//! until handoff; only FDT loading and bounded preparation may mutate it.
#[cfg(feature = "ffs")]
use crate::boot::MemoryPolicy;
use crate::boot::MemoryWindow;
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicU8, Ordering};
use fstart_core::services::ServiceError;

#[derive(Clone, Copy, PartialEq, Eq)]
struct Workspace {
    source: MemoryWindow,
    destination: MemoryWindow,
}
struct Slot(UnsafeCell<Option<Workspace>>);
// SAFETY: single publication using STATE; contents never change afterwards.
unsafe impl Sync for Slot {}
static SLOT: Slot = Slot(UnsafeCell::new(None));
static STATE: AtomicU8 = AtomicU8::new(0);
const PATCH_GROWTH: u64 = 8192;

#[cfg(test)]
#[path = "fdt_workspace_tests.rs"]
mod tests;

fn workspace() -> Option<Workspace> {
    if STATE.load(Ordering::Acquire) != 2 {
        return None;
    }
    // SAFETY: immutable contents follow completed release publication.
    unsafe { *SLOT.0.get() }
}

/// Register exactly one bounded FDT destination before loading executables.
/// Source may be a bootloader-owned DTB envelope, or the destination itself for
/// an FFS override. Registering the same configuration again is idempotent.
///
/// # Safety
/// Source must be readable RAM and destination exclusively writable RAM for the
/// rest of this stage, neither aliasing live code/stack/heap/handoff/DMA memory.
/// Calls are serialized on the boot CPU, after mainstage policy installation
/// when FFS is enabled. Source bytes must remain stable during preparation.
pub unsafe fn configure_fdt_workspace(
    source: MemoryWindow,
    destination: MemoryWindow,
) -> Result<(), ServiceError> {
    let configured = Workspace {
        source,
        destination,
    };
    for range in [source, destination] {
        if range.start == 0
            || range.start % 4 != 0
            || range.size < 40
            || range.size > isize::MAX as u64
            || range
                .start
                .checked_add(range.size)
                .and_then(|n| usize::try_from(n).ok())
                .is_none()
        {
            return Err(ServiceError::InvalidParam);
        }
    }
    if let Some(old) = workspace() {
        return if old == configured {
            Ok(())
        } else {
            Err(ServiceError::InvalidParam)
        };
    }
    if source.start != destination.start && source.overlaps(destination) {
        return Err(ServiceError::InvalidParam);
    }
    #[cfg(feature = "ffs")]
    {
        let policy = crate::directory::load_policy().ok_or(ServiceError::NotInitialized)?;
        // An existing reservation for this exact source envelope may be edited
        // in place. No other reservation (especially code/heap) is bypassed.
        let mapped = MemoryPolicy {
            writable: policy.writable,
            reserved: &[],
            entry_alignment: 1,
        };
        if !mapped.permits(destination.start, destination.size)
            || policy.reserved.iter().any(|r| {
                r.overlaps(destination) && !(source.start == destination.start && *r == source)
            })
        {
            return Err(ServiceError::InvalidParam);
        }
        drop(crate::loaded::begin_fdt(destination).map_err(|_| ServiceError::InvalidParam)?);
    }
    if STATE
        .compare_exchange(0, 1, Ordering::Acquire, Ordering::Relaxed)
        .is_err()
    {
        return Err(ServiceError::InvalidParam);
    }
    // SAFETY: this caller owns the unpublished slot; registration is serialized.
    unsafe {
        *SLOT.0.get() = Some(configured);
    }
    STATE.store(2, Ordering::Release);
    Ok(())
}

#[cfg(feature = "ffs")]
pub(crate) fn destination() -> Option<MemoryWindow> {
    workspace().map(|w| w.destination)
}

#[cfg(feature = "ffs")]
pub(crate) fn conflicts(range: MemoryWindow) -> bool {
    destination().is_some_and(|destination| destination.overlaps(range))
}

fn contains(outer: MemoryWindow, address: u64, size: u64) -> bool {
    address >= outer.start
        && address
            .checked_add(size)
            .is_some_and(|end| end <= outer.start + outer.size)
}

fn be32(bytes: &[u8], offset: usize) -> Result<usize, ServiceError> {
    Ok(u32::from_be_bytes(
        bytes
            .get(offset..offset + 4)
            .ok_or(ServiceError::InvalidParam)?
            .try_into()
            .unwrap(),
    ) as usize)
}

/// Validate all header-described ranges before any pointer walk or mutation.
fn used_size(bytes: &[u8]) -> Result<usize, ServiceError> {
    if bytes.len() < 40
        || be32(bytes, 0)? != 0xd00d_feed
        || be32(bytes, 4)? != bytes.len()
        || be32(bytes, 20)? != 17
        || be32(bytes, 24)? > 17
    {
        return Err(ServiceError::InvalidParam);
    }
    let structure = be32(bytes, 8)?;
    let strings = be32(bytes, 12)?;
    let reservations = be32(bytes, 16)?;
    if structure < 40
        || strings < 40
        || reservations < 40
        || structure % 4 != 0
        || reservations % 8 != 0
    {
        return Err(ServiceError::InvalidParam);
    }
    let structure_end = structure
        .checked_add(be32(bytes, 36)?)
        .filter(|&n| n <= bytes.len())
        .ok_or(ServiceError::InvalidParam)?;
    let strings_end = strings
        .checked_add(be32(bytes, 32)?)
        .filter(|&n| n <= bytes.len())
        .ok_or(ServiceError::InvalidParam)?;
    let mut end = reservations;
    loop {
        let next = end.checked_add(16).ok_or(ServiceError::InvalidParam)?;
        let record = bytes.get(end..next).ok_or(ServiceError::InvalidParam)?;
        end = next;
        if record == [0; 16] {
            break;
        }
    }
    let ranges = [
        MemoryWindow {
            start: structure as u64,
            size: (structure_end - structure) as u64,
        },
        MemoryWindow {
            start: strings as u64,
            size: (strings_end - strings) as u64,
        },
        MemoryWindow {
            start: reservations as u64,
            size: (end - reservations) as u64,
        },
    ];
    for (index, range) in ranges.iter().enumerate() {
        if ranges[..index].iter().any(|other| range.overlaps(*other)) {
            return Err(ServiceError::InvalidParam);
        }
    }
    Ok(structure_end.max(strings_end).max(end))
}

fn prepare(
    source: u64,
    destination: u64,
    patches: Option<(&str, u64, u64)>,
) -> Result<usize, ServiceError> {
    let config = workspace().ok_or(ServiceError::NotInitialized)?;
    prepare_in(config, source, destination, patches)
}

fn prepare_in(
    config: Workspace,
    source: u64,
    destination: u64,
    patches: Option<(&str, u64, u64)>,
) -> Result<usize, ServiceError> {
    if destination != config.destination.start
        || source % 4 != 0
        || !(contains(config.source, source, 40) || contains(config.destination, source, 40))
    {
        return Err(ServiceError::InvalidParam);
    }
    #[cfg(feature = "ffs")]
    let _pending =
        crate::loaded::begin_fdt(config.destination).map_err(|_| ServiceError::InvalidParam)?;
    // SAFETY: registration establishes readable, stable source RAM; its header
    // fits one of the permitted source envelopes before any access.
    let total =
        unsafe { u32::from_be(core::ptr::read_unaligned((source + 4) as *const u32)) } as u64;
    if total < 40
        || !(contains(config.source, source, total) || contains(config.destination, source, total))
    {
        return Err(ServiceError::InvalidParam);
    }
    let used = {
        // SAFETY: complete source extent was checked above; borrow ends before
        // the destination can be written, including when source == destination.
        let bytes = unsafe { core::slice::from_raw_parts(source as *const u8, total as usize) };
        used_size(bytes)?
    };
    let growth = if patches.is_some() { PATCH_GROWTH } else { 0 };
    if (used as u64)
        .checked_add(growth)
        .filter(|&n| n <= config.destination.size)
        .is_none()
    {
        return Err(ServiceError::InvalidParam);
    }
    // SAFETY: destination capacity, source extent and non-aliasing with live
    // executables were checked. ptr::copy also permits a same-buffer no-op.
    unsafe {
        core::ptr::copy(source as *const u8, destination as *mut u8, used);
        core::ptr::write_unaligned((destination + 4) as *mut u32, (used as u32).to_be());
    }
    #[cfg(feature = "fdt")]
    if let Some((bootargs, base, size)) = patches {
        let capacity = config.destination.size as usize;
        if !bootargs.is_empty() {
            // SAFETY: this dedicated workspace has precisely the supplied bound.
            unsafe {
                crate::fdt_patch::fdt_set_bootargs(destination as *mut u8, capacity, bootargs)
            }
            .map_err(|_| ServiceError::InvalidParam)?;
        }
        if base != 0 && size != 0 {
            base.checked_add(size).ok_or(ServiceError::InvalidParam)?;
            unsafe {
                crate::fdt_patch::fdt_set_memory(destination as *mut u8, capacity, base, size)
            }
            .map_err(|_| ServiceError::InvalidParam)?;
        }
    }
    #[cfg(not(feature = "fdt"))]
    if patches.is_some() {
        return Err(ServiceError::NotSupported);
    }
    // SAFETY: successful bounded preparation leaves an initialized FDT header.
    Ok(
        unsafe { u32::from_be(core::ptr::read_unaligned((destination + 4) as *const u32)) }
            as usize,
    )
}

/// Copy a validated FDT into its dedicated workspace without property patching.
pub fn copy_fdt_to_workspace(source: u64, destination: u64) -> Result<usize, ServiceError> {
    prepare(source, destination, None)
}

#[cfg(feature = "fdt")]
pub fn fdt_prepare_platform(
    source: u64,
    destination: u64,
    bootargs: &str,
    base: u64,
    size: u64,
) -> Result<(), ServiceError> {
    prepare(source, destination, Some((bootargs, base, size))).map(|_| ())
}
