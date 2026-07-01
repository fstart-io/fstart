use crate as board;
use fstart_capabilities::smbios::{ProcessorDesc, SmbiosDesc};
use fstart_driver_i2c_ck505::I2cCk505Config;
use fstart_driver_intel_ich7::IntelIch7Config;
use fstart_driver_intel_pineview::IntelPineviewConfig;
use fstart_superio::{
    CirConfig, ComPortConfig, EcConfig, KbcConfig, MouseConfig, ParallelConfig, SuperIoConfig,
};
use fstart_types::{hstr, hvec};

pub const BOARD_NAME: &str = "foxconn-d41s";
pub const BOARD_PACKAGE: &str = "fstart-board-foxconn-d41s";

pub const FLASH_FFS_BASE: u64 = 0xff00_0000;
pub const FLASH_FFS_SIZE_U64: u64 = 0x00f0_0000;
pub const FLASH_FFS_SIZE: usize = FLASH_FFS_SIZE_U64 as usize;
pub const FLASH_BOOTBLOCK_BASE: u64 = 0xfff0_0000;
pub const FLASH_BOOTBLOCK_SIZE_U64: u64 = 0x0010_0000;
pub const FLASH_BOOTBLOCK_SIZE: usize = FLASH_BOOTBLOCK_SIZE_U64 as usize;
pub const RAM_BASE: u64 = 0x0010_0000;
pub const RAM_SIZE: u64 = 0x3ff0_0000;
pub const CAR_BASE: u64 = 0xfefc_0000;
pub const CAR_SIZE: u64 = 0x8000;

pub const BOOTBLOCK_LOAD_ADDR: u64 = 0xffff_ffff;
pub const BOOTBLOCK_HEAP_SIZE: usize = 0x100;
pub const RAMSTAGE_LOAD_ADDR: u64 = 0x0400_0000;
pub const RAMSTAGE_HEAP_SIZE: usize = 0x200000;
pub const RAMSTAGE_STACK_SIZE: u64 = 0x8000;
pub const NEXT_STAGE_NAME: &str = "ramstage";

pub const KERNEL_LOAD_ADDR: u64 = 0x0200_0000;
pub const ZERO_PAGE_ADDR: u64 = 0x0009_0000;
pub const BOOTARGS: &str =
    "console=ttyS0,115200n8 earlycon=uart8250,io,0x3f8,115200n8 ignore_loglevel loglevel=8";

pub const NORTHBRIDGE_NODE: &str = "northbridge";
pub const SOUTHBRIDGE_NODE: &str = "southbridge";
pub const SUPERIO_NODE: &str = "superio";
pub const CK505_NODE: &str = "ck505";

pub const ICH7_PMBASE: u32 = 0x0500;
pub const CK505_ADDR: u8 = 0x69;

pub fn pineview_config() -> IntelPineviewConfig {
    board::pineview_ich7_config().pineview
}

pub fn ich7_config() -> IntelIch7Config {
    board::pineview_ich7_config().ich7
}

pub fn superio_config() -> SuperIoConfig {
    SuperIoConfig {
        com1: Some(ComPortConfig {
            io_base: 0x3f8,
            irq: 4,
            baud_rate: 115_200,
        }),
        com2: Some(ComPortConfig {
            io_base: 0x2f8,
            irq: 3,
            baud_rate: 115_200,
        }),
        parallel: Some(ParallelConfig {
            io_base: 0x378,
            irq: 7,
        }),
        env_controller: Some(EcConfig {
            io_base: 0xa10,
            io_ext: 0xa00,
        }),
        keyboard: Some(KbcConfig {
            io_base: 0x60,
            io_ext: 0x64,
            irq: 1,
        }),
        mouse: Some(MouseConfig { irq: 12 }),
        cir: Some(CirConfig {
            io_base: 0x3e0,
            irq: 10,
        }),
        gpio: None,
        acpi_name: Some(hstr("SIO0")),
        console_port: Some(hstr("com1")),
    }
}

pub fn ck505_config() -> I2cCk505Config {
    I2cCk505Config {
        mask: hvec([0x00, 0x80, 0xff, 0xff, 0xff]),
        regs: hvec([0x00, 0x80, 0xfe, 0xff, 0xfc]),
    }
}

static PROCESSORS: [ProcessorDesc<'static>; 1] = [ProcessorDesc {
    socket: "FCBGA559",
    manufacturer: "Intel",
    family: 0x28,
    max_speed_mhz: 0,
    core_count: 0,
    thread_count: 0,
    caches: &[],
}];

const BIOS_RELEASE_DATE: &str = match option_env!("FSTART_SMBIOS_DATE") {
    Some(date) => date,
    None => "04/15/2026",
};

pub static SMBIOS_DESC: SmbiosDesc<'static> = SmbiosDesc {
    bios_vendor: "fstart",
    bios_version: "0.1.0",
    bios_release_date: BIOS_RELEASE_DATE,
    sys_manufacturer: "Foxconn",
    sys_product: "D41S",
    sys_version: "1.0",
    sys_serial: None,
    bb_manufacturer: "Foxconn",
    bb_product: "D41S",
    chassis_type: 0x03,
    chassis_manufacturer: "Foxconn",
    processors: &PROCESSORS,
    memory_devices: &[],
    ram_base: RAM_BASE,
    ram_end: RAM_BASE + RAM_SIZE - 1,
};
