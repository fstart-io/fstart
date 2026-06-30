//! Static fixed-flow adapter for QEMU AArch64 UEFI.

use fstart_board_qemu_aarch64_uefi_facts as facts;
use fstart_driver_pl011::{Pl011, Pl011Config};
use fstart_services::{InitContext, ServiceError};
use fstart_stage::crabefi::{MemoryRegion, MemoryType, UefiLaunchConfig};
use fstart_stage::fixed_helpers::{console_ready, MemoryMappedUefiBoot, StaticConsole};
use fstart_stage_runtime::StaticBoard;

fn uart0_config() -> Pl011Config {
    Pl011Config {
        base_addr: facts::UART0_BASE,
        clock_freq: facts::UART0_CLOCK_FREQ,
        baud_rate: facts::UART0_BAUD_RATE,
        acpi_name: None,
        acpi_gsiv: None,
        acpi_dbg2: false,
    }
}

static STATIC_MEMORY: [MemoryRegion; 1] = [MemoryRegion {
    base: facts::FLASH_BASE,
    size: facts::FLASH_SIZE_U64,
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
            devices: StageDevices::new(uart0_config()),
            boot: MemoryMappedUefiBoot::new(
                facts::FLASH_BASE,
                facts::FLASH_SIZE,
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
        console_ready(facts::UART0_NODE, "pl011");
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
            facts::FIRMWARE_LOAD_ADDR,
            self.boot.fdt_addr(),
        );
        fstart_platform::boot_bl31_and_resume(facts::FIRMWARE_LOAD_ADDR, self.boot.fdt_addr());

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
                ecam_base: Some(facts::PCI0_ECAM_BASE),
                runtime_region: None,
            },
            &STATIC_MEMORY,
            facts::RAM_BASE,
            facts::RAM_SIZE,
            facts::FW_DATA_ADDR,
            facts::FW_STACK_SIZE,
            fdt_reservation,
        )
    }
}
