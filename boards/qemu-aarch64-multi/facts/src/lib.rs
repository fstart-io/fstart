#![no_std]

pub const BOARD_NAME: &str = "qemu-aarch64-multi";
pub const BOARD_PACKAGE: &str = "fstart-board-qemu-aarch64-multi";

pub const FLASH_BASE: u64 = 0x0000_0000;
pub const FLASH_SIZE: usize = 0x0800_0000;
pub const RAM_BASE: u64 = 0x4000_0000;
pub const RAM_SIZE: u64 = 0x0800_0000;

pub const UART0_NODE: &str = "uart0";
pub const UART0_BASE: u64 = 0x0900_0000;
pub const UART0_CLOCK: u32 = 1_843_200;
pub const UART0_BAUD: u32 = 115_200;

pub const BOOTBLOCK_LOAD_ADDR: u64 = FLASH_BASE;
pub const MAIN_LOAD_ADDR: u64 = 0x4040_0000;
pub const NEXT_STAGE_NAME: &str = "main";
