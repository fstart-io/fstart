//! Intel-owned bootstrap memory and retained-directory handoff policy.
//!
//! Current boards use development-integrity: no claim is made that flash write
//! protection or persistent rollback enforcement has been established. The
//! software chain still authenticates each executable before entry.

use crate::layout::IntelBootLayout;
use fstart_core::layout::RegionKind;
use fstart_core::services::ServiceError;
use fstart_ffs::root::{BootstrapDescriptor, BootstrapRole};
use fstart_stage::boot::MemoryWindow;

/// Initial stage windows come from the linked descriptor, not authored
/// board bounds. Each contains code plus loader scratch and leaves the
/// next stage disjoint.

#[cfg(any(fstart_stage_env = "car", fstart_stage_env = "postcar"))]
pub(crate) fn bootstrap_window(
    descriptor: &BootstrapDescriptor,
    role: BootstrapRole,
    expected_address: u64,
    ram_end: u64,
    geometry: IntelBootLayout<'static>,
) -> Result<MemoryWindow, ServiceError> {
    if descriptor.role != role
        || descriptor.load_addr != expected_address
        || descriptor.entry_offset != 0
    {
        return Err(ServiceError::InvalidParam);
    }
    let kind = match role {
        BootstrapRole::Postcar => RegionKind::BootstrapPostcar,
        BootstrapRole::Mainstage => RegionKind::BootstrapMainstage,
    };
    let window = geometry.destination(kind, ram_end)?;
    if window.base != expected_address
        || fstart_stage::boot::bootstrap_footprint(descriptor)
            .map_err(|_| ServiceError::InvalidParam)?
            > window.size
    {
        return Err(ServiceError::InvalidParam);
    }
    Ok(MemoryWindow {
        start: window.base,
        size: window.size,
    })
}

/// The stash remains reserved through mainstage import. Its provenance comes
/// from the predecessor, not from validation of its magic/version fields.
#[cfg(any(fstart_stage_env = "postcar", fstart_stage_env = "ram"))]
pub(crate) fn handoff(
    image_base: u64,
    image_size: usize,
) -> Result<&'static fstart_arch::x86::boot::car_teardown::PostcarMtrrStash, ServiceError> {
    use fstart_arch::x86::boot::car_teardown::{POSTCAR_STASH_ADDR, PostcarMtrrStash};
    // SAFETY: Intel's fixed flow reserves this low-DRAM page across CAR teardown.
    let stash = unsafe { &*(POSTCAR_STASH_ADDR as *const PostcarMtrrStash) };
    if !stash.valid_header()
        || stash.image_base != image_base
        || stash.image_size != image_size as u64
    {
        return Err(ServiceError::InvalidParam);
    }
    let locator = fstart_core::ffs::locator::LocatorBlock::parse(&stash.locator)
        .ok_or(ServiceError::InvalidParam)?;
    if locator.image_offset != 0 || !locator.media().validate(image_size as u64) {
        return Err(ServiceError::InvalidParam);
    }
    Ok(stash)
}

/// Load a bootstrap stage and keep its S3 cache entry coherent.
///
/// Cold boot: verify the stage from flash, then copy its compressed body into
/// a firmware-store entry. S3 resume: load it from that entry through the
/// same verified path, which re-checks the stored digest against the freshly
/// authenticated descriptor. A resume without a usable entry is fatal
/// (reset): resuming a mixed-revision or corrupted image is never
/// acceptable, and the flash copy may belong to a flash update that happened
/// while suspended.
#[cfg(any(fstart_stage_env = "car", fstart_stage_env = "postcar"))]
pub(crate) fn load_stage_with_cache(
    media: &(impl fstart_core::services::BootMedia + ?Sized),
    stage: fstart_stage::stage_cache::CachedStage,
    descriptor: &BootstrapDescriptor,
    window: MemoryWindow,
    reserved: &[MemoryWindow],
    store: &mut fstart_store::Store,
    resume: bool,
) -> Result<fstart_stage::boot::VerifiedExecutable, ServiceError> {
    use fstart_stage::stage_cache::{CachedStage, STAGE_CACHE_HEADER_LEN};
    let tag = match stage {
        CachedStage::Postcar => fstart_store::tag::STAGE_CACHE_POSTCAR,
        CachedStage::Mainstage => fstart_store::tag::STAGE_CACHE_RAMSTAGE,
    };
    let policy = fstart_stage::boot::MemoryPolicy {
        writable: core::slice::from_ref(&window),
        reserved,
        entry_alignment: 1,
    };
    if resume {
        let verified = store.find(tag).and_then(|entry| {
            // SAFETY: the entry lies in the reopened store; only shared
            // reads of it exist on the resume path.
            let slot = unsafe {
                fstart_core::services::boot_media::MemoryMapped::from_raw_addr(
                    store.address(&entry) as u64,
                    entry.len(),
                )
            };
            fstart_stage::stage_cache::load_from_slot(&slot, stage, descriptor, &policy)
        });
        return match verified {
            Some(verified) => {
                fstart_log::info!("stage cache: {} loaded from store", stage.name());
                Ok(verified)
            }
            None => {
                fstart_log::error!(
                    "stage cache: no valid {} entry on resume; resetting",
                    stage.name()
                );
                fstart_log::flush();
                fstart_arch::x86::boot::system_reset(true)
            }
        };
    }

    // SAFETY: trained DRAM, bounded family-owned window, and all live stage
    // code/data/stack plus the store excluded by `reserved`. The loader
    // verifies final bytes.
    let verified = unsafe { fstart_stage::boot::load_bootstrap(media, descriptor, &policy) }
        .map_err(|_| ServiceError::HardwareError)?;
    let stored = usize::try_from(descriptor.stored_size).map_err(|_| ServiceError::InvalidParam)?;
    fstart_timestamp::add(fstart_timestamp::id::STORE_STAGE_CACHE);
    // Copy the stored bytes the loader just verified in RAM rather than
    // reading the flash a second time.
    let input =
        fstart_stage::boot::stored_input(descriptor).map_err(|_| ServiceError::InvalidParam)?;
    // SAFETY: the loader filled this part of the exclusive destination window
    // and nothing runs from it before entry.
    let input = unsafe {
        fstart_core::services::boot_media::MemoryMapped::from_raw_addr(input.start, stored)
    };
    let in_ram = BootstrapDescriptor {
        offset: 0,
        ..*descriptor
    };
    let cached = store
        .add(tag, STAGE_CACHE_HEADER_LEN + stored, 3)
        .ok()
        .and_then(|entry| {
            // SAFETY: a fresh entry nothing else references.
            let slot = unsafe { store.bytes_mut(&entry) };
            fstart_stage::stage_cache::store(slot, stage, &input, &in_ram).ok()
        });
    match cached {
        Some(()) => fstart_log::info!("stage cache: {} stored", stage.name()),
        None => fstart_log::error!(
            "stage cache: cannot store {} bytes of {}; resume will reset",
            stored as u32,
            stage.name()
        ),
    }
    Ok(verified)
}

#[cfg(fstart_stage_env = "ram")]
pub(crate) fn import_intel_directory(
    image_base: u64,
    image_size: usize,
) -> Result<(), ServiceError> {
    use fstart_core::services::boot_media::MemoryMapped;
    use fstart_ffs::root::DirectoryRef;
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
    let locator = fstart_core::ffs::locator::LocatorBlock::parse(&stash.locator)
        .ok_or(ServiceError::InvalidParam)?;
    // SAFETY: same predecessor/lifetime as the directory, bounded by the
    // independently linked firmware window. No keys or policy are transported.
    unsafe { fstart_stage::anchor::install_locator(locator, image_size as u64) }
        .map_err(|_| ServiceError::InvalidParam)?;
    unsafe { fstart_stage::directory::install_directory_context(&media, directory) }
        .map_err(|_| ServiceError::HardwareError)
}

pub(crate) fn running_reservations(
    geometry: IntelBootLayout<'static>,
) -> Result<heapless::Vec<MemoryWindow, 16>, ServiceError> {
    let mut windows = heapless::Vec::new();
    for region in [
        geometry.region(RegionKind::Image)?,
        geometry.region(RegionKind::Writable)?,
    ]
    .into_iter()
    .chain(geometry.exclusions())
    {
        windows
            .push(MemoryWindow {
                start: region.base,
                size: region.size,
            })
            .map_err(|_| ServiceError::InvalidParam)?;
    }
    Ok(windows)
}

/// Exclude everything the firmware rewrites across an S3 resume from the map
/// handed to the OS: the bootstrap windows, the boot-media arena, the store
/// window and the low conventional-memory scratch the resume path uses. A
/// normal boot later returns the unused part of the store window.
///
/// Without this, Linux is free to allocate over the very bytes that postcar
/// and the ramstage are reloaded into on wake.
#[cfg(fstart_stage_env = "ram")]
pub(crate) fn reserve_firmware_memory(
    e820: &mut fstart_core::services::memory_detect::E820State,
    geometry: IntelBootLayout<'static>,
) -> Result<(), ServiceError> {
    use fstart_core::services::memory_detect::E820Kind;
    for region in geometry.firmware_owned_regions()? {
        e820.reserve_range_as(region.base, region.size, E820Kind::Reserved)?;
    }
    // Postcar handoff stash, S3 wake trampoline and SIPI page.
    e820.reserve_range_as(
        fstart_arch::x86::boot::LOW_SCRATCH_START,
        fstart_arch::x86::boot::LOW_SCRATCH_END - fstart_arch::x86::boot::LOW_SCRATCH_START,
        E820Kind::Reserved,
    )?;
    // Default-SMBASE ASEG: SMM relocation rewrites it during MP init, which
    // runs again on an S3 resume while the suspended OS image is live.
    let (aseg_start, aseg_end) = fstart_arch::x86::mp::SMM_DEFAULT_ASEG;
    e820.reserve_range_as(aseg_start, aseg_end - aseg_start, E820Kind::Reserved)?;
    Ok(())
}

#[cfg(fstart_stage_env = "ram")]
pub(crate) fn install_intel_load_policy(
    entries: &[fstart_core::services::memory_detect::E820Entry],
    geometry: IntelBootLayout<'static>,
) -> Result<(), ServiceError> {
    use fstart_core::services::memory_detect::{E820Kind, MAX_E820_ENTRIES};
    use fstart_stage::boot::MemoryPolicy;
    let mut writable = heapless::Vec::<MemoryWindow, MAX_E820_ENTRIES>::new();
    let mut reserved = heapless::Vec::<MemoryWindow, { MAX_E820_ENTRIES + 16 }>::new();
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
    // Never grant generic file loads the IVT/BDA, trampoline, SMRAM, handoff
    // page or the firmware store window.
    for window in running_reservations(geometry)? {
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
