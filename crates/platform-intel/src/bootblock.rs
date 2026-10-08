//! Fixed Intel CAR flow: console, memory training/recovery, DMI/PM,
//! authenticated bootstrap and retained stage publication.

use crate::{
    FfsLoadSpec, IntelBoard, IntelChipsetConfig, IntelEarlyBoardHooks, IntelEarlyCtx,
    IntelEarlyPlatform, IntelSmbusRouting, SmbusRoute,
};
use fstart_core::layout::RegionKind;
use fstart_core::services::memory_detect::MemoryDetector;
use fstart_core::services::{ConsoleDevice, ServiceError};
use fstart_driver_intel::{BootPath, IntelNorthbridgeDriver, IntelSouthbridgeDriver};

/// Boot timestamps until the firmware store exists, in CAR.
#[repr(C, align(8))]
struct CarTimestamps([u8; fstart_timestamp::table_size(32)]);
static mut CAR_TIMESTAMPS: CarTimestamps = CarTimestamps([0; fstart_timestamp::table_size(32)]);

/// Capacity of the store's timestamp table for the whole boot.
const TIMESTAMP_CAPACITY: usize = 96;

pub(crate) fn run_intel_bootblock<B: IntelBoard>(
    spec: FfsLoadSpec<B::Console>,
    hooks: &mut B::EarlyHooks,
) -> Result<(), ServiceError> {
    let entry = fstart_timestamp::now();
    // SAFETY: CAR-resident bootblock data, used only through the table.
    unsafe {
        let car = &raw mut CAR_TIMESTAMPS;
        fstart_timestamp::init(
            car.cast(),
            size_of::<CarTimestamps>(),
            fstart_arch::x86::boot::reset_tsc(),
        );
    }
    fstart_timestamp::add_at(fstart_timestamp::id::BOOTBLOCK_START, entry);
    type Nb<B> = <<B as crate::IntelBoardFacts>::Platform as IntelEarlyPlatform>::Northbridge;
    type Sb<B> = <<B as crate::IntelBoardFacts>::Platform as IntelEarlyPlatform>::Southbridge;
    let mut northbridge = Nb::<B>::new_from_config(B::CONFIG.northbridge())?;
    let mut southbridge = Sb::<B>::new_from_config(B::CONFIG.southbridge())?;
    let FfsLoadSpec {
        platform,
        geometry,
        console_config,
        console_node,
    } = spec;
    let (firmware_base, firmware_size) = geometry.firmware()?;
    let postcar_load_addr = geometry.region(RegionKind::BootstrapPostcar)?.base;
    let ramstage_load_addr = geometry.region(RegionKind::BootstrapMainstage)?.base;
    let dram_end = geometry
        .region(RegionKind::BootstrapRam)?
        .end()
        .ok_or(ServiceError::InvalidParam)?;
    northbridge.pre_console_init()?;
    southbridge.pre_console_init()?;
    hooks.before_console(&mut IntelEarlyCtx::new(&mut southbridge))?;
    let mut console = B::Console::new(console_config)?;
    console.init()?;
    fstart_timestamp::add(fstart_timestamp::id::CONSOLE_READY);
    // SAFETY: successful stage handoff never returns past this console's lifetime.
    unsafe { fstart_log::init(&console) };
    fstart_log::info!("{}: {} console ready", console_node, B::Console::NAME);
    fstart_log::info!("{} bootblock console ready", platform);
    fstart_timestamp::add(fstart_timestamp::id::EARLY_INIT);
    northbridge.early_init()?;
    southbridge.early_init()?;
    // Consume the southbridge indication exactly once, before board clock
    // programming. Resume hooks must not perturb retained DRAM frequency.
    let boot_path = if southbridge.detect_s3_resume() {
        BootPath::S3Resume
    } else if northbridge.detect_warm_reset() {
        BootPath::WarmReset
    } else {
        BootPath::Normal
    };
    northbridge.set_boot_path(boot_path);
    let result = (|| {
        if boot_path == BootPath::S3Resume
            && (!B::FACTS.memory_cache || !northbridge.supports_s3_replay())
        {
            return Err(ServiceError::NotSupported);
        }
        B::EarlyHooks::select_smbus(&southbridge, SmbusRoute::Spd)?;
        hooks.before_memory(&mut IntelEarlyCtx::with_boot_path(
            &mut southbridge,
            boot_path,
        ))?;
        fstart_timestamp::add(fstart_timestamp::id::INITRAM_START);
        fstart_log::info!("{}: initializing DRAM", platform);

        #[cfg(feature = "memory-cache")]
        let key = if B::FACTS.memory_cache {
            Some(crate::memory_cache::runtime_key::<B>(
                &northbridge,
                &southbridge,
                geometry,
            )?)
        } else {
            None
        };
        #[cfg(feature = "memory-cache")]
        let mut capture = [0; crate::memory_cache::MAX_PAYLOAD];
        #[cfg(feature = "memory-cache")]
        let captured = if let Some(key) = key {
            use crate::memory_cache::{BANK_SIZE, EXTENT_SIZE, Journal, MappedRead};
            if firmware_size < EXTENT_SIZE as usize + 4096 {
                return Err(ServiceError::InvalidParam);
            }
            // The extent comes from linked board policy, not an unverified directory.
            let mut flash =
                unsafe { MappedRead::new(firmware_base as usize, EXTENT_SIZE as usize) };
            let cached = Journal::new(BANK_SIZE, EXTENT_SIZE)?
                .load(&mut flash)?
                .map(|(_, record)| record)
                .filter(|record| record.key == key);
            northbridge.dram_init_cached(
                southbridge.smbus_mut(),
                cached.as_ref().map(|record| record.payload()),
                &mut capture,
            )?
        } else {
            if boot_path == BootPath::S3Resume {
                return Err(ServiceError::NotSupported);
            }
            northbridge.dram_init_with_smbus(southbridge.smbus_mut())?;
            None
        };
        #[cfg(not(feature = "memory-cache"))]
        {
            if boot_path == BootPath::S3Resume {
                return Err(ServiceError::NotSupported);
            }
            northbridge.dram_init_with_smbus(southbridge.smbus_mut())?;
        }

        // SB DMI enable -> NB negotiation -> SB polling -> NB PM/IGD.
        fstart_timestamp::add(fstart_timestamp::id::POST_DRAM_INIT);
        southbridge.prepare_early_post_dram_init()?;
        northbridge.early_post_dram_init()?;
        southbridge.early_post_dram_init()?;
        northbridge.finish_early_post_dram_init()?;
        B::EarlyHooks::select_smbus(&southbridge, SmbusRoute::Eeprom)?;
        hooks.after_memory(&mut IntelEarlyCtx::with_boot_path(
            &mut southbridge,
            boot_path,
        ))?;
        fstart_timestamp::add(fstart_timestamp::id::INITRAM_END);
        fstart_log::info!("{}: DRAM ready", platform);
        let ram_end = dram_end.min(northbridge.total_ram_bytes()?);
        let mut store =
            crate::store::bootblock(geometry, ram_end, boot_path == BootPath::S3Resume)?;
        crate::store::move_timestamps(&mut store, TIMESTAMP_CAPACITY);
        crate::memory_info::publish(&mut store, northbridge.memory_info().as_ref())?;
        #[cfg(feature = "memory-cache")]
        if let (Some(key), Some(length)) = (key, captured) {
            let entry = store
                .add(fstart_store::tag::TRAINING, 0x1000, 3)
                .map_err(|_| ServiceError::InvalidParam)?;
            let pending = fstart_core::layout::Region {
                kind: RegionKind::FirmwareStore,
                base: store.address(&entry) as u64,
                size: entry.len() as u64,
            };
            // Only cold/warm initialization captures, after the driver's RAM test.
            unsafe {
                crate::memory_cache::publish_pending(pending, key, &capture[..length])?;
            }
        }

        fstart_log::info!(
            "boot trust: development-integrity; RO root and rollback enforcement not established"
        );
        fstart_arch::x86::boot::enable_boot_media_rom_cache();
        let media = unsafe {
            fstart_core::services::boot_media::MemoryMapped::from_raw_addr(
                firmware_base,
                firmware_size,
            )
        };
        let locator =
            unsafe {
                fstart_core::ffs::locator::LocatorRef::read_volatile(
                    fstart_stage::fstart_anchor_bytes(),
                )
            }
            .ok_or(ServiceError::InvalidParam)?
            .value();
        if locator.image_offset != 0 || !locator.media().validate(firmware_size as u64) {
            return Err(ServiceError::InvalidParam);
        }
        let mut locator_bytes = [0; fstart_core::ffs::locator::LOCATOR_SIZE];
        locator.write_to(&mut locator_bytes);
        fstart_timestamp::add(fstart_timestamp::id::VERIFY_BOOT_ROOT);
        let root = fstart_stage::root::authenticate_boot_root(&locator_bytes, &media)
            .map_err(|_| ServiceError::HardwareError)?;
        #[cfg(feature = "memory-cache")]
        if let Some(key) = key {
            if crate::memory_cache::runtime_key::<B>(&northbridge, &southbridge, geometry)? != key {
                return Err(ServiceError::HardwareError);
            }
        }
        let [Some(postcar), Some(ramstage)] = root.descriptors() else {
            return Err(ServiceError::InvalidParam);
        };
        let postcar_window = crate::boot::bootstrap_window(
            postcar,
            fstart_ffs::root::BootstrapRole::Postcar,
            postcar_load_addr,
            ram_end,
            geometry,
        )?;
        crate::boot::bootstrap_window(
            ramstage,
            fstart_ffs::root::BootstrapRole::Mainstage,
            ramstage_load_addr,
            ram_end,
            geometry,
        )?;
        let reserved = crate::boot::running_reservations(geometry)?;
        fstart_timestamp::add(fstart_timestamp::id::LOAD_POSTCAR);
        let verified = crate::boot::load_stage_with_cache(
            &media,
            fstart_stage::stage_cache::CachedStage::Postcar,
            postcar,
            postcar_window,
            &reserved,
            &mut store,
            boot_path == BootPath::S3Resume,
        )?;
        let boot_flags = if boot_path == BootPath::S3Resume {
            fstart_arch::x86::boot::car_teardown::BOOT_FLAG_S3_RESUME
        } else {
            0
        };
        let published = unsafe {
            fstart_arch::x86::boot::car_teardown::write_postcar_stash(
                ram_end,
                firmware_base,
                firmware_size as u64,
                fstart_arch::x86::boot::car_teardown::PostcarBootContext {
                    descriptor: ramstage.encode(),
                    directory: root.directory().encode(),
                    image_family: root.root().image_family,
                    security_version: root.root().security_version,
                    locator: locator_bytes,
                    boot_flags,
                },
            )
        };
        if !published {
            return Err(ServiceError::HardwareError);
        }
        hooks.before_handoff(&mut IntelEarlyCtx::with_boot_path(
            &mut southbridge,
            boot_path,
        ))?;
        fstart_log::info!(
            "jumping to {} at {:#x}",
            crate::POSTCAR_STAGE_NAME,
            verified.entry()
        );
        crate::store::write_back(&store);
        fstart_arch::x86::boot::jump_to(verified.entry())
    })();
    if result.is_err() && boot_path == BootPath::S3Resume {
        fstart_log::error!(
            "{}: invalid retained boot state; resetting cleanly",
            platform
        );
        northbridge.prepare_resume_reset();
        southbridge.system_reset(true);
    }
    result
}
