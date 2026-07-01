pub const BOARD_NAME: &str = "qemu-aarch64-uefi";
pub const BOARD_PACKAGE: &str = "fstart-board-qemu-aarch64-uefi";

pub const FLASH_BASE: u64 = 0x0000_0000;
pub const FLASH_SIZE_U64: u64 = 0x0800_0000;
pub const FLASH_SIZE: usize = FLASH_SIZE_U64 as usize;
pub const RAM_BASE: u64 = 0x4000_0000;
pub const RAM_SIZE: u64 = 0x1_0000_0000;

pub const STAGE_LOAD_ADDR: u64 = FLASH_BASE;
pub const STAGE_STACK_SIZE: u32 = 0x300000;
pub const STAGE_HEAP_SIZE: u32 = 0x100000;
pub const STAGE_DATA_ADDR: u64 = 0x4020_0000;
pub const FW_DATA_ADDR: u64 = STAGE_DATA_ADDR;
pub const FW_STACK_SIZE: u64 = STAGE_STACK_SIZE as u64;

pub const FIRMWARE_FILE: &str = "bl31.bin";
pub const FIRMWARE_LOAD_ADDR: u64 = 0x4010_0000;

pub const UART0_NODE: &str = "uart0";
pub const UART0_BASE: u64 = 0x0900_0000;
pub const UART0_CLOCK_FREQ: u32 = 1_843_200;
pub const UART0_BAUD_RATE: u32 = 115_200;

pub const PCI0_NODE: &str = "pci0";
pub const PCI0_ECAM_BASE: u64 = 0x0040_1000_0000;
pub const PCI0_ECAM_SIZE: u64 = 0x1000_0000;
pub const PCI0_MMIO32_BASE: u64 = 0x1000_0000;
pub const PCI0_MMIO32_SIZE: u64 = 0x2eff_0000;
pub const PCI0_MMIO64_BASE: u64 = 0x0080_0000_0000;
pub const PCI0_MMIO64_SIZE: u64 = 0x0080_0000_0000;
pub const PCI0_PIO_BASE: u64 = 0x3eff_0000;
pub const PCI0_PIO_SIZE: u64 = 0x10000;
pub const PCI0_BUS_START: u8 = 0;
pub const PCI0_BUS_END: u8 = 255;

pub const BOCHS0_NODE: &str = "bochs0";
pub const BOCHS0_DEVICE: u8 = 3;
pub const BOCHS0_FUNCTION: u8 = 0;
pub const BOCHS0_WIDTH: u32 = 1024;
pub const BOCHS0_HEIGHT: u32 = 768;
