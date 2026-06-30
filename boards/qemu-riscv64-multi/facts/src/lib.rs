#![no_std]

pub const BOARD_NAME: &str = "qemu-riscv64-multi";
pub const BOARD_PACKAGE: &str = "fstart-board-qemu-riscv64-multi";

pub const FLASH_BASE: u64 = 0x2000_0000;
pub const FLASH_SIZE: usize = 0x0200_0000;
pub const RAM_BASE: u64 = 0x8000_0000;
pub const RAM_SIZE: u64 = 0x0800_0000;

pub const UART0_NODE: &str = "uart0";
pub const UART0_BASE: u64 = 0x1000_0000;
pub const UART0_REG_SHIFT: u8 = 0;
pub const UART0_REG_WIDTH: u8 = 0;
pub const UART0_CLOCK_FREQ: u32 = 3_686_400;
pub const UART0_BAUD_RATE: u32 = 115_200;

pub const BOOTBLOCK_LOAD_ADDR: u64 = FLASH_BASE;
pub const MAIN_LOAD_ADDR: u64 = 0x8010_0000;
pub const NEXT_STAGE_NAME: &str = "main";
