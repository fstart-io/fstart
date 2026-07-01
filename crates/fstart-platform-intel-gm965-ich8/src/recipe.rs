//! GM965/ICH8 UEFI-style static stage recipe.
//!
//! Board crates or selected-board stage wrappers supply [`Gm965Ich8UefiBoard`]
//! facts. This module owns the reusable bootblock and ramstage sequencing.

use core::marker::PhantomData;

use fstart_driver_intel_gm965::IntelGm965;
use fstart_driver_ns16550::{Ns16550, Ns16550Config};
use fstart_services::memory_detect::{E820Entry, MAX_E820_ENTRIES};
use fstart_services::{
    Device, DeviceError, HardwareInit, InitContext, PciRootBus, ServiceError, StageLocalInit,
};
use fstart_stage::crabefi::{MemoryRegion, MemoryType, UefiLaunchConfig};
use fstart_stage::fixed_helpers::{
    console_ready, MemoryMappedFfs, MemoryMappedUefiBoot, StaticConsole,
};
use fstart_stage_runtime::StaticBoard;

use crate::{
    Gm965Ich8Config, Gm965Ich8Southbridge, GM965_NEXT_STAGE_NAME, GM965_NORTHBRIDGE_NODE,
    GM965_RAMSTAGE_LOAD_ADDR,
};

/// Board facts and hooks required by the reusable GM965/ICH8 UEFI recipe.
pub trait Gm965Ich8UefiBoard: Sized + 'static {
    /// Board-specific southbridge wrapper or plain southbridge device.
    type Southbridge: Gm965Ich8Southbridge;

    /// Return the complete GM965/ICH8 board configuration.
    fn platform_config() -> Gm965Ich8Config;

    /// Return the boot console configuration.
    fn console_config() -> Ns16550Config;

    /// Return the board topology node name for the boot console.
    fn console_node() -> &'static str;

    /// Construct the board southbridge hook stack.
    fn new_southbridge() -> Result<Self::Southbridge, ServiceError>;

    /// Run board-selected multiprocessing initialization.
    fn init_mp() -> Result<(), ServiceError> {
        Ok(())
    }

    /// Build ACPI tables and return the RSDP address when the board supports ACPI.
    fn prepare_acpi(_devices: &mut Gm965Ich8RamstageDevices<Self>) -> Option<u64> {
        None
    }

    /// Prepare board-selected SMBIOS tables.
    fn prepare_smbios() {}
}

fn device_error_to_service_error(_err: DeviceError) -> ServiceError {
    ServiceError::HardwareError
}

fn new_gm965<B: Gm965Ich8UefiBoard>() -> Result<IntelGm965, ServiceError> {
    IntelGm965::new(B::platform_config().gm965).map_err(device_error_to_service_error)
}

/// Reusable GM965/ICH8 bootblock `StaticBoard` implementation.
pub struct Gm965Ich8BootblockBoard<B: Gm965Ich8UefiBoard> {
    devices: (IntelGm965, B::Southbridge, StaticConsole<Ns16550>),
    ffs: MemoryMappedFfs,
    ramstage_loaded: bool,
    _board: PhantomData<B>,
}

impl<B> StaticBoard for Gm965Ich8BootblockBoard<B>
where
    B: Gm965Ich8UefiBoard,
{
    type Devices = (IntelGm965, B::Southbridge, StaticConsole<Ns16550>);

    fn new() -> Result<Self, ServiceError> {
        let config = B::platform_config();
        Ok(Self {
            devices: (
                new_gm965::<B>()?,
                B::new_southbridge()?,
                StaticConsole::new(B::console_config()),
            ),
            ffs: MemoryMappedFfs::new(config.firmware_base, config.firmware_size),
            ramstage_loaded: false,
            _board: PhantomData,
        })
    }

    fn devices_mut(&mut self) -> &mut Self::Devices {
        &mut self.devices
    }

    fn halt() -> ! {
        fstart_platform_x86_64::halt()
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
            <Self as StaticBoard>::halt();
        }
        fstart_log::info!("jumping to ramstage at {:#x}", GM965_RAMSTAGE_LOAD_ADDR);
        fstart_platform_x86_64::jump_to(GM965_RAMSTAGE_LOAD_ADDR)
    }
}

/// Runtime device set owned by the GM965/ICH8 ramstage recipe.
pub struct Gm965Ich8RamstageDevices<B: Gm965Ich8UefiBoard> {
    northbridge: IntelGm965,
    southbridge: B::Southbridge,
    console: StaticConsole<Ns16550>,
    e820: [E820Entry; MAX_E820_ENTRIES],
    e820_count: usize,
    total_ram: u64,
    acpi_rsdp: Option<u64>,
}

impl<B> Gm965Ich8RamstageDevices<B>
where
    B: Gm965Ich8UefiBoard,
{
    fn new() -> Result<Self, ServiceError> {
        Ok(Self {
            northbridge: new_gm965::<B>()?,
            southbridge: B::new_southbridge()?,
            console: StaticConsole::new(B::console_config()),
            e820: [E820Entry::zeroed(); MAX_E820_ENTRIES],
            e820_count: 0,
            total_ram: 0,
            acpi_rsdp: None,
        })
    }

    /// Return the northbridge driver.
    #[must_use]
    pub const fn northbridge(&self) -> &IntelGm965 {
        &self.northbridge
    }

    /// Return the board southbridge wrapper.
    #[must_use]
    pub const fn southbridge(&self) -> &B::Southbridge {
        &self.southbridge
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
        self.southbridge.pre_console(ctx)
    }

    fn console(&mut self, ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        self.console.console(ctx)
    }

    fn post_console(&mut self, ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        self.southbridge.post_console(ctx)
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

/// Reusable GM965/ICH8 UEFI ramstage `StaticBoard` implementation.
pub struct Gm965Ich8RamstageBoard<B: Gm965Ich8UefiBoard> {
    devices: Gm965Ich8RamstageDevices<B>,
    boot: MemoryMappedUefiBoot,
    payload_ready: bool,
}

impl<B> StaticBoard for Gm965Ich8RamstageBoard<B>
where
    B: Gm965Ich8UefiBoard,
{
    type Devices = Gm965Ich8RamstageDevices<B>;

    fn new() -> Result<Self, ServiceError> {
        let config = B::platform_config();
        Ok(Self {
            devices: Gm965Ich8RamstageDevices::new()?,
            boot: MemoryMappedUefiBoot::new(config.firmware_base, config.firmware_size, 0),
            payload_ready: false,
        })
    }

    fn devices_mut(&mut self) -> &mut Self::Devices {
        &mut self.devices
    }

    fn halt() -> ! {
        fstart_platform_x86_64::halt()
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
        self.devices.southbridge.finalize()
    }

    fn boot_payload(self) -> ! {
        if !self.payload_ready {
            fstart_log::error!("UEFI handoff requested before payload setup");
            <Self as StaticBoard>::halt();
        }
        let console = match self.devices.console_device() {
            Some(console) => console,
            None => {
                fstart_log::error!("UEFI handoff requested before console init");
                <Self as StaticBoard>::halt();
            }
        };

        let acpi_base = self.devices.acpi_rsdp().unwrap_or(0) & !0xfff;
        let config = B::platform_config();
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
