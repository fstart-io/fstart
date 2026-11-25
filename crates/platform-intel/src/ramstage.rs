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

type Nb<B> = <<B as crate::IntelBoardFacts>::Platform as IntelEarlyPlatform>::Northbridge;
type Sb<B> = <<B as crate::IntelBoardFacts>::Platform as IntelEarlyPlatform>::Southbridge;

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
    /// The outcome of every option knob resolved so far, for the payload's
    /// settings menu.
    options: fstart_options::Report,
    /// Optional PCI display initialized by its authenticated option ROM.
    pci_framebuffer: Option<fstart_core::services::FramebufferInfo>,
    video_aml: Vec<u8>,
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
            options: self.options,
            pci_framebuffer: self.pci_framebuffer,
            video_aml: self.video_aml,
            _state: PhantomData,
        }
    }

    /// Apply the stored overrides of the board's `knobs` to `config`
    /// (`fstart_options`), before the board uses it. `check` is the whole-set
    /// validation of the consumer: if the overridden configuration fails
    /// it, `config` keeps the board's values. The knobs are shown in the
    /// payload's settings menu.
    pub fn resolve_options<C: Clone + 'static>(
        &mut self,
        knobs: &'static [fstart_options::Knob<C>],
        config: &mut C,
        check: impl FnOnce(&C) -> Result<(), &'static str>,
    ) {
        let source = crate::options::store();
        fstart_log::option::resolve_into(knobs, &source, config, &mut self.options, check);
    }

    /// The board's values of `set` with the stored overrides
    /// ([`Self::resolve_options`]). The set is one of the board's
    /// [`IntelBoardFacts::OPTIONS`](crate::IntelBoardFacts::OPTIONS), so
    /// the build lists its options.
    pub fn resolve_set<C: Clone + 'static>(
        &mut self,
        set: &'static fstart_options::OptionSet<C>,
        check: impl FnOnce(&C) -> Result<(), &'static str>,
    ) -> C {
        let mut config = set.board.clone();
        self.resolve_options(set.knobs, &mut config, check);
        config
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
        if let Err(error) = f(self) {
            fstart_log::error!("{} ramstage: {}: {:?}", B::Platform::NAME, name, error);
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
        // S3 resume is bootblock policy transported through the postcar stash.
        let resume = crate::boot::handoff(firmware_base, firmware_size)
            .map(|stash| {
                stash.boot_flags & fstart_arch::x86::boot::car_teardown::BOOT_FLAG_S3_RESUME != 0
            })
            .unwrap_or_else(|_| B::Platform::reset_failed_postcar_resume());
        let store = crate::store::open(geometry).unwrap_or_else(|e| {
            if resume {
                B::Platform::reset_failed_postcar_resume();
            }
            halt(e)
        });
        crate::store::attach_timestamps(&store);
        fstart_timestamp::add_at(fstart_timestamp::id::RAMSTAGE_START, entry);
        southbridge.set_resume(resume);
        if cfg!(feature = "ehci-console")
            && let Some(debug) = B::CONFIG.ehci_debug()
        {
            southbridge.set_ehci_debug(fstart_pci::devfn(debug.device, debug.function), debug.bar);
        }
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
            options: fstart_options::Report::new(),
            pci_framebuffer: None,
            video_aml: Vec::new(),
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
        crate::debug_console::install::<B>(true);
        self.uart = console.uart_port();
        // Preserve the original board-console handoff unless debug fan-out
        // was explicitly selected. In debug builds NoConsole must not hide
        // the optional EHCI input/output from the payload.
        self.console = Some(console);
        #[cfg(any(feature = "ehci-console", feature = "flash-console"))]
        {
            self.console = Some(&fstart_log::ConsoleMux);
        }
        fstart_log::info!(
            "{} ramstage: entered ({} console ready)",
            B::Platform::NAME,
            C::NAME
        );
        crate::options::chipset_into(
            &mut self.northbridge,
            &mut self.southbridge,
            &mut self.options,
        );
        let early = self
            .northbridge
            .ramstage_early_init()
            .and_then(|()| self.southbridge.early_init());
        if let Err(error) = early {
            fstart_log::error!("early init: {:?}", error);
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
            // coreboot chip_operations.init: before PCI enumeration, e.g.
            // hiding functions the board disables.
            if self.resume {
                let store = self.store.as_mut().ok_or(ServiceError::NotInitialized)?;
                if !self
                    .southbridge
                    .matches_acpi_population(crate::acpi_bars::cold_snapshot(store))
                {
                    return Err(ServiceError::HardwareError);
                }
            }
            self.northbridge.chip_init()?;
            self.southbridge.chip_init()?;
            // Cache is still off: the RAM-stage entry tore down CAR. The ranges
            // the northbridge just published are enough to restore caching now.
            // The complete solution is selected after PCI allocation.
            // SAFETY: BSP-only, after the memory map is published.
            unsafe { fstart_arch::x86::mtrr::setup_ram_wb() }
        })();
        if let Err(error) = detected {
            fstart_log::error!("memory detection: {:?}", error);
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
        if B::FACTS.memory_cache && !self.resume {
            self.commit_memory_cache();
        }
        self.into()
    }

    fn commit_memory_cache(&mut self) {
        fstart_timestamp::add(fstart_timestamp::id::COMMIT_MEMORY_CACHE);
        let store = self.store.as_ref().unwrap_or_else(|| unreachable!());
        let persisted = crate::memory_cache::commit_pending::<B>(
            &self.northbridge,
            &self.southbridge,
            self.geometry,
            store,
        );
        use fstart_resume::stage_cache::CachedStage;
        let enabled = self.northbridge.supports_s3_replay()
            && persisted.is_ok()
            && fstart_resume::cached(store, &[CachedStage::Postcar, CachedStage::Mainstage]);
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
        // The root bridge is described once: the allocator is built from
        // it and keeps it, for the tables and for the payload.
        let mut pci = (self.northbridge.pci_root())
            .and_then(|root| fstart_pci::PciEcam::new([root]))
            .inspect_err(|error| fstart_log::error!("pci: root setup failed: {:?}", error))
            .map_err(|_| ServiceError::HardwareError)?;
        let mut fixed_bars = self.southbridge.fixed_pci_bars();
        // Without --debug ehci no console initialized this BAR: let PCI
        // allocate it normally.
        if cfg!(feature = "ehci-console")
            && let Some(debug) = B::CONFIG.ehci_debug()
        {
            fixed_bars
                .push(debug.fixed_bar())
                .map_err(|_| ServiceError::InvalidParam)?;
        }
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
            // coreboot sets subsystem IDs while enabling resources, before
            // init; the registers are commonly write-once.
            for entry in B::CONFIG.pci_subsystem_ids() {
                let dev = fstart_pci::ecam::EcamDevice::new(0, entry.device, entry.function);
                if dev.is_present() && !rs.southbridge.program_subsystem_id(dev, entry.id) {
                    fstart_log::error!(
                        "pci: 00:{:02x}.{} has no subsystem ID register",
                        entry.device,
                        entry.function
                    );
                }
            }
            // coreboot pci_dev_set_subsystem(): every enabled onboard function
            // gets a subsystem ID, defaulting to its own VID/DID.
            if rs.southbridge.onboard_subsystem_fallback() {
                let listed = |d: u8, f: u8| {
                    B::CONFIG
                        .pci_subsystem_ids()
                        .iter()
                        .any(|e| e.device == d && e.function == f)
                };
                for d in 0..32u8 {
                    let head = fstart_pci::ecam::EcamDevice::new(0, d, 0);
                    if !head.is_present() {
                        continue;
                    }
                    let functions = if head.read8(0x0e) & 0x80 != 0 { 8 } else { 1 };
                    for f in 0..functions {
                        let dev = fstart_pci::ecam::EcamDevice::new(0, d, f);
                        if dev.is_present() && !listed(d, f) {
                            rs.southbridge.program_subsystem_id(dev, 0);
                        }
                    }
                }
            }
            // A suspended OS keeps the cold DSDT. Never hide Serial IO at a
            // different address; phase failure takes the cold reset path on S3.
            let store = rs.store.as_mut().ok_or(ServiceError::NotInitialized)?;
            let retained = match rs.southbridge.acpi_bar_snapshot() {
                Ok(bars) => crate::acpi_bars::retain(store, &bars, rs.resume)?,
                Err(e) if rs.resume => return Err(e),
                Err(e) => {
                    fstart_log::warn!("ACPI BAR snapshot failed: {:?}", e);
                    false
                }
            };
            if !retained {
                fstart_log::warn!("ACPI BAR snapshot not retained; withholding S3");
                rs.northbridge.set_s3_enabled(false);
                rs.southbridge.set_s3_enabled(false);
            }
            if B::Platform::IOAPIC_VIRTUAL_WIRE {
                fstart_arch::x86::lapic::Lapic::from_msr().setup_virtual_wire(false);
            }
            rs.northbridge.post_dram_init()?;
            rs.southbridge.post_dram_init()
        });
        self.into()
    }
}

impl<B: IntelBoard> Ramstage<B, Chipset> {
    /// Probe a discrete LPC/MMIO TIS TPM at [`fstart_driver_tpm::tis::BASE`],
    /// enforce S3 population retention in `TPM_STATE`, and run `Startup(Clear)`
    /// or `Startup(State)`.
    ///
    /// On S3 resume, a changed population or failed `Startup(State)` aborts
    /// resume via [`Self::fail`]. On cold boot, failures withhold S3 (and, for
    /// failed startup, omit the TPM device from ACPI) without aborting boot.
    pub fn init_tis_tpm(&mut self) -> crate::tpm::TisProbe {
        use fstart_driver_tpm::tis;

        let resume = self.resume;
        // SAFETY: LPC TIS window, exclusively used by the BSP before handoff.
        let mut tis = unsafe { tis::Tis::new(tis::BASE, fstart_arch::x86::Udelay) };
        let id = tis.probe();
        let irq = id.and_then(|_| tis.irq());
        let same_tpm = self
            .store
            .as_mut()
            .is_some_and(|store| crate::tpm::same_population(store, resume, id, irq));
        if !same_tpm {
            if resume {
                self.fail("retained TPM identity");
            }
            fstart_log::warn!("TPM: could not retain identity; withholding S3");
            self.northbridge.set_s3_enabled(false);
            self.southbridge.set_s3_enabled(false);
        }
        let mut family = id.map(|i| i.family);
        if let Some(kind) = family
            && fstart_driver_tpm::startup(&mut tis, kind, resume).is_err()
        {
            if resume {
                self.fail("TPM resume");
            }
            fstart_log::warn!("TPM: cold startup failed; withholding TPM device and S3");
            family = None;
            self.northbridge.set_s3_enabled(false);
            self.southbridge.set_s3_enabled(false);
        }
        crate::tpm::TisProbe { family, irq }
    }

    /// Bring up the APs with the CPU driver and, when fbuild embedded an SMM
    /// image ([`crate::SMM_IMAGE`]), relocate SMBASE, install the handler in
    /// TSEG and lock SMRAM.
    pub fn start_cpus(mut self) -> Ramstage<B, Cpus> {
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
        if self.northbridge.post_mp_init().is_err() {
            self.fail("northbridge post-MP init");
        }
        if self.southbridge.post_mp_init().is_err() {
            self.fail("southbridge post-MP init");
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
                // Point the chipset back at what the cold boot published,
                // e.g. the graphics OpRegion kept in the store.
                let opregion = rs.store.as_ref().and_then(|store| {
                    let entry = store.find(fstart_store::tag::IGD_OPREGION)?;
                    u32::try_from(store.address(&entry)).ok()
                });
                return rs.northbridge.resume_init(opregion);
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
            // The OpRegion lives in the store, which survives S3 (coreboot
            // keeps it in CBMEM).
            let store = &mut rs.store;
            rs.northbridge.post_verify_init(&mut |size| {
                let store = store.as_mut()?;
                let entry = store.add(fstart_store::tag::IGD_OPREGION, size, 12).ok()?;
                // SAFETY: a fresh store entry of `size` bytes, identity
                // mapped and owned by the OS once published; never handed
                // out twice.
                Some(unsafe {
                    core::slice::from_raw_parts_mut(store.address(&entry) as *mut u8, size)
                })
            })
        });
        self.into()
    }
}

impl<B: IntelBoard> Ramstage<B, Displayed> {
    /// POST an optional discrete display through the shared VBIOS executor.
    /// The board supplies its vendor and existing ACPI companion scope. Assets
    /// are authenticated FFS files named `pciVVVV,DDDD.rom`. An absent GPU or
    /// missing ROM leaves a headless boot; a failed POST is never retried.
    #[cfg(target_arch = "x86_64")]
    pub fn init_pci_display(mut self, vendor: u16, scope: fstart_acpi::AcpiPath) -> Self {
        if self.resume {
            return self;
        }
        self.phase(fstart_timestamp::id::DISPLAY_INIT, |rs| {
            let pci = rs.pci.as_ref().ok_or(ServiceError::NotInitialized)?;
            let Some(address) = pci.devices().find(|&address| {
                pci.device(address)
                    .is_some_and(|d| d.vendor_id() == vendor && d.read8(0x0b) == 3)
            }) else {
                return Ok(());
            };
            let device = pci.device(address).ok_or(ServiceError::NotInitialized)?;
            let name = alloc::format!("pci{:04x},{:04x}.rom", vendor, device.device_id());
            let Some(rom) = fstart_core::services::ffs_context::read_verified_asset(&name) else {
                fstart_log::error!(
                    "display: missing authenticated {} (headless)",
                    name.as_str()
                );
                return Ok(());
            };
            let windows: Vec<_> = pci
                .assigned_memory_bars()
                .filter(|(a, _)| *a == address)
                .map(|(_, window)| window)
                .collect();
            // SAFETY: verified ROM, BSP ownership, allocated identity-mapped BARs.
            let display = unsafe {
                fstart_driver_vbios::initialize(
                    pci,
                    address,
                    rom,
                    device.device_id(),
                    &windows,
                    &fstart_arch::x86::Udelay,
                )
            }
            .map_err(|error| {
                fstart_log::error!("display: VBIOS {}", error.name());
                ServiceError::HardwareError
            })?;
            rs.pci_framebuffer = Some(display.framebuffer);
            let rom = fstart_acpi::video::rom_aml(&display.rom)
                .map_err(|_| ServiceError::HardwareError)?;
            rs.video_aml = fstart_acpi::aml_linker::scope_vec(scope.as_str(), &rom)
                .map_err(|_| ServiceError::HardwareError)?;
            Ok(())
        });
        self
    }

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
    /// board's ACPI device, then seal the store. The board finds the chipset
    /// devices it attaches to through its platform's
    /// [`PcNamespace`](fstart_acpi::PcNamespace).
    pub fn emit_tables(
        self,
        identity: &crate::tables::SmbiosIdentity<'_>,
        board: &impl fstart_acpi::device::AcpiDevice<Config = ()>,
    ) -> Ramstage<B, Ready> {
        self.emit_tables_with_onboard(identity, board, &[])
    }

    /// Like [`Self::emit_tables`], plus board-defined SMBIOS Type 41 records.
    pub fn emit_tables_with_onboard(
        mut self,
        identity: &crate::tables::SmbiosIdentity<'_>,
        board: &impl fstart_acpi::device::AcpiDevice<Config = ()>,
        extra_onboard: &[crate::tables::ExtraOnboardDevice<'_>],
    ) -> Ramstage<B, Ready> {
        self.mainboard = (identity.bb_manufacturer.into(), identity.bb_product.into());
        self.phase(fstart_timestamp::id::FINALIZE, |rs| {
            rs.northbridge.finalize_init()
        });
        self.phase(fstart_timestamp::id::EMIT_TABLES, |rs| {
            rs.emit_acpi(board)?;
            rs.emit_smbios(identity, extra_onboard)?;
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
        board: &impl fstart_acpi::device::AcpiDevice<Config = ()>,
    ) -> Result<(), ServiceError> {
        use fstart_acpi::platform::X86PlatformProvider;
        let mut x86 = self
            .southbridge
            .x86_platform_config(u32::from(fstart_arch::x86::mp::online_cpus()));
        // CPU ordinals are not LAPIC IDs. MP recorded each responding CPU's
        // hardware ID, including holes in Arrandale's APIC numbering.
        x86.apic_ids = fstart_arch::x86::mp::online_apic_ids().collect();
        for (uid, apic_id) in x86.apic_ids.iter().enumerate() {
            fstart_log::info!("ACPI: CPU {} LAPIC ID {}", uid, apic_id);
        }
        let platform = fstart_acpi::platform::PlatformConfig::X86(x86);
        let (northbridge, southbridge) = (&self.northbridge, &self.southbridge);
        // What the OS is told about the root bridge is what the allocator
        // was given.
        let root = self.pci.as_ref().and_then(|pci| pci.roots().first());
        let root = root.ok_or(ServiceError::NotInitialized)?;
        let tables = crate::acpi::collect(northbridge, southbridge, root, board, &self.video_aml)
            .map_err(|_| ServiceError::HardwareError)?;
        let store = self.store.as_mut().ok_or(ServiceError::NotInitialized)?;
        let rsdp = crate::tables::prepare_acpi(store, &platform, &tables.dsdt, &tables.extra)
            .map_err(|_| ServiceError::HardwareError)?;
        self.ctx.set_acpi_rsdp(Some(rsdp));
        Ok(())
    }

    fn emit_smbios(
        &mut self,
        identity: &crate::tables::SmbiosIdentity<'_>,
        extra_onboard: &[crate::tables::ExtraOnboardDevice<'_>],
    ) -> Result<(), ServiceError> {
        let store = self.store.as_mut().ok_or(ServiceError::NotInitialized)?;
        let memory = crate::memory_info::read(store);
        let runtime = crate::tables::SmbiosRuntime {
            rom_size: B::FACTS.flash_size,
            memory: memory.as_ref(),
            pci: self.pci.as_ref(),
            ports: B::CONFIG.pcie_attached(),
            extra_onboard,
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
            let size =
                fstart_payload::coreboot::window_size().ok_or(ServiceError::NotInitialized)?;
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
        let acpi = self.store.as_ref().and_then(|s| {
            let e = s.find(fstart_store::tag::ACPI)?;
            Some((s.address(&e) as u64, e.len() as u64))
        });
        self.wake_vector = fstart_acpi::platform::x86::wake::find_wakeup_vector(|addr, size| {
            crate::tables::wake_readable(e820, acpi, addr, size)
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
            if rs.resume {
                rs.northbridge.finalize_init()?;
            }
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
        self.hand_off()
    }

    /// Describe the machine to the build's payload launcher and boot it.
    fn hand_off(&self) -> ! {
        use fstart_payload::{AcpiTables, Description, Handoff, Placement, Range};
        let (firmware_base, firmware_size) = self.ctx.firmware_region();
        // ACPI, SMBIOS and the coreboot window all live in the store.
        let tables: heapless::Vec<Range, 1> = self
            .store
            .iter()
            .map(|store| Range::new(store.base() as u64, store.used() as u64))
            .collect();
        let mut description =
            Description::new(self.e820(), Range::new(firmware_base, firmware_size as u64));
        description.console = self.console;
        description.uart = self.uart;
        // The payload is told about the root bridge the allocator has.
        description.pci_root = self
            .pci
            .as_ref()
            .and_then(|pci| pci.roots().first())
            .map(|root| root.info());
        description.framebuffer = self
            .pci_framebuffer
            .or_else(|| self.northbridge.framebuffer_info());
        description.acpi = self.acpi_rsdp().map(AcpiTables::at);
        let acpi_s5 = {
            use fstart_acpi::platform::X86PlatformProvider;
            use fstart_driver_intel::southbridge::pmio_ich;
            [fstart_core::description::AcpiSleepControl::new(
                self.southbridge.x86_platform_config(0).pmbase + pmio_ich::PM1_CNT,
                pmio_ich::S5_SLEEP_TYPE,
            )]
        };
        {
            description.acpi_s5 = &acpi_s5;
        }
        description.smbios = self.ctx.smbios();
        description.mainboard = (&self.mainboard.0, &self.mainboard.1);
        description.tables = &tables;
        #[allow(unused_mut)]
        let mut placement = Placement::default();
        // SAFETY: the northbridge opened the PAM shadow at early init and
        // nothing of the firmware lives in it; the window is a sealed,
        // reserved store entry.
        #[cfg(feature = "payload-coreboot")]
        {
            placement.coreboot_window = self
                .coreboot_window
                .map(|(base, size)| unsafe { fstart_payload::coreboot::Window::new(base, size) });
        }
        #[allow(unused_mut)]
        let mut handoff = Handoff::new(description, placement);
        // CrabEFI shows the options in its menu and writes them to CMOS.
        #[cfg(feature = "payload-uefi-basic")]
        {
            handoff.settings = Some(fstart_payload::crabefi::settings::FirmwareSettings {
                cfr: fstart_options::cfr::encode(&self.options),
                cmos: Some(crate::options::store()),
            });
        }
        fstart_payload::boot(handoff, ())
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
    let cpu = P::cpu_driver(fstart_stage::microcode_blob(), northbridge, southbridge);
    let drivers: [&dyn fstart_arch::x86::mp::CpuDriver; 1] = [&cpu];
    // The supported Intel platforms are single-socket. The CPU model reports
    // its enabled threads when CPUID cannot (coreboot model_2065x
    // get_cpu_count()); otherwise CPUID's package-local count is used, as
    // in model_1067x. Multi-socket platforms need system-wide discovery.
    let num_cpus = fstart_arch::x86::mp::CpuDriver::logical_cpu_count(&cpu).unwrap_or_else(|| {
        let (_, ebx, _, _) = fstart_arch::x86::cpuid(1);
        (((ebx >> 16) & 0xff) as u16).max(1)
    });
    use fstart_arch::x86::cpu::intel::smm::{IntelSmm, IntelSmmError};
    use fstart_arch::x86::mp::{MpConfig, MpError, MpHandle, mp_init_with_setup};
    let config = MpConfig {
        cpu_drivers: &drivers,
        num_cpus,
        max_cpus,
    };
    let smm = IntelSmm::new(P::NAME, northbridge, southbridge, &cpu, resume);
    let mut relocated = None;
    let report = |e: IntelSmmError| {
        fstart_log::error!("{} SMM installation failed: {}", P::NAME, e.name());
        ServiceError::HardwareError
    };
    let mut relocate = |mp: &MpHandle| {
        relocated = crate::SMM_IMAGE
            .map(|image| smm.relocate(mp, image))
            .transpose()
            .map_err(report)?;
        Ok(())
    };
    let mp = if P::RELOCATE_BEFORE_CPU_INIT {
        mp_init_with_setup(&config, !P::IOAPIC_VIRTUAL_WIRE, |mp| {
            relocate(mp).map_err(|_: ServiceError| MpError::PlatformSetupFailed)
        })
        .map_err(|_| ServiceError::HardwareError)?
    } else {
        let mp = fstart_arch::x86::mp::mp_init(&config).map_err(|_| ServiceError::HardwareError)?;
        relocate(&mp)?;
        mp
    };
    if let Some(handler) = relocated {
        handler.activate(&mp).map_err(report)?;
    }
    Ok(())
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

    pub(crate) fn set_acpi_rsdp(&mut self, rsdp: Option<u64>) {
        self.acpi_rsdp = rsdp;
    }

    pub(crate) fn set_smbios(&mut self, smbios: Option<u64>) {
        self.smbios = smbios;
    }
}
