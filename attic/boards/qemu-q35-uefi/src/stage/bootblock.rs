//! Bootblock fixed-flow adapter for QEMU Q35 UEFI.

use crate::facts;
use fstart_driver_ns16550::{AccessMode, Ns16550, Ns16550Config};
use fstart_services::{InitContext, ServiceError};
use fstart_stage::fixed_helpers::{console_ready, MemoryMappedFfs, StaticConsole};
use fstart_stage_runtime::StaticBoard;

const UART0_CONFIG: Ns16550Config = Ns16550Config {
    regs: AccessMode::Pio {
        base: facts::UART0_PIO_BASE,
    },
    clock_freq: facts::UART0_CLOCK_FREQ,
    baud_rate: facts::UART0_BAUD_RATE,
};

type BootblockDevices = StaticConsole<Ns16550>;

pub struct BootblockBoard {
    devices: BootblockDevices,
    ffs: MemoryMappedFfs,
    main_loaded: bool,
}

impl StaticBoard for BootblockBoard {
    type Devices = BootblockDevices;

    fn new() -> Result<Self, ServiceError> {
        Ok(Self {
            devices: BootblockDevices::new(UART0_CONFIG),
            ffs: MemoryMappedFfs::new(facts::FFS_BASE, facts::FFS_SIZE),
            main_loaded: false,
        })
    }

    fn devices_mut(&mut self) -> &mut Self::Devices {
        &mut self.devices
    }

    fn halt() -> ! {
        super::fstart_platform::halt()
    }

    fn install_console(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        console_ready(facts::UART0_NODE, "ns16550");
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
        super::fstart_platform::jump_to(facts::MAIN_LOAD_ADDR)
    }
}
