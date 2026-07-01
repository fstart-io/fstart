//! Static hardware facts for LeMaker Banana Pi M1 (Allwinner A20/sun7i).

pub const RAM_BASE: u64 = 0x4000_0000;
pub const RAM_SIZE: u64 = 0x4000_0000;

pub const CCU_BASE: u64 = 0x01c2_0000;
pub const PIO_BASE: u64 = 0x01c2_0800;
pub const DRAMC_BASE: u64 = 0x01c0_1000;

pub const UART0_BASE: u64 = 0x01c2_8000;
pub const UART0_REG_SHIFT: u8 = 2;
pub const UART0_REG_WIDTH: u8 = 4;
pub const UART0_CLOCK_FREQ: u32 = 24_000_000;
pub const UART0_BAUD_RATE: u32 = 115_200;
pub const UART0_NODE: &str = "uart0";

pub const MMC0_BASE: u64 = 0x01c0_f000;
pub const MMC0_INDEX: u8 = 0;
pub const MMC0_NODE: &str = "mmc0";

/// Allwinner BROM SD/MMC eGON sector offset for SPL/boot0 images.
pub const MMC_FIRMWARE_IMAGE_OFFSET: u64 = 8 * 1024;

pub const BOOTBLOCK_LOAD_ADDR: u64 = 0x0000_0000;
pub const MAIN_LOAD_ADDR: u64 = 0x4100_0000;
pub const HANDOFF_ADDR: u64 = MAIN_LOAD_ADDR - 0x1000;

pub const KERNEL_LOAD_ADDR: u64 = 0x4200_0000;
pub const FDT_ADDR: u64 = 0x4300_0000;
pub const BOOTARGS: &str = "earlycon console=ttyS0,115200";
