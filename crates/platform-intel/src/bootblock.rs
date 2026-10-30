//! Intel CAR bootblock phases: console, memory training/recovery, DMI/PM,
//! authenticated bootstrap and retained stage publication.
//!
//! The board's bootblock `main` calls the phases in order and does its own
//! work between them. Each phase is a state transition, so a phase cannot
//! run before the one it depends on:
//!
//! ```ignore
//! let bb = Bootblock::<Board>::enter();
//! // board: decode/route the console
//! let bb = bb.console(&CONSOLE, console_config());
//! // board: make the SPDs reachable
//! let mut bb = bb.train_memory().post_dram();
//! // board: devices that need trained DRAM or the SPD branch
//! bb.load_postcar()
//! ```
//!
//! A phase that fails halts, or on an S3 resume resets cleanly instead of
//! booting over retained state.

use crate::{IntelBoard, IntelChipsetConfig, IntelEarlyPlatform, IntelPlatform};
use core::marker::PhantomData;
use fstart_core::layout::RegionKind;
use fstart_core::services::memory_detect::MemoryDetector;
use fstart_core::services::{ConsoleDevice, ServiceError};
use fstart_driver_intel::{BootPath, IntelNorthbridgeDriver, IntelSouthbridgeDriver};
use fstart_log::StaticConsole;

/// Boot timestamps until the firmware store exists, in CAR.
#[repr(C, align(8))]
struct CarTimestamps([u8; fstart_timestamp::table_size(32)]);
static mut CAR_TIMESTAMPS: CarTimestamps = CarTimestamps([0; fstart_timestamp::table_size(32)]);

/// Capacity of the store's timestamp table for the whole boot.
const TIMESTAMP_CAPACITY: usize = 96;

type Nb<B> = <<B as crate::IntelBoardFacts>::Platform as IntelEarlyPlatform>::Northbridge;
type Sb<B> = <<B as crate::IntelBoardFacts>::Platform as IntelEarlyPlatform>::Southbridge;

/// Chipset decode only: no console, no DRAM.
pub struct Early;
/// The console is up and the boot path is known.
pub struct Console;
/// DRAM is trained or recovered; DMI and the post-DRAM chipset setup have
/// not run.
pub struct Trained;
/// DRAM and the chipset are ready for the next stage.
pub struct Dram;

/// Memory-cache state carried from training to the store. The captured
/// record itself stays in CAR, out of the moving phase state.
#[cfg(feature = "memory-cache")]
struct Training {
    key: Option<[u8; 32]>,
    captured: Option<usize>,
}

#[cfg(feature = "memory-cache")]
static mut CAPTURE: [u8; crate::memory_cache::MAX_PAYLOAD] = [0; crate::memory_cache::MAX_PAYLOAD];

/// The bootblock of an Intel CAR board, in phase `S`.
pub struct Bootblock<B: IntelBoard, S = Early> {
    northbridge: Nb<B>,
    southbridge: Sb<B>,
    geometry: crate::layout::IntelBootLayout<'static>,
    boot_path: BootPath,
    #[cfg(feature = "memory-cache")]
    training: Training,
    _state: PhantomData<(B, S)>,
}

impl<B: IntelBoard, S> Bootblock<B, S> {
    fn into<T>(self) -> Bootblock<B, T> {
        Bootblock {
            northbridge: self.northbridge,
            southbridge: self.southbridge,
            geometry: self.geometry,
            boot_path: self.boot_path,
            #[cfg(feature = "memory-cache")]
            training: self.training,
            _state: PhantomData,
        }
    }

    pub fn northbridge(&self) -> &Nb<B> {
        &self.northbridge
    }

    pub fn southbridge(&self) -> &Sb<B> {
        &self.southbridge
    }

    pub fn southbridge_mut(&mut self) -> &mut Sb<B> {
        &mut self.southbridge
    }

    /// Cold, warm or S3 resume. `Normal` until [`Bootblock::console`].
    pub fn boot_path(&self) -> BootPath {
        self.boot_path
    }

    /// Stop the boot: an S3 resume resets instead of booting over retained
    /// state, anything else halts.
    pub fn fail(&self, what: &str) -> ! {
        fstart_log::error!("{} bootblock: {} failed", B::Platform::NAME, what);
        if self.boot_path == BootPath::S3Resume {
            fstart_log::error!(
                "{}: invalid retained boot state; resetting cleanly",
                B::Platform::NAME
            );
            self.northbridge.prepare_resume_reset();
            self.southbridge.system_reset(true);
        }
        fstart_arch::x86::boot::halt()
    }

    fn check<T>(&self, what: &str, result: Result<T, ServiceError>) -> T {
        result.unwrap_or_else(|_| self.fail(what))
    }
}

impl<B: IntelBoard> Bootblock<B, Early> {
    /// Start the boot timestamps in CAR, bind the chipset from the board's
    /// configuration and open the decode windows the console needs.
    pub fn enter() -> Self {
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
        let (Ok(mut northbridge), Ok(mut southbridge), Ok(geometry)) = (
            Nb::<B>::new_from_config(B::CONFIG.northbridge()),
            Sb::<B>::new_from_config(B::CONFIG.southbridge()),
            crate::layout::IntelBootLayout::current(0),
        ) else {
            fstart_arch::x86::boot::halt()
        };
        if northbridge.pre_console_init().is_err() || southbridge.pre_console_init().is_err() {
            fstart_arch::x86::boot::halt()
        }
        Self {
            northbridge,
            southbridge,
            geometry,
            boot_path: BootPath::Normal,
            #[cfg(feature = "memory-cache")]
            training: Training {
                key: None,
                captured: None,
            },
            _state: PhantomData,
        }
    }

    /// Bring up the console, finish the chipset's early init and consume the
    /// boot path indication.
    pub fn console<C: ConsoleDevice + 'static>(
        mut self,
        slot: &'static StaticConsole<C>,
        config: C::Config,
    ) -> Bootblock<B, Console> {
        let Ok(mut console) = C::new(config) else {
            fstart_arch::x86::boot::halt()
        };
        if console.init().is_err() {
            fstart_arch::x86::boot::halt()
        }
        slot.install(console);
        fstart_timestamp::add(fstart_timestamp::id::CONSOLE_READY);
        fstart_log::info!("{} bootblock: {} console ready", B::Platform::NAME, C::NAME);
        fstart_timestamp::add(fstart_timestamp::id::EARLY_INIT);
        let early = self
            .northbridge
            .early_init()
            .and_then(|()| self.southbridge.early_init());
        self.check("early init", early);
        // Consume the southbridge indication exactly once, before board clock
        // programming. Resume work must not perturb retained DRAM frequency.
        self.boot_path = if self.southbridge.detect_s3_resume() {
            BootPath::S3Resume
        } else if self.northbridge.detect_warm_reset() {
            BootPath::WarmReset
        } else {
            BootPath::Normal
        };
        self.northbridge.set_boot_path(self.boot_path);
        if self.boot_path == BootPath::S3Resume
            && (!B::FACTS.memory_cache || !self.northbridge.supports_s3_replay())
        {
            self.fail("S3 resume without a memory cache");
        }
        self.into()
    }
}

impl<B: IntelBoard> Bootblock<B, Console> {
    /// Train DRAM, or replay the cached training. The SPDs must be
    /// reachable on the southbridge's SMBus.
    pub fn train_memory(mut self) -> Bootblock<B, Trained> {
        fstart_timestamp::add(fstart_timestamp::id::INITRAM_START);
        fstart_log::info!("{}: initializing DRAM", B::Platform::NAME);
        let trained = self.dram_init();
        self.check("DRAM init", trained);
        self.into()
    }

    #[cfg(feature = "memory-cache")]
    fn dram_init(&mut self) -> Result<(), ServiceError> {
        use crate::memory_cache::{BANK_SIZE, EXTENT_SIZE, Journal, MappedRead};
        if !B::FACTS.memory_cache {
            if self.boot_path == BootPath::S3Resume {
                return Err(ServiceError::NotSupported);
            }
            return self
                .northbridge
                .dram_init_with_smbus(self.southbridge.smbus_mut());
        }
        let key = crate::memory_cache::runtime_key::<B>(
            &self.northbridge,
            &self.southbridge,
            self.geometry,
        )?;
        let (firmware_base, firmware_size) = self.geometry.firmware()?;
        if firmware_size < EXTENT_SIZE as usize + 4096 {
            return Err(ServiceError::InvalidParam);
        }
        // The extent comes from linked board policy, not an unverified directory.
        let mut flash = unsafe { MappedRead::new(firmware_base as usize, EXTENT_SIZE as usize) };
        let cached = Journal::new(BANK_SIZE, EXTENT_SIZE)?
            .load(&mut flash)?
            .map(|(_, record)| record)
            .filter(|record| record.key == key);
        self.training.key = Some(key);
        // SAFETY: the bootblock is single-threaded; only training writes it.
        let capture = unsafe { &mut *core::ptr::addr_of_mut!(CAPTURE) };
        self.training.captured = self.northbridge.dram_init_cached(
            self.southbridge.smbus_mut(),
            cached.as_ref().map(|record| record.payload()),
            capture,
        )?;
        Ok(())
    }

    #[cfg(not(feature = "memory-cache"))]
    fn dram_init(&mut self) -> Result<(), ServiceError> {
        if self.boot_path == BootPath::S3Resume {
            return Err(ServiceError::NotSupported);
        }
        self.northbridge
            .dram_init_with_smbus(self.southbridge.smbus_mut())
    }
}

impl<B: IntelBoard> Bootblock<B, Trained> {
    /// SB DMI enable -> NB negotiation -> SB polling -> NB PM/IGD.
    pub fn post_dram(mut self) -> Bootblock<B, Dram> {
        fstart_timestamp::add(fstart_timestamp::id::POST_DRAM_INIT);
        let done = (|| {
            self.southbridge.prepare_early_post_dram_init()?;
            self.northbridge.early_post_dram_init()?;
            self.southbridge.early_post_dram_init()?;
            self.northbridge.finish_early_post_dram_init()
        })();
        self.check("post-DRAM init", done);
        fstart_timestamp::add(fstart_timestamp::id::INITRAM_END);
        fstart_log::info!("{}: DRAM ready", B::Platform::NAME);
        self.into()
    }
}

impl<B: IntelBoard> Bootblock<B, Dram> {
    /// Open the store in DRAM, authenticate the boot root, load postcar and
    /// jump to it.
    pub fn load_postcar(mut self) -> ! {
        let result = self.load_postcar_inner();
        match result {
            Ok(never) => match never {},
            Err(_) => self.fail("postcar load"),
        }
    }

    fn load_postcar_inner(&mut self) -> Result<core::convert::Infallible, ServiceError> {
        let geometry = self.geometry;
        let boot_path = self.boot_path;
        let (firmware_base, firmware_size) = geometry.firmware()?;
        let postcar_load_addr = geometry.region(RegionKind::BootstrapPostcar)?.base;
        let ramstage_load_addr = geometry.region(RegionKind::BootstrapMainstage)?.base;
        let dram_end = geometry
            .region(RegionKind::BootstrapRam)?
            .end()
            .ok_or(ServiceError::InvalidParam)?;
        let ram_end = dram_end.min(self.northbridge.total_ram_bytes()?);
        let mut store =
            crate::store::bootblock(geometry, ram_end, boot_path == BootPath::S3Resume)?;
        crate::store::move_timestamps(&mut store, TIMESTAMP_CAPACITY);
        crate::memory_info::publish(&mut store, self.northbridge.memory_info().as_ref())?;
        #[cfg(feature = "memory-cache")]
        if let (Some(key), Some(length)) = (self.training.key, self.training.captured) {
            let entry = store
                .add(fstart_store::tag::TRAINING, 0x1000, 3)
                .map_err(|_| ServiceError::InvalidParam)?;
            let pending = fstart_core::layout::Region {
                kind: RegionKind::FirmwareStore,
                base: store.address(&entry) as u64,
                size: entry.len() as u64,
            };
            // Only cold/warm initialization captures, after the driver's RAM test.
            // SAFETY: training is done; nothing writes the capture any more.
            let capture = unsafe { &*core::ptr::addr_of!(CAPTURE) };
            unsafe { crate::memory_cache::publish_pending(pending, key, &capture[..length])? };
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
        if let Some(key) = self.training.key
            && crate::memory_cache::runtime_key::<B>(
                &self.northbridge,
                &self.southbridge,
                geometry,
            )? != key
        {
            return Err(ServiceError::HardwareError);
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
        fstart_log::info!(
            "jumping to {} at {:#x}",
            crate::POSTCAR_STAGE_NAME,
            verified.entry()
        );
        crate::store::write_back(&store);
        fstart_arch::x86::boot::jump_to(verified.entry())
    }
}
