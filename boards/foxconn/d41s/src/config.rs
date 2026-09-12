//! Foxconn D41S board metadata and build policy.

use fstart_core::smbios::{ChassisType, ProcessorFamily, SmbiosProcessor};
use fstart_core::{FlashLayout, Platform, SmbiosConfig, X86LegacyFlashLayout, hstr, hvec};
use fstart_driver_intel::generic::ck505::I2cCk505Config;
use fstart_driver_intel::southbridge::gpio_ich as gpio;
use fstart_driver_superio::ite8721f;
use fstart_driver_intel::southbridge::hda;
use fstart_platform_intel::pineview::{
    LpcFixedIoDecode, LpcGenericIoDecode, LpcParallelDecode, LpcSerialDecode, PineviewIch7Config,
    PineviewIgdConfig, SataConfig, SataMode, UsbConfig,
};

pub const BOARD_NAME: &str = "foxconn-d41s";
pub const BOARD_PACKAGE: &str = "fstart-board-foxconn-d41s";
pub const PLATFORM: Platform = Platform::X86_64;
pub const UART0_NODE: &str = "superio/com1";
pub const UART0_PIO_BASE: u16 = 0x3f8;
pub const UART0_CLOCK_FREQ: u32 = 1_843_200;
pub const UART0_BAUD_RATE: u32 = 115_200;
pub const SUPERIO_NODE: &str = "superio";
pub const SUPERIO_PNP_BASE: u16 = 0x2e;
pub const CK505_NODE: &str = "ck505";
pub const CK505_ADDR: u8 = 0x69;
pub static D41S_PLATFORM: PineviewIch7Config = PineviewIch7Config::new()
    .max_cpus(4)
    .igd(d41s_igd_config())
    .ck505_pre_raminit(true)
    .pcie_port(0, true)
    .pcie_port(1, true)
    .pirq_routing([0x0b; 8])
    .lpc_fixed_io(LpcFixedIoDecode {
        com_a: LpcSerialDecode::Com1,
        com_b: LpcSerialDecode::Com2,
        lpt: Some(LpcParallelDecode::Lpt378),
        fdd: None,
    })
    .lpc_generic_io([LpcGenericIoDecode {
        base: 0x0a00,
        size: 0x0100,
    }])
    .sata(SataConfig {
        mode: SataMode::Ahci,
        ports: 0x03,
    })
    .usb(UsbConfig {
        ehci: true,
        uhci: [true, true, true, true],
    })
    .hda(d41s_hda_config())
    .gpio_pins(d41s_gpio_pins())
    .gpe0_en(0x0441)
    .build();

/// 16-Mbit SPI flash, contiguous legacy mapping (no IFD).
/// Erase geometry and persistent variable capacity are not inferred from this.
pub const FLASH_SIZE: u32 = 0x0100_0000;
pub const FLASH: FlashLayout = FlashLayout::X86Legacy(X86LegacyFlashLayout { size: FLASH_SIZE });

impl fstart_platform_intel::facts::IntelBoardFacts for crate::Board {
    const FACTS: fstart_platform_intel::facts::BoardFacts =
        fstart_platform_intel::facts::BoardFacts::new(
            FLASH,
            FLASH_SIZE,
            D41S_PLATFORM.max_cpus,
            fstart_platform_intel::facts::Chipset::PineviewIch7,
        );
}

#[must_use]
pub const fn board_name() -> &'static str {
    BOARD_NAME
}

pub fn d41s_smbios() -> SmbiosConfig {
    SmbiosConfig {
        bios_vendor: hstr("fstart"),
        bios_version: hstr("0.1.0"),
        bios_release_date: hstr(option_env!("FSTART_SMBIOS_DATE").unwrap_or("04/15/2026")),
        system_manufacturer: hstr("Foxconn"),
        system_product: hstr("D41S"),
        system_version: hstr("1.0"),
        system_serial: hstr(""),
        baseboard_manufacturer: hstr("Foxconn"),
        baseboard_product: hstr("D41S"),
        chassis_type: ChassisType::Desktop,
        chassis_manufacturer: hstr("Foxconn"),
        processors: hvec([SmbiosProcessor {
            socket: hstr("FCBGA559"),
            manufacturer: hstr("Intel"),
            processor_family: ProcessorFamily::X86_64,
            max_speed_mhz: None,
            core_count: None,
            thread_count: None,
            caches: hvec([]),
        }]),
        memory_devices: hvec([]),
    }
}

pub const fn d41s_igd_config() -> PineviewIgdConfig {
    PineviewIgdConfig {
        use_crt: true,
        use_lvds: false,
        spread_spectrum: false,
        vbt_file: Some("data.vbt"),
    }
}

pub const fn d41s_hda_config() -> hda::HdaConfig {
    hda::HdaConfig::new().verb(
        hda::HdaVerbTable::new(0x10ec_0662, 0x105b_0d55)
            .pin(hda::pin_config(
                0x14,
                hda::PinDevice::LineOut,
                hda::PinConn::Jack,
                hda::PinLoc::External,
                hda::PinGeoLoc::Rear,
                hda::PinConnector::StereoMono18,
                hda::PinColor::Green,
                0x0c,
                1,
                0,
            ))
            .pin(hda::pin_not_connected(0x15, 0))
            .pin(hda::pin_not_connected(0x16, 0))
            .pin(hda::pin_config(
                0x18,
                hda::PinDevice::MicIn,
                hda::PinConn::Jack,
                hda::PinLoc::External,
                hda::PinGeoLoc::Rear,
                hda::PinConnector::StereoMono18,
                hda::PinColor::Pink,
                0x0c,
                3,
                0,
            ))
            .pin(hda::pin_config(
                0x19,
                hda::PinDevice::MicIn,
                hda::PinConn::Jack,
                hda::PinLoc::External,
                hda::PinGeoLoc::Front,
                hda::PinConnector::StereoMono18,
                hda::PinColor::Pink,
                0x0c,
                3,
                1,
            ))
            .pin(hda::pin_config(
                0x1a,
                hda::PinDevice::LineIn,
                hda::PinConn::Jack,
                hda::PinLoc::External,
                hda::PinGeoLoc::Rear,
                hda::PinConnector::StereoMono18,
                hda::PinColor::Blue,
                0x04,
                3,
                15,
            ))
            .pin(hda::pin_config(
                0x1b,
                hda::PinDevice::HpOut,
                hda::PinConn::Jack,
                hda::PinLoc::External,
                hda::PinGeoLoc::Front,
                hda::PinConnector::StereoMono18,
                hda::PinColor::Green,
                0x0c,
                1,
                15,
            ))
            .pin(hda::pin_not_connected(0x1c, 0))
            .pin(hda::pin_config(
                0x1d,
                hda::PinDevice::DeviceOther,
                hda::PinConn::Nc,
                hda::PinLoc::External,
                hda::PinGeoLoc::NA,
                hda::PinConnector::Optical,
                hda::PinColor::Orange,
                0x06,
                0,
                3,
            ))
            .pin(hda::pin_config(
                0x1e,
                hda::PinDevice::SpdifOut,
                hda::PinConn::Integrated,
                hda::PinLoc::Internal,
                hda::PinGeoLoc::Special9,
                hda::PinConnector::AtapiInternal,
                hda::PinColor::ColorUnknown,
                0x01,
                2,
                0,
            )),
    )
}

pub const fn d41s_gpio_pins() -> [gpio::GpioPin; 19] {
    [
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
    ]
}

pub fn d41s_superio_config() -> ite8721f::Ite8721fConfig {
    ite8721f::Ite8721fConfig(ite8721f::SuperIoConfig {
        com1: Some(ite8721f::ComPortConfig {
            io_base: UART0_PIO_BASE,
            irq: 4,
            baud_rate: UART0_BAUD_RATE,
        }),
        com2: Some(ite8721f::ComPortConfig {
            io_base: 0x2f8,
            irq: 3,
            baud_rate: UART0_BAUD_RATE,
        }),
        parallel: Some(ite8721f::ParallelConfig {
            io_base: 0x378,
            irq: 7,
        }),
        env_controller: Some(ite8721f::EcConfig {
            io_base: 0x0a10,
            io_ext: 0x0a00,
        }),
        keyboard: Some(ite8721f::KbcConfig {
            io_base: 0x60,
            io_ext: 0x64,
            irq: 1,
        }),
        mouse: Some(ite8721f::MouseConfig { irq: 12 }),
        cir: Some(ite8721f::CirConfig {
            io_base: 0x3e0,
            irq: 10,
        }),
        gpio: None,
        acpi_name: Some(hstr("SIO0")),
        console_port: Some(hstr("com1")),
    })
}

pub fn d41s_ck505_config() -> I2cCk505Config {
    I2cCk505Config {
        mask: hvec([0x00, 0x80, 0xff, 0xff, 0xff]),
        regs: hvec([0x00, 0x80, 0xfe, 0xff, 0xfc]),
    }
}

static D41S_SMBIOS_PROCESSORS: [fstart_acpi::smbios::ProcessorDesc<'static>; 1] =
    [fstart_acpi::smbios::ProcessorDesc {
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

pub static D41S_SMBIOS_DESC: fstart_acpi::smbios::SmbiosDesc<'static> = fstart_acpi::smbios::SmbiosDesc {
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
    processors: &D41S_SMBIOS_PROCESSORS,
    memory_devices: &[],
    ram_base: 0x0010_0000,
    ram_end: 0x3fff_ffff,
};
