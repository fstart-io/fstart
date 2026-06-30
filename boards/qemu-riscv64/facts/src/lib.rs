#![no_std]

pub const BOARD_NAME: &str = "qemu-riscv64";
pub const BOARD_PACKAGE: &str = "fstart-board-qemu-riscv64";

pub const FLASH_BASE: u64 = 0x2000_0000;
pub const FLASH_SIZE_U64: u64 = 0x0200_0000;
pub const FLASH_SIZE: usize = FLASH_SIZE_U64 as usize;
pub const RAM_BASE: u64 = 0x8000_0000;
pub const RAM_SIZE: u64 = 0x0800_0000;

pub const STAGE_LOAD_ADDR: u64 = FLASH_BASE;
pub const STAGE_STACK_SIZE: u32 = 0x100000;
pub const STAGE_HEAP_SIZE: u32 = 0x40000;
pub const STAGE_DATA_ADDR: u64 = 0x8100_0000;

pub const UART0_NODE: &str = "uart0";
pub const UART0_BASE: u64 = 0x1000_0000;
pub const UART0_REG_SHIFT: u8 = 0;
pub const UART0_REG_WIDTH: u8 = 0;
pub const UART0_CLOCK_FREQ: u32 = 3_686_400;
pub const UART0_BAUD_RATE: u32 = 115_200;

pub const KERNEL_FILE: &str = "vmlinux";
pub const KERNEL_LOAD_ADDR: u64 = 0x8200_0000;
pub const FIRMWARE_FILE: &str = "fw_dynamic.bin";
pub const FIRMWARE_LOAD_ADDR: u64 = 0x8010_0000;
pub const FDT_ADDR: u64 = 0x87f0_0000;
pub const BOOTARGS: &str = "console=ttyS0 earlycon=sbi";
