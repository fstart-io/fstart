//! Intel DRAM ramstage phases.
//!
//! The board's ramstage `main` calls the phases in order and does its own
//! work between them. Every phase is a state transition, so the order the
//! chipset needs is checked by the compiler, and tables can only be emitted
//! on a cold boot:
//!
//! ```ignore
//! let rs = Ramstage::<Board>::enter();
//! // board: route the console
//! let rs = rs.console(&CONSOLE, console_config()).memory_map();
//! let rs = rs.scan_bus(&[]);
//! // board: population policy the chipset init reads
//! let rs = rs.init_chipset();
//! // board: EC, clocks, identity
//! let rs = rs.start_cpus().verify_boot_media().init_display();
//! let rs = match rs.boot_path() {
//!     Path::Cold(rs) => rs.emit_tables(&identity, &board_acpi),
//!     Path::Resume(rs) => rs.keep_tables(),
//! };
//! // board: last word before the OS
//! rs.boot()
//! ```
//!
//! A failing phase logs and halts, or resets on S3 resume.

extern crate alloc;

use crate::boot::{import_intel_directory, install_intel_load_policy};
use crate::{
    IntelBoard, IntelChipsetConfig, IntelEarlyPlatform, IntelPlatform, IntelPlatformConfig, layout,
};
use alloc::string::String;
use alloc::vec::Vec;
use core::marker::PhantomData;
use fstart_core::services::memory_detect::{E820Entry, MemoryDetector};
use fstart_core::services::{ConsoleDevice, ServiceError, UartPort};
use fstart_driver_intel::{IntelNorthbridgeDriver, IntelSouthbridgeDriver};
use fstart_log::StaticConsole;
use fstart_stage::payload::MainstagePayload;

type Nb<B> = <<B as crate::IntelBoardFacts>::Platform as IntelEarlyPlatform>::Northbridge;
type Sb<B> = <<B as crate::IntelBoardFacts>::Platform as IntelEarlyPlatform>::Southbridge;
type AcpiContext<B> = <<B as crate::IntelBoardFacts>::Platform as IntelEarlyPlatform>::AcpiContext;

/// Entered: the store and boot media are published; the chipset decode
/// windows are open, nothing else.
pub struct Entered;
/// The console is up and the chipset's early init is done.
pub struct Console;
/// The memory map is published, cached and reserved for the firmware.
pub struct MemoryMap;
/// PCI is enumerated and allocated, the final page tables are in.
pub struct Enumerated;
/// The chipset devices are initialized.
pub struct Chipset;
/// All CPUs are up and SMM is installed.
pub struct Cpus;
/// The boot media is verified; verified assets (the VBT) can be read.
pub struct Verified;
/// Display init is done; everything the tables describe exists.
pub struct Displayed;
/// A cold or warm boot: the tables can be built.
pub struct Cold;
/// An S3 resume: the OS owns the tables; they must not be rebuilt.
pub struct Resume;
/// The tables are in place; the next step is the OS.
pub struct Ready;

/// Which way the boot ends, decided once the hardware is up.
pub enum Path<B: IntelBoard> {
    Cold(Ramstage<B, Cold>),
    Resume(Ramstage<B, Resume>),
}

/// The ramstage of an Intel board, in phase `S`.
pub struct Ramstage<B: IntelBoard, S = Entered> {
    northbridge: Nb<B>,
    southbridge: Sb<B>,
    /// Constructed when the ramstage reaches bus scanning.
    pci: Option<fstart_pci::PciEcam>,
    console: Option<&'static dyn fstart_core::services::Console>,
    uart: Option<UartPort>,
    ctx: MainstageCtx,
    geometry: layout::IntelBootLayout<'static>,
    boot_media: fstart_stage::fixed_helpers::MemoryMappedFfs,
    /// Firmware store, reopened at entry.
    store: Option<fstart_store::Store>,
    /// Store entry for the coreboot table and the payload's files.
    #[cfg(feature = "payload-coreboot")]
    coreboot_window: Option<(u64, usize)>,
    /// Board manufacturer and product for payloads that name the board.
    #[cfg_attr(not(feature = "payload-coreboot"), allow(dead_code))]
    mainboard: (String, String),
    /// S3 resume: the OS wake vector is where this boot ends, and hardware
    /// that survived in RAM must not be reinitialized blindly.
    resume: bool,
    wake_vector: Option<u32>,
    _state: PhantomData<(B, S)>,
}

impl<B: IntelBoard, S> Ramstage<B, S> {
    fn into<T>(self) -> Ramstage<B, T> {
        Ramstage {
            northbridge: self.northbridge,
            southbridge: self.southbridge,
            pci: self.pci,
            console: self.console,
            uart: self.uart,
            ctx: self.ctx,
            geometry: self.geometry,
            boot_media: self.boot_media,
            store: self.store,
            #[cfg(feature = "payload-coreboot")]
            coreboot_window: self.coreboot_window,
            mainboard: self.mainboard,
            resume: self.resume,
            wake_vector: self.wake_vector,
            _state: PhantomData,
        }
    }

    /// Run one named phase: timestamp, log, and stop the boot on failure.
    fn phase(
        &mut self,
        id: fstart_timestamp::Id,
        f: impl FnOnce(&mut Self) -> Result<(), ServiceError>,
    ) {
        fstart_timestamp::add(id);
        let name = fstart_timestamp::name(id);
        fstart_log::info!("{} ramstage: {}", B::Platform::NAME, name);
        if f(self).is_err() {
            self.fail(name);
        }
    }

    /// Stop the boot: an S3 resume resets cleanly instead of booting over the
    /// suspended OS, anything else halts.
    pub fn fail(&self, what: &str) -> ! {
        fstart_log::error!("{} ramstage: {} failed", B::Platform::NAME, what);
        if self.resume {
            fstart_log::flush();
            self.reset_system();
        }
        fstart_arch::x86::boot::halt()
    }

    fn reset_system(&self) -> ! {
        if self.resume {
            self.northbridge.prepare_resume_reset();
        }
        self.southbridge.system_reset(true)
    }

    /// This boot is an S3 resume.
    #[must_use]
    pub const fn is_resume(&self) -> bool {
        self.resume
    }

    #[must_use]
    pub const fn northbridge(&self) -> &Nb<B> {
        &self.northbridge
    }

    #[must_use]
    pub const fn southbridge(&self) -> &Sb<B> {
        &self.southbridge
    }

    pub fn southbridge_mut(&mut self) -> &mut Sb<B> {
        &mut self.southbridge
    }

    /// The southbridge's SMBus, for board devices on it (clock generator,
    /// EEPROM).
    pub fn smbus(&mut self) -> &mut impl fstart_core::services::SmBus {
        &mut self.southbridge
    }

    #[must_use]
    pub const fn ctx(&self) -> &MainstageCtx {
        &self.ctx
    }

    #[must_use]
    pub fn e820(&self) -> &[E820Entry] {
        self.ctx.e820()
    }

    #[must_use]
    pub const fn acpi_rsdp(&self) -> Option<u64> {
        self.ctx.acpi_rsdp()
    }

    fn refresh_load_policy(&mut self) -> Result<(), ServiceError> {
        install_intel_load_policy(self.ctx.e820(), self.geometry)
    }
}

impl<B: IntelBoard> Ramstage<B, Entered> {
    /// Reopen the store, recover the boot path from the postcar stash,
    /// publish the verified boot context and open the chipset decode the
    /// console needs.
    pub fn enter() -> Self {
        let entry = fstart_timestamp::now();
        // Leave the bootblock's ROM page tables first: every walk reads
        // flash, and an AP must not depend on them. Build the low 4 GiB in
        // reserved low scratch; the memory map phase replaces them with the
        // final map. Protected-mode stages run unpaged.
        // SAFETY: PAGE_TABLES_ADDR is reserved low scratch, identity mapped
        // by the ROM tables, and the ramstage runs from DRAM below 4 GiB,
        // which the new tables map identically.
        #[cfg(target_arch = "x86_64")]
        {
            use fstart_arch::x86::boot::{PAGE_TABLES_ADDR, paging};
            let Ok(tables) = (unsafe {
                paging::IdentityTables::build_in(PAGE_TABLES_ADDR, paging::TABLE_BYTES, [])
            }) else {
                fstart_arch::x86::boot::halt()
            };
            unsafe { tables.install() };
        }
        fn halt<T>(_: ServiceError) -> T {
            fstart_arch::x86::boot::halt()
        }
        let geometry = layout::IntelBootLayout::current(2).unwrap_or_else(halt);
        let (firmware_base, firmware_size) = geometry.firmware().unwrap_or_else(halt);
        let northbridge = Nb::<B>::new_from_config(B::CONFIG.northbridge()).unwrap_or_else(halt);
        let mut southbridge =
            Sb::<B>::new_from_config(B::CONFIG.southbridge()).unwrap_or_else(halt);
        let store = crate::store::open(geometry).unwrap_or_else(halt);
        crate::store::attach_timestamps(&store);
        fstart_timestamp::add_at(fstart_timestamp::id::RAMSTAGE_START, entry);
        // S3 resume is bootblock policy transported through the postcar stash.
        let resume = crate::boot::handoff(firmware_base, firmware_size)
            .map(|stash| {
                stash.boot_flags & fstart_arch::x86::boot::car_teardown::BOOT_FLAG_S3_RESUME != 0
            })
            .unwrap_or(false);
        southbridge.set_resume(resume);
        let mut rs = Self {
            northbridge,
            southbridge,
            pci: None,
            console: None,
            uart: None,
            ctx: MainstageCtx::new(firmware_base, firmware_size),
            geometry,
            boot_media: fstart_stage::fixed_helpers::MemoryMappedFfs::new(
                firmware_base,
                firmware_size,
            ),
            store: Some(store),
            #[cfg(feature = "payload-coreboot")]
            coreboot_window: None,
            mainboard: (String::new(), String::new()),
            resume,
            wake_vector: None,
            _state: PhantomData,
        };
        // Postcar authenticated our initialized image before entry. Import only
        // the bounded directory reference, then retain its verified bytes in RAM.
        // Drivers use the published verified asset service, not a new signature.
        rs.phase(fstart_timestamp::id::IMPORT_BOOT_CONTEXT, |_| {
            import_intel_directory(firmware_base, firmware_size)
        });
        // Import publishes the inherited locator; mounting before it must fail.
        // Neither metadata phase performs chipset/device initialization.
        rs.phase(fstart_timestamp::id::PUBLISH_BOOT_MEDIA, |rs| {
            rs.boot_media.mount()
        });
        rs.phase(fstart_timestamp::id::PRE_BUS_SCAN, |rs| {
            rs.northbridge.pre_console_init()?;
            rs.southbridge.pre_console_init()
        });
        rs
    }

    /// Bring up the console and the chipset's early init.
    pub fn console<C: ConsoleDevice + 'static>(
        mut self,
        slot: &'static StaticConsole<C>,
        config: C::Config,
    ) -> Ramstage<B, Console> {
        let Ok(mut console) = C::new(config) else {
            fstart_arch::x86::boot::halt()
        };
        if console.init().is_err() {
            fstart_arch::x86::boot::halt()
        }
        let console = slot.install(console);
        self.uart = console.uart_port();
        self.console = Some(console);
        fstart_log::info!("{} ramstage: {} console ready", B::Platform::NAME, C::NAME);
        let early = self
            .northbridge
            .early_init()
            .and_then(|()| self.southbridge.early_init());
        if early.is_err() {
            self.fail("early init");
        }
        self.into()
    }
}

impl<B: IntelBoard> Ramstage<B, Console> {
    /// Rebuild the memory map from the chipset (DRAM was trained by the
    /// bootblock), cache it, reserve the firmware's own windows and commit a
    /// pending memory-training record.
    pub fn memory_map(mut self) -> Ramstage<B, MemoryMap> {
        let detected = (|| {
            let count = self
                .northbridge
                .detect_memory(self.ctx.e820_state_mut().entries_mut())?;
            let total = self.northbridge.total_ram_bytes()?;
            fstart_log::info!(
                "Detected {} MiB RAM, {} e820 entries from {}",
                total >> 20,
                count,
                B::Platform::NAME,
            );
            self.ctx.e820_state_mut().set_detected(count, total)?;
            self.northbridge.memory_detected(self.ctx.e820_state());
            // Cache is still off: the RAM-stage entry tore down CAR. The ranges
            // the northbridge just published are enough to restore caching now.
            // The complete solution is selected after PCI allocation.
            // SAFETY: BSP-only, after the memory map is published.
            unsafe { fstart_arch::x86::mtrr::setup_ram_wb() }
        })();
        if detected.is_err() {
            self.fail("memory detection");
        }
        // Reserve the firmware's own windows before any loader policy or table
        // allocation reads the map: on resume the OS may not own the bytes the
        // next boot reloads postcar and the ramstage into.
        self.phase(fstart_timestamp::id::RESERVE_FIRMWARE_MEMORY, |rs| {
            crate::boot::reserve_firmware_memory(rs.ctx.e820_state_mut(), rs.geometry)
        });
        self.phase(
            fstart_timestamp::id::LOAD_MEMORY_POLICY,
            Self::refresh_load_policy,
        );
        #[cfg(feature = "memory-cache")]
        if B::FACTS.memory_cache && !self.resume {
            self.commit_memory_cache();
        }
        self.into()
    }

    #[cfg(feature = "memory-cache")]
    fn commit_memory_cache(&mut self) {
        fstart_timestamp::add(fstart_timestamp::id::COMMIT_MEMORY_CACHE);
        let store = self.store.as_ref().unwrap_or_else(|| unreachable!());
        let persisted = crate::memory_cache::commit_pending::<B>(
            &self.northbridge,
            &self.southbridge,
            self.geometry,
            store,
        );
        let complete = |tag, role| {
            store.find(tag).is_some_and(|entry| {
                let media = unsafe {
                    fstart_core::services::boot_media::MemoryMapped::from_raw_addr(
                        store.address(&entry) as u64,
                        entry.len(),
                    )
                };
                fstart_stage::stage_cache::has_complete_slot(&media, role)
            })
        };
        let enabled = self.northbridge.supports_s3_replay()
            && persisted.is_ok()
            && complete(
                fstart_store::tag::STAGE_CACHE_POSTCAR,
                fstart_stage::stage_cache::CachedStage::Postcar,
            )
            && complete(
                fstart_store::tag::STAGE_CACHE_RAMSTAGE,
                fstart_stage::stage_cache::CachedStage::Mainstage,
            );
        if let Err(error) = persisted {
            fstart_log::error!(
                "memory cache: commit failed code {}; S3 withheld",
                error as u8
            );
        }
        self.northbridge.set_s3_enabled(enabled);
        self.southbridge.set_s3_enabled(enabled);
    }
}

impl<B: IntelBoard> Ramstage<B, MemoryMap> {
    /// Enumerate and allocate PCI around the chipset's fixed BARs and the
    /// board's fixed non-PCI `resources`, then switch to the final page
    /// tables and cache policy.
    pub fn scan_bus(
        mut self,
        resources: &[fstart_core::acpi::AcpiResource],
    ) -> Ramstage<B, Enumerated> {
        self.phase(fstart_timestamp::id::BUS_SCAN, |rs| rs.bus_scan(resources));
        self.phase(fstart_timestamp::id::INSTALL_PAGE_TABLES, |rs| {
            rs.install_page_tables()
        });
        self.into()
    }

    fn bus_scan(
        &mut self,
        resources: &[fstart_core::acpi::AcpiResource],
    ) -> Result<(), ServiceError> {
        let mut pci = fstart_pci::PciEcam::from_provider(&self.northbridge)
            .inspect_err(|error| fstart_log::error!("pci: root setup failed: {:?}", error))
            .map_err(|_| ServiceError::HardwareError)?;
        let fixed_bars = self.southbridge.fixed_pci_bars();
        pci.add_fixed_bars(fixed_bars.as_slice())
            .inspect_err(|error| {
                fstart_log::error!("pci: fixed BAR reservation failed: {:?}", error);
            })
            .map_err(|_| ServiceError::HardwareError)?;
        for resource in resources {
            use fstart_core::acpi::AcpiResource;
            match *resource {
                AcpiResource::Pio { base, size } => {
                    pci.reserve_io_range(u64::from(base), u64::from(size))
                }
                AcpiResource::Mmio { base, size } => pci.reserve_mmio_range(base, size),
            }
            .inspect_err(|error| {
                fstart_log::error!("pci: board resource reservation failed: {:?}", error)
            })
            .map_err(|_| ServiceError::HardwareError)?;
        }
        let pci = self.pci.insert(pci);
        pci.enumerate_and_allocate()
            .inspect_err(|error| {
                fstart_log::error!("pci: enumeration/allocation failed: {:?}", error);
            })
            .map_err(|_| ServiceError::HardwareError)
    }

    fn install_page_tables(&mut self) -> Result<(), ServiceError> {
        let pci = self.pci.as_ref().ok_or(ServiceError::NotInitialized)?;
        let ram = self
            .ctx
            .e820()
            .iter()
            .filter(|entry| {
                entry.kind == fstart_core::services::memory_detect::E820Kind::Ram as u32
            })
            .map(|entry| (entry.addr, entry.size));
        let bars = pci
            .assigned_memory_windows()
            .map(|window| (window.base, window.size));
        // SAFETY: firmware's low-DRAM heap is identity mapped by postcar and
        // reserved by reserve_firmware_memory. Preserve the complete low map,
        // add detected high RAM and assigned BARs, and switch before AP startup.
        #[cfg(target_arch = "x86_64")]
        {
            let tables =
                unsafe { fstart_arch::x86::boot::paging::IdentityTables::build(ram.chain(bars))? };
            let base = unsafe { tables.install() };
            fstart_log::info!("paging: final DRAM identity tables at {:#x}", base);
        }
        // Protected-mode stages run unpaged.
        #[cfg(not(target_arch = "x86_64"))]
        let _ = (ram, bars);
        let wc = pci
            .prefetchable_vga_windows()
            .map(|window| (window.base, window.size))
            .collect::<Vec<_>>();
        // SAFETY: BSP-only, after allocation, before device initialization/APs.
        // Hardware RAM facts were published before e820 reservations; only
        // assigned prefetchable VGA apertures are eligible for optional WC.
        unsafe { fstart_arch::x86::mtrr::install_final_solution(&wc) }
    }
}

impl<B: IntelBoard> Ramstage<B, Enumerated> {
    /// Program the chipset's devices.
    pub fn init_chipset(mut self) -> Ramstage<B, Chipset> {
        self.phase(fstart_timestamp::id::INIT_DEVICES, |rs| {
            rs.northbridge.post_dram_init()?;
            rs.southbridge.post_dram_init()
        });
        self.into()
    }
}

impl<B: IntelBoard> Ramstage<B, Chipset> {
    /// Bring up the APs with the CPU driver and, when fbuild embedded an SMM
    /// image ([`crate::SMM_IMAGE`]), relocate SMBASE, install the handler in
    /// TSEG and lock SMRAM.
    pub fn start_cpus(self) -> Ramstage<B, Cpus> {
        if init_mp::<B::Platform>(
            &self.northbridge,
            &self.southbridge,
            B::CONFIG.max_cpus(),
            self.resume,
        )
        .is_err()
        {
            self.fail("MP init");
        }
        self.into()
    }
}

impl<B: IntelBoard> Ramstage<B, Cpus> {
    /// Map the boot media and verify everything later phases read from it.
    pub fn verify_boot_media(mut self) -> Ramstage<B, Verified> {
        self.phase(fstart_timestamp::id::MOUNT_BOOT_MEDIA, |rs| {
            fstart_arch::x86::boot::enable_boot_media_rom_cache();
            if rs.resume {
                return Ok(());
            }
            rs.northbridge.stage_local_init()
        });
        self.phase(fstart_timestamp::id::VERIFY_BOOT_MEDIA, |rs| {
            rs.boot_media.verify()
        });
        self.into()
    }
}

impl<B: IntelBoard> Ramstage<B, Verified> {
    /// The graphics OpRegion and modeset read the VBT out of the verified
    /// boot media. On S3 resume the platform decides: chipsets whose OS
    /// driver restores the screen skip the modeset.
    pub fn init_display(mut self) -> Ramstage<B, Displayed> {
        self.phase(fstart_timestamp::id::DISPLAY_INIT, |rs| {
            if rs.resume && !B::Platform::RESUME_DISPLAY_INIT {
                fstart_log::info!("display init skipped on S3 resume");
                return Ok(());
            }
            rs.northbridge.post_verify_init()
        });
        self.into()
    }
}

impl<B: IntelBoard> Ramstage<B, Displayed> {
    /// Cold boots build tables; S3 resumes keep the ones the OS owns.
    pub fn boot_path(self) -> Path<B> {
        if self.resume {
            Path::Resume(self.into())
        } else {
            Path::Cold(self.into())
        }
    }
}

impl<B: IntelBoard> Ramstage<B, Cold> {
    /// Build ACPI and SMBIOS from the chipset, the board's `identity` and the
    /// board's ACPI device, then seal the store.
    pub fn emit_tables(
        mut self,
        identity: &crate::tables::SmbiosIdentity<'_>,
        board: &impl fstart_acpi::device::AcpiDevice<Config = AcpiContext<B>>,
    ) -> Ramstage<B, Ready> {
        self.mainboard = (identity.bb_manufacturer.into(), identity.bb_product.into());
        self.phase(fstart_timestamp::id::EMIT_TABLES, |rs| {
            rs.emit_acpi(board)?;
            rs.emit_smbios(identity)?;
            rs.reserve_coreboot_window()
        });
        // Nothing is added to the store after the tables; a resume reuses it.
        self.phase(fstart_timestamp::id::SEAL_STORE, Self::seal_store);
        // Table allocation changes the memory map; payload loads must respect it.
        self.phase(
            fstart_timestamp::id::LOAD_MEMORY_POLICY,
            Self::refresh_load_policy,
        );
        self.into()
    }

    fn emit_acpi(
        &mut self,
        board: &impl fstart_acpi::device::AcpiDevice<Config = AcpiContext<B>>,
    ) -> Result<(), ServiceError> {
        use fstart_acpi::device::AcpiDevice;
        use fstart_acpi::platform::X86PlatformProvider;
        let acpi_ctx = AcpiContext::<B>::default();
        let platform = fstart_acpi::platform::PlatformConfig::X86(
            self.southbridge
                .x86_platform_config(u32::from(fstart_arch::x86::mp::online_cpus())),
        );
        let (northbridge, southbridge) = (&self.northbridge, &self.southbridge);
        let store = self.store.as_mut().ok_or(ServiceError::NotInitialized)?;
        let rsdp = crate::tables::prepare_acpi(store, &platform, |dsdt, extra| {
            dsdt.extend(northbridge.dsdt_aml(northbridge.config()));
            dsdt.extend(southbridge.dsdt_aml(southbridge.config()));
            dsdt.extend(board.dsdt_aml(&acpi_ctx));
            extra.extend(northbridge.extra_tables(northbridge.config()));
            extra.extend(southbridge.extra_tables(southbridge.config()));
            extra.extend(board.extra_tables(&acpi_ctx));
        })
        .map_err(|_| ServiceError::HardwareError)?;
        self.ctx.set_acpi_rsdp(Some(rsdp));
        Ok(())
    }

    fn emit_smbios(
        &mut self,
        identity: &crate::tables::SmbiosIdentity<'_>,
    ) -> Result<(), ServiceError> {
        let store = self.store.as_mut().ok_or(ServiceError::NotInitialized)?;
        let memory = crate::memory_info::read(store);
        let runtime = crate::tables::SmbiosRuntime {
            rom_size: B::FACTS.flash_size,
            memory: memory.as_ref(),
            pci: self.pci.as_ref(),
        };
        let smbios =
            crate::tables::prepare_smbios(store, self.ctx.e820_state(), identity, &runtime)?;
        self.ctx.set_smbios(Some(smbios));
        Ok(())
    }

    /// Filled at payload launch; sized and reserved now, before sealing.
    fn reserve_coreboot_window(&mut self) -> Result<(), ServiceError> {
        #[cfg(feature = "payload-coreboot")]
        {
            let size = fstart_stage::coreboot::window_size().ok_or(ServiceError::NotInitialized)?;
            let store = self.store.as_mut().ok_or(ServiceError::NotInitialized)?;
            let entry = store
                .add(fstart_store::tag::COREBOOT, size, 12)
                .map_err(|_| ServiceError::InvalidParam)?;
            self.coreboot_window = Some((store.address(&entry) as u64, size));
        }
        Ok(())
    }

    fn seal_store(&mut self) -> Result<(), ServiceError> {
        let store = self.store.as_mut().ok_or(ServiceError::NotInitialized)?;
        crate::store::seal(store, self.ctx.e820_state_mut(), self.geometry)?;
        // ACPI tables, FACS included, must survive S3: keep them as NVS.
        if let Some(acpi) = store.find(fstart_store::tag::ACPI) {
            self.ctx.e820_state_mut().set_range_kind(
                store.address(&acpi) as u64,
                (acpi.len() as u64).next_multiple_of(0x1000),
                fstart_core::services::memory_detect::E820Kind::Nvs,
            )?;
        }
        Ok(())
    }
}

impl<B: IntelBoard> Ramstage<B, Resume> {
    /// Recover the OS wake vector from the surviving FACS. The suspended OS
    /// owns the live tables; rebuilding them would overwrite its memory.
    pub fn keep_tables(mut self) -> Ramstage<B, Ready> {
        let e820 = self.ctx.e820();
        self.wake_vector = fstart_acpi::platform::x86::wake::find_wakeup_vector(|addr, size| {
            let Some(end) = addr.checked_add(size as u64) else {
                return false;
            };
            // BDA, EBDA and the legacy BIOS scan window are below 1 MiB.
            // Every table pointer above that must stay inside a chipset-
            // reported physical-memory span, never an unlisted MMIO hole.
            end <= 0x10_0000
                || e820.iter().any(|entry| {
                    entry.size != 0
                        && entry.kind
                            != fstart_core::services::memory_detect::E820Kind::Unusable as u32
                        && addr >= entry.addr
                        && entry
                            .addr
                            .checked_add(entry.size)
                            .is_some_and(|entry_end| end <= entry_end)
                })
        });
        self.phase(
            fstart_timestamp::id::LOAD_MEMORY_POLICY,
            Self::refresh_load_policy,
        );
        self.into()
    }
}

impl<B: IntelBoard> Ramstage<B, Ready> {
    /// Lock down the chipset, quiesce the CPUs and leave: to the OS wake
    /// vector on S3 resume, to the payload otherwise.
    pub fn boot(mut self) -> ! {
        self.phase(fstart_timestamp::id::FINALIZE, |rs| {
            rs.southbridge.finalize_init()
        });
        // mp_init leaves APs polling firmware mailboxes. Stop that activity before
        // either payload or S3-resume ownership is transferred to the OS.
        if !fstart_arch::x86::mp::park_aps_for_payload() {
            fstart_log::error!(
                "{}: failed to quiesce APs for OS handoff; resetting",
                B::Platform::NAME
            );
            fstart_log::flush();
            self.reset_system();
        }
        if self.resume {
            match self.wake_vector {
                Some(vector) => {
                    fstart_log::info!("resuming OS at wake vector {:#x}", vector);
                    fstart_log::flush();
                    fstart_arch::x86::boot::s3_wake::jump_to_wakeup_vector(vector);
                }
                None => {
                    // No valid vector means the surviving tables are unusable;
                    // come up cleanly instead of booting over the suspended OS.
                    fstart_log::error!("S3 resume without a wake vector, resetting");
                    fstart_log::flush();
                    self.reset_system();
                }
            }
        }
        fstart_timestamp::add(fstart_timestamp::id::LOAD_PAYLOAD);
        fstart_stage::payload::BuildSelectedPayload::boot(self)
    }
}

fn init_mp<P: IntelEarlyPlatform>(
    northbridge: &P::Northbridge,
    southbridge: &P::Southbridge,
    max_cpus: u16,
    resume: bool,
) -> Result<(), ServiceError> {
    // APs must run the same updated microcode as the BSP, whose update
    // happens in pre-CAR assembly; the blob sits in boot flash.
    let cpu = P::cpu_driver(fstart_stage::microcode_blob());
    let drivers: [&dyn fstart_arch::x86::mp::CpuDriver; 1] = [&cpu];
    // The supported Core 2 / Pineview platforms are single-socket, as in
    // coreboot's model_1067x get_cpu_count(). CPUID is package-local; future
    // multi-socket platforms must provide system-wide discovery instead.
    let (_, ebx, _, _) = fstart_arch::x86::cpuid(1);
    let num_cpus = (((ebx >> 16) & 0xff) as u16).max(1);
    let mp = fstart_arch::x86::mp::mp_init(&fstart_arch::x86::mp::MpConfig {
        cpu_drivers: &drivers,
        num_cpus,
        max_cpus,
    })
    .map_err(|_| ServiceError::HardwareError)?;
    if let Some(image) = crate::SMM_IMAGE {
        let smm = fstart_arch::x86::cpu::intel::smm::IntelSmm::new(
            P::NAME,
            northbridge,
            southbridge,
            &cpu,
            resume,
        );
        if let Err(error) = smm.install(&mp, image) {
            fstart_log::error!("{} SMM installation failed: {}", P::NAME, error.name());
            return Err(ServiceError::HardwareError);
        }
    }
    Ok(())
}

#[cfg(feature = "payload-linux")]
impl<B: IntelBoard, S> fstart_stage::payload::X86LinuxPayloadContext for Ramstage<B, S> {
    fn e820(&self) -> &[E820Entry] {
        self.e820()
    }

    fn acpi_rsdp(&self) -> Option<u64> {
        self.acpi_rsdp()
    }
}

impl<B: IntelBoard, S> fstart_stage::payload::X86UefiPayloadContext for Ramstage<B, S> {
    fn console(&self) -> Option<&dyn fstart_core::services::Console> {
        self.console
    }

    fn e820(&self) -> &[E820Entry] {
        self.e820()
    }

    fn firmware_region(&self) -> (u64, u64) {
        let (base, size) = self.ctx.firmware_region();
        (base, size as u64)
    }

    fn acpi_rsdp(&self) -> Option<u64> {
        self.acpi_rsdp()
    }

    fn smbios(&self) -> Option<u64> {
        self.ctx.smbios()
    }

    fn pci_root(&self) -> Option<fstart_pci::PciRootInfo> {
        Some(fstart_pci::PciRootProvider::root_info(&self.northbridge))
    }

    #[cfg(feature = "payload-uefi-basic")]
    fn framebuffer(&self) -> Option<fstart_core::services::FramebufferInfo> {
        self.northbridge.framebuffer_info()
    }
}

// SAFETY: the northbridge opened the PAM shadow at early init and nothing of
// the firmware lives in it; the window is a sealed, reserved store entry.
#[cfg(feature = "payload-coreboot")]
unsafe impl<B: IntelBoard, S> fstart_stage::coreboot::X86CorebootPayloadContext for Ramstage<B, S> {
    fn e820(&self) -> &[E820Entry] {
        self.e820()
    }

    fn acpi_rsdp(&self) -> Option<u64> {
        self.acpi_rsdp()
    }

    fn framebuffer(&self) -> Option<fstart_core::services::FramebufferInfo> {
        self.northbridge.framebuffer_info()
    }

    fn serial(&self) -> Option<fstart_coreboot::Serial> {
        self.uart.map(|port| fstart_coreboot::Serial {
            kind: if port.io_port {
                fstart_coreboot::SERIAL_IO_MAPPED
            } else {
                fstart_coreboot::SERIAL_MEMORY_MAPPED
            }
            .into(),
            base: (port.base as u32).into(),
            baud: port.baud.into(),
            regwidth: port.reg_stride.into(),
            input_hertz: port.clock_hz.into(),
        })
    }

    fn mainboard(&self) -> (&str, &str) {
        (&self.mainboard.0, &self.mainboard.1)
    }

    fn table_window(&self) -> Option<(u64, usize)> {
        self.coreboot_window
    }

    fn table_ranges(&self) -> heapless::Vec<(u64, u64), 4> {
        // ACPI, SMBIOS and the coreboot window all live in the store.
        self.store
            .iter()
            .map(|store| (store.base() as u64, store.used() as u64))
            .collect()
    }
}

pub struct MainstageCtx {
    e820: fstart_core::services::memory_detect::E820State,
    acpi_rsdp: Option<u64>,
    smbios: Option<u64>,
    firmware_base: u64,
    firmware_size: usize,
}

impl MainstageCtx {
    pub(crate) fn new(firmware_base: u64, firmware_size: usize) -> Self {
        Self {
            e820: fstart_core::services::memory_detect::E820State::new(),
            acpi_rsdp: None,
            smbios: None,
            firmware_base,
            firmware_size,
        }
    }

    #[must_use]
    pub fn e820(&self) -> &[E820Entry] {
        self.e820.entries()
    }

    #[must_use]
    pub fn total_ram(&self) -> u64 {
        self.e820.total_ram()
    }

    #[must_use]
    pub fn e820_state(&self) -> &fstart_core::services::memory_detect::E820State {
        &self.e820
    }

    pub(crate) fn e820_state_mut(
        &mut self,
    ) -> &mut fstart_core::services::memory_detect::E820State {
        &mut self.e820
    }

    #[must_use]
    pub const fn acpi_rsdp(&self) -> Option<u64> {
        self.acpi_rsdp
    }

    #[must_use]
    pub const fn smbios(&self) -> Option<u64> {
        self.smbios
    }

    #[must_use]
    pub const fn firmware_region(&self) -> (u64, usize) {
        (self.firmware_base, self.firmware_size)
    }

    #[cfg(feature = "acpi")]
    pub(crate) fn set_acpi_rsdp(&mut self, rsdp: Option<u64>) {
        self.acpi_rsdp = rsdp;
    }

    #[cfg(feature = "smbios")]
    pub(crate) fn set_smbios(&mut self, smbios: Option<u64>) {
        self.smbios = smbios;
    }
}
