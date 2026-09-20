//! Fixed Intel DRAM mainstage flow and its state.

use crate::boot::{import_intel_directory, install_intel_load_policy};
use crate::{
    IntelBoard, IntelChipsetConfig, IntelEarlyBoardHooks, IntelEarlyCtx, IntelEarlyPlatform, layout,
};
use fstart_core::services::memory_detect::{E820Entry, MemoryDetector};
use fstart_core::services::{ConsoleDevice, ServiceError};
use fstart_driver_intel::{IntelNorthbridgeDriver, IntelSouthbridgeDriver};
use fstart_stage::payload::MainstagePayload;

/// Mainstage type for a board.
pub type Mainstage<B> = IntelMainstage<
    <B as IntelBoard>::Platform,
    <B as IntelBoard>::Hooks,
    <B as IntelBoard>::Console,
>;

fn run_mainstage_phase(
    platform: &str,
    name: &str,
    phase: impl FnOnce() -> Result<(), ServiceError>,
) {
    fstart_log::info!("{} mainstage: {}", platform, name);
    if phase().is_err() {
        fstart_log::error!("{} mainstage: {} failed", platform, name);
        fstart_arch::x86_64::halt();
    }
}

/// Handwritten fixed Intel mainstage flow. Ordering is this function.
pub(crate) fn run_intel_mainstage<B: IntelBoard>() -> ! {
    let platform = B::Platform::NAME;
    let Ok(hooks) = B::hooks() else {
        fstart_arch::x86_64::halt();
    };
    let Ok(layout) = layout::IntelBootLayout::current(2) else {
        fstart_arch::x86_64::halt()
    };
    let Ok(mut mainstage) = Mainstage::<B>::bind::<B>(layout, hooks) else {
        fstart_arch::x86_64::halt();
    };

    let (firmware_base, firmware_size) = mainstage.ctx.firmware_region();
    let boot_media =
        fstart_stage::fixed_helpers::MemoryMappedFfs::new(firmware_base, firmware_size);
    // S3 resume is bootblock policy transported through the postcar stash.
    let resume = crate::boot::handoff(firmware_base, firmware_size)
        .map(|stash| stash.boot_flags & fstart_arch::x86_64::car_teardown::BOOT_FLAG_S3_RESUME != 0)
        .unwrap_or(false);
    mainstage.resume = resume;
    // Postcar authenticated our initialized image before entry. Import only
    // the bounded directory reference, then retain its verified bytes in RAM.
    // Drivers use the published verified asset service, not a new signature.
    run_mainstage_phase(platform, "import_boot_context", || {
        import_intel_directory(firmware_base, firmware_size)
    });
    // Import publishes the inherited locator; mounting before it must fail.
    // Neither metadata phase performs chipset/device initialization.
    run_mainstage_phase(platform, "publish_boot_media", || boot_media.mount());
    run_mainstage_phase(platform, "pre_bus_scan", || mainstage.pre_bus_scan());
    // Reserve the firmware's own windows before any loader policy or table
    // allocation reads the map: on resume the OS may not own the bytes the
    // next boot reloads postcar and the ramstage into.
    run_mainstage_phase(platform, "reserve_firmware_memory", || {
        mainstage.reserve_firmware_memory()
    });
    run_mainstage_phase(platform, "load_memory_policy", || {
        mainstage.refresh_load_policy()
    });
    run_mainstage_phase(platform, "bus_scan", || mainstage.bus_scan());
    run_mainstage_phase(platform, "init_devices", || mainstage.init_devices());
    run_mainstage_phase(platform, "mount_boot_media", || {
        fstart_arch::x86_64::enable_boot_media_rom_cache();
        mainstage.northbridge.stage_local_init()
    });
    run_mainstage_phase(platform, "verify_boot_media", || boot_media.verify());
    // The graphics OpRegion and modeset read the VBT out of the verified boot
    // media, so they run only after verification. On S3 resume the platform
    // decides: Intel skips the modeset, the OS display driver restores it.
    run_mainstage_phase(platform, "display_init", || {
        if mainstage.resume && !B::Platform::RESUME_DISPLAY_INIT {
            fstart_log::info!("{} mainstage: display init skipped on S3 resume", platform);
            return Ok(());
        }
        mainstage.northbridge.post_verify_init()
    });
    // The OS wake vector must be read from the *surviving* FACS before the
    // table set is re-emitted: rebuild zeroes it (coreboot runs
    // BS_OS_RESUME_CHECK before BS_WRITE_TABLES for the same reason).
    #[cfg(feature = "acpi")]
    let wake_vector = if resume {
        fstart_acpi::platform::x86::wake::find_wakeup_vector()
    } else {
        None
    };
    run_mainstage_phase(platform, "emit_tables", || mainstage.emit_tables());
    // Table allocation changes the memory map; payload loads must respect it.
    run_mainstage_phase(platform, "load_memory_policy", || {
        mainstage.refresh_load_policy()
    });
    run_mainstage_phase(platform, "finalize", || mainstage.finalize());

    // Leave the legacy keyboard controller quiet before the payload/OS probes it.
    fstart_driver_superio::quiesce_i8042_for_os();

    #[cfg(feature = "acpi")]
    if resume {
        match wake_vector {
            Some(vector) => {
                fstart_log::info!("{}: resuming OS at wake vector {:#x}", platform, vector);
                fstart_log::flush();
                fstart_arch::x86_64::s3_wake::jump_to_wakeup_vector(vector);
            }
            None => {
                // Decided policy: no valid vector means the surviving tables
                // are unusable; come up cleanly instead of booting over the
                // suspended OS image.
                fstart_log::error!("{}: S3 resume without a wake vector, resetting", platform);
                fstart_log::flush();
                mainstage.southbridge().system_reset(true);
            }
        }
    }
    #[cfg(not(feature = "acpi"))]
    if resume {
        // S3 needs ACPI (FACS wake vector); without tables there is nothing
        // to resume into.
        fstart_log::error!("{}: S3 resume without ACPI, resetting", platform);
        fstart_log::flush();
        mainstage.southbridge().system_reset(true);
    }

    // mp_init leaves APs polling firmware mailboxes so mainstage can dispatch
    // scoped work. Stop that firmware activity before transferring ownership
    // to a payload; an OS expects every AP to remain quiescent until its own
    // INIT/SIPI sequence.
    #[cfg(feature = "mp")]
    if !fstart_arch::mp::park_aps_for_payload() {
        fstart_log::error!("{}: failed to quiesce APs for payload handoff", platform);
        fstart_log::flush();
        fstart_arch::x86_64::halt();
    }

    B::Payload::boot(mainstage)
}

/// Bring up BSP + APs with the chipset's CPU driver and, when fbuild embedded
/// an SMM image into this stage ([`SMM_IMAGE`]), relocate SMBASE, install the
/// handler in TSEG and lock SMRAM through the shared gen1 flow.
#[cfg(feature = "mp")]
fn init_mp<P: IntelEarlyPlatform>(
    northbridge: &P::Northbridge,
    southbridge: &P::Southbridge,
    max_cpus: u16,
) -> Result<(), ServiceError> {
    // APs must run the same updated microcode as the BSP, whose update
    // happens in pre-CAR assembly; the blob sits in boot flash.
    let cpu = P::cpu_driver(crate::intel_microcode_blob());
    let drivers: [&dyn fstart_arch::mp::CpuDriver; 1] = [&cpu];
    let smm = fstart_arch::cpu_intel::smm::IntelSmm::new(
        P::NAME,
        northbridge,
        southbridge,
        P::SMM_BSP_ONLY_DISPATCH,
    );
    fstart_arch::mp::mp_init(&fstart_arch::mp::MpConfig {
        cpu_drivers: &drivers,
        smm: crate::SMM_IMAGE.map(|_| &smm as &dyn fstart_arch::mp::SmmOps),
        smm_image: crate::SMM_IMAGE,
        max_cpus,
    })
    .map(|_| ())
    .map_err(|_| ServiceError::HardwareError)
}

pub struct IntelMainstage<P, Hooks, C>
where
    P: IntelEarlyPlatform,
    Hooks: IntelEarlyBoardHooks<P>,
    C: ConsoleDevice,
{
    northbridge: P::Northbridge,
    southbridge: P::Southbridge,
    /// Constructed only when the mainstage reaches bus scanning.
    pci: Option<fstart_pci::PciEcam>,
    hooks: Hooks,
    console: C,
    ctx: MainstageCtx,
    geometry: layout::IntelBootLayout<'static>,
    console_node: &'static str,
    max_cpus: u16,
    /// S3 resume: the OS wake vector is where this boot ends, and hardware
    /// that survived in RAM must not be reinitialized blindly.
    pub resume: bool,
    /// Board-declared direct Linux payload, when the plan selected one.
    #[cfg_attr(not(feature = "payload-linux"), allow(dead_code))]
    linux: Option<crate::facts::X86LinuxBoot>,
    #[cfg(feature = "smbios")]
    smbios_desc: &'static crate::tables::SmbiosDesc<'static>,
}

impl<P, Hooks, C> IntelMainstage<P, Hooks, C>
where
    P: IntelEarlyPlatform,
    Hooks: IntelEarlyBoardHooks<P>,
    C: ConsoleDevice,
{
    fn bind<B>(
        geometry: layout::IntelBootLayout<'static>,
        hooks: Hooks,
    ) -> Result<Self, ServiceError>
    where
        B: IntelBoard<Platform = P, Hooks = Hooks, Console = C>,
    {
        let (firmware_base, firmware_size) = geometry.firmware()?;
        Ok(Self {
            northbridge: P::Northbridge::new_from_config(B::CONFIG.northbridge())?,
            southbridge: P::Southbridge::new_from_config(B::CONFIG.southbridge())?,
            pci: None,
            hooks,
            console: C::new(B::console_config())?,
            ctx: MainstageCtx::new(firmware_base, firmware_size),
            geometry,
            console_node: B::console_node(),
            max_cpus: B::CONFIG.max_cpus(),
            resume: false,
            // The very constant the host plan packaged the payload with: the
            // launcher cannot jump anywhere the image was not built for.
            linux: B::FACTS.linux,
            #[cfg(feature = "smbios")]
            smbios_desc: B::smbios_desc(),
        })
    }

    #[must_use]
    pub const fn northbridge(&self) -> &P::Northbridge {
        &self.northbridge
    }

    #[must_use]
    pub const fn southbridge(&self) -> &P::Southbridge {
        &self.southbridge
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

    fn pre_bus_scan(&mut self) -> Result<(), ServiceError> {
        self.northbridge.pre_console_init()?;
        self.southbridge.pre_console_init()?;
        self.hooks
            .before_console(&mut IntelEarlyCtx::new(&mut self.southbridge))?;

        self.console.init()?;
        // SAFETY: the mainstage owns the console until it hands control to the payload.
        unsafe { fstart_log::init(&self.console) };
        fstart_log::info!("fstart ramstage console ready");
        fstart_log::info!("{}: {} console ready", self.console_node, C::NAME);

        self.northbridge.early_init()?;
        self.southbridge.early_init()?;

        self.hooks
            .before_memory(&mut IntelEarlyCtx::new(&mut self.southbridge))?;
        let count = self
            .northbridge
            .detect_memory(self.ctx.e820_state_mut().entries_mut())?;
        let total = self.northbridge.total_ram_bytes()?;
        fstart_log::info!(
            "Detected {} MiB RAM, {} e820 entries from {}",
            total >> 20,
            count,
            P::NAME,
        );
        self.ctx.e820_state_mut().set_detected(count, total);
        self.northbridge.memory_detected(self.ctx.e820_state());

        // DRAM was initialized by the bootblock. Mainstage only reconstructs the
        // memory map and must not retrain or issue JEDEC commands again.
        //
        // Cache is still off at this point: the RAM-stage entry tore down CAR
        // (CR0.CD=1, MTRRs disabled) and nothing re-enabled it yet. The ranges
        // the northbridge just published are enough to restore caching now,
        // instead of leaving the whole mainstage uncached until MP init repeats
        // the same MTRR program per CPU.
        // SAFETY: BSP-only, after the memory map is published.
        unsafe {
            fstart_arch::x86::mtrr::setup_ram_wb();
        }
        Ok(())
    }

    fn refresh_load_policy(&self) -> Result<(), ServiceError> {
        install_intel_load_policy(self.ctx.e820(), self.geometry)
    }

    /// Mark the firmware's bootstrap windows, boot-media arena, stage cache
    /// slots and low scratch as reserved for the OS and for later loads.
    fn reserve_firmware_memory(&mut self) -> Result<(), ServiceError> {
        crate::boot::reserve_firmware_memory(self.ctx.e820_state_mut(), self.geometry)
    }

    fn bus_scan(&mut self) -> Result<(), ServiceError> {
        if self.pci.is_none() {
            self.pci = Some(
                fstart_pci::PciEcam::from_provider(&self.northbridge)
                    .map_err(|_| ServiceError::HardwareError)?,
            );
        }
        let pci = self.pci.as_mut().ok_or(ServiceError::NotInitialized)?;
        pci.enumerate_and_allocate()
            .map_err(|_| ServiceError::HardwareError)
    }

    fn init_devices(&mut self) -> Result<(), ServiceError> {
        self.northbridge.post_dram_init()?;
        self.southbridge.post_dram_init()?;
        self.hooks
            .after_memory(&mut IntelEarlyCtx::new(&mut self.southbridge))?;
        #[cfg(feature = "mp")]
        init_mp::<P>(&self.northbridge, &self.southbridge, self.max_cpus)?;
        Ok(())
    }

    fn emit_tables(&mut self) -> Result<(), ServiceError> {
        self.emit_acpi()?;
        #[cfg(feature = "smbios")]
        crate::tables::prepare_smbios(self.ctx.e820_state_mut(), self.smbios_desc);
        Ok(())
    }

    fn finalize(&mut self) -> Result<(), ServiceError> {
        self.hooks
            .before_handoff(&mut IntelEarlyCtx::new(&mut self.southbridge))?;
        self.southbridge.finalize_init()
    }
}

#[cfg(feature = "acpi")]
impl<P, Hooks, C> IntelMainstage<P, Hooks, C>
where
    P: IntelEarlyPlatform,
    Hooks: IntelEarlyBoardHooks<P>,
    C: ConsoleDevice,
{
    fn emit_acpi(&mut self) -> Result<(), ServiceError> {
        use fstart_acpi::device::AcpiDevice;
        use fstart_acpi::platform::X86PlatformProvider;
        let acpi_ctx = P::AcpiContext::default();
        let platform = fstart_acpi::platform::PlatformConfig::X86(
            self.southbridge
                .x86_platform_config(u32::from(fstart_arch::mp::online_cpus())),
        );
        let (northbridge, southbridge, hooks) = (&self.northbridge, &self.southbridge, &self.hooks);
        let rsdp =
            crate::tables::prepare_acpi(self.ctx.e820_state_mut(), &platform, |dsdt, extra| {
                dsdt.extend(northbridge.dsdt_aml(northbridge.config()));
                dsdt.extend(southbridge.dsdt_aml(southbridge.config()));
                dsdt.extend(hooks.dsdt_aml(&acpi_ctx));
                extra.extend(northbridge.extra_tables(northbridge.config()));
                extra.extend(southbridge.extra_tables(southbridge.config()));
                extra.extend(hooks.extra_tables(&acpi_ctx));
            })
            .map_err(|_| ServiceError::HardwareError)?;
        self.ctx.set_acpi_rsdp(Some(rsdp));
        Ok(())
    }
}

#[cfg(not(feature = "acpi"))]
impl<P, Hooks, C> IntelMainstage<P, Hooks, C>
where
    P: IntelEarlyPlatform,
    Hooks: IntelEarlyBoardHooks<P>,
    C: ConsoleDevice,
{
    fn emit_acpi(&mut self) -> Result<(), ServiceError> {
        Ok(())
    }
}

/// Direct x86 Linux payload: the ramstage hands the kernel the memory map and
/// ACPI tables it produced, through the board's own payload policy.
#[cfg(feature = "payload-linux")]
impl<P, Hooks, C> fstart_stage::payload::X86LinuxPayloadContext for IntelMainstage<P, Hooks, C>
where
    P: IntelEarlyPlatform,
    Hooks: IntelEarlyBoardHooks<P>,
    C: ConsoleDevice,
{
    fn x86_linux_payload_config(&self) -> Option<fstart_stage::payload::X86LinuxPayloadConfig> {
        self.linux
            .map(|linux| fstart_stage::payload::X86LinuxPayloadConfig {
                kernel_load_addr: linux.kernel_load_addr,
                zero_page_addr: linux.zero_page_addr,
                bootargs: linux.bootargs,
                print_x86_mtrrs: linux.print_x86_mtrrs,
            })
    }

    fn e820(&self) -> &[E820Entry] {
        self.e820()
    }

    fn acpi_rsdp(&self) -> Option<u64> {
        self.acpi_rsdp()
    }
}

impl<P, Hooks, C> fstart_stage::payload::X86UefiPayloadContext for IntelMainstage<P, Hooks, C>
where
    P: IntelEarlyPlatform,
    Hooks: IntelEarlyBoardHooks<P>,
    C: ConsoleDevice,
{
    fn console(&self) -> Option<&dyn fstart_core::services::Console> {
        Some(&self.console)
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

    fn pci_root(&self) -> Option<fstart_pci::PciRootInfo> {
        Some(fstart_pci::PciRootProvider::root_info(&self.northbridge))
    }

    #[cfg(feature = "payload-uefi-basic")]
    fn framebuffer(&self) -> Option<fstart_stage::crabefi::FramebufferConfig> {
        self.northbridge
            .framebuffer_info()
            .map(|info| fstart_stage::crabefi::FramebufferConfig {
                physical_address: info.base_addr,
                width: info.width,
                height: info.height,
                stride: info.stride,
                bits_per_pixel: info.bits_per_pixel,
                red_mask_pos: info.red_pos,
                red_mask_size: info.red_size,
                green_mask_pos: info.green_pos,
                green_mask_size: info.green_size,
                blue_mask_pos: info.blue_pos,
                blue_mask_size: info.blue_size,
            })
    }
}

pub struct MainstageCtx {
    e820: fstart_core::services::memory_detect::E820State,
    acpi_rsdp: Option<u64>,
    firmware_base: u64,
    firmware_size: usize,
}

impl MainstageCtx {
    pub(crate) fn new(firmware_base: u64, firmware_size: usize) -> Self {
        Self {
            e820: fstart_core::services::memory_detect::E820State::new(),
            acpi_rsdp: None,
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
    pub const fn firmware_region(&self) -> (u64, usize) {
        (self.firmware_base, self.firmware_size)
    }

    #[cfg(feature = "acpi")]
    pub(crate) fn set_acpi_rsdp(&mut self, rsdp: Option<u64>) {
        self.acpi_rsdp = rsdp;
    }
}
