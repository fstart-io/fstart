//! Static fixed-flow adapter for the QEMU RISC-V `virt` board.

use fstart_driver_ns16550::{AccessMode, Ns16550, Ns16550Config};
use fstart_services::{InitContext, ServiceError};
use fstart_stage::fixed_helpers::{console_ready, MemoryMappedLinuxBoot, StaticConsole};
use fstart_stage_runtime::StaticBoard;

const FLASH_BASE: u64 = 0x2000_0000;
const FLASH_SIZE: usize = 0x0200_0000;
const RAM_BASE: u64 = 0x8000_0000;
const RAM_SIZE: u64 = 0x0800_0000;
const KERNEL_LOAD_ADDR: u64 = 0x8200_0000;
const FIRMWARE_LOAD_ADDR: u64 = 0x8010_0000;
const FDT_ADDR: u64 = 0x87f0_0000;
const BOOTARGS: &str = "console=ttyS0 earlycon=sbi";

static UART0_CONFIG: Ns16550Config = Ns16550Config {
    regs: AccessMode::Mmio {
        base: 0x1000_0000,
        reg_shift: 0,
        reg_width: 0,
    },
    clock_freq: 3_686_400,
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
            boot: MemoryMappedLinuxBoot::new(
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
        let params = self.boot.boot_params(
            KERNEL_LOAD_ADDR,
            FIRMWARE_LOAD_ADDR,
            fstart_platform::boot_hart_id(),
            BOOTARGS,
        );
        fstart_platform::boot_linux(&params)
    }
}
