//! Fixed Intel bootblock flow (CAR): console, DRAM training, authentication
//! and load of postcar.

use crate::{
    FfsLoadSpec, IntelBoard, IntelChipsetConfig, IntelEarlyBoardHooks, IntelEarlyCtx,
    IntelEarlyPlatform,
};
use fstart_core::services::memory_detect::MemoryDetector;
use fstart_core::services::{ConsoleDevice, ServiceError};
use fstart_driver_intel::{BootPath, IntelNorthbridgeDriver, IntelSouthbridgeDriver};

pub(crate) fn run_intel_bootblock<B: IntelBoard>(
    spec: FfsLoadSpec<B::Console>,
    hooks: &mut B::Hooks,
) -> Result<(), ServiceError> {
    type Nb<B> = <<B as IntelBoard>::Platform as IntelEarlyPlatform>::Northbridge;
    type Sb<B> = <<B as IntelBoard>::Platform as IntelEarlyPlatform>::Southbridge;
    let mut northbridge = Nb::<B>::new_from_config(B::CONFIG.northbridge())?;
    let mut southbridge = Sb::<B>::new_from_config(B::CONFIG.southbridge())?;
    use fstart_core::layout::RegionKind;
    let FfsLoadSpec {
        platform,
        geometry,
        console_config,
        console_node,
    } = spec;
    let (firmware_base, firmware_size) = geometry.firmware()?;
    // Trusted preferred addresses, independent of the signed images' requests.
    let postcar_load_addr = geometry.region(RegionKind::BootstrapPostcar)?.base;
    let ramstage_load_addr = geometry.region(RegionKind::BootstrapMainstage)?.base;
    // End of the linked low-DRAM envelope; the trained limit itself travels in
    // the postcar MTRR stash.
    let dram_end = geometry
        .region(RegionKind::BootstrapRam)?
        .end()
        .ok_or(ServiceError::InvalidParam)?;

    northbridge.pre_console_init()?;
    southbridge.pre_console_init()?;
    hooks.before_console(&mut IntelEarlyCtx::new(&mut southbridge))?;

    let mut console = B::Console::new(console_config)?;
    console.init()?;
    // SAFETY: this function never returns after installing the stack-owned console.
    unsafe { fstart_log::init(&console) };
    fstart_log::info!("{}: {} console ready", console_node, B::Console::NAME);
    fstart_log::info!("{} bootblock console ready", platform);

    // Complete the pre-RAM chipset flow before touching the postcar load
    // address. The bootblock itself executes from ROM with its writable state
    // in CAR, but the next stage is loaded into ordinary DRAM.
    northbridge.early_init()?;
    southbridge.early_init()?;
    hooks.before_memory(&mut IntelEarlyCtx::new(&mut southbridge))?;

    fstart_log::info!("{}: initializing DRAM", platform);
    let boot_path = if southbridge.detect_s3_resume() {
        BootPath::S3Resume
    } else if northbridge.detect_warm_reset() {
        BootPath::WarmReset
    } else {
        BootPath::Normal
    };
    northbridge.set_boot_path(boot_path);
    northbridge.dram_init_with_smbus(southbridge.smbus_mut())?;
    northbridge.early_post_dram_init()?;
    southbridge.early_post_dram_init()?;
    hooks.after_memory(&mut IntelEarlyCtx::new(&mut southbridge))?;
    fstart_log::info!("{}: DRAM ready", platform);

    fstart_log::info!(
        "boot trust: development-integrity; RO root and rollback enforcement not established"
    );
    fstart_arch::x86_64::enable_boot_media_rom_cache();
    // SAFETY: the firmware window comes from trusted linked/platform geometry.
    let media = unsafe {
        fstart_core::services::boot_media::MemoryMapped::from_raw_addr(firmware_base, firmware_size)
    };
    // Snapshot locator fields once. The same bounded bytes drive root lookup
    // and the retained handoff; AP vendor microcode does not gain directory auth.
    let locator = unsafe {
        fstart_core::ffs::locator::LocatorRef::read_volatile(fstart_stage::fstart_anchor_bytes())
    }
    .ok_or(ServiceError::InvalidParam)?
    .value();
    if locator.image_offset != 0 || !locator.media().validate(firmware_size as u64) {
        return Err(ServiceError::InvalidParam);
    }
    let mut locator_bytes = [0; fstart_core::ffs::locator::LOCATOR_SIZE];
    locator.write_to(&mut locator_bytes);
    let root = fstart_stage::root::authenticate_boot_root(&locator_bytes, &media)
        .map_err(|_| ServiceError::HardwareError)?;
    let [Some(postcar), Some(ramstage)] = root.descriptors() else {
        return Err(ServiceError::InvalidParam);
    };
    // Only trained low memory may be used, regardless of the signed request.
    let ram_end = dram_end.min(northbridge.total_ram_bytes()?);
    let postcar_window = crate::boot::bootstrap_window(
        postcar,
        fstart_ffs::root::BootstrapRole::Postcar,
        postcar_load_addr,
        ram_end,
        geometry,
    )?;
    let _ = crate::boot::bootstrap_window(
        ramstage,
        fstart_ffs::root::BootstrapRole::Mainstage,
        ramstage_load_addr,
        ram_end,
        geometry,
    )?;
    let reserved = crate::boot::running_reservations(geometry)?;
    // SAFETY: trained DRAM, bounded family-owned postcar window, and all live
    // bootblock code/data/stack excluded. The loader verifies final bytes.
    let verified = unsafe {
        fstart_stage::boot::load_bootstrap(
            &media,
            postcar,
            &fstart_stage::boot::MemoryPolicy {
                writable: &[postcar_window],
                reserved: &reserved,
                entry_alignment: 1,
            },
        )
    }
    .map_err(|_| ServiceError::HardwareError)?;

    // Explicit wire bytes avoid coupling the assembly MTRR ABI to Rust types.
    // The low handoff page is disjoint from both family bootstrap windows.
    let published = unsafe {
        fstart_arch::x86_64::car_teardown::write_postcar_stash(
            ram_end,
            firmware_base,
            firmware_size as u64,
            fstart_arch::x86_64::car_teardown::PostcarBootContext {
                descriptor: ramstage.encode(),
                directory: root.directory().encode(),
                image_family: root.root().image_family,
                security_version: root.root().security_version,
                locator: locator_bytes,
            },
        )
    };
    if !published {
        return Err(ServiceError::HardwareError);
    }

    hooks.before_handoff(&mut IntelEarlyCtx::new(&mut southbridge))?;
    fstart_log::info!(
        "jumping to {} at {:#x}",
        crate::POSTCAR_STAGE_NAME,
        verified.entry()
    );
    fstart_arch::x86_64::jump_to(verified.entry())
}
