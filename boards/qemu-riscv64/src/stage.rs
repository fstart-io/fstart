//! QEMU RISC-V virt stage recipe binding.

use fstart_board_qemu_riscv64_facts as facts;
use fstart_driver_ns16550::{AccessMode, Ns16550, Ns16550Config};
use fstart_services::boot::BootLinuxParams;
use fstart_stage::fixed_helpers::QemuVirtLinuxBoard;

pub struct Board;

impl QemuVirtLinuxBoard for Board {
    type Console = Ns16550;

    const UART0_NODE: &'static str = facts::UART0_NODE;
    const UART0_DRIVER: &'static str = "ns16550";
    const FLASH_BASE: u64 = facts::FLASH_BASE;
    const FLASH_SIZE: usize = facts::FLASH_SIZE;
    const FDT_ADDR: u64 = facts::FDT_ADDR;
    const BOOTARGS: &'static str = facts::BOOTARGS;
    const RAM_BASE: u64 = facts::RAM_BASE;
    const RAM_SIZE: u64 = facts::RAM_SIZE;
    const KERNEL_LOAD_ADDR: u64 = facts::KERNEL_LOAD_ADDR;
    const FIRMWARE_LOAD_ADDR: u64 = facts::FIRMWARE_LOAD_ADDR;
    const LOAD_FIRMWARE: bool = true;
    const FIRMWARE_NAME: &'static str = "OpenSBI";

    fn console_config() -> Ns16550Config {
        Ns16550Config {
            regs: AccessMode::Mmio {
                base: facts::UART0_BASE,
                reg_shift: facts::UART0_REG_SHIFT,
                reg_width: facts::UART0_REG_WIDTH,
            },
            clock_freq: facts::UART0_CLOCK_FREQ,
            baud_rate: facts::UART0_BAUD_RATE,
        }
    }

    fn source_dtb_addr() -> u64 {
        fstart_platform_riscv64::boot_dtb_addr()
    }

    fn boot_hart_id() -> u64 {
        fstart_platform_riscv64::boot_hart_id()
    }

    fn halt() -> ! {
        fstart_platform_riscv64::halt()
    }

    fn boot_linux(params: &BootLinuxParams<'_>) -> ! {
        fstart_platform_riscv64::boot_linux(params)
    }
}
