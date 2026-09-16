//! Fixed Intel postcar flow: fresh DRAM stack, caching on; authenticate and
//! load the ramstage.

use crate::FfsLoadSpec;
use fstart_core::services::{ConsoleDevice, ServiceError};

/// Post-CAR loader on a fresh DRAM stack. The bootblock-authenticated descriptor
/// crosses the transition in reserved RAM. Postcar verifies stored compressed
/// input and final initialized output before entering ramstage; it needs SHA-256
/// but no directory parser or public-key verifier in the linked execution path.
pub(crate) fn run_intel_postcar<C: ConsoleDevice>(spec: FfsLoadSpec<C>) -> ! {
    use fstart_core::layout::RegionKind;
    let FfsLoadSpec {
        platform,
        geometry,
        console_config,
        console_node,
    } = spec;
    let ramstage_name = crate::RAMSTAGE_NAME;
    let mut console = match C::new(console_config) {
        Ok(console) => console,
        Err(_) => fstart_arch::x86_64::halt(),
    };
    if console.init().is_err() {
        fstart_arch::x86_64::halt();
    }
    // SAFETY: postcar owns this console until it jumps to the ramstage.
    unsafe { fstart_log::init(&console) };
    fstart_log::info!("{}: {} console ready", console_node, C::NAME);
    fstart_log::info!("{} postcar console ready", platform);

    // The early page tables live in the cache-as-RAM window at the top of the
    // 4 GiB space, where only the boot CPU can read them: an AP started later
    // begins in real mode with no cache and reads garbage, triple-faults, and the
    // chipset resets the platform. Postcar runs from DRAM, so build a replacement
    // set there and switch to it, giving every later stage and CPU one
    // DRAM-backed address space. The early tables cover too little for comfort as
    // well, so the new ones map the whole low 4 GiB.
    // SAFETY: PAGE_TABLES_ADDR is reserved low scratch, identity mapped by the
    // current tables, and postcar executes from DRAM, which the new tables map
    // identically. The region stays reserved until the payload replaces CR3.
    let tables = unsafe {
        fstart_arch::x86_64::paging::install_identity_tables(fstart_arch::x86_64::PAGE_TABLES_ADDR)
    };
    fstart_log::info!(
        "{} postcar: DRAM page tables at {:#x}, CR3 loaded",
        platform,
        tables
    );

    let (firmware_base, firmware_size) = match geometry.firmware() {
        Ok(window) => window,
        Err(_) => fstart_arch::x86_64::halt(),
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
        let resume = stash.boot_flags & fstart_arch::x86_64::car_teardown::BOOT_FLAG_S3_RESUME != 0;
        let verified = crate::boot::load_stage_with_cache(
            &media,
            fstart_stage::stage_cache::CachedStage::Mainstage,
            &descriptor,
            window,
            &reserved,
            geometry.region(RegionKind::StageCacheMainstage)?,
            resume,
        )?;
        Ok(verified.entry())
    })();
    let Ok(entry) = entry else {
        fstart_log::error!(
            "{} postcar: authentication/load failed for '{}'",
            platform,
            ramstage_name
        );
        fstart_arch::x86_64::halt();
    };

    fstart_log::info!("jumping to {} at {:#x}", ramstage_name, entry);
    fstart_arch::x86_64::jump_to(entry)
}
