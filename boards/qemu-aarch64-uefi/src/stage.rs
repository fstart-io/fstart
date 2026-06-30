//! Static fixed-flow adapter for QEMU AArch64 UEFI.

use fstart_driver_pl011::{Pl011, Pl011Config};
use fstart_services::{InitContext, ServiceError};
use fstart_stage::crabefi::{MemoryRegion, MemoryType, UefiLaunchConfig};
use fstart_stage::fixed_helpers::{console_ready, MemoryMappedUefiBoot, StaticConsole};
use fstart_stage_runtime::StaticBoard;

const FLASH_BASE: u64 = 0x0000_0000;
const FLASH_SIZE: usize = 0x0800_0000;
const RAM_BASE: u64 = 0x4000_0000;
const RAM_SIZE: u64 = 0x1_0000_0000;
const FIRMWARE_LOAD_ADDR: u64 = 0x4010_0000;
const FW_DATA_ADDR: u64 = 0x4020_0000;
const FW_STACK_SIZE: u64 = 0x300000;
const PCI_ECAM_BASE: u64 = 0x0040_1000_0000;

static UART0_CONFIG: Pl011Config = Pl011Config {
    base_addr: 0x0900_0000,
    clock_freq: 1_843_200,
    baud_rate: 115_200,
    acpi_name: None,
    acpi_gsiv: None,
    acpi_dbg2: false,
};

static STATIC_MEMORY: [MemoryRegion; 1] = [MemoryRegion {
    base: FLASH_BASE,
    size: FLASH_SIZE as u64,
    region_type: MemoryType::Reserved,
}];

type StageDevices = StaticConsole<Pl011>;

pub struct StageBoard {
    devices: StageDevices,
    boot: MemoryMappedUefiBoot,
}

impl StaticBoard for StageBoard {
    type Devices = StageDevices;

    fn new() -> Result<Self, ServiceError> {
        Ok(Self {
            devices: StageDevices::new(&UART0_CONFIG),
            boot: MemoryMappedUefiBoot::new(
                FLASH_BASE,
                FLASH_SIZE,
                fstart_platform::boot_dtb_addr(),
            ),
        })
    }

    fn devices_mut(&mut self) -> &mut Self::Devices {
        &mut self.devices
    }

    fn halt() -> ! {
        fstart_platform::halt()
    }

    fn install_console(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        console_ready("uart0", "pl011");
        Ok(())
    }

    fn mount_firmware_volume(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        self.boot.mount()
    }

    fn verify_firmware_volume(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        self.boot.verify()
    }

    fn load_payload(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        self.boot.load_firmware()
    }

    fn boot_payload(self) -> ! {
        if !self.boot.firmware_loaded() {
            fstart_log::error!("UEFI handoff requested before BL31 firmware load");
            Self::halt();
        }

        let fdt = unsafe { self.boot.fdt_bytes() };
        let fdt_reservation = unsafe { self.boot.fdt_reservation() };

        fstart_log::info!(
            "initializing BL31 at {:#x}, resume FDT at {:#x}",
            FIRMWARE_LOAD_ADDR,
            self.boot.fdt_addr(),
        );
        fstart_platform::boot_bl31_and_resume(FIRMWARE_LOAD_ADDR, self.boot.fdt_addr());

        let console = match self.devices.device() {
            Some(console) => console,
            None => {
                fstart_log::error!("UEFI handoff requested before console init");
                Self::halt();
            }
        };

        fstart_log::info!("launching CrabEFI");
        fstart_stage::crabefi::launch_flat_uefi(
            UefiLaunchConfig {
                console: Some(console),
                framebuffer: None,
                acpi_rsdp: None,
                smbios: None,
                fdt,
                ecam_base: Some(PCI_ECAM_BASE),
                runtime_region: None,
            },
            &STATIC_MEMORY,
            RAM_BASE,
            RAM_SIZE,
            FW_DATA_ADDR,
            FW_STACK_SIZE,
            fdt_reservation,
        )
    }
}
