//! Fixed GM965/ICH8 Intel early/mainstage flow.

use fstart_driver_intel_gm965::IntelGm965;
use fstart_driver_intel_ich8::IntelIch8;
use fstart_driver_ns16550::{Ns16550, Ns16550Config};
use fstart_services::memory_detect::{E820Entry, MemoryDetector, MAX_E820_ENTRIES};
use fstart_services::{MemoryController, PciRootBus, ServiceError};
use fstart_stage::fixed_helpers::MemoryMappedFfs;
use fstart_stage::payload::MainstagePayload;
#[cfg(feature = "crabefi")]
use fstart_stage::payload::X86UefiPayloadContext;
use fstart_stage::{StageBoard, StageKind};

use crate::{
    Gm965Ich8AcpiContext, Gm965Ich8Config, GM965_NEXT_STAGE_NAME, GM965_NORTHBRIDGE_NODE,
    GM965_RAMSTAGE_LOAD_ADDR,
};

/// Marker for Intel platform families with handwritten early flows.
pub trait IntelPlatform {}

/// Board contract common to Intel handwritten early flows.
pub trait IntelEarlyBoard: StageBoard {
    type Platform: IntelEarlyPlatform;
    type Hooks: IntelEarlyBoardHooks<Self::Platform>;

    fn hooks() -> Result<Self::Hooks, ServiceError>;
}

/// Fixed Intel early-flow platform contract.
pub trait IntelEarlyPlatform: IntelPlatform {
    type Southbridge;
    type State: Default;
}

/// Mutable context passed to board hooks.
pub struct IntelEarlyCtx<'a, P: IntelEarlyPlatform> {
    southbridge: &'a mut P::Southbridge,
}

impl<'a, P: IntelEarlyPlatform> IntelEarlyCtx<'a, P> {
    fn new(southbridge: &'a mut P::Southbridge) -> Self {
        Self { southbridge }
    }

    #[must_use]
    pub fn southbridge(&mut self) -> &mut P::Southbridge {
        self.southbridge
    }
}

/// Board hooks at the fixed Intel early-flow seams.
pub trait IntelEarlyBoardHooks<P: IntelEarlyPlatform> {
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

/// GM965 northbridge + ICH8 southbridge Intel early-flow platform.
pub struct Gm965Ich8;

impl IntelPlatform for Gm965Ich8 {}

impl IntelEarlyPlatform for Gm965Ich8 {
    type Southbridge = IntelIch8;
    type State = ();
}

impl Gm965Ich8 {
    pub fn run_early<B>(hooks: &mut B::Hooks) -> Result<(), ServiceError>
    where
        B: Gm965Ich8Board,
    {
        run_gm965_ich8_bootblock::<B>(hooks)
    }

    pub fn run_stage<B>(stage: StageKind, _handoff: usize) -> !
    where
        B: Gm965Ich8Board,
    {
        if stage.is_named("bootblock") {
            let Ok(mut hooks) = B::hooks() else {
                B::halt();
            };
            if Self::run_early::<B>(&mut hooks).is_err() {
                B::halt();
            }
            B::halt()
        } else if stage.is_named(GM965_NEXT_STAGE_NAME) {
            run_gm965_ich8_mainstage::<B>()
        } else {
            B::halt()
        }
    }
}

/// Board facts required by the GM965/ICH8 flow.
pub trait Gm965Ich8Board: IntelEarlyBoard<Platform = Gm965Ich8> {
    type Payload: MainstagePayload<Gm965Ich8Mainstage<Self>>;

    fn config() -> &'static Gm965Ich8Config;
    fn ifd_flash_layout() -> fstart_types::IntelIfdFlashLayout;
    fn console_config() -> Ns16550Config;
    fn console_node() -> &'static str;
    fn halt() -> !;

    fn init_mp() -> Result<(), ServiceError> {
        Ok(())
    }

    fn prepare_acpi(_mainstage: &mut Gm965Ich8Mainstage<Self>) -> Option<u64> {
        None
    }

    fn prepare_smbios() {}
}

fn firmware_window<B>() -> Result<(u64, usize), ServiceError>
where
    B: Gm965Ich8Board,
{
    let layout = B::ifd_flash_layout();
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

/// Handwritten fixed GM965/ICH8 bootblock flow. Ordering is this function.
fn run_gm965_ich8_bootblock<B>(hooks: &mut B::Hooks) -> Result<(), ServiceError>
where
    B: Gm965Ich8Board,
{
    let config = B::config();
    let Ok((firmware_base, firmware_size)) = firmware_window::<B>() else {
        return Err(ServiceError::NotInitialized);
    };
    let Ok(mut northbridge) = IntelGm965::new(config.northbridge_config()) else {
        return Err(ServiceError::HardwareError);
    };
    let Ok(mut southbridge) = IntelIch8::new(config.southbridge_config()) else {
        return Err(ServiceError::HardwareError);
    };
    let ffs = MemoryMappedFfs::new(firmware_base, firmware_size);

    if northbridge.pre_console_init().is_err()
        || southbridge.pre_console_init().is_err()
        || hooks
            .before_console(&mut IntelEarlyCtx::new(&mut southbridge))
            .is_err()
    {
        return Err(ServiceError::HardwareError);
    }

    let Ok(mut console) = Ns16550::new(B::console_config()) else {
        return Err(ServiceError::HardwareError);
    };
    if console.init().is_err() {
        return Err(ServiceError::HardwareError);
    }
    // SAFETY: this function never returns after installing the stack-owned console.
    let console_ref = &console;
    unsafe { fstart_log::init(console_ref) };
    fstart_log::info!("{}: ns16550 console ready", B::console_node());
    fstart_log::info!("gm965/ich8 bootblock console ready");

    fstart_platform_x86_64::enable_boot_media_rom_cache();
    if ffs.mount().is_err()
        || ffs.verify().is_err()
        || ffs.load_file_by_name(GM965_NEXT_STAGE_NAME).is_err()
    {
        fstart_log::error!("gm965/ich8 bootblock failed");
        return Err(ServiceError::HardwareError);
    }

    fstart_log::info!("jumping to ramstage at {:#x}", GM965_RAMSTAGE_LOAD_ADDR);
    fstart_platform_x86_64::jump_to(GM965_RAMSTAGE_LOAD_ADDR)
}

/// GM965/ICH8 mainstage: fixed platform devices bound from typed config and
/// driven through explicit handwritten phases.
pub struct Gm965Ich8Mainstage<B: Gm965Ich8Board> {
    northbridge: IntelGm965,
    southbridge: IntelIch8,
    hooks: B::Hooks,
    console: Ns16550,
    e820: [E820Entry; MAX_E820_ENTRIES],
    e820_count: usize,
    total_ram: u64,
    acpi_rsdp: Option<u64>,
    firmware_base: u64,
    firmware_size: usize,
}

impl<B> Gm965Ich8Mainstage<B>
where
    B: Gm965Ich8Board,
{
    /// Bind fixed platform devices from the board's typed config. No hardware
    /// is touched; construction failures are config errors.
    fn bind() -> Result<Self, ServiceError> {
        let config = B::config();
        let (firmware_base, firmware_size) = firmware_window::<B>()?;
        Ok(Self {
            northbridge: IntelGm965::new(config.northbridge_config())
                .map_err(|_| ServiceError::HardwareError)?,
            southbridge: IntelIch8::new(config.southbridge_config())
                .map_err(|_| ServiceError::HardwareError)?,
            hooks: B::hooks()?,
            console: Ns16550::new(B::console_config()).map_err(|_| ServiceError::HardwareError)?,
            e820: [E820Entry::zeroed(); MAX_E820_ENTRIES],
            e820_count: 0,
            total_ram: 0,
            acpi_rsdp: None,
            firmware_base,
            firmware_size,
        })
    }

    #[must_use]
    pub const fn northbridge(&self) -> &IntelGm965 {
        &self.northbridge
    }

    #[must_use]
    pub const fn southbridge(&self) -> &IntelIch8 {
        &self.southbridge
    }

    #[must_use]
    pub const fn acpi_context(&self) -> Gm965Ich8AcpiContext {
        Gm965Ich8AcpiContext
    }

    #[must_use]
    pub fn e820(&self) -> &[E820Entry] {
        &self.e820[..self.e820_count]
    }

    #[must_use]
    pub const fn acpi_rsdp(&self) -> Option<u64> {
        self.acpi_rsdp
    }

    /// Enable bridges/decode, bring up the console, and get DRAM usable —
    /// everything that must happen before PCI enumeration.
    fn pre_bus_scan(&mut self) -> Result<(), ServiceError> {
        self.northbridge.pre_console_init()?;
        self.southbridge.pre_console_init()?;
        self.hooks
            .before_console(&mut IntelEarlyCtx::new(&mut self.southbridge))?;

        self.console
            .init()
            .map_err(|_| ServiceError::HardwareError)?;
        // SAFETY: the mainstage owns the console until it hands control to
        // the payload.
        unsafe { fstart_log::init(&self.console) };
        fstart_log::info!("fstart ramstage console ready");
        fstart_log::info!("{}: ns16550 console ready", B::console_node());

        self.northbridge.early_init()?;
        self.southbridge.early_init()?;

        self.hooks
            .before_memory(&mut IntelEarlyCtx::new(&mut self.southbridge))?;
        let count = self.northbridge.detect_memory(&mut self.e820)?;
        let total = self.northbridge.total_ram_bytes()?;
        fstart_log::info!(
            "Detected {} MiB RAM, {} e820 entries from {}",
            total >> 20,
            count,
            GM965_NORTHBRIDGE_NODE,
        );
        // Publish the e820 map so PCI window allocation and table emission
        // can read it.
        // SAFETY: single-threaded firmware init, stored once per stage.
        unsafe {
            fstart_services::memory_detect::e820_state_mut().store(&self.e820, count, total);
        }
        self.e820_count = count;
        self.total_ram = total;

        self.northbridge.dram_init()
    }

    /// Enumerate PCI. BAR assignment and window confirmation happen in the
    /// same ECAM pass as the scan.
    fn bus_scan(&mut self) -> Result<(), ServiceError> {
        self.northbridge.init_bus()
    }

    /// Initialize the devices the selected boot mode needs: southbridge
    /// post-DRAM functions, board-attached devices, and APs.
    fn init_devices(&mut self) -> Result<(), ServiceError> {
        self.southbridge.post_dram_init()?;
        self.hooks
            .after_memory(&mut IntelEarlyCtx::new(&mut self.southbridge))?;
        B::init_mp()
    }

    /// Emit ACPI/SMBIOS tables from the existing board + platform code.
    fn emit_tables(&mut self) -> Result<(), ServiceError> {
        self.acpi_rsdp = B::prepare_acpi(self);
        B::prepare_smbios();
        Ok(())
    }

    /// Board lockdown and southbridge quiesce before payload handoff.
    fn finalize(&mut self) -> Result<(), ServiceError> {
        self.hooks
            .before_handoff(&mut IntelEarlyCtx::new(&mut self.southbridge))?;
        self.southbridge.finalize_init()
    }
}

/// Handwritten fixed GM965/ICH8 mainstage flow. Ordering is this function.
pub fn run_gm965_ich8_mainstage<B>() -> !
where
    B: Gm965Ich8Board,
{
    let Ok(mut mainstage) = Gm965Ich8Mainstage::<B>::bind() else {
        B::halt();
    };
    let boot_media = MemoryMappedFfs::new(mainstage.firmware_base, mainstage.firmware_size);
    run_mainstage_phase::<B>("pre_bus_scan", || mainstage.pre_bus_scan());
    run_mainstage_phase::<B>("bus_scan", || mainstage.bus_scan());
    run_mainstage_phase::<B>("init_devices", || mainstage.init_devices());
    run_mainstage_phase::<B>("mount_boot_media", || {
        fstart_platform_x86_64::enable_boot_media_rom_cache();
        boot_media.mount()?;
        mainstage.northbridge.stage_local_init()
    });
    run_mainstage_phase::<B>("verify_boot_media", || boot_media.verify());
    run_mainstage_phase::<B>("emit_tables", || mainstage.emit_tables());
    run_mainstage_phase::<B>("finalize", || mainstage.finalize());

    B::Payload::boot(mainstage)
}

fn run_mainstage_phase<B>(name: &str, phase: impl FnOnce() -> Result<(), ServiceError>)
where
    B: Gm965Ich8Board,
{
    fstart_log::info!("gm965/ich8 mainstage: {}", name);
    if phase().is_err() {
        fstart_log::error!("gm965/ich8 mainstage: {} failed", name);
        B::halt();
    }
}

#[cfg(feature = "crabefi")]
impl<B> X86UefiPayloadContext for Gm965Ich8Mainstage<B>
where
    B: Gm965Ich8Board,
{
    fn console(&self) -> Option<&dyn fstart_services::Console> {
        Some(&self.console)
    }

    fn e820(&self) -> &[E820Entry] {
        self.e820()
    }

    fn firmware_region(&self) -> (u64, u64) {
        (self.firmware_base, self.firmware_size as u64)
    }

    fn acpi_rsdp(&self) -> Option<u64> {
        self.acpi_rsdp()
    }

    fn ecam_base(&self) -> Option<u64> {
        Some(self.northbridge.config().ecam_base)
    }
}
