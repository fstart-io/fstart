//! Static fixed-flow adapter for SiFive Unmatched under QEMU `sifive_u`.

use fstart_board_sifive_unmatched_facts as facts;
use fstart_driver_sifive_uart::{SifiveUart, SifiveUartConfig};
use fstart_services::{InitContext, ServiceError};
use fstart_stage::fixed_helpers::{console_ready, MemoryMappedLinuxBoot, StaticConsole};
use fstart_stage_runtime::StaticBoard;

const UART0_CONFIG: SifiveUartConfig = SifiveUartConfig {
    base_addr: facts::UART0_BASE,
    clock_freq: facts::UART0_CLOCK,
    baud_rate: facts::UART0_BAUD,
};

type StageDevices = StaticConsole<SifiveUart>;

pub struct StageBoard {
    devices: StageDevices,
    boot: MemoryMappedLinuxBoot,
}

impl StaticBoard for StageBoard {
    type Devices = StageDevices;

    fn new() -> Result<Self, ServiceError> {
        Ok(Self {
            devices: StageDevices::new(UART0_CONFIG),
            boot: MemoryMappedLinuxBoot::new(
                facts::FFS_BASE,
                facts::FFS_SIZE,
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
        console_ready(facts::UART0_NODE, "sifive-uart");
        Ok(())
    }

    fn mount_firmware_volume(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        self.boot.mount()
    }

    fn verify_firmware_volume(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        self.boot.verify()
    }

    fn load_payload(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        self.boot.load_firmware_and_kernel()
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
        if !self.boot.firmware_loaded() || !self.boot.kernel_loaded() {
            fstart_log::error!("payload handoff requested before firmware/kernel load");
            Self::halt();
        }

        fstart_log::info!(
            "booting OpenSBI at {:#x}, kernel at {:#x}, dtb at {:#x}",
            facts::FIRMWARE_LOAD_ADDR,
            facts::KERNEL_LOAD_ADDR,
            self.boot.dtb_addr(),
        );
        let params = self.boot.boot_params(
            facts::KERNEL_LOAD_ADDR,
            facts::FIRMWARE_LOAD_ADDR,
            u64::from(facts::BOOT_HART_ID),
            facts::BOOTARGS,
        );
        fstart_platform::boot_linux(&params)
    }
}
