//! Static fixed-flow adapter for SiFive Unmatched under QEMU `sifive_u`.

use fstart_driver_sifive_uart::{SifiveUart, SifiveUartConfig};
use fstart_services::{InitContext, ServiceError};
use fstart_stage::fixed_helpers::{console_ready, MemoryMappedLinuxBoot, StaticConsole};
use fstart_stage_runtime::StaticBoard;

const FFS_BASE: u64 = 0x8000_0000;
const FFS_SIZE: usize = 0x0800_0000;
const RAM_BASE: u64 = 0x8000_0000;
const RAM_SIZE: u64 = 0x2000_0000;
const KERNEL_LOAD_ADDR: u64 = 0x8400_0000;
const FIRMWARE_LOAD_ADDR: u64 = 0x8300_0000;
const FDT_ADDR: u64 = 0x8f00_0000;
const BOOT_HART_ID: u64 = 1;
const BOOTARGS: &str = "console=ttySIF0 earlycon=sbi";

static UART0_CONFIG: SifiveUartConfig = SifiveUartConfig {
    base_addr: 0x1001_0000,
    clock_freq: 500_000_000,
    baud_rate: 115_200,
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
            devices: StageDevices::new(&UART0_CONFIG),
            boot: MemoryMappedLinuxBoot::new(FFS_BASE, FFS_SIZE, fstart_platform::boot_dtb_addr()),
        })
    }

    fn devices_mut(&mut self) -> &mut Self::Devices {
        &mut self.devices
    }

    fn halt() -> ! {
        fstart_platform::halt()
    }

    fn install_console(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        console_ready("uart0", "sifive-uart");
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
        self.boot
            .prepare_fdt(FDT_ADDR, BOOTARGS, RAM_BASE, RAM_SIZE)
    }

    fn boot_payload(self) -> ! {
        if !self.boot.firmware_loaded() || !self.boot.kernel_loaded() {
            fstart_log::error!("payload handoff requested before firmware/kernel load");
            Self::halt();
        }

        fstart_log::info!(
            "booting OpenSBI at {:#x}, kernel at {:#x}, dtb at {:#x}",
            FIRMWARE_LOAD_ADDR,
            KERNEL_LOAD_ADDR,
            self.boot.dtb_addr(),
        );
        let params =
            self.boot
                .boot_params(KERNEL_LOAD_ADDR, FIRMWARE_LOAD_ADDR, BOOT_HART_ID, BOOTARGS);
        fstart_platform::boot_linux(&params)
    }
}
