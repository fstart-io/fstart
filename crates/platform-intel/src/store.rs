//! Intel placement of the firmware store ([`fstart_store`]).
//!
//! The platform links a store window into every stage's layout. The
//! bootblock creates the store at its base once DRAM is trained, or reopens
//! it on S3 resume; postcar and the ramstage reopen it. On a normal boot the
//! ramstage seals it after the tables: the used part stays reserved and the
//! rest of the window goes back to the OS. A resume never adds entries, so
//! it never grows into memory the OS owns.

use crate::layout::IntelBootLayout;
use core::ptr::NonNull;
use fstart_core::layout::{Region, RegionKind};
use fstart_core::services::ServiceError;
use fstart_store::Store;

fn open_window(region: Region, create: bool) -> Result<Store, ServiceError> {
    let base = NonNull::new(region.base as *mut u8).ok_or(ServiceError::InvalidParam)?;
    let size = usize::try_from(region.size).map_err(|_| ServiceError::InvalidParam)?;
    // SAFETY: the window is platform-reserved DRAM, excluded from every stage
    // load window and from OS-visible RAM until the store is sealed.
    let store = unsafe {
        if create {
            Store::create(base, size)
        } else {
            Store::open(base, size)
        }
    };
    store.map_err(|_| {
        fstart_log::error!("store: no valid store at {:#x}", region.base);
        ServiceError::HardwareError
    })
}

/// Create the store, or reopen it when resuming from S3.
#[cfg(fstart_stage_env = "car")]
pub(crate) fn bootblock(
    geometry: IntelBootLayout<'static>,
    ram_end: u64,
    resume: bool,
) -> Result<Store, ServiceError> {
    open_window(
        geometry.destination(RegionKind::FirmwareStore, ram_end)?,
        !resume,
    )
}

/// Reopen the store the bootblock published.
#[cfg(any(fstart_stage_env = "postcar", fstart_stage_env = "ram"))]
pub(crate) fn open(geometry: IntelBootLayout<'static>) -> Result<Store, ServiceError> {
    open_window(geometry.region(RegionKind::FirmwareStore)?, false)
}

/// Write the used store back from the cache. The bootblock writes DRAM
/// before CAR teardown, whose INVD would discard dirty lines.
#[cfg(fstart_stage_env = "car")]
pub(crate) fn write_back(store: &Store) {
    // SAFETY: the used store is mapped DRAM; CLFLUSH only writes it back.
    #[cfg(all(target_arch = "x86_64", target_os = "none"))]
    unsafe {
        core::arch::asm!("mfence", options(nostack, preserves_flags));
        for address in (store.base()..store.base() + store.used()).step_by(64) {
            core::arch::x86_64::_mm_clflush(address as *const u8);
        }
        core::arch::asm!("mfence", options(nostack, preserves_flags));
    }
}

/// Stop the store's growth and return the window above it to the OS.
/// Run once per normal boot, after the last entry is added.
#[cfg(fstart_stage_env = "ram")]
pub(crate) fn seal(
    store: &mut Store,
    e820: &mut fstart_core::services::memory_detect::E820State,
    geometry: IntelBootLayout<'static>,
) -> Result<(), ServiceError> {
    use fstart_core::services::memory_detect::E820Kind;
    let window = geometry.region(RegionKind::FirmwareStore)?;
    let kept = store.limit(0x1000) as u64;
    e820.set_range_kind(
        window.base + kept,
        window.size.saturating_sub(kept),
        E820Kind::Ram,
    )?;
    fstart_log::info!(
        "store: {} of {} KiB used, {} KiB kept reserved",
        (store.used() >> 10) as u32,
        (window.size >> 10) as u32,
        (kept >> 10) as u32
    );
    Ok(())
}
