#![no_std]

use fstart_capabilities::smbios::{ProcessorDesc, SmbiosDesc};
use fstart_driver_i2c_ck505::I2cCk505Config;
use fstart_driver_intel_ich7::{
    IntelIch7Config, LpcDecodeConfig, LpcFixedIoDecode, LpcGenericIoDecode, SataConfig, SataMode,
    UsbConfig,
};
use fstart_driver_intel_pineview::{IgdConfig, IntelPineviewConfig};
use fstart_gpio_ich as gpio;
use fstart_hda as hda;
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
    IntelPineviewConfig {
        mchbar: 0xfed1_0000,
        dmibar: 0xfed1_8000,
        epbar: 0xfed1_9000,
        ecam_base: 0xe000_0000,
        igd: Some(IgdConfig {
            use_crt: true,
            use_lvds: false,
            spread_spectrum: false,
            vbt_file: Some(hstr("data.vbt")),
        }),
        spd_addresses: [0x50, 0x51, 0, 0],
        ck505_pre_raminit: true,
        acpi_name: Some(hstr("MCHC")),
    }
}

pub fn ich7_config() -> IntelIch7Config {
    IntelIch7Config {
        rcba: 0xfed1_c000,
        pirq_routing: [0x0b; 8],
        gpe0_en: 0x441,
        lpc_decode: LpcDecodeConfig {
            fixed_io: LpcFixedIoDecode::default(),
            generic_io: hvec([LpcGenericIoDecode {
                base: 0x0a00,
                size: 0x0100,
            }]),
        },
        hda: Some(d41s_hda_config()),
        sata: Some(SataConfig {
            mode: SataMode::Ahci,
            ports: 0x3,
        }),
        usb: Some(UsbConfig {
            ehci: true,
            uhci: [true, true, true, true],
        }),
        pata: false,
        smbus_base: 0x0400,
        gpio: d41s_gpio_config(),
        acpi_name: Some(hstr("LPCB")),
        c3_latency: 85,
        power_on_after_fail: 0,
    }
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

pub fn d41s_hda_config() -> hda::HdaConfig {
    hda::HdaConfig {
        verbs: hvec([hda::HdaVerbTable {
            vendor_id: 0x10ec_0662,
            subsystem_id: 0x105b_0d55,
            pins: hvec([
                hda::pin_config(
                    0x14,
                    hda::PinDevice::LineOut,
                    hda::PinConn::Jack,
                    hda::PinLoc::External,
                    hda::PinGeoLoc::Rear,
                    hda::PinConnector::StereoMono18,
                    hda::PinColor::Green,
                    0xC,
                    1,
                    0,
                ),
                hda::pin_not_connected(0x15, 0),
                hda::pin_not_connected(0x16, 0),
                hda::pin_config(
                    0x18,
                    hda::PinDevice::MicIn,
                    hda::PinConn::Jack,
                    hda::PinLoc::External,
                    hda::PinGeoLoc::Rear,
                    hda::PinConnector::StereoMono18,
                    hda::PinColor::Pink,
                    0xC,
                    3,
                    0,
                ),
                hda::pin_config(
                    0x19,
                    hda::PinDevice::MicIn,
                    hda::PinConn::Jack,
                    hda::PinLoc::External,
                    hda::PinGeoLoc::Front,
                    hda::PinConnector::StereoMono18,
                    hda::PinColor::Pink,
                    0xC,
                    3,
                    1,
                ),
                hda::pin_config(
                    0x1a,
                    hda::PinDevice::LineIn,
                    hda::PinConn::Jack,
                    hda::PinLoc::External,
                    hda::PinGeoLoc::Rear,
                    hda::PinConnector::StereoMono18,
                    hda::PinColor::Blue,
                    0x4,
                    3,
                    15,
                ),
                hda::pin_config(
                    0x1b,
                    hda::PinDevice::HpOut,
                    hda::PinConn::Jack,
                    hda::PinLoc::External,
                    hda::PinGeoLoc::Front,
                    hda::PinConnector::StereoMono18,
                    hda::PinColor::Green,
                    0xC,
                    1,
                    15,
                ),
                hda::pin_not_connected(0x1c, 0),
                hda::pin_config(
                    0x1d,
                    hda::PinDevice::DeviceOther,
                    hda::PinConn::Nc,
                    hda::PinLoc::External,
                    hda::PinGeoLoc::NA,
                    hda::PinConnector::Optical,
                    hda::PinColor::Orange,
                    0x6,
                    0,
                    3,
                ),
                hda::pin_config(
                    0x1e,
                    hda::PinDevice::SpdifOut,
                    hda::PinConn::Integrated,
                    hda::PinLoc::Internal,
                    hda::PinGeoLoc::Special9,
                    hda::PinConnector::AtapiInternal,
                    hda::PinColor::ColorUnknown,
                    0x1,
                    2,
                    0,
                ),
            ]),
            extra_verbs: hvec([]),
        }]),
    }
}

pub fn d41s_gpio_config() -> gpio::GpioConfig {
    gpio::GpioConfig {
        pins: hvec([
            gpio::output(0, gpio::GpioLevel::Low),
            gpio::output(6, gpio::GpioLevel::Low),
            gpio::output(7, gpio::GpioLevel::Low),
            gpio::output(8, gpio::GpioLevel::Low),
            gpio::output(9, gpio::GpioLevel::Low),
            gpio::output(10, gpio::GpioLevel::Low),
            gpio::output(12, gpio::GpioLevel::Low),
            gpio::output(13, gpio::GpioLevel::Low),
            gpio::output(14, gpio::GpioLevel::Low),
            gpio::output(15, gpio::GpioLevel::Low),
            gpio::output(24, gpio::GpioLevel::Low),
            gpio::output(25, gpio::GpioLevel::Low),
            gpio::output(26, gpio::GpioLevel::Low),
            gpio::output(27, gpio::GpioLevel::Low),
            gpio::output(28, gpio::GpioLevel::Low),
            gpio::input(33),
            gpio::input(34),
            gpio::input(38),
            gpio::input(39),
        ]),
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
