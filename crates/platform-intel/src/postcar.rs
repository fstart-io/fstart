//! Intel postcar: fresh DRAM stack, caching on; authenticate and load the
//! ramstage. Postcar has no board work; a board's postcar `main` is one call.

use crate::IntelBoard;
#[cfg(any(feature = "ehci-console", feature = "flash-console"))]
use crate::{IntelChipsetConfig, IntelEarlyPlatform};
use fstart_core::services::{ConsoleDevice, ServiceError};
#[cfg(any(feature = "ehci-console", feature = "flash-console"))]
use fstart_driver_intel::IntelNorthbridgeDriver;
use fstart_log::StaticConsole;

#[cfg(any(feature = "ehci-console", feature = "flash-console"))]
type Nb<B> = <<B as crate::IntelBoardFacts>::Platform as IntelEarlyPlatform>::Northbridge;

/// Postcar keeps the bootblock's identity page tables, which live in the
/// flash the SPI flash log programs: a page walk while the chip is busy
/// would read garbage. Copy them to postcar's BSS first (the ramstage builds
/// its own before it loads anything). Returns whether postcar now runs on
/// DRAM tables, so the flash log is safe.
#[cfg(all(feature = "flash-console", target_arch = "x86_64"))]
fn dram_page_tables() -> bool {
    use fstart_arch::x86::boot::paging::{EntryTables, copy_entry_tables, load_cr3};
    static mut TABLES: EntryTables = EntryTables::new();
    // SAFETY: postcar's identity-mapped BSS, used for nothing else and
    // reserved through ramstage entry.
    match unsafe { copy_entry_tables(&mut *core::ptr::addr_of_mut!(TABLES)) } {
        Some(root) => {
            load_cr3(root);
            true
        }
        None => false,
    }
}
#[cfg(not(all(feature = "flash-console", target_arch = "x86_64")))]
fn dram_page_tables() -> bool {
    true
}

/// Post-CAR loader on a fresh DRAM stack. The bootblock-authenticated descriptor
/// crosses the transition in reserved RAM. Postcar verifies stored compressed
/// input and final initialized output before entering ramstage; it needs SHA-256
/// but no directory parser or public-key verifier in the linked execution path.
pub fn run<B: IntelBoard, C: ConsoleDevice + 'static>(
    slot: &'static StaticConsole<C>,
    config: C::Config,
) -> ! {
    use crate::IntelPlatform;
    use fstart_core::layout::RegionKind;
    let platform = B::Platform::NAME;
    let Ok(geometry) = crate::layout::IntelBootLayout::current(1) else {
        fstart_arch::x86::boot::halt()
    };
    let entry = fstart_timestamp::now();
    let mut store = crate::store::open(geometry).ok();
    if let Some(store) = &store {
        crate::store::attach_timestamps(store);
        fstart_timestamp::add_at(fstart_timestamp::id::POSTCAR_START, entry);
    }
    // The debug consoles reach PCI (the EHCI controller, the LPC bridge's
    // SPI registers) through the ECAM accessor, which each stage binds in
    // its own BSS, as the bootblock and ramstage do before their consoles.
    // PCIEXBAR itself stays programmed from the bootblock.
    #[cfg(any(feature = "ehci-console", feature = "flash-console"))]
    if Nb::<B>::new_from_config(B::CONFIG.northbridge())
        .and_then(|mut northbridge| northbridge.pre_console_init())
        .is_err()
    {
        fstart_arch::x86::boot::halt();
    }
    let ramstage_name = crate::RAMSTAGE_NAME;
    let mut console = match C::new(config) {
        Ok(console) => console,
        Err(_) => fstart_arch::x86::boot::halt(),
    };
    if console.init().is_err() {
        fstart_arch::x86::boot::halt();
    }
    slot.install(console);
    let flash_log = dram_page_tables();
    crate::debug_console::install::<B>(flash_log);
    fstart_log::info!("{} postcar: {} console ready", platform, C::NAME);

    // Postcar keeps the bootblock's ROM page tables: it runs uncached from
    // DRAM, so it stays small, and the ramstage builds the DRAM tables first.
    let (firmware_base, firmware_size) = match geometry.firmware() {
        Ok(window) => window,
        Err(_) => fstart_arch::x86::boot::halt(),
    };
    let entry = (|| -> Result<u64, ServiceError> {
        let stash = crate::boot::handoff(firmware_base, firmware_size)?;
        let descriptor = fstart_ffs::root::BootstrapDescriptor::parse(&stash.descriptor)
            .map_err(|_| ServiceError::InvalidParam)?;
        // Trusted preferred address, independent of the signed image's request.
        let ramstage_load_addr = geometry.region(RegionKind::BootstrapMainstage)?.base;
        let window = crate::boot::bootstrap_window(
            &descriptor,
            fstart_ffs::root::BootstrapRole::Mainstage,
            ramstage_load_addr,
            stash.ram_end,
            geometry,
        )?;
        let reserved = crate::boot::running_reservations(geometry)?;
        // SAFETY: family-owned trained DRAM range, live postcar excluded, and
        // the media mapping is trusted configuration, not descriptor data.
        let media = unsafe {
            fstart_core::services::boot_media::MemoryMapped::from_raw_addr(
                firmware_base,
                firmware_size,
            )
        };
        let resume =
            stash.boot_flags & fstart_arch::x86::boot::car_teardown::BOOT_FLAG_S3_RESUME != 0;
        let store = store.as_mut().ok_or(ServiceError::NotInitialized)?;
        fstart_timestamp::add(fstart_timestamp::id::LOAD_RAMSTAGE);
        let verified = crate::boot::load_stage_with_cache(
            &media,
            fstart_resume::stage_cache::CachedStage::Mainstage,
            &descriptor,
            window,
            &reserved,
            store,
            resume,
            const { fstart_stage::boot::codec(crate::compression::RAMSTAGE) },
        )?;
        Ok(verified.entry())
    })();
    let Ok(entry) = entry else {
        fstart_log::error!(
            "{} postcar: authentication/load failed for '{}'",
            platform,
            ramstage_name
        );
        fstart_arch::x86::boot::halt();
    };

    fstart_log::info!("jumping to {} at {:#x}", ramstage_name, entry);
    fstart_arch::x86::boot::jump_to(entry)
}
