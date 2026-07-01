pub const BOARD_NAME: &str = "qemu-q35";
pub const BOARD_PACKAGE: &str = "fstart-board-qemu-q35";

pub const FLASH_BASE: u64 = 0xff80_0000;
pub const FLASH_SIZE: u64 = 0x0080_0000;
pub const WORKRAM_BASE: u64 = 0x0010_0000;
pub const WORKRAM_SIZE: u64 = 0x00f0_0000;

pub const STAGE_LOAD_ADDR: u64 = FLASH_BASE;
pub const STAGE_STACK_SIZE: u32 = 0x80000;
pub const STAGE_HEAP_SIZE: u32 = 0x40000;
pub const STAGE_DATA_ADDR: u64 = 0x100000;
pub const PAGE_TABLE_ADDR: u64 = 0x1000;
pub const PAGE_TABLE_SIZE: u64 = 0x4000;

pub const FFS_BASE: u64 = 0xff90_0000;
pub const FFS_SIZE_U64: u64 = 0x006f_f000;
pub const FFS_SIZE: usize = FFS_SIZE_U64 as usize;
pub const FFS_TEMP_RAM_BASE: u64 = 0x0200_0000;
pub const FFS_TEMP_RAM_SIZE: u64 = 0x0100_0000;

pub const KERNEL_FILE: &str = "bzImage";
pub const KERNEL_LOAD_ADDR: u64 = 0x0100_0000;
pub const ZERO_PAGE_ADDR: u64 = 0x0009_0000;
pub const METADATA_BOOTARGS: &str = "console=ttyS0 earlycon=uart8250,io,0x3f8,115200n8";
pub const STAGE_BOOTARGS: &str = "console=ttyS0 earlyprintk=serial,ttyS0,115200";

pub const UART0_NODE: &str = "uart0";
pub const UART0_PIO_BASE: u64 = 0x3f8;
pub const UART0_CLOCK_FREQ: u32 = 1_843_200;
pub const UART0_BAUD_RATE: u32 = 115_200;

pub const FW_CFG0_NODE: &str = "fw_cfg0";
pub const FW_CFG_CTL_PORT: u16 = 0x510;
pub const FW_CFG_DATA_PORT: u16 = 0x511;

pub const PCI0_NODE: &str = "pci0";
pub const PCI0_ECAM_BASE: u64 = 0xb000_0000;
pub const PCI0_ECAM_SIZE: u64 = 0x1000_0000;
pub const PCI0_BUS_START: u8 = 0;
pub const PCI0_BUS_END: u8 = 255;

pub const BOCHS0_NODE: &str = "bochs0";
pub const BOCHS0_DEVICE: u8 = 2;
pub const BOCHS0_FUNCTION: u8 = 0;
pub const BOCHS0_WIDTH: u32 = 1024;
pub const BOCHS0_HEIGHT: u32 = 768;

pub const SMM_ENTRY_POINTS: u16 = 4;
pub const SMM_STACK_SIZE: u32 = 0x400;
