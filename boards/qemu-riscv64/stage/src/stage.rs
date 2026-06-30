//! Static fixed-flow adapter for the QEMU RISC-V `virt` board.

use fstart_board_qemu_riscv64_facts as facts;
use fstart_driver_ns16550::{AccessMode, Ns16550, Ns16550Config};
use fstart_services::{InitContext, ServiceError};
use fstart_stage::fixed_helpers::{console_ready, MemoryMappedLinuxBoot, StaticConsole};
use fstart_stage_runtime::StaticBoard;

static UART0_CONFIG: Ns16550Config = Ns16550Config {
    regs: AccessMode::Mmio {
        base: facts::UART0_BASE,
        reg_shift: facts::UART0_REG_SHIFT,
        reg_width: facts::UART0_REG_WIDTH,
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
            devices: StageDevices::new(UART0_CONFIG.clone()),
            boot: MemoryMappedLinuxBoot::new(
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
            fstart_platform::boot_hart_id(),
            facts::BOOTARGS,
        );
        fstart_platform::boot_linux(&params)
    }
}
