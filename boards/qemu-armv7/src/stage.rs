//! QEMU ARMv7 virt stage recipe binding.

use crate::facts;
use fstart_driver_pl011::{Pl011, Pl011Config};
use fstart_services::boot::BootLinuxParams;
use fstart_stage::fixed_helpers::QemuVirtLinuxBoard;

pub struct Board;

impl QemuVirtLinuxBoard for Board {
    type Console = Pl011;

    const UART0_NODE: &'static str = facts::UART0_NODE;
    const UART0_DRIVER: &'static str = "pl011";
    const FLASH_BASE: u64 = facts::FLASH_BASE;
    const FLASH_SIZE: usize = facts::FLASH_SIZE;
    const FDT_ADDR: u64 = facts::FDT_ADDR;
    const BOOTARGS: &'static str = facts::BOOTARGS;
    const RAM_BASE: u64 = facts::RAM_BASE;
    const RAM_SIZE: u64 = facts::RAM_SIZE;
    const KERNEL_LOAD_ADDR: u64 = facts::KERNEL_LOAD_ADDR;
    const FIRMWARE_LOAD_ADDR: u64 = 0;
    const LOAD_FIRMWARE: bool = false;
    const FIRMWARE_NAME: &'static str = "";

    fn console_config() -> Pl011Config {
        Pl011Config {
            base_addr: facts::UART0_BASE,
            clock_freq: facts::UART0_CLOCK,
            baud_rate: facts::UART0_BAUD,
            acpi_name: None,
            acpi_gsiv: None,
            acpi_dbg2: false,
        }
    }

    fn source_dtb_addr() -> u64 {
        facts::SRC_FDT_ADDR
    }

    fn boot_hart_id() -> u64 {
        0
    }

    fn halt() -> ! {
        fstart_platform_armv7::halt()
    }

    fn boot_linux(params: &BootLinuxParams<'_>) -> ! {
        fstart_platform_armv7::boot_linux(params)
    }
}
