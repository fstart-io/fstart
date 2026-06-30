//! Lenovo ThinkPad X61 UEFI ramstage fixed-flow adapter.

use fstart_acpi::device::AcpiDevice;
use fstart_acpi::platform::{PlatformConfig, X86PlatformProvider};
use fstart_board_lenovo_x61 as board;
use fstart_driver_intel_gm965::IntelGm965;
use fstart_driver_intel_ich8::IntelIch8;
use fstart_driver_ns16550::Ns16550;
use fstart_mainboard_lenovo_x61::{LenovoX61Mainboard, X61_SMBIOS_DESC};
use fstart_platform_intel_gm965_ich8 as platform;
use fstart_services::memory_detect::{E820Entry, MAX_E820_ENTRIES};
use fstart_services::{
    EarlyInit, FinalizeInit, HardwareInit, InitContext, Mainboard, PciRootBus, PostDramInit,
    PreConsoleInit, ServiceError, StageLocalInit,
};
use fstart_stage::crabefi::{MemoryRegion, MemoryType, UefiLaunchConfig};
use fstart_stage::fixed_helpers::{console_ready, MemoryMappedUefiBoot, StaticConsole};
use fstart_stage_runtime::StaticBoard;

use crate::common;

pub struct MainDevices {
    northbridge: IntelGm965,
    southbridge: IntelIch8,
    mainboard: LenovoX61Mainboard,
    console: StaticConsole<Ns16550>,
    e820: [E820Entry; MAX_E820_ENTRIES],
    e820_count: usize,
    total_ram: u64,
    acpi_rsdp: Option<u64>,
}

impl MainDevices {
    fn new() -> Result<Self, ServiceError> {
        Ok(Self {
            northbridge: common::new_gm965()?,
            southbridge: common::new_ich8()?,
            mainboard: common::new_mainboard()?,
            console: StaticConsole::new(common::UART0_CONFIG),
            e820: [E820Entry::zeroed(); MAX_E820_ENTRIES],
            e820_count: 0,
            total_ram: 0,
            acpi_rsdp: None,
        })
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

    fn prepare_acpi(&mut self) {
        let platform = PlatformConfig::X86(
            self.southbridge
                .x86_platform_config(fstart_mp::online_cpus() as u32),
        );
        let rsdp =
            fstart_capabilities::acpi::prepare_with_options(&platform, true, |dsdt, extra| {
                dsdt.extend(self.northbridge.dsdt_aml(self.northbridge.config()));
                dsdt.extend(self.southbridge.dsdt_aml(self.southbridge.config()));
                dsdt.extend(self.mainboard.dsdt_aml(self.mainboard.config()));
                extra.extend(self.northbridge.extra_tables(self.northbridge.config()));
                extra.extend(self.southbridge.extra_tables(self.southbridge.config()));
            });
        self.acpi_rsdp = Some(rsdp);
    }

    fn prepare_smbios(&self) {
        fstart_capabilities::smbios::prepare(&X61_SMBIOS_DESC);
    }

    fn init_mp(&self) -> Result<(), ServiceError> {
        let cpu = fstart_cpu_intel::core2_cpu::Core2CpuDriver::new(platform::ICH8_PMBASE, None);
        let drivers: [&dyn fstart_mp::CpuDriver; 1] = [&cpu];
        fstart_mp::mp_init(&fstart_mp::MpConfig {
            cpu_drivers: &drivers,
            smm: None,
            smm_image: None,
            max_cpus: 2,
        })
        .map(|_| ())
        .map_err(|_| ServiceError::HardwareError)
    }
}

impl HardwareInit for MainDevices {
    fn pre_console(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        self.northbridge.pre_console_init()?;
        self.southbridge.pre_console_init()?;
        self.mainboard
            .pre_console_init_with_southbridge(&mut self.southbridge)
    }

    fn console(&mut self, ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        self.console.console(ctx)
    }

    fn post_console(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        self.southbridge.early_init()
    }

    fn memory_discovery(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        let (count, total) = fstart_capabilities::memory_detect(
            &self.northbridge,
            &mut self.e820,
            platform::GM965_NORTHBRIDGE_NODE,
        )?;
        self.e820_count = count;
        self.total_ram = total;
        Ok(())
    }

    fn bus_probe(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        self.northbridge.init_bus()?;
        self.southbridge.post_dram_init()?;
        self.mainboard
            .ramstage_init_with_southbridge(&mut self.southbridge)?;
        self.init_mp()?;
        Ok(())
    }

    fn drivers_ready(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        fstart_capabilities::driver_init_complete(4);
        Ok(())
    }

    fn handoff(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        self.prepare_acpi();
        self.prepare_smbios();
        Ok(())
    }
}

pub struct MainBoard {
    devices: MainDevices,
    boot: MemoryMappedUefiBoot,
    payload_ready: bool,
}

impl StaticBoard for MainBoard {
    type Devices = MainDevices;

    fn new() -> Result<Self, ServiceError> {
        let runtime = common::runtime_config();
        Ok(Self {
            devices: MainDevices::new()?,
            boot: MemoryMappedUefiBoot::new(runtime.firmware_base, runtime.firmware_size, 0),
            payload_ready: false,
        })
    }

    fn devices_mut(&mut self) -> &mut Self::Devices {
        &mut self.devices
    }

    fn halt() -> ! {
        fstart_platform::halt()
    }

    fn install_console(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        console_ready(board::UART0_NODE, "ns16550");
        Ok(())
    }

    fn mount_firmware_volume(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        fstart_platform::enable_boot_media_rom_cache();
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
        self.devices.mainboard.finalize()?;
        self.devices.southbridge.finalize_init()
    }

    fn boot_payload(self) -> ! {
        if !self.payload_ready {
            fstart_log::error!("UEFI handoff requested before payload setup");
            Self::halt();
        }
        let console = match self.devices.console_device() {
            Some(console) => console,
            None => {
                fstart_log::error!("UEFI handoff requested before console init");
                Self::halt();
            }
        };

        let acpi_base = self.devices.acpi_rsdp().unwrap_or(0) & !0xfff;
        let runtime = common::runtime_config();
        let platform_entries = [
            MemoryRegion {
                base: runtime.firmware_base,
                size: runtime.firmware_size as u64,
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
