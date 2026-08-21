//! Shared Intel platform machinery and chipset-specific handwritten flows.
//!
//! Intel-common code lives here only when it is shared by multiple Intel
//! chipsets. Ordering stays in each chipset module's handwritten flow.

#![no_std]

#[cfg(feature = "stage")]
extern crate ufmt;

#[cfg(any(feature = "acpi", feature = "smbios"))]
pub mod tables;

pub mod gm965;
pub mod pineview;

#[cfg(feature = "stage")]
use core::marker::PhantomData;
#[cfg(feature = "stage")]
use fstart_core::services::memory_detect::E820Entry;
#[cfg(feature = "stage")]
use fstart_core::services::{ConsoleDevice, ServiceError};
#[cfg(feature = "stage")]
pub use fstart_driver_intel::{
    BootPath, IntelEcamConfig, IntelNorthbridgeDriver, IntelSouthbridgeDriver,
};
#[cfg(feature = "stage")]
pub use fstart_stage::{StageBoard, StageEnvironment, payload::MainstagePayload};

/// Native SMM handler image built by fbuild, embedded into stages whose build
/// had `FSTART_SMM_IMAGE` set (the DRAM mainstage of SMM-capable boards).
///
/// `None` in stages built without an SMM image (bootblocks, non-SMM boards);
/// MP setup then skips SMM relocation and SMRAM stays unlocked.
#[cfg(all(feature = "stage", fstart_intel_has_smm_image))]
pub const SMM_IMAGE: Option<&'static [u8]> = Some(include_bytes!(env!("FSTART_SMM_IMAGE")));
#[cfg(all(feature = "stage", not(fstart_intel_has_smm_image)))]
pub const SMM_IMAGE: Option<&'static [u8]> = None;

/// Locate the concatenated Intel microcode blob in the mounted memory-mapped
/// FFS window.
///
/// The bootblock applies BSP microcode before CAR setup; MP init calls this
/// so every AP gets the same update. The blob's location is recorded in the
/// image anchor, so this goes through the published FFS context and the FFS
/// reader rather than open-coded flash mappings. Returns `None` when no
/// window is mounted or the anchor records no blob.
#[cfg(all(feature = "stage", feature = "mp", target_arch = "x86_64"))]
#[must_use]
pub fn intel_microcode_blob() -> Option<&'static [u8]> {
    use fstart_ffs::FfsReader;

    let ctx = fstart_core::services::ffs_context::memory_mapped()?;
    // SAFETY: both accessors are documented as valid for the whole stage
    // execution once the boot-media provider published them.
    let (image, anchor_bytes) = unsafe { (ctx.image_bytes(), ctx.anchor_bytes()) };
    // SAFETY: the anchor bytes are the stage's aligned `.fstart.anchor` static.
    let anchor = unsafe { fstart_ffs::FfsReader::read_anchor_volatile(anchor_bytes) }.ok()?;
    FfsReader::new(image).intel_microcode(&anchor)
}

#[cfg(feature = "stage")]
pub(crate) fn firmware_window(
    layout: fstart_core::FlashLayout,
) -> Result<(u64, usize), ServiceError> {
    let (base, size) = match layout {
        fstart_core::FlashLayout::IntelIfd(layout) => {
            let Some(bios) = layout.bios_region() else {
                return Err(ServiceError::NotInitialized);
            };
            let Some(base) = layout.base().checked_add(u64::from(bios.offset)) else {
                return Err(ServiceError::NotInitialized);
            };
            (base, bios.size)
        }
        fstart_core::FlashLayout::X86Legacy(layout) => (layout.base(), layout.size()),
    };
    if size == 0 {
        return Err(ServiceError::NotInitialized);
    }
    Ok((base, size as usize))
}

#[cfg(feature = "stage")]
pub(crate) fn run_mainstage_phase(
    platform: &str,
    name: &str,
    halt: fn() -> !,
    phase: impl FnOnce() -> Result<(), ServiceError>,
) {
    fstart_log::info!("{} mainstage: {}", platform, name);
    if phase().is_err() {
        fstart_log::error!("{} mainstage: {} failed", platform, name);
        halt();
    }
}

#[cfg(feature = "stage")]
pub(crate) trait IntelMainstageFlow: MainstagePhases {
    fn firmware_region(&self) -> (u64, usize);
    fn stage_local_init(&mut self) -> Result<(), ServiceError>;
}

#[cfg(feature = "stage")]
pub(crate) fn run_intel_mainstage<M, Payload>(
    platform: &str,
    halt: fn() -> !,
    mut mainstage: M,
) -> !
where
    M: IntelMainstageFlow,
    Payload: MainstagePayload<M>,
{
    let (firmware_base, firmware_size) = mainstage.firmware_region();
    let boot_media =
        fstart_stage::fixed_helpers::MemoryMappedFfs::new(firmware_base, firmware_size);
    // Publish the window before device init: drivers brought up during
    // init_devices (per-AP microcode, SMM install) read board assets through
    // the FFS services rather than open-coded flash mappings.
    run_mainstage_phase(platform, "publish_boot_media", halt, || boot_media.mount());
    run_mainstage_phase(platform, "pre_bus_scan", halt, || mainstage.pre_bus_scan());
    run_mainstage_phase(platform, "bus_scan", halt, || mainstage.bus_scan());
    run_mainstage_phase(platform, "init_devices", halt, || mainstage.init_devices());
    run_mainstage_phase(platform, "mount_boot_media", halt, || {
        fstart_arch::x86_64::enable_boot_media_rom_cache();
        mainstage.stage_local_init()
    });
    run_mainstage_phase(platform, "verify_boot_media", halt, || boot_media.verify());
    run_mainstage_phase(platform, "emit_tables", halt, || mainstage.emit_tables());
    run_mainstage_phase(platform, "finalize", halt, || mainstage.finalize());

    // Leave the legacy keyboard controller quiet before the payload/OS probes it.
    fstart_driver_superio::quiesce_i8042_for_os();

    Payload::boot(mainstage)
}

#[cfg(feature = "stage")]
pub(crate) struct BootblockSpec<C: ConsoleDevice> {
    pub platform: &'static str,
    pub next_stage: &'static str,
    pub ramstage_load_addr: u64,
    pub flash_layout: fstart_core::FlashLayout,
    pub console_config: C::Config,
    pub console_node: &'static str,
}

/// Shared Intel mainstage machinery. Chipset modules bind board facts once,
/// then the common phase code owns the rest.
#[cfg(feature = "stage")]
pub struct IntelMainstage<P, NB, SB, Hooks, C, AcpiContext>
where
    P: IntelEarlyPlatform<Southbridge = SB>,
    NB: IntelNorthbridgeDriver,
    SB: IntelSouthbridgeDriver,
    Hooks: IntelEarlyBoardHooks<P>,
    C: ConsoleDevice,
{
    northbridge: NB,
    southbridge: SB,
    /// Constructed only when the mainstage reaches bus scanning. The
    /// bootblock owns no PCI allocator state.
    #[cfg(not(fstart_stage_env = "car"))]
    pci: Option<fstart_pci::PciEcam>,
    hooks: Hooks,
    console: C,
    ctx: MainstageCtx,
    platform_node: &'static str,
    console_node: &'static str,
    #[cfg(feature = "mp")]
    init_mp: fn() -> Result<(), ServiceError>,
    #[cfg(feature = "smbios")]
    smbios_desc: &'static crate::tables::SmbiosDesc<'static>,
    _platform: PhantomData<P>,
    _acpi: PhantomData<AcpiContext>,
}

#[cfg(feature = "stage")]
impl<P, NB, SB, Hooks, C, AcpiContext> IntelMainstage<P, NB, SB, Hooks, C, AcpiContext>
where
    P: IntelEarlyPlatform<Southbridge = SB>,
    NB: IntelNorthbridgeDriver,
    SB: IntelSouthbridgeDriver,
    Hooks: IntelEarlyBoardHooks<P>,
    C: ConsoleDevice,
{
    #[must_use]
    pub const fn northbridge(&self) -> &NB {
        &self.northbridge
    }

    #[must_use]
    pub const fn southbridge(&self) -> &SB {
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
}

#[cfg(all(feature = "stage", feature = "acpi"))]
trait MainstageAcpi {
    fn emit_acpi(&mut self) -> Result<(), ServiceError>;
}

#[cfg(all(feature = "stage", feature = "acpi"))]
impl<P, NB, SB, Hooks, C, AcpiContext> MainstageAcpi
    for IntelMainstage<P, NB, SB, Hooks, C, AcpiContext>
where
    P: IntelEarlyPlatform<Southbridge = SB, AcpiContext = AcpiContext>,
    Hooks: IntelEarlyBoardHooks<P>,
    C: ConsoleDevice,
    NB: IntelNorthbridgeDriver
        + fstart_acpi::device::AcpiDevice<Config = <NB as IntelNorthbridgeDriver>::Config>,
    SB: IntelSouthbridgeDriver
        + fstart_acpi::device::AcpiDevice<Config = <SB as IntelSouthbridgeDriver>::Config>
        + fstart_acpi::platform::X86PlatformProvider,
    Hooks: fstart_acpi::device::AcpiDevice<Config = AcpiContext>,
    AcpiContext: Default,
{
    fn emit_acpi(&mut self) -> Result<(), ServiceError> {
        let acpi_ctx = AcpiContext::default();
        let rsdp = emit_x86_acpi_tables(
            self.ctx.e820_state_mut(),
            &self.northbridge,
            self.northbridge.config(),
            &self.southbridge,
            self.southbridge.config(),
            &self.hooks,
            &acpi_ctx,
        );
        self.ctx.set_acpi_rsdp(Some(rsdp));
        Ok(())
    }
}

#[cfg(all(feature = "stage", not(feature = "acpi")))]
trait MainstageAcpi {
    fn emit_acpi(&mut self) -> Result<(), ServiceError>;
}

#[cfg(all(feature = "stage", not(feature = "acpi")))]
impl<T> MainstageAcpi for T {
    fn emit_acpi(&mut self) -> Result<(), ServiceError> {
        Ok(())
    }
}

#[cfg(feature = "stage")]
impl<P, NB, SB, Hooks, C, AcpiContext> IntelMainstageFlow
    for IntelMainstage<P, NB, SB, Hooks, C, AcpiContext>
where
    P: IntelEarlyPlatform<Southbridge = SB>,
    NB: IntelNorthbridgeDriver,
    SB: IntelSouthbridgeDriver,
    Hooks: IntelEarlyBoardHooks<P>,
    C: ConsoleDevice,
    IntelMainstage<P, NB, SB, Hooks, C, AcpiContext>: MainstageAcpi,
{
    fn firmware_region(&self) -> (u64, usize) {
        self.ctx.firmware_region()
    }

    fn stage_local_init(&mut self) -> Result<(), ServiceError> {
        self.northbridge.stage_local_init()
    }
}

#[cfg(feature = "stage")]
pub(crate) struct MainstageSpec<NB, SB, C>
where
    NB: IntelNorthbridgeDriver,
    SB: IntelSouthbridgeDriver,
    C: ConsoleDevice,
{
    pub flash_layout: fstart_core::FlashLayout,
    pub nb_config: &'static NB::Config,
    pub sb_config: &'static SB::Config,
    pub console_config: C::Config,
    pub console_node: &'static str,
    pub platform_node: &'static str,
    #[cfg(feature = "mp")]
    pub init_mp: fn() -> Result<(), ServiceError>,
    #[cfg(feature = "smbios")]
    pub smbios_desc: &'static crate::tables::SmbiosDesc<'static>,
}

#[cfg(feature = "stage")]
pub(crate) fn bind_intel_mainstage<P, NB, SB, Hooks, C, AcpiContext>(
    spec: MainstageSpec<NB, SB, C>,
    hooks: Hooks,
) -> Result<IntelMainstage<P, NB, SB, Hooks, C, AcpiContext>, ServiceError>
where
    P: IntelEarlyPlatform<Southbridge = SB>,
    NB: IntelNorthbridgeDriver,
    SB: IntelSouthbridgeDriver,
    Hooks: IntelEarlyBoardHooks<P>,
    C: ConsoleDevice,
{
    let (firmware_base, firmware_size) = firmware_window(spec.flash_layout)?;
    Ok(IntelMainstage {
        northbridge: NB::new_from_config(spec.nb_config)?,
        southbridge: SB::new_from_config(spec.sb_config)?,
        #[cfg(not(fstart_stage_env = "car"))]
        pci: None,
        hooks,
        console: C::new(spec.console_config)?,
        ctx: MainstageCtx::new(firmware_base, firmware_size),
        platform_node: spec.platform_node,
        console_node: spec.console_node,
        #[cfg(feature = "mp")]
        init_mp: spec.init_mp,
        #[cfg(feature = "smbios")]
        smbios_desc: spec.smbios_desc,
        _platform: PhantomData,
        _acpi: PhantomData,
    })
}

#[cfg(feature = "stage")]
impl<P, NB, SB, Hooks, C, AcpiContext> MainstagePhases
    for IntelMainstage<P, NB, SB, Hooks, C, AcpiContext>
where
    P: IntelEarlyPlatform<Southbridge = SB>,
    NB: IntelNorthbridgeDriver,
    SB: IntelSouthbridgeDriver,
    Hooks: IntelEarlyBoardHooks<P>,
    C: ConsoleDevice,
    IntelMainstage<P, NB, SB, Hooks, C, AcpiContext>: MainstageAcpi,
{
    fn pre_bus_scan(&mut self) -> Result<(), ServiceError> {
        intel_pre_bus_scan::<P, _, _, _, C>(
            self.platform_node,
            self.console_node,
            &mut self.northbridge,
            &mut self.southbridge,
            &mut self.hooks,
            &mut self.console,
            &mut self.ctx,
        )
    }

    fn bus_scan(&mut self) -> Result<(), ServiceError> {
        #[cfg(not(fstart_stage_env = "car"))]
        {
            if self.pci.is_none() {
                self.pci = Some(
                    fstart_pci::PciEcam::from_provider(&self.northbridge)
                        .map_err(|_| ServiceError::HardwareError)?,
                );
            }
            let pci = self.pci.as_mut().ok_or(ServiceError::NotInitialized)?;
            return intel_bus_scan(pci);
        }

        #[cfg(fstart_stage_env = "car")]
        Err(ServiceError::NotSupported)
    }

    fn init_devices(&mut self) -> Result<(), ServiceError> {
        intel_init_devices::<P, _, _>(&mut self.southbridge, &mut self.hooks, || {
            #[cfg(feature = "mp")]
            (self.init_mp)()?;
            Ok(())
        })
    }

    fn emit_tables(&mut self) -> Result<(), ServiceError> {
        self.emit_acpi()?;
        #[cfg(feature = "smbios")]
        crate::tables::prepare_smbios(self.ctx.e820_state_mut(), self.smbios_desc);
        Ok(())
    }

    fn finalize(&mut self) -> Result<(), ServiceError> {
        intel_finalize::<P, _, _>(&mut self.southbridge, &mut self.hooks)
    }
}

#[cfg(feature = "stage")]
impl<P, NB, SB, Hooks, C, AcpiContext> fstart_stage::payload::X86UefiPayloadContext
    for IntelMainstage<P, NB, SB, Hooks, C, AcpiContext>
where
    P: IntelEarlyPlatform<Southbridge = SB>,
    NB: IntelNorthbridgeDriver,
    NB::Config: IntelEcamConfig,
    SB: IntelSouthbridgeDriver,
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

    fn ecam_base(&self) -> Option<u64> {
        Some(self.northbridge.config().ecam_base())
    }
}

#[cfg(feature = "stage")]
pub(crate) fn run_intel_bootblock<P, NB, SB, Hooks, C>(
    spec: BootblockSpec<C>,
    hooks: &mut Hooks,
    mut northbridge: NB,
    mut southbridge: SB,
) -> Result<(), ServiceError>
where
    P: IntelEarlyPlatform<Southbridge = SB>,
    NB: IntelNorthbridgeDriver,
    SB: IntelSouthbridgeDriver,
    Hooks: IntelEarlyBoardHooks<P>,
    C: ConsoleDevice,
{
    let (firmware_base, firmware_size) = firmware_window(spec.flash_layout)?;
    let ffs = fstart_stage::fixed_helpers::MemoryMappedFfs::new(firmware_base, firmware_size);

    northbridge.pre_console_init()?;
    southbridge.pre_console_init()?;
    hooks.before_console(&mut IntelEarlyCtx::new(&mut southbridge))?;

    let mut console = C::new(spec.console_config)?;
    console.init()?;
    // SAFETY: this function never returns after installing the stack-owned console.
    unsafe { fstart_log::init(&console) };
    fstart_log::info!("{}: {} console ready", spec.console_node, C::NAME);
    fstart_log::info!("{} bootblock console ready", spec.platform);

    // Complete the pre-RAM chipset flow before touching the ramstage load
    // address. The bootblock itself executes from ROM with its writable state
    // in CAR, but the next stage is loaded into ordinary DRAM.
    northbridge.early_init()?;
    southbridge.early_init()?;
    hooks.before_memory(&mut IntelEarlyCtx::new(&mut southbridge))?;

    fstart_log::info!("{}: initializing DRAM", spec.platform);
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
    fstart_log::info!("{}: DRAM ready", spec.platform);

    fstart_arch::x86_64::enable_boot_media_rom_cache();
    if ffs.mount().is_err()
        || ffs.verify().is_err()
        || ffs.load_file_by_name(spec.next_stage).is_err()
    {
        fstart_log::error!("{} bootblock failed", spec.platform);
        return Err(ServiceError::HardwareError);
    }

    hooks.before_handoff(&mut IntelEarlyCtx::new(&mut southbridge))?;
    fstart_log::info!("jumping to ramstage at {:#x}", spec.ramstage_load_addr);
    fstart_arch::x86_64::jump_to(spec.ramstage_load_addr)
}

#[cfg(feature = "stage")]
pub(crate) fn intel_pre_bus_scan<P, NB, SB, Hooks, C>(
    platform_node: &str,
    console_node: &str,
    northbridge: &mut NB,
    southbridge: &mut SB,
    hooks: &mut Hooks,
    console: &mut C,
    ctx: &mut MainstageCtx,
) -> Result<(), ServiceError>
where
    P: IntelEarlyPlatform<Southbridge = SB>,
    NB: IntelNorthbridgeDriver,
    SB: IntelSouthbridgeDriver,
    Hooks: IntelEarlyBoardHooks<P>,
    C: ConsoleDevice,
{
    northbridge.pre_console_init()?;
    southbridge.pre_console_init()?;
    hooks.before_console(&mut IntelEarlyCtx::new(southbridge))?;

    console.init()?;
    // SAFETY: the mainstage owns the console until it hands control to the payload.
    unsafe { fstart_log::init(console) };
    fstart_log::info!("fstart ramstage console ready");
    fstart_log::info!("{}: {} console ready", console_node, C::NAME);

    northbridge.early_init()?;
    southbridge.early_init()?;

    hooks.before_memory(&mut IntelEarlyCtx::new(southbridge))?;
    let count = northbridge.detect_memory(ctx.e820_state_mut().entries_mut())?;
    let total = northbridge.total_ram_bytes()?;
    fstart_log::info!(
        "Detected {} MiB RAM, {} e820 entries from {}",
        total >> 20,
        count,
        platform_node,
    );
    ctx.e820_state_mut().set_detected(count, total);
    northbridge.memory_detected(ctx.e820_state());

    // DRAM was initialized by the bootblock. Mainstage only reconstructs the
    // memory map and must not retrain or issue JEDEC commands again.
    //
    // Cache is still off at this point: the RAM-stage entry tore down CAR
    // (CR0.CD=1, MTRRs disabled) and nothing re-enabled it yet. The ranges
    // the northbridge just published are enough to restore caching now,
    // instead of leaving the whole mainstage uncached until MP init repeats
    // the same MTRR program per CPU.
    #[cfg(target_arch = "x86_64")]
    unsafe {
        fstart_arch::x86::mtrr::setup_ram_wb();
    }
    Ok(())
}

#[cfg(all(feature = "stage", not(fstart_stage_env = "car")))]
pub(crate) fn intel_bus_scan(pci: &mut fstart_pci::PciEcam) -> Result<(), ServiceError> {
    pci.enumerate_and_allocate()
        .map_err(|_| ServiceError::HardwareError)
}

#[cfg(feature = "stage")]
pub(crate) fn intel_init_devices<P, SB, Hooks>(
    southbridge: &mut SB,
    hooks: &mut Hooks,
    init_mp: impl FnOnce() -> Result<(), ServiceError>,
) -> Result<(), ServiceError>
where
    P: IntelEarlyPlatform<Southbridge = SB>,
    SB: IntelSouthbridgeDriver,
    Hooks: IntelEarlyBoardHooks<P>,
{
    southbridge.post_dram_init()?;
    hooks.after_memory(&mut IntelEarlyCtx::new(southbridge))?;
    init_mp()
}

#[cfg(feature = "stage")]
pub(crate) fn intel_finalize<P, SB, Hooks>(
    southbridge: &mut SB,
    hooks: &mut Hooks,
) -> Result<(), ServiceError>
where
    P: IntelEarlyPlatform<Southbridge = SB>,
    SB: IntelSouthbridgeDriver,
    Hooks: IntelEarlyBoardHooks<P>,
{
    hooks.before_handoff(&mut IntelEarlyCtx::new(southbridge))?;
    southbridge.finalize_init()
}

#[cfg(all(feature = "stage", feature = "acpi"))]
pub(crate) fn emit_acpi_tables<Northbridge, Southbridge, Hooks>(
    e820: &mut fstart_core::services::memory_detect::E820State,
    platform: &fstart_acpi::platform::PlatformConfig,
    northbridge: &Northbridge,
    northbridge_config: &Northbridge::Config,
    southbridge: &Southbridge,
    southbridge_config: &Southbridge::Config,
    hooks: &Hooks,
    hooks_config: &Hooks::Config,
) -> u64
where
    Northbridge: fstart_acpi::device::AcpiDevice,
    Southbridge: fstart_acpi::device::AcpiDevice,
    Hooks: fstart_acpi::device::AcpiDevice,
{
    crate::tables::prepare_acpi(e820, platform, |dsdt, extra| {
        dsdt.extend(northbridge.dsdt_aml(northbridge_config));
        dsdt.extend(southbridge.dsdt_aml(southbridge_config));
        dsdt.extend(hooks.dsdt_aml(hooks_config));
        extra.extend(northbridge.extra_tables(northbridge_config));
        extra.extend(southbridge.extra_tables(southbridge_config));
        extra.extend(hooks.extra_tables(hooks_config));
    })
}

#[cfg(all(feature = "stage", feature = "acpi"))]
pub(crate) fn emit_x86_acpi_tables<Northbridge, Southbridge, Hooks>(
    e820: &mut fstart_core::services::memory_detect::E820State,
    northbridge: &Northbridge,
    northbridge_config: &Northbridge::Config,
    southbridge: &Southbridge,
    southbridge_config: &Southbridge::Config,
    hooks: &Hooks,
    hooks_config: &Hooks::Config,
) -> u64
where
    Northbridge: fstart_acpi::device::AcpiDevice,
    Southbridge: fstart_acpi::device::AcpiDevice + fstart_acpi::platform::X86PlatformProvider,
    Hooks: fstart_acpi::device::AcpiDevice,
{
    let platform = fstart_acpi::platform::PlatformConfig::X86(
        southbridge.x86_platform_config(u32::from(fstart_arch::mp::online_cpus())),
    );
    emit_acpi_tables(
        e820,
        &platform,
        northbridge,
        northbridge_config,
        southbridge,
        southbridge_config,
        hooks,
        hooks_config,
    )
}

/// Marker for Intel platform families with handwritten early flows.
#[cfg(feature = "stage")]
pub trait IntelPlatform {}

/// Board contract common to Intel handwritten early flows.
#[cfg(feature = "stage")]
pub trait IntelEarlyBoard: StageBoard {
    type Platform: IntelEarlyPlatform;
    type Hooks: IntelEarlyBoardHooks<Self::Platform>;

    fn hooks() -> Result<Self::Hooks, ServiceError>;
}

/// Fixed Intel early-flow platform contract.
#[cfg(feature = "stage")]
pub trait IntelEarlyPlatform: IntelPlatform {
    type Southbridge;
    type State: Default;
    /// Platform-owned ACPI namespace context handed to `AcpiDevice` emitters.
    #[cfg(feature = "acpi")]
    type AcpiContext;
}

/// Mainboard hooks contribute ACPI fragments through the same [`AcpiDevice`]
/// abstraction chipset drivers use. Vacuous when ACPI is disabled.
///
/// [`AcpiDevice`]: fstart_acpi::device::AcpiDevice
#[cfg(all(feature = "stage", feature = "acpi"))]
pub trait MainboardAcpi<P: IntelEarlyPlatform>:
    fstart_acpi::device::AcpiDevice<Config = P::AcpiContext>
{
}
#[cfg(all(feature = "stage", feature = "acpi"))]
impl<P, T> MainboardAcpi<P> for T
where
    P: IntelEarlyPlatform,
    T: fstart_acpi::device::AcpiDevice<Config = P::AcpiContext>,
{
}
#[cfg(all(feature = "stage", not(feature = "acpi")))]
pub trait MainboardAcpi<P> {}
#[cfg(all(feature = "stage", not(feature = "acpi")))]
impl<P, T> MainboardAcpi<P> for T {}

/// Mutable context passed to board hooks.
#[cfg(feature = "stage")]
pub struct IntelEarlyCtx<'a, P: IntelEarlyPlatform> {
    southbridge: &'a mut P::Southbridge,
}

#[cfg(feature = "stage")]
impl<'a, P: IntelEarlyPlatform> IntelEarlyCtx<'a, P> {
    pub(crate) fn new(southbridge: &'a mut P::Southbridge) -> Self {
        Self { southbridge }
    }

    #[must_use]
    pub fn southbridge(&mut self) -> &mut P::Southbridge {
        self.southbridge
    }
}

/// Board hooks at the fixed Intel early-flow seams.
#[cfg(feature = "stage")]
pub trait IntelEarlyBoardHooks<P: IntelEarlyPlatform>: MainboardAcpi<P> {
    fn before_console(&mut self, _ctx: &mut IntelEarlyCtx<P>) -> Result<(), ServiceError> {
        Ok(())
    }

    fn before_memory(&mut self, _ctx: &mut IntelEarlyCtx<P>) -> Result<(), ServiceError> {
        Ok(())
    }

    fn after_memory(&mut self, _ctx: &mut IntelEarlyCtx<P>) -> Result<(), ServiceError> {
        Ok(())
    }

    fn before_handoff(&mut self, _ctx: &mut IntelEarlyCtx<P>) -> Result<(), ServiceError> {
        Ok(())
    }
}

/// Shared mainstage state owned by the flow, not by board hooks or drivers.
///
/// Owns the authoritative [`E820State`]: memory detection populates it and
/// table emission carves reservations from it, so the payload hands the OS
/// a map that includes ACPI/SMBIOS regions. There is no global copy.
#[cfg(feature = "stage")]
pub struct MainstageCtx {
    e820: fstart_core::services::memory_detect::E820State,
    acpi_rsdp: Option<u64>,
    firmware_base: u64,
    firmware_size: usize,
}

#[cfg(feature = "stage")]
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

/// Phase-oriented contract for DRAM-backed mainstage flows.
#[cfg(feature = "stage")]
pub trait MainstagePhases: Sized {
    fn pre_bus_scan(&mut self) -> Result<(), ServiceError>;
    fn bus_scan(&mut self) -> Result<(), ServiceError>;
    fn init_devices(&mut self) -> Result<(), ServiceError>;
    fn emit_tables(&mut self) -> Result<(), ServiceError>;
    fn finalize(&mut self) -> Result<(), ServiceError>;
}
