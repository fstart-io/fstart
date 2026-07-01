//! Static fixed-flow adapter for QEMU Q35.

use crate::facts;
use fstart_driver_ns16550::{AccessMode, Ns16550, Ns16550Config};
use fstart_services::{InitContext, ServiceError};
use fstart_stage::fixed_helpers::{console_ready, MemoryMappedLinuxBoot, StaticConsole};
use fstart_stage_runtime::StaticBoard;

const UART0_CONFIG: Ns16550Config = Ns16550Config {
    regs: AccessMode::Pio {
        base: facts::UART0_PIO_BASE,
    },
    clock_freq: facts::UART0_CLOCK_FREQ,
    baud_rate: facts::UART0_BAUD_RATE,
};

type StageDevices = StaticConsole<Ns16550>;

pub struct StageBoard {
    devices: StageDevices,
    boot: MemoryMappedLinuxBoot,
}

impl StaticBoard for StageBoard {
    type Devices = StageDevices;

    fn new() -> Result<Self, ServiceError> {
        Ok(Self {
            devices: StageDevices::new(UART0_CONFIG),
            boot: MemoryMappedLinuxBoot::new(facts::FFS_BASE, facts::FFS_SIZE, 0),
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
        self.boot.mount()
    }

    fn verify_firmware_volume(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        self.boot.verify()
    }

    fn load_payload(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        self.boot.load_kernel()
    }

    fn boot_payload(self) -> ! {
        if !self.boot.kernel_loaded() {
            fstart_log::error!("payload handoff requested before kernel load");
            Self::halt();
        }

        fstart_log::info!("booting Linux bzImage at {:#x}", facts::KERNEL_LOAD_ADDR);
        let params = self.boot.x86_boot_params(
            facts::KERNEL_LOAD_ADDR,
            facts::ZERO_PAGE_ADDR,
            facts::STAGE_BOOTARGS,
        );
        super::fstart_platform::boot_linux(&params)
    }
}
