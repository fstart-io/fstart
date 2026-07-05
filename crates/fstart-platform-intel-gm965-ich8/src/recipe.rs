//! Reusable GM965/ICH8 fixed-flow stage recipe.

use core::marker::PhantomData;

use fstart_driver_intel_gm965::IntelGm965;
use fstart_driver_intel_ich8::IntelIch8;
use fstart_driver_ns16550::{Ns16550, Ns16550Config};
use fstart_services::memory_detect::{E820Entry, MAX_E820_ENTRIES};
#[cfg(feature = "crabefi")]
use fstart_services::StageLocalInit;
use fstart_services::{
    Device, DeviceError, EarlyInit, MemoryController, PciRootBus, PreConsoleInit, ServiceError,
    Southbridge,
};
#[cfg(feature = "crabefi")]
use fstart_stage::crabefi::{MemoryRegion, MemoryType, UefiLaunchConfig};
#[cfg(feature = "crabefi")]
use fstart_stage::fixed_helpers::MemoryMappedUefiBoot;
use fstart_stage::fixed_helpers::{console_ready, MemoryMappedFfs};
use fstart_stage::{FirmwareBoard, StageKind, StageRecipe};

use crate::{
    Gm965Ich8AcpiContext, Gm965Ich8Config, GM965_NEXT_STAGE_NAME, GM965_NORTHBRIDGE_NODE,
    GM965_RAMSTAGE_LOAD_ADDR,
};

/// Platform recipe selected by GM965/ICH8 fixed-flow board crates.
pub struct Gm965Ich8Recipe<B>(PhantomData<B>);

impl<B> StageRecipe<B> for Gm965Ich8Recipe<B>
where
    B: Gm965Ich8StageBoard,
{
    fn run(stage: StageKind, _handoff: usize) -> ! {
        if stage.is_named("bootblock") {
            run_gm965_ich8_bootblock::<B>()
        } else if stage.is_named(GM965_NEXT_STAGE_NAME) {
            #[cfg(feature = "crabefi")]
            {
                run_gm965_ich8_ramstage::<B>()
            }
            #[cfg(not(feature = "crabefi"))]
            {
                B::halt()
            }
        } else {
            B::halt()
        }
    }
}

/// Board facts and hooks required by the GM965/ICH8 recipe.
pub trait Gm965Ich8StageBoard: FirmwareBoard<Recipe = Gm965Ich8Recipe<Self>> {
    type Mainboard: Gm965Ich8Mainboard;

    fn config() -> &'static Gm965Ich8Config;
    fn mainboard() -> Result<Self::Mainboard, ServiceError>;
    fn console_config() -> Ns16550Config;
    fn console_node() -> &'static str;
    fn halt() -> !;

    fn init_mp() -> Result<(), ServiceError> {
        Ok(())
    }

    fn prepare_acpi(_devices: &mut Gm965Ich8RamstageDevices<Self>) -> Option<u64> {
        None
    }

    fn prepare_smbios() {}
}

/// Board-specific hooks. Generic framework code never learns these names.
pub trait Gm965Ich8Mainboard {
    fn before_console(&mut self, _ich8: &mut IntelIch8) -> Result<(), ServiceError> {
        Ok(())
    }

    fn post_dram(&mut self, _ich8: &mut IntelIch8) -> Result<(), ServiceError> {
        Ok(())
    }

    fn finalize(&mut self, _ich8: &mut IntelIch8) -> Result<(), ServiceError> {
        Ok(())
    }
}

fn device_error_to_service_error(_err: DeviceError) -> ServiceError {
    ServiceError::HardwareError
}

fn new_gm965<B: Gm965Ich8StageBoard>() -> Result<IntelGm965, ServiceError> {
    IntelGm965::new(B::config().gm965_driver_config()).map_err(device_error_to_service_error)
}

fn new_ich8<B: Gm965Ich8StageBoard>() -> Result<IntelIch8, ServiceError> {
    IntelIch8::new(B::config().ich8_driver_config()).map_err(device_error_to_service_error)
}

fn run_gm965_ich8_bootblock<B>() -> !
where
    B: Gm965Ich8StageBoard,
{
    let config = B::config();
    let Ok(mut northbridge) = new_gm965::<B>() else {
        B::halt();
    };
    let Ok(mut southbridge) = new_ich8::<B>() else {
        B::halt();
    };
    let Ok(mut mainboard) = B::mainboard() else {
        B::halt();
    };
    let ffs = MemoryMappedFfs::new(config.firmware_base, config.firmware_size);

    if northbridge.pre_console_init().is_err()
        || PreConsoleInit::pre_console_init(&mut southbridge).is_err()
        || mainboard.before_console(&mut southbridge).is_err()
    {
        B::halt();
    }

    let Ok(mut console) = Ns16550::new(B::console_config()).map_err(device_error_to_service_error)
    else {
        B::halt();
    };
    if console.init().is_err() {
        B::halt();
    }
    // SAFETY: this function never returns after installing the stack-owned console.
    let console_ref = &console;
    unsafe { fstart_log::init(console_ref) };
    console_ready(B::console_node(), "ns16550");
    fstart_log::info!("gm965/ich8 bootblock console ready");

    fstart_platform_x86_64::enable_boot_media_rom_cache();
    if ffs.mount().is_err()
        || ffs.verify().is_err()
        || ffs.load_file_by_name(GM965_NEXT_STAGE_NAME).is_err()
    {
        fstart_log::error!("gm965/ich8 bootblock failed");
        B::halt();
    }

    fstart_log::info!("jumping to ramstage at {:#x}", GM965_RAMSTAGE_LOAD_ADDR);
    fstart_platform_x86_64::jump_to(GM965_RAMSTAGE_LOAD_ADDR)
}

/// Runtime device set owned by the GM965/ICH8 ramstage recipe.
pub struct Gm965Ich8RamstageDevices<B: Gm965Ich8StageBoard> {
    northbridge: IntelGm965,
    southbridge: IntelIch8,
    mainboard: B::Mainboard,
    console: Ns16550,
    e820: [E820Entry; MAX_E820_ENTRIES],
    e820_count: usize,
    total_ram: u64,
    acpi_rsdp: Option<u64>,
}

#[cfg_attr(not(feature = "crabefi"), allow(dead_code))]
impl<B> Gm965Ich8RamstageDevices<B>
where
    B: Gm965Ich8StageBoard,
{
    fn new() -> Result<Self, ServiceError> {
        Ok(Self {
            northbridge: new_gm965::<B>()?,
            southbridge: new_ich8::<B>()?,
            mainboard: B::mainboard()?,
            console: Ns16550::new(B::console_config()).map_err(device_error_to_service_error)?,
            e820: [E820Entry::zeroed(); MAX_E820_ENTRIES],
            e820_count: 0,
            total_ram: 0,
            acpi_rsdp: None,
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

    fn e820(&self) -> &[E820Entry] {
        &self.e820[..self.e820_count]
    }

    fn acpi_rsdp(&self) -> Option<u64> {
        self.acpi_rsdp
    }

    fn pre_console(&mut self) -> Result<(), ServiceError> {
        self.northbridge.pre_console_init()?;
        PreConsoleInit::pre_console_init(&mut self.southbridge)?;
        self.mainboard.before_console(&mut self.southbridge)
    }

    fn init_console(&mut self) -> Result<(), ServiceError> {
        self.console.init().map_err(device_error_to_service_error)?;
        // SAFETY: ramstage owns the console until it hands control to CrabEFI.
        unsafe { fstart_log::init(&self.console) };
        fstart_log::info!("fstart ramstage console ready");
        console_ready(B::console_node(), "ns16550");
        Ok(())
    }

    fn post_console(&mut self) -> Result<(), ServiceError> {
        self.northbridge.early_init()?;
        EarlyInit::early_init(&mut self.southbridge)
    }

    fn memory_discovery(&mut self) -> Result<(), ServiceError> {
        let (count, total) = fstart_capabilities::memory_detect(
            &self.northbridge,
            &mut self.e820,
            GM965_NORTHBRIDGE_NODE,
        )?;
        self.e820_count = count;
        self.total_ram = total;
        Ok(())
    }

    fn dram(&mut self) -> Result<(), ServiceError> {
        self.northbridge.dram_init()
    }

    fn bus_probe(&mut self) -> Result<(), ServiceError> {
        self.northbridge.init_bus()?;
        self.southbridge.ramstage_init()?;
        self.mainboard.post_dram(&mut self.southbridge)?;
        B::init_mp()?;
        fstart_capabilities::driver_init_complete(4);
        Ok(())
    }

    fn handoff(&mut self) -> Result<(), ServiceError> {
        self.acpi_rsdp = B::prepare_acpi(self);
        B::prepare_smbios();
        Ok(())
    }
}

#[cfg(feature = "crabefi")]
fn run_gm965_ich8_ramstage<B>() -> !
where
    B: Gm965Ich8StageBoard,
{
    let mut devices = match Gm965Ich8RamstageDevices::<B>::new() {
        Ok(devices) => devices,
        Err(_) => B::halt(),
    };
    let config = B::config();
    let boot = MemoryMappedUefiBoot::new(config.firmware_base, config.firmware_size, 0);
    run_ramstage_step::<B>("pre-console", || devices.pre_console());
    run_ramstage_step::<B>("console", || devices.init_console());
    run_ramstage_step::<B>("post-console", || devices.post_console());
    run_ramstage_step::<B>("memory-discovery", || devices.memory_discovery());
    run_ramstage_step::<B>("dram", || devices.dram());
    run_ramstage_step::<B>("bus-probe", || devices.bus_probe());
    run_ramstage_step::<B>("storage", || {
        fstart_platform_x86_64::enable_boot_media_rom_cache();
        boot.mount()?;
        devices.northbridge.stage_local_init()
    });
    run_ramstage_step::<B>("security", || boot.verify());
    run_ramstage_step::<B>("handoff", || {
        devices.handoff()?;
        devices.mainboard.finalize(&mut devices.southbridge)?;
        devices.southbridge.finalize()
    });

    boot_gm965_ich8_payload::<B>(devices)
}

#[cfg(feature = "crabefi")]
fn run_ramstage_step<B>(name: &str, step: impl FnOnce() -> Result<(), ServiceError>)
where
    B: Gm965Ich8StageBoard,
{
    fstart_log::info!("gm965/ich8 ramstage: {}", name);
    if step().is_err() {
        fstart_log::error!("gm965/ich8 ramstage: {} failed", name);
        B::halt();
    }
}

#[cfg(feature = "crabefi")]
fn boot_gm965_ich8_payload<B>(devices: Gm965Ich8RamstageDevices<B>) -> !
where
    B: Gm965Ich8StageBoard,
{
    let console = &devices.console;
    let acpi_base = devices.acpi_rsdp().unwrap_or(0) & !0xfff;
    let config = B::config();
    let platform_entries = [
        MemoryRegion {
            base: config.firmware_base,
            size: config.firmware_size as u64,
            region_type: MemoryType::RuntimeServicesCode,
        },
        MemoryRegion {
            base: acpi_base,
            size: 0x10000,
            region_type: MemoryType::AcpiReclaimable,
        },
    ];

    fstart_log::info!(
        "launching CrabEFI: ram={} MiB, rsdp={:#x}, ecam={:#x}",
        (devices.total_ram >> 20) as u32,
        devices.acpi_rsdp().unwrap_or(0),
        devices.northbridge.config().ecam_base,
    );
    fstart_stage::crabefi::launch_x86_uefi(
        UefiLaunchConfig {
            console: Some(console),
            framebuffer: None,
            acpi_rsdp: devices.acpi_rsdp(),
            smbios: None,
            fdt: None,
            ecam_base: Some(devices.northbridge.config().ecam_base),
            runtime_region: Some(fstart_stage::crabefi::compute_runtime_region()),
        },
        devices.e820(),
        &platform_entries,
    )
}
