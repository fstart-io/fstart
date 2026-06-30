//! Foxconn D41S UEFI main-stage fixed-flow adapter.

extern crate alloc;

use alloc::vec::Vec;

use fstart_acpi::device::AcpiDevice;
use fstart_acpi::platform::{PlatformConfig, X86PlatformProvider};
use fstart_acpi::{Aml, AmlSink};
use fstart_board_foxconn_d41s_facts as facts;
use fstart_driver_i2c_ck505::I2cCk505;
use fstart_driver_intel_ich7::IntelIch7;
use fstart_driver_intel_pineview::IntelPineview;
use fstart_driver_ite8721f::Ite8721f;
use fstart_services::memory_detect::{E820Entry, MAX_E820_ENTRIES};
use fstart_services::{
    BusDevice, EarlyInit, HardwareInit, InitContext, PciRootBus, PostDramInit, PreConsoleInit,
    ServiceError, SmBus, StageLocalInit,
};
use fstart_stage::crabefi::{MemoryRegion, MemoryType, UefiLaunchConfig};
use fstart_stage::fixed_helpers::MemoryMappedUefiBoot;
use fstart_stage_runtime::StaticBoard;
use fstart_superio::SuperIoConfig;

use crate::common;

struct RawAml<'a>(&'a [u8]);

impl Aml for RawAml<'_> {
    fn to_aml_bytes(&self, sink: &mut dyn AmlSink) {
        sink.vec(self.0);
    }
}

fn scope_aml(path: &str, children: &[u8]) -> Vec<u8> {
    let raw = RawAml(children);
    let scope = fstart_acpi::aml::Scope::new(
        fstart_acpi::aml::Path::new(path),
        alloc::vec![&raw as &dyn Aml],
    );
    let mut bytes = Vec::new();
    scope.to_aml_bytes(&mut bytes);
    bytes
}

pub struct MainDevices {
    northbridge: IntelPineview,
    southbridge: IntelIch7,
    superio_config: SuperIoConfig,
    superio: Option<Ite8721f>,
    e820: [E820Entry; MAX_E820_ENTRIES],
    e820_count: usize,
    total_ram: u64,
    acpi_rsdp: Option<u64>,
}

impl MainDevices {
    fn new() -> Result<Self, ServiceError> {
        Ok(Self {
            northbridge: common::new_pineview()?,
            southbridge: common::new_ich7()?,
            superio_config: common::superio_config(),
            superio: None,
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

    fn console_device(&self) -> Option<&Ite8721f> {
        self.superio.as_ref()
    }

    fn prepare_acpi(&mut self) {
        let platform = PlatformConfig::X86(
            self.southbridge
                .x86_platform_config(fstart_mp::online_cpus() as u32),
        );
        let rsdp =
            fstart_capabilities::acpi::prepare_with_options(&platform, false, |dsdt, extra| {
                dsdt.extend(self.northbridge.dsdt_aml(self.northbridge.config()));
                dsdt.extend(self.southbridge.dsdt_aml(self.southbridge.config()));
                if let Some(superio) = self.superio.as_ref() {
                    let sio = superio.dsdt_aml(&self.superio_config);
                    dsdt.extend(scope_aml("\\_SB_.PCI0.LPCB", &sio));
                }
                extra.extend(self.northbridge.extra_tables(self.northbridge.config()));
                extra.extend(self.southbridge.extra_tables(self.southbridge.config()));
            });
        self.acpi_rsdp = Some(rsdp);
    }

    fn prepare_smbios(&self) {
        let _ = self.total_ram;
        fstart_capabilities::smbios::prepare(&facts::SMBIOS_DESC);
    }

    fn init_ck505(&mut self) -> Result<(), ServiceError> {
        let bus = &mut self.southbridge as &mut dyn SmBus;
        let mut ck505 = I2cCk505::new_on_bus_at(
            common::ck505_config(),
            bus,
            Some(fstart_types::BusAddress::I2c(facts::CK505_ADDR)),
        )
        .map_err(common::device_error_to_service_error)?;
        ck505
            .init_on_bus(bus)
            .map_err(common::device_error_to_service_error)
    }

    fn init_mp(&self) -> Result<(), ServiceError> {
        static SMM_IMAGE: &[u8] = include_bytes!(env!("FSTART_SMM_IMAGE"));
        let cpu = fstart_cpu_intel::pineview::PineviewCpuDriver::new(facts::ICH7_PMBASE, None);
        let drivers: [&dyn fstart_mp::CpuDriver; 1] = [&cpu];
        fstart_mp::mp_init(&fstart_mp::MpConfig {
            cpu_drivers: &drivers,
            smm: Some(&self.northbridge),
            smm_image: Some(SMM_IMAGE),
            max_cpus: 4,
        })
        .map(|_| ())
        .map_err(|_| ServiceError::HardwareError)
    }
}

impl HardwareInit for MainDevices {
    fn pre_console(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        self.northbridge.pre_console_init()?;
        self.southbridge.pre_console_init()
    }

    fn console(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        let superio = common::init_superio(self.superio_config.clone(), &mut self.southbridge)?;
        self.superio = Some(superio);
        let console = self.superio.as_ref().expect("superio console initialized");
        common::install_superio_console(console)
    }

    fn post_console(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        self.southbridge.early_init()
    }

    fn memory_discovery(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        let (count, total) = fstart_capabilities::memory_detect(
            &self.northbridge,
            &mut self.e820,
            facts::NORTHBRIDGE_NODE,
        )?;
        self.e820_count = count;
        self.total_ram = total;
        Ok(())
    }

    fn bus_probe(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        self.northbridge.init_bus()?;
        self.southbridge.post_dram_init()?;
        self.init_ck505()?;
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
        Ok(Self {
            devices: MainDevices::new()?,
            boot: MemoryMappedUefiBoot::new(facts::FLASH_FFS_BASE, facts::FLASH_FFS_SIZE, 0),
            payload_ready: false,
        })
    }

    fn devices_mut(&mut self) -> &mut Self::Devices {
        &mut self.devices
    }

    fn halt() -> ! {
        fstart_platform::halt()
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
        // UEFI payload support is linked in via the `crabefi` feature; no external
        // FFS firmware blob needs to be copied for this board.
        self.payload_ready = true;
        Ok(())
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
        let platform_entries = [
            MemoryRegion {
                base: facts::FLASH_FFS_BASE,
                size: facts::FLASH_FFS_SIZE_U64,
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
            facts::pineview_config().ecam_base,
        );
        fstart_stage::crabefi::launch_x86_uefi(
            UefiLaunchConfig {
                console: Some(console),
                framebuffer: None,
                acpi_rsdp: self.devices.acpi_rsdp(),
                smbios: None,
                fdt: None,
                ecam_base: Some(facts::pineview_config().ecam_base),
                runtime_region: Some(fstart_stage::crabefi::compute_runtime_region()),
            },
            self.devices.e820(),
            &platform_entries,
        )
    }
}
