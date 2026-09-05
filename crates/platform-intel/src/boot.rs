//! Intel-owned bootstrap memory and retained-directory handoff policy.
//!
//! Current boards use development-integrity: no claim is made that flash write
//! protection or persistent rollback enforcement has been established. The
//! software chain still authenticates each executable before entry.

use fstart_arch::x86_64::car_teardown::{POSTCAR_STASH_ADDR, PostcarMtrrStash};
use fstart_core::services::ServiceError;
use fstart_core::services::boot_media::MemoryMapped;
use fstart_core::services::memory_detect::{E820Entry, E820Kind, MAX_E820_ENTRIES};
use fstart_ffs::root::{BootstrapDescriptor, BootstrapRole, DirectoryRef};
use fstart_stage::boot::{MemoryPolicy, MemoryWindow};

/// Initial stage windows are family layout policy, not image-provided bounds.
/// Each contains code plus loader scratch and leaves the next stage disjoint.
const BOOTSTRAP_WINDOW_SIZE: u64 = 0x0100_0000;

pub(crate) fn bootstrap_window(
    descriptor: &BootstrapDescriptor,
    role: BootstrapRole,
    expected_address: u64,
    ram_end: u64,
) -> Result<MemoryWindow, ServiceError> {
    if descriptor.role != role
        || descriptor.load_addr != expected_address
        || descriptor.entry_offset != 0
    {
        return Err(ServiceError::InvalidParam);
    }
    let end = expected_address
        .checked_add(BOOTSTRAP_WINDOW_SIZE)
        .ok_or(ServiceError::InvalidParam)?
        .min(ram_end);
    let size = end
        .checked_sub(expected_address)
        .filter(|&size| size != 0)
        .ok_or(ServiceError::InvalidParam)?;
    Ok(MemoryWindow {
        start: expected_address,
        size,
    })
}

/// The stash remains reserved through mainstage import. Its provenance comes
/// from the predecessor, not from validation of its magic/version fields.
pub(crate) fn handoff(
    image_base: u64,
    image_size: usize,
) -> Result<&'static PostcarMtrrStash, ServiceError> {
    // SAFETY: Intel's fixed flow reserves this low-DRAM page across CAR teardown.
    let stash = unsafe { &*(POSTCAR_STASH_ADDR as *const PostcarMtrrStash) };
    if !stash.valid_header()
        || stash.image_base != image_base
        || stash.image_size != image_size as u64
    {
        return Err(ServiceError::InvalidParam);
    }
    Ok(stash)
}

pub(crate) fn import_intel_directory(
    image_base: u64,
    image_size: usize,
) -> Result<(), ServiceError> {
    let stash = handoff(image_base, image_size)?;
    let descriptor =
        BootstrapDescriptor::parse(&stash.descriptor).map_err(|_| ServiceError::InvalidParam)?;
    if descriptor.role != BootstrapRole::Mainstage {
        return Err(ServiceError::InvalidParam);
    }
    let directory =
        DirectoryRef::parse(&stash.directory).map_err(|_| ServiceError::InvalidParam)?;
    directory
        .validate(image_size as u64, 64 * 1024)
        .map_err(|_| ServiceError::InvalidParam)?;
    // SAFETY: the platform supplies the mapped firmware window. The digest was
    // authenticated in bootblock and carried in protected-lifetime handoff RAM.
    let media = unsafe { MemoryMapped::from_raw_addr(image_base, image_size) };
    unsafe { fstart_stage::directory::install_directory_context(&media, directory) }
        .map_err(|_| ServiceError::HardwareError)
}

pub(crate) fn install_intel_load_policy(entries: &[E820Entry]) -> Result<(), ServiceError> {
    let mut writable = heapless::Vec::<MemoryWindow, MAX_E820_ENTRIES>::new();
    let mut reserved = heapless::Vec::<MemoryWindow, { MAX_E820_ENTRIES + 4 }>::new();
    for entry in entries.iter().filter(|entry| entry.size != 0) {
        let window = MemoryWindow {
            start: entry.addr,
            size: entry.size,
        };
        if entry.kind == E820Kind::Ram as u32 {
            writable
                .push(window)
                .map_err(|_| ServiceError::InvalidParam)?;
        } else {
            reserved
                .push(window)
                .map_err(|_| ServiceError::InvalidParam)?;
        }
    }
    // Never grant generic file loads the IVT/BDA, trampoline, SMRAM or handoff
    // page. Preserve the family-owned temporary boot-media arena as well.
    for window in [
        MemoryWindow {
            start: 0,
            size: 0x0010_0000,
        },
        MemoryWindow {
            start: 0x0200_0000,
            size: 0x0100_0000,
        },
    ]
    .into_iter()
    .chain(fstart_stage::boot::running_stage_windows().map_err(|_| ServiceError::InvalidParam)?)
    {
        reserved
            .push(window)
            .map_err(|_| ServiceError::InvalidParam)?;
    }
    // SAFETY: E820 was detected by the chipset and the reserved ranges cover
    // this stage's complete image, heap, stack, scratch and firmware tables.
    // The stage context copies the policy; it never borrows these stack arrays.
    unsafe {
        fstart_stage::directory::set_load_policy(&MemoryPolicy {
            writable: &writable,
            reserved: &reserved,
            entry_alignment: 1,
        })
    }
    .map_err(|_| ServiceError::InvalidParam)
}
