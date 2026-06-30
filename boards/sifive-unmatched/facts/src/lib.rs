#![no_std]

pub const BOARD_NAME: &str = "sifive-unmatched";
pub const BOARD_PACKAGE: &str = "fstart-board-sifive-unmatched";

pub const FFS_BASE: u64 = 0x8000_0000;
pub const FFS_SIZE: usize = 0x0800_0000;
pub const RAM_BASE: u64 = 0x8000_0000;
pub const RAM_SIZE: u64 = 0x2000_0000;

pub const UART0_NODE: &str = "uart0";
pub const UART0_BASE: u64 = 0x1001_0000;
pub const UART0_CLOCK: u32 = 500_000_000;
pub const UART0_BAUD: u32 = 115_200;

pub const KERNEL_FILE: &str = "Image";
pub const KERNEL_LOAD_ADDR: u64 = 0x8400_0000;
pub const FIRMWARE_FILE: &str = "fw_dynamic.bin";
pub const FIRMWARE_LOAD_ADDR: u64 = 0x8300_0000;
pub const FDT_ADDR: u64 = 0x8f00_0000;
pub const BOOT_HART_ID: u32 = 1;
pub const BOOTARGS: &str = "console=ttySIF0 earlycon=sbi";
