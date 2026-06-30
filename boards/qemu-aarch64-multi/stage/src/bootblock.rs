//! Bootblock fixed-flow adapter for QEMU AArch64 multi-stage demo.

use fstart_board_qemu_aarch64_multi_facts as facts;
use fstart_driver_pl011::{Pl011, Pl011Config};
use fstart_services::{InitContext, ServiceError};
use fstart_stage::fixed_helpers::{console_ready, MemoryMappedFfs, StaticConsole};
use fstart_stage_runtime::StaticBoard;

fn uart0_config() -> Pl011Config {
    Pl011Config {
        base_addr: facts::UART0_BASE,
        clock_freq: facts::UART0_CLOCK,
        baud_rate: facts::UART0_BAUD,
        acpi_name: None,
        acpi_gsiv: None,
        acpi_dbg2: false,
    }
}

type BootblockDevices = StaticConsole<Pl011>;

pub struct BootblockBoard {
    devices: BootblockDevices,
    ffs: MemoryMappedFfs,
    main_loaded: bool,
}

impl StaticBoard for BootblockBoard {
    type Devices = BootblockDevices;

    fn new() -> Result<Self, ServiceError> {
        Ok(Self {
            devices: BootblockDevices::new(uart0_config()),
            ffs: MemoryMappedFfs::new(facts::FLASH_BASE, facts::FLASH_SIZE),
            main_loaded: false,
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
        self.ffs.mount()
    }

    fn verify_firmware_volume(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        self.ffs.verify()
    }

    fn load_payload(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        self.ffs.load_file_by_name(facts::NEXT_STAGE_NAME)?;
        self.main_loaded = true;
        Ok(())
    }

    fn boot_payload(self) -> ! {
        if !self.main_loaded {
            fstart_log::error!("main stage handoff requested before load");
            Self::halt();
        }
        fstart_log::info!("jumping to main at {:#x}", facts::MAIN_LOAD_ADDR);
        fstart_platform::jump_to(facts::MAIN_LOAD_ADDR)
    }
}
