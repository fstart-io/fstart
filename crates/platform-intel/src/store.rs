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

/// Body capacity of the DRAM firmware log in `fstart_store::tag::CONSOLE`.
pub(crate) const CONSOLE_LOG_CAPACITY: usize = 64 * 1024;

/// Platform-owned CAR storage for early bootblock console logging.
///
/// One-shot exclusive take, mirroring `TrainingWorkspace`. Only compiled into
/// bootblock binaries so later stages carry zero bytes of CAR log BSS.
#[cfg(feature = "bundle-bootblock")]
#[repr(C, align(8))]
pub(crate) struct CarLogWorkspace<const N: usize> {
    // First field inherits the struct's 8-byte alignment for `fstart_log::mem::init`.
    buf: core::cell::UnsafeCell<[u8; N]>,
    taken: core::sync::atomic::AtomicBool,
}

#[cfg(feature = "bundle-bootblock")]
// SAFETY: bootblock runs single-threaded on the BSP; `taken` enforces one-shot
// exclusive mutable access.
unsafe impl<const N: usize> Sync for CarLogWorkspace<N> {}

#[cfg(feature = "bundle-bootblock")]
impl<const N: usize> CarLogWorkspace<N> {
    pub(crate) const fn new() -> Self {
        Self {
            buf: core::cell::UnsafeCell::new([0; N]),
            taken: core::sync::atomic::AtomicBool::new(false),
        }
    }

    pub(crate) fn take(&'static self) -> Option<&'static mut [u8]> {
        if self.taken.swap(true, core::sync::atomic::Ordering::AcqRel) {
            return None;
        }
        // SAFETY: one-shot exclusive take in BSP CAR BSS.
        Some(unsafe { &mut *self.buf.get() })
    }
}

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
pub(crate) fn open(geometry: IntelBootLayout<'static>) -> Result<Store, ServiceError> {
    open_window(geometry.region(RegionKind::FirmwareStore)?, false)
}

/// Move the boot timestamps from CAR into the store, reusing the entry a
/// resumed store already has. Timestamps are diagnostics: without room
/// they stay in CAR and are lost with it.
pub(crate) fn move_timestamps(store: &mut Store, capacity: usize) {
    let size = fstart_timestamp::table_size(capacity);
    let entry = store
        .find(fstart_store::tag::TIMESTAMPS)
        .filter(|entry| entry.len() >= size)
        .or_else(|| store.add(fstart_store::tag::TIMESTAMPS, size, 3).ok());
    if let Some(entry) = entry {
        // SAFETY: a store entry owned by the timestamp table from now on.
        unsafe { fstart_timestamp::move_to(store.address(&entry) as *mut u8, entry.len()) };
    }
}

/// Attach to the timestamp table the bootblock moved into the store.
pub(crate) fn attach_timestamps(store: &Store) {
    if let Some(entry) = store.find(fstart_store::tag::TIMESTAMPS) {
        // SAFETY: the store entry holds the bootblock's table.
        unsafe { fstart_timestamp::attach(store.address(&entry) as *mut u8, entry.len()) };
    }
}

/// Move the bootblock console log from CAR into the firmware store (or start
/// recording into DRAM for platforms without a CAR log buffer). On S3 resume,
/// reuse the existing store entry without growing the store.
pub(crate) fn move_console(store: &mut Store, capacity: usize, resume: bool) {
    let size = fstart_log::mem::buffer_size(capacity);
    if resume {
        if let Some(entry) = store.find(fstart_store::tag::CONSOLE) {
            // SAFETY: existing DRAM store entry owned by the memory console.
            unsafe { fstart_log::mem::append_to(store.address(&entry) as *mut u8, entry.len()) };
        }
        return;
    }
    let entry = store
        .find(fstart_store::tag::CONSOLE)
        .filter(|entry| entry.len() >= size)
        .or_else(|| store.add(fstart_store::tag::CONSOLE, size, 3).ok());
    if let Some(entry) = entry {
        // SAFETY: a store entry owned by the memory console from now on.
        unsafe { fstart_log::mem::move_to(store.address(&entry) as *mut u8, entry.len()) };
    }
}

/// Attach to the in-memory console log the bootblock published in the store.
pub(crate) fn attach_console(store: &Store) {
    if let Some(entry) = store.find(fstart_store::tag::CONSOLE) {
        // SAFETY: the store entry holds the bootblock's memory log.
        unsafe { fstart_log::mem::attach(store.address(&entry) as *mut u8, entry.len()) };
    }
}

/// Write the used store back from the cache. The bootblock writes DRAM
/// before CAR teardown, whose INVD would discard dirty lines.
pub(crate) fn write_back(store: &Store) {
    // SAFETY: the used store is mapped DRAM; CLFLUSH only writes it back.
    #[cfg(all(any(target_arch = "x86", target_arch = "x86_64"), target_os = "none"))]
    unsafe {
        core::arch::asm!("mfence", options(nostack, preserves_flags));
        for address in (store.base()..store.base() + store.used()).step_by(64) {
            fstart_arch::x86::flush_cache_line(address as *const u8);
        }
        core::arch::asm!("mfence", options(nostack, preserves_flags));
    }
}

/// Stop the store's growth and return the window above it to the OS.
/// Run once per normal boot, after the last entry is added.
#[cfg(feature = "ramstage")]
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
