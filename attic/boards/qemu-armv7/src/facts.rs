pub const BOARD_NAME: &str = "qemu-armv7";
pub const BOARD_PACKAGE: &str = "fstart-board-qemu-armv7";

pub const FLASH_BASE: u64 = 0x0000_0000;
pub const FLASH_SIZE: usize = 0x0800_0000;
pub const RAM_BASE: u64 = 0x4000_0000;
pub const RAM_SIZE: u64 = 0x0800_0000;

pub const UART0_NODE: &str = "uart0";
pub const UART0_BASE: u64 = 0x0900_0000;
pub const UART0_CLOCK: u32 = 1_843_200;
pub const UART0_BAUD: u32 = 115_200;

pub const KERNEL_FILE: &str = "zImage";
pub const KERNEL_LOAD_ADDR: u64 = 0x4100_0000;
pub const SRC_FDT_ADDR: u64 = 0x4000_0000;
pub const FDT_ADDR: u64 = 0x40f0_0000;
pub const BOOTARGS: &str = "console=ttyAMA0 earlycon=pl011,mmio32,0x09000000";
