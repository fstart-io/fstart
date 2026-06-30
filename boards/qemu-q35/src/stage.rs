//! Static fixed-flow adapter for QEMU Q35.

use fstart_driver_ns16550::{AccessMode, Ns16550, Ns16550Config};
use fstart_services::{InitContext, ServiceError};
use fstart_stage::fixed_helpers::{console_ready, MemoryMappedLinuxBoot, StaticConsole};
use fstart_stage_runtime::StaticBoard;

const FFS_BASE: u64 = 0xff90_0000;
const FFS_SIZE: usize = 0x006f_f000;
const KERNEL_LOAD_ADDR: u64 = 0x0100_0000;
const ZERO_PAGE_ADDR: u64 = 0x0009_0000;
const BOOTARGS: &str = "console=ttyS0 earlyprintk=serial,ttyS0,115200";

static UART0_CONFIG: Ns16550Config = Ns16550Config {
    regs: AccessMode::Pio { base: 0x3f8 },
    clock_freq: 1_843_200,
    baud_rate: 115_200,
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
            devices: StageDevices::new(&UART0_CONFIG),
            boot: MemoryMappedLinuxBoot::new(FFS_BASE, FFS_SIZE, 0),
        })
    }

    fn devices_mut(&mut self) -> &mut Self::Devices {
        &mut self.devices
    }

    fn halt() -> ! {
        fstart_platform::halt()
    }

    fn install_console(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        console_ready("uart0", "ns16550");
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

        fstart_log::info!("booting Linux bzImage at {:#x}", KERNEL_LOAD_ADDR);
        let params = self
            .boot
            .x86_boot_params(KERNEL_LOAD_ADDR, ZERO_PAGE_ADDR, BOOTARGS);
        fstart_platform::boot_linux(&params)
    }
}
