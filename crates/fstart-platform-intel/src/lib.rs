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
use fstart_core::services::memory_detect::{E820Entry, MemoryDetector, MAX_E820_ENTRIES};
#[cfg(feature = "stage")]
use fstart_core::services::{MemoryController, ServiceError};
#[cfg(feature = "stage")]
pub use fstart_stage::{payload::MainstagePayload, StageBoard, StageKind};

#[cfg(feature = "stage")]
pub(crate) fn firmware_window(
    layout: fstart_core::IntelIfdFlashLayout,
) -> Result<(u64, usize), ServiceError> {
    let Some(bios) = layout.bios_region() else {
        return Err(ServiceError::NotInitialized);
    };
    if bios.size == 0 {
        return Err(ServiceError::NotInitialized);
    }
    let Some(base) = layout.base.checked_add(u64::from(bios.offset)) else {
        return Err(ServiceError::NotInitialized);
    };
    Ok((base, bios.size as usize))
}

#[cfg(feature = "stage")]
pub(crate) fn publish_e820(ctx: &mut MainstageCtx, count: usize, total: u64) {
    // SAFETY: single-threaded firmware init, stored once per stage.
    unsafe {
        fstart_core::services::memory_detect::e820_state_mut().store(ctx.e820(), count, total);
    }
    ctx.store_e820(count, total);
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
    run_mainstage_phase(platform, "pre_bus_scan", halt, || mainstage.pre_bus_scan());
    run_mainstage_phase(platform, "bus_scan", halt, || mainstage.bus_scan());
    run_mainstage_phase(platform, "init_devices", halt, || mainstage.init_devices());
    run_mainstage_phase(platform, "mount_boot_media", halt, || {
        fstart_platform_x86_64::enable_boot_media_rom_cache();
        boot_media.mount()?;
        mainstage.stage_local_init()
    });
    run_mainstage_phase(platform, "verify_boot_media", halt, || boot_media.verify());
    run_mainstage_phase(platform, "emit_tables", halt, || mainstage.emit_tables());
    run_mainstage_phase(platform, "finalize", halt, || mainstage.finalize());

    Payload::boot(mainstage)
}

#[cfg(feature = "stage")]
pub trait IntelNorthbridgeDriver:
    MemoryDetector + MemoryController + fstart_pci::PciRootBus + Sized
{
    type Config: 'static;

    fn new_from_config(config: &'static Self::Config) -> Result<Self, ServiceError>;
    fn config(&self) -> &'static Self::Config;
    fn pre_console_init(&mut self) -> Result<(), ServiceError>;
    fn early_init(&mut self) -> Result<(), ServiceError>;
    fn stage_local_init(&mut self) -> Result<(), ServiceError>;
}

#[cfg(feature = "stage")]
pub trait IntelEcamConfig {
    fn ecam_base(&self) -> u64;
}

#[cfg(feature = "stage")]
pub trait IntelSouthbridgeDriver: Sized {
    type Config: 'static;

    fn new_from_config(config: &'static Self::Config) -> Result<Self, ServiceError>;
    #[cfg(feature = "acpi")]
    fn config(&self) -> &'static Self::Config;
    fn pre_console_init(&mut self) -> Result<(), ServiceError>;
    fn early_init(&mut self) -> Result<(), ServiceError>;
    fn post_dram_init(&mut self) -> Result<(), ServiceError>;
    fn finalize_init(&mut self) -> Result<(), ServiceError>;
}

#[cfg(feature = "stage")]
pub(crate) struct BootblockSpec {
    pub platform: &'static str,
    pub next_stage: &'static str,
    pub ramstage_load_addr: u64,
    pub flash_layout: fstart_core::IntelIfdFlashLayout,
    pub console_config: fstart_driver_uart::ns16550::Ns16550Config,
    pub console_node: &'static str,
}

#[cfg(feature = "stage")]
pub trait IntelMainstageBoard<P, NB, SB>: IntelEarlyBoard<Platform = P>
where
    P: IntelEarlyPlatform<Southbridge = SB>,
    NB: IntelNorthbridgeDriver,
    SB: IntelSouthbridgeDriver,
{
    const NB_CONFIG: &'static NB::Config;
    const SB_CONFIG: &'static SB::Config;

    fn ifd_flash_layout() -> fstart_core::IntelIfdFlashLayout;
    fn console_config() -> fstart_driver_uart::ns16550::Ns16550Config;
    fn console_node() -> &'static str;
    fn platform_node() -> &'static str;

    #[cfg(feature = "mp")]
    fn init_mp() -> Result<(), ServiceError>;

    #[cfg(feature = "smbios")]
    fn smbios_desc() -> &'static crate::tables::SmbiosDesc<'static>;
}

/// Shared Intel mainstage machinery. Chipset modules supply only their driver
/// types, platform node names, and MP policy.
#[cfg(feature = "stage")]
pub struct IntelMainstage<P, B, NB, SB, AcpiContext>
where
    P: IntelEarlyPlatform<Southbridge = SB>,
    B: IntelMainstageBoard<P, NB, SB>,
    NB: IntelNorthbridgeDriver,
    SB: IntelSouthbridgeDriver,
{
    northbridge: NB,
    southbridge: SB,
    hooks: B::Hooks,
    console: fstart_driver_uart::ns16550::Ns16550,
    ctx: MainstageCtx,
    _platform: PhantomData<P>,
    _acpi: PhantomData<AcpiContext>,
}

#[cfg(feature = "stage")]
impl<P, B, NB, SB, AcpiContext> IntelMainstage<P, B, NB, SB, AcpiContext>
where
    P: IntelEarlyPlatform<Southbridge = SB>,
    B: IntelMainstageBoard<P, NB, SB>,
    NB: IntelNorthbridgeDriver,
    SB: IntelSouthbridgeDriver,
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

    #[cfg(feature = "acpi")]
    fn emit_acpi(&self) -> u64
    where
        P: IntelEarlyPlatform<Southbridge = SB, AcpiContext = AcpiContext>,
        AcpiContext: Default,
        NB: fstart_acpi::device::AcpiDevice<Config = <NB as IntelNorthbridgeDriver>::Config>,
        SB: fstart_acpi::device::AcpiDevice<Config = <SB as IntelSouthbridgeDriver>::Config>
            + fstart_acpi::platform::X86PlatformProvider,
        B::Hooks: fstart_acpi::device::AcpiDevice<Config = AcpiContext>,
    {
        let acpi_ctx = AcpiContext::default();
        emit_x86_acpi_tables(
            &self.northbridge,
            self.northbridge.config(),
            &self.southbridge,
            self.southbridge.config(),
            &self.hooks,
            &acpi_ctx,
        )
    }
}

#[cfg(all(feature = "stage", feature = "acpi"))]
impl<P, B, NB, SB, AcpiContext> IntelMainstageFlow for IntelMainstage<P, B, NB, SB, AcpiContext>
where
    P: IntelEarlyPlatform<Southbridge = SB, AcpiContext = AcpiContext>,
    B: IntelMainstageBoard<P, NB, SB>,
    NB: IntelNorthbridgeDriver
        + fstart_acpi::device::AcpiDevice<Config = <NB as IntelNorthbridgeDriver>::Config>,
    SB: IntelSouthbridgeDriver
        + fstart_acpi::device::AcpiDevice<Config = <SB as IntelSouthbridgeDriver>::Config>
        + fstart_acpi::platform::X86PlatformProvider,
    B::Hooks: fstart_acpi::device::AcpiDevice<Config = AcpiContext>,
    AcpiContext: Default,
{
    fn firmware_region(&self) -> (u64, usize) {
        self.ctx.firmware_region()
    }

    fn stage_local_init(&mut self) -> Result<(), ServiceError> {
        self.northbridge.stage_local_init()
    }
}

#[cfg(all(feature = "stage", not(feature = "acpi")))]
impl<P, B, NB, SB, AcpiContext> IntelMainstageFlow for IntelMainstage<P, B, NB, SB, AcpiContext>
where
    P: IntelEarlyPlatform<Southbridge = SB>,
    B: IntelMainstageBoard<P, NB, SB>,
    NB: IntelNorthbridgeDriver,
    SB: IntelSouthbridgeDriver,
{
    fn firmware_region(&self) -> (u64, usize) {
        self.ctx.firmware_region()
    }

    fn stage_local_init(&mut self) -> Result<(), ServiceError> {
        self.northbridge.stage_local_init()
    }
}

#[cfg(feature = "stage")]
fn bind_intel_mainstage<P, B, NB, SB, AcpiContext>(
) -> Result<IntelMainstage<P, B, NB, SB, AcpiContext>, ServiceError>
where
    P: IntelEarlyPlatform<Southbridge = SB>,
    B: IntelMainstageBoard<P, NB, SB>,
    NB: IntelNorthbridgeDriver,
    SB: IntelSouthbridgeDriver,
{
    let (firmware_base, firmware_size) = firmware_window(B::ifd_flash_layout())?;
    Ok(IntelMainstage {
        northbridge: NB::new_from_config(B::NB_CONFIG)?,
        southbridge: SB::new_from_config(B::SB_CONFIG)?,
        hooks: B::hooks()?,
        console: fstart_driver_uart::ns16550::Ns16550::new(B::console_config())
            .map_err(|_| ServiceError::HardwareError)?,
        ctx: MainstageCtx::new(firmware_base, firmware_size),
        _platform: PhantomData,
        _acpi: PhantomData,
    })
}

#[cfg(feature = "stage")]
fn pre_bus_scan_intel_mainstage<P, B, NB, SB, AcpiContext>(
    mainstage: &mut IntelMainstage<P, B, NB, SB, AcpiContext>,
) -> Result<(), ServiceError>
where
    P: IntelEarlyPlatform<Southbridge = SB>,
    B: IntelMainstageBoard<P, NB, SB>,
    NB: IntelNorthbridgeDriver,
    SB: IntelSouthbridgeDriver,
{
    intel_pre_bus_scan::<P, _, _, _>(
        B::platform_node(),
        B::console_node(),
        &mut mainstage.northbridge,
        &mut mainstage.southbridge,
        &mut mainstage.hooks,
        &mut mainstage.console,
        &mut mainstage.ctx,
    )
}

#[cfg(feature = "stage")]
fn init_intel_mainstage_devices<P, B, NB, SB, AcpiContext>(
    mainstage: &mut IntelMainstage<P, B, NB, SB, AcpiContext>,
) -> Result<(), ServiceError>
where
    P: IntelEarlyPlatform<Southbridge = SB>,
    B: IntelMainstageBoard<P, NB, SB>,
    NB: IntelNorthbridgeDriver,
    SB: IntelSouthbridgeDriver,
{
    intel_init_devices::<P, _, _>(&mut mainstage.southbridge, &mut mainstage.hooks, || {
        #[cfg(feature = "mp")]
        B::init_mp()?;
        Ok(())
    })
}

#[cfg(all(feature = "stage", feature = "acpi"))]
impl<P, B, NB, SB, AcpiContext> MainstagePhases for IntelMainstage<P, B, NB, SB, AcpiContext>
where
    P: IntelEarlyPlatform<Southbridge = SB, AcpiContext = AcpiContext>,
    B: IntelMainstageBoard<P, NB, SB>,
    NB: IntelNorthbridgeDriver
        + fstart_acpi::device::AcpiDevice<Config = <NB as IntelNorthbridgeDriver>::Config>,
    SB: IntelSouthbridgeDriver
        + fstart_acpi::device::AcpiDevice<Config = <SB as IntelSouthbridgeDriver>::Config>
        + fstart_acpi::platform::X86PlatformProvider,
    B::Hooks: fstart_acpi::device::AcpiDevice<Config = AcpiContext>,
    AcpiContext: Default,
{
    fn bind() -> Result<Self, ServiceError> {
        bind_intel_mainstage::<P, B, NB, SB, AcpiContext>()
    }

    fn pre_bus_scan(&mut self) -> Result<(), ServiceError> {
        pre_bus_scan_intel_mainstage::<P, B, NB, SB, AcpiContext>(self)
    }

    fn bus_scan(&mut self) -> Result<(), ServiceError> {
        intel_bus_scan(&mut self.northbridge)
    }

    fn init_devices(&mut self) -> Result<(), ServiceError> {
        init_intel_mainstage_devices::<P, B, NB, SB, AcpiContext>(self)
    }

    fn emit_tables(&mut self) -> Result<(), ServiceError> {
        let rsdp = self.emit_acpi();
        self.ctx.set_acpi_rsdp(Some(rsdp));
        #[cfg(feature = "smbios")]
        crate::tables::prepare_smbios(B::smbios_desc());
        Ok(())
    }

    fn finalize(&mut self) -> Result<(), ServiceError> {
        intel_finalize::<P, _, _>(&mut self.southbridge, &mut self.hooks)
    }
}

#[cfg(all(feature = "stage", not(feature = "acpi")))]
impl<P, B, NB, SB, AcpiContext> MainstagePhases for IntelMainstage<P, B, NB, SB, AcpiContext>
where
    P: IntelEarlyPlatform<Southbridge = SB>,
    B: IntelMainstageBoard<P, NB, SB>,
    NB: IntelNorthbridgeDriver,
    SB: IntelSouthbridgeDriver,
{
    fn bind() -> Result<Self, ServiceError> {
        bind_intel_mainstage::<P, B, NB, SB, AcpiContext>()
    }

    fn pre_bus_scan(&mut self) -> Result<(), ServiceError> {
        pre_bus_scan_intel_mainstage::<P, B, NB, SB, AcpiContext>(self)
    }

    fn bus_scan(&mut self) -> Result<(), ServiceError> {
        intel_bus_scan(&mut self.northbridge)
    }

    fn init_devices(&mut self) -> Result<(), ServiceError> {
        init_intel_mainstage_devices::<P, B, NB, SB, AcpiContext>(self)
    }

    fn emit_tables(&mut self) -> Result<(), ServiceError> {
        #[cfg(feature = "smbios")]
        crate::tables::prepare_smbios(B::smbios_desc());
        Ok(())
    }

    fn finalize(&mut self) -> Result<(), ServiceError> {
        intel_finalize::<P, _, _>(&mut self.southbridge, &mut self.hooks)
    }
}

#[cfg(feature = "stage")]
impl<P, B, NB, SB, AcpiContext> fstart_stage::payload::X86UefiPayloadContext
    for IntelMainstage<P, B, NB, SB, AcpiContext>
where
    P: IntelEarlyPlatform<Southbridge = SB>,
    B: IntelMainstageBoard<P, NB, SB>,
    NB: IntelNorthbridgeDriver,
    NB::Config: IntelEcamConfig,
    SB: IntelSouthbridgeDriver,
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
pub(crate) fn run_intel_bootblock<P, NB, SB, Hooks>(
    spec: BootblockSpec,
    hooks: &mut Hooks,
    mut northbridge: NB,
    mut southbridge: SB,
) -> Result<(), ServiceError>
where
    P: IntelEarlyPlatform<Southbridge = SB>,
    NB: IntelNorthbridgeDriver,
    SB: IntelSouthbridgeDriver,
    Hooks: IntelEarlyBoardHooks<P>,
{
    let (firmware_base, firmware_size) = firmware_window(spec.flash_layout)?;
    let ffs = fstart_stage::fixed_helpers::MemoryMappedFfs::new(firmware_base, firmware_size);

    northbridge.pre_console_init()?;
    southbridge.pre_console_init()?;
    hooks.before_console(&mut IntelEarlyCtx::new(&mut southbridge))?;

    let mut console = fstart_driver_uart::ns16550::Ns16550::new(spec.console_config)
        .map_err(|_| ServiceError::HardwareError)?;
    console.init().map_err(|_| ServiceError::HardwareError)?;
    // SAFETY: this function never returns after installing the stack-owned console.
    unsafe { fstart_log::init(&console) };
    fstart_log::info!("{}: ns16550 console ready", spec.console_node);
    fstart_log::info!("{} bootblock console ready", spec.platform);

    fstart_platform_x86_64::enable_boot_media_rom_cache();
    if ffs.mount().is_err()
        || ffs.verify().is_err()
        || ffs.load_file_by_name(spec.next_stage).is_err()
    {
        fstart_log::error!("{} bootblock failed", spec.platform);
        return Err(ServiceError::HardwareError);
    }

    fstart_log::info!("jumping to ramstage at {:#x}", spec.ramstage_load_addr);
    fstart_platform_x86_64::jump_to(spec.ramstage_load_addr)
}

#[cfg(feature = "stage")]
pub(crate) fn intel_pre_bus_scan<P, NB, SB, Hooks>(
    platform_node: &str,
    console_node: &str,
    northbridge: &mut NB,
    southbridge: &mut SB,
    hooks: &mut Hooks,
    console: &mut fstart_driver_uart::ns16550::Ns16550,
    ctx: &mut MainstageCtx,
) -> Result<(), ServiceError>
where
    P: IntelEarlyPlatform<Southbridge = SB>,
    NB: IntelNorthbridgeDriver,
    SB: IntelSouthbridgeDriver,
    Hooks: IntelEarlyBoardHooks<P>,
{
    northbridge.pre_console_init()?;
    southbridge.pre_console_init()?;
    hooks.before_console(&mut IntelEarlyCtx::new(southbridge))?;

    console.init().map_err(|_| ServiceError::HardwareError)?;
    // SAFETY: the mainstage owns the console until it hands control to the payload.
    unsafe { fstart_log::init(console) };
    fstart_log::info!("fstart ramstage console ready");
    fstart_log::info!("{}: ns16550 console ready", console_node);

    northbridge.early_init()?;
    southbridge.early_init()?;

    hooks.before_memory(&mut IntelEarlyCtx::new(southbridge))?;
    let count = northbridge.detect_memory(ctx.e820_mut())?;
    let total = northbridge.total_ram_bytes()?;
    fstart_log::info!(
        "Detected {} MiB RAM, {} e820 entries from {}",
        total >> 20,
        count,
        platform_node,
    );
    publish_e820(ctx, count, total);

    northbridge.dram_init()
}

#[cfg(feature = "stage")]
pub(crate) fn intel_bus_scan<NB>(northbridge: &mut NB) -> Result<(), ServiceError>
where
    NB: fstart_pci::PciRootBus,
{
    northbridge.init_bus()
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
    crate::tables::prepare_acpi(platform, |dsdt, extra| {
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
#[cfg(feature = "stage")]
pub struct MainstageCtx {
    e820: [E820Entry; MAX_E820_ENTRIES],
    e820_count: usize,
    total_ram: u64,
    acpi_rsdp: Option<u64>,
    firmware_base: u64,
    firmware_size: usize,
}

#[cfg(feature = "stage")]
impl MainstageCtx {
    pub(crate) fn new(firmware_base: u64, firmware_size: usize) -> Self {
        Self {
            e820: [E820Entry::zeroed(); MAX_E820_ENTRIES],
            e820_count: 0,
            total_ram: 0,
            acpi_rsdp: None,
            firmware_base,
            firmware_size,
        }
    }

    #[must_use]
    pub fn e820(&self) -> &[E820Entry] {
        &self.e820[..self.e820_count]
    }

    #[must_use]
    pub const fn total_ram(&self) -> u64 {
        self.total_ram
    }

    #[must_use]
    pub const fn acpi_rsdp(&self) -> Option<u64> {
        self.acpi_rsdp
    }

    #[must_use]
    pub const fn firmware_region(&self) -> (u64, usize) {
        (self.firmware_base, self.firmware_size)
    }

    pub(crate) fn e820_mut(&mut self) -> &mut [E820Entry; MAX_E820_ENTRIES] {
        &mut self.e820
    }

    pub(crate) fn store_e820(&mut self, count: usize, total: u64) {
        self.e820_count = count;
        self.total_ram = total;
    }

    #[cfg(feature = "acpi")]
    pub(crate) fn set_acpi_rsdp(&mut self, rsdp: Option<u64>) {
        self.acpi_rsdp = rsdp;
    }
}

/// Phase-oriented contract for DRAM-backed mainstage flows.
#[cfg(feature = "stage")]
pub trait MainstagePhases: Sized {
    fn bind() -> Result<Self, ServiceError>;
    fn pre_bus_scan(&mut self) -> Result<(), ServiceError>;
    fn bus_scan(&mut self) -> Result<(), ServiceError>;
    fn init_devices(&mut self) -> Result<(), ServiceError>;
    fn emit_tables(&mut self) -> Result<(), ServiceError>;
    fn finalize(&mut self) -> Result<(), ServiceError>;
}
