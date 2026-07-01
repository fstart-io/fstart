//! Reusable GM965/ICH8 UEFI-style stage recipe.

use core::marker::PhantomData;

use fstart_driver_intel_gm965::IntelGm965;
use fstart_driver_intel_ich8::IntelIch8;
use fstart_driver_ns16550::{Ns16550, Ns16550Config};
use fstart_services::memory_detect::{E820Entry, MAX_E820_ENTRIES};
#[cfg(feature = "crabefi")]
use fstart_services::StageLocalInit;
use fstart_services::{
    Device, DeviceError, HardwareInit, InitContext, PciRootBus, ServiceError, Southbridge,
};
#[cfg(feature = "crabefi")]
use fstart_stage::crabefi::{MemoryRegion, MemoryType, UefiLaunchConfig};
#[cfg(feature = "crabefi")]
use fstart_stage::fixed_helpers::MemoryMappedUefiBoot;
use fstart_stage::fixed_helpers::{console_ready, MemoryMappedFfs, StaticConsole};
use fstart_stage::{FirmwareBoard, StageKind, StageRecipe};
use fstart_stage_runtime::StageFlow;

use crate::{
    Gm965Ich8AcpiContext, Gm965Ich8Board, GM965_NEXT_STAGE_NAME, GM965_NORTHBRIDGE_NODE,
    GM965_RAMSTAGE_LOAD_ADDR,
};

/// Platform recipe selected by GM965/ICH8 UEFI-style board crates.
pub struct Gm965Ich8UefiRecipe<B>(PhantomData<B>);

impl<B> StageRecipe<B> for Gm965Ich8UefiRecipe<B>
where
    B: Gm965Ich8UefiBoard,
{
    fn run(stage: StageKind, _handoff: usize) -> ! {
        if stage.is_named("bootblock") {
            fstart_stage::run_stage_flow::<Gm965Ich8BootblockStage<B>>()
        } else if stage.is_named(GM965_NEXT_STAGE_NAME) {
            #[cfg(feature = "crabefi")]
            {
                fstart_stage::run_stage_flow::<Gm965Ich8Ramstage<B>>()
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

/// Board facts and hooks required by the GM965/ICH8 UEFI recipe.
pub trait Gm965Ich8UefiBoard: FirmwareBoard<Recipe = Gm965Ich8UefiRecipe<Self>> {
    type Mainboard: Gm965Ich8Mainboard;

    fn board() -> Gm965Ich8Board;
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
    fn pre_console(&mut self, _ich8: &mut IntelIch8) -> Result<(), ServiceError> {
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

fn new_gm965<B: Gm965Ich8UefiBoard>() -> Result<IntelGm965, ServiceError> {
    IntelGm965::new(B::board().northbridge.config).map_err(device_error_to_service_error)
}

fn new_ich8<B: Gm965Ich8UefiBoard>() -> Result<IntelIch8, ServiceError> {
    IntelIch8::new(B::board().southbridge.driver_config()).map_err(device_error_to_service_error)
}

pub struct Gm965Ich8BootblockDevices<B: Gm965Ich8UefiBoard> {
    northbridge: IntelGm965,
    southbridge: IntelIch8,
    mainboard: B::Mainboard,
    console: StaticConsole<Ns16550>,
}

impl<B> Gm965Ich8BootblockDevices<B>
where
    B: Gm965Ich8UefiBoard,
{
    fn new() -> Result<Self, ServiceError> {
        Ok(Self {
            northbridge: new_gm965::<B>()?,
            southbridge: new_ich8::<B>()?,
            mainboard: B::mainboard()?,
            console: StaticConsole::new(B::console_config()),
        })
    }
}

impl<B> HardwareInit for Gm965Ich8BootblockDevices<B>
where
    B: Gm965Ich8UefiBoard,
{
    fn pre_console(&mut self, ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        self.northbridge.pre_console(ctx)?;
        self.southbridge.pre_console(ctx)?;
        self.mainboard.pre_console(&mut self.southbridge)
    }

    fn console(&mut self, ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        self.console.console(ctx)
    }
}

/// GM965/ICH8 bootblock fixed-flow stage.
pub struct Gm965Ich8BootblockStage<B: Gm965Ich8UefiBoard> {
    devices: Gm965Ich8BootblockDevices<B>,
    ffs: MemoryMappedFfs,
    ramstage_loaded: bool,
}

impl<B> StageFlow for Gm965Ich8BootblockStage<B>
where
    B: Gm965Ich8UefiBoard,
{
    type Devices = Gm965Ich8BootblockDevices<B>;

    fn new() -> Result<Self, ServiceError> {
        let board = B::board();
        Ok(Self {
            devices: Gm965Ich8BootblockDevices::new()?,
            ffs: MemoryMappedFfs::new(board.firmware_base(), board.firmware_size()),
            ramstage_loaded: false,
        })
    }

    fn devices_mut(&mut self) -> &mut Self::Devices {
        &mut self.devices
    }

    fn halt() -> ! {
        B::halt()
    }

    fn install_console(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        console_ready(B::console_node(), "ns16550");
        Ok(())
    }

    fn mount_firmware_volume(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        fstart_platform_x86_64::enable_boot_media_rom_cache();
        self.ffs.mount()
    }

    fn verify_firmware_volume(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        self.ffs.verify()
    }

    fn load_payload(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        self.ffs.load_file_by_name(GM965_NEXT_STAGE_NAME)?;
        self.ramstage_loaded = true;
        Ok(())
    }

    fn boot_payload(self) -> ! {
        if !self.ramstage_loaded {
            fstart_log::error!("ramstage handoff requested before load");
            B::halt();
        }
        fstart_log::info!("jumping to ramstage at {:#x}", GM965_RAMSTAGE_LOAD_ADDR);
        fstart_platform_x86_64::jump_to(GM965_RAMSTAGE_LOAD_ADDR)
    }
}

/// Runtime device set owned by the GM965/ICH8 ramstage recipe.
pub struct Gm965Ich8RamstageDevices<B: Gm965Ich8UefiBoard> {
    northbridge: IntelGm965,
    southbridge: IntelIch8,
    mainboard: B::Mainboard,
    console: StaticConsole<Ns16550>,
    e820: [E820Entry; MAX_E820_ENTRIES],
    e820_count: usize,
    total_ram: u64,
    acpi_rsdp: Option<u64>,
}

#[cfg_attr(not(feature = "crabefi"), allow(dead_code))]
impl<B> Gm965Ich8RamstageDevices<B>
where
    B: Gm965Ich8UefiBoard,
{
    fn new() -> Result<Self, ServiceError> {
        Ok(Self {
            northbridge: new_gm965::<B>()?,
            southbridge: new_ich8::<B>()?,
            mainboard: B::mainboard()?,
            console: StaticConsole::new(B::console_config()),
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

    fn console_device(&self) -> Option<&Ns16550> {
        self.console.device()
    }
}

impl<B> HardwareInit for Gm965Ich8RamstageDevices<B>
where
    B: Gm965Ich8UefiBoard,
{
    fn pre_console(&mut self, ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        self.northbridge.pre_console(ctx)?;
        self.southbridge.pre_console(ctx)?;
        self.mainboard.pre_console(&mut self.southbridge)
    }

    fn console(&mut self, ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        self.console.console(ctx)
    }

    fn memory_discovery(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        let (count, total) = fstart_capabilities::memory_detect(
            &self.northbridge,
            &mut self.e820,
            GM965_NORTHBRIDGE_NODE,
        )?;
        self.e820_count = count;
        self.total_ram = total;
        Ok(())
    }

    fn bus_probe(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        self.northbridge.init_bus()?;
        self.southbridge.ramstage_init()?;
        self.mainboard.post_dram(&mut self.southbridge)?;
        B::init_mp()
    }

    fn drivers_ready(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        fstart_capabilities::driver_init_complete(4);
        Ok(())
    }

    fn handoff(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        self.acpi_rsdp = B::prepare_acpi(self);
        B::prepare_smbios();
        Ok(())
    }
}

/// GM965/ICH8 ramstage fixed-flow stage.
#[cfg(feature = "crabefi")]
pub struct Gm965Ich8Ramstage<B: Gm965Ich8UefiBoard> {
    devices: Gm965Ich8RamstageDevices<B>,
    boot: MemoryMappedUefiBoot,
    payload_ready: bool,
}

#[cfg(feature = "crabefi")]
impl<B> StageFlow for Gm965Ich8Ramstage<B>
where
    B: Gm965Ich8UefiBoard,
{
    type Devices = Gm965Ich8RamstageDevices<B>;

    fn new() -> Result<Self, ServiceError> {
        let board = B::board();
        Ok(Self {
            devices: Gm965Ich8RamstageDevices::new()?,
            boot: MemoryMappedUefiBoot::new(board.firmware_base(), board.firmware_size(), 0),
            payload_ready: false,
        })
    }

    fn devices_mut(&mut self) -> &mut Self::Devices {
        &mut self.devices
    }

    fn halt() -> ! {
        B::halt()
    }

    fn install_console(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        console_ready(B::console_node(), "ns16550");
        Ok(())
    }

    fn mount_firmware_volume(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        fstart_platform_x86_64::enable_boot_media_rom_cache();
        self.boot.mount()?;
        self.devices.northbridge.stage_local_init()
    }

    fn verify_firmware_volume(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        self.boot.verify()
    }

    fn load_payload(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        self.payload_ready = true;
        Ok(())
    }

    fn finalize_handoff(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        self.devices
            .mainboard
            .finalize(&mut self.devices.southbridge)?;
        self.devices.southbridge.finalize()
    }

    fn boot_payload(self) -> ! {
        if !self.payload_ready {
            fstart_log::error!("UEFI handoff requested before payload setup");
            B::halt();
        }
        let console = match self.devices.console_device() {
            Some(console) => console,
            None => {
                fstart_log::error!("UEFI handoff requested before console init");
                B::halt();
            }
        };

        let acpi_base = self.devices.acpi_rsdp().unwrap_or(0) & !0xfff;
        let board = B::board();
        let platform_entries = [
            MemoryRegion {
                base: board.firmware_base(),
                size: board.firmware_size() as u64,
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
            (self.devices.total_ram >> 20) as u32,
            self.devices.acpi_rsdp().unwrap_or(0),
            self.devices.northbridge.config().ecam_base,
        );
        fstart_stage::crabefi::launch_x86_uefi(
            UefiLaunchConfig {
                console: Some(console),
                framebuffer: None,
                acpi_rsdp: self.devices.acpi_rsdp(),
                smbios: None,
                fdt: None,
                ecam_base: Some(self.devices.northbridge.config().ecam_base),
                runtime_region: Some(fstart_stage::crabefi::compute_runtime_region()),
            },
            self.devices.e820(),
            &platform_entries,
        )
    }
}
