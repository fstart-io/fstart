//! Static fixed-flow adapter for QEMU ARMv7 `virt`.

use fstart_board_qemu_armv7_facts as facts;
use fstart_driver_pl011::{Pl011, Pl011Config};
use fstart_services::{InitContext, ServiceError};
use fstart_stage::fixed_helpers::{console_ready, MemoryMappedLinuxBoot, StaticConsole};
use fstart_stage_runtime::StaticBoard;

static UART0_CONFIG: Pl011Config = Pl011Config {
    base_addr: facts::UART0_BASE,
    clock_freq: facts::UART0_CLOCK,
    baud_rate: facts::UART0_BAUD,
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
            boot: MemoryMappedLinuxBoot::new(
                facts::FLASH_BASE,
                facts::FLASH_SIZE,
                facts::SRC_FDT_ADDR,
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
        self.boot.load_kernel()
    }

    fn finalize_handoff(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        self.boot.prepare_fdt(
            facts::FDT_ADDR,
            facts::BOOTARGS,
            facts::RAM_BASE,
            facts::RAM_SIZE,
        )
    }

    fn boot_payload(self) -> ! {
        if !self.boot.kernel_loaded() {
            fstart_log::error!("payload handoff requested before kernel load");
            Self::halt();
        }

        fstart_log::info!(
            "booting ARM Linux at {:#x}, dtb at {:#x}",
            facts::KERNEL_LOAD_ADDR,
            self.boot.dtb_addr(),
        );
        let params = self
            .boot
            .boot_params(facts::KERNEL_LOAD_ADDR, 0, 0, facts::BOOTARGS);
        fstart_platform::boot_linux(&params)
    }
}
