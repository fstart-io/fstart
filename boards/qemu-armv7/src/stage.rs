//! Static fixed-flow adapter for QEMU ARMv7 `virt`.

use fstart_driver_pl011::{Pl011, Pl011Config};
use fstart_services::{InitContext, ServiceError};
use fstart_stage::fixed_helpers::{console_ready, MemoryMappedLinuxBoot, StaticConsole};
use fstart_stage_runtime::StaticBoard;

const FLASH_BASE: u64 = 0x0000_0000;
const FLASH_SIZE: usize = 0x0800_0000;
const RAM_BASE: u64 = 0x4000_0000;
const RAM_SIZE: u64 = 0x0800_0000;
const KERNEL_LOAD_ADDR: u64 = 0x4100_0000;
const SRC_FDT_ADDR: u64 = 0x4000_0000;
const FDT_ADDR: u64 = 0x40f0_0000;
const BOOTARGS: &str = "console=ttyAMA0 earlycon=pl011,mmio32,0x09000000";

static UART0_CONFIG: Pl011Config = Pl011Config {
    base_addr: 0x0900_0000,
    clock_freq: 1_843_200,
    baud_rate: 115_200,
    acpi_name: None,
    acpi_gsiv: None,
    acpi_dbg2: false,
};

type StageDevices = StaticConsole<Pl011>;

pub struct StageBoard {
    devices: StageDevices,
    boot: MemoryMappedLinuxBoot,
}

impl StaticBoard for StageBoard {
    type Devices = StageDevices;

    fn new() -> Result<Self, ServiceError> {
        Ok(Self {
            devices: StageDevices::new(&UART0_CONFIG),
            boot: MemoryMappedLinuxBoot::new(FLASH_BASE, FLASH_SIZE, SRC_FDT_ADDR),
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
        self.boot.load_kernel()
    }

    fn finalize_handoff(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        self.boot
            .prepare_fdt(FDT_ADDR, BOOTARGS, RAM_BASE, RAM_SIZE)
    }

    fn boot_payload(self) -> ! {
        if !self.boot.kernel_loaded() {
            fstart_log::error!("payload handoff requested before kernel load");
            Self::halt();
        }

        fstart_log::info!(
            "booting ARM Linux at {:#x}, dtb at {:#x}",
            KERNEL_LOAD_ADDR,
            self.boot.dtb_addr(),
        );
        let params = self.boot.boot_params(KERNEL_LOAD_ADDR, 0, 0, BOOTARGS);
        fstart_platform::boot_linux(&params)
    }
}
