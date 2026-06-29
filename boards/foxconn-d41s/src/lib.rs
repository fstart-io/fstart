//! Foxconn D41S Rust board metadata.

use fstart_board_meta::DriverBinding;
use fstart_driver_i2c_ck505::I2cCk505Config;
use fstart_driver_ite8721f as ite8721f;
use fstart_gpio_ich as gpio;
use fstart_hda as hda;
use fstart_platform_intel_pineview_ich7::{
    LpcGenericIoDecode, PcieRootPort, PineviewIch7Platform, SataConfig, SataMode, UsbConfig,
};
use fstart_types::smbios::{ChassisType, ProcessorFamily, SmbiosProcessor};
use fstart_types::{
    hstr, hvec, io16, BoardConfig, BoardInfo, BuildInfo, BusAddress, Platform, SmbiosConfig,
};

pub const BOARD_NAME: &str = "foxconn-d41s";
pub const BOARD_PACKAGE: &str = "fstart-board-foxconn-d41s";
pub const PLATFORM: Platform = Platform::X86_64;

fn board() -> PineviewIch7Platform {
    PineviewIch7Platform::new(BOARD_NAME, BOARD_PACKAGE)
        .pcie_port(PcieRootPort::Port0, true)
        .pcie_port(PcieRootPort::Port1, true)
        .lpc_generic_io(LpcGenericIoDecode {
            base: 0x0a00,
            size: 0x0100,
        })
        .gpe0_en(0x441)
        .sata(SataConfig {
            mode: SataMode::Ahci,
            ports: 0x3,
        })
        .usb(UsbConfig {
            ehci: true,
            uhci: [true, true, true, true],
        })
        .hda(d41s_hda_config())
        .gpio(d41s_gpio_config())
        .superio("superio", io16(0x2e), d41s_superio_config())
        .on_smbus(|smbus| {
            smbus.runtime("ck505", BusAddress::I2c(0x69), d41s_ck505_config());
        })
        .smbios(d41s_smbios())
}

#[must_use]
pub fn board_config() -> BoardConfig {
    board().board_config()
}

#[must_use]
pub fn driver_bindings() -> Vec<DriverBinding> {
    board().driver_bindings()
}

#[must_use]
pub fn board_info() -> BoardInfo {
    board().board_info()
}

#[must_use]
pub fn build_info() -> BuildInfo {
    board().build_info()
}

#[must_use]
pub const fn board_name() -> &'static str {
    BOARD_NAME
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

pub fn d41s_superio_config() -> ite8721f::Ite8721fConfig {
    ite8721f::Ite8721fConfig(ite8721f::SuperIoConfig {
        com1: Some(ite8721f::ComPortConfig {
            io_base: 0x3f8,
            irq: 4,
            baud_rate: 115200,
        }),
        com2: Some(ite8721f::ComPortConfig {
            io_base: 0x2f8,
            irq: 3,
            baud_rate: 115200,
        }),
        parallel: Some(ite8721f::ParallelConfig {
            io_base: 0x378,
            irq: 7,
        }),
        env_controller: Some(ite8721f::EcConfig {
            io_base: 0xa10,
            io_ext: 0xa00,
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

pub fn d41s_smbios() -> SmbiosConfig {
    let processors = hvec([SmbiosProcessor {
        socket: hstr("FCBGA559"),
        manufacturer: hstr("Intel"),
        processor_family: ProcessorFamily::X86_64,
        max_speed_mhz: None,
        core_count: None,
        thread_count: None,
        caches: hvec([]),
    }]);

    SmbiosConfig {
        bios_vendor: hstr("fstart"),
        bios_version: hstr("0.1.0"),
        bios_release_date: hstr(option_env!("FSTART_SMBIOS_DATE").unwrap_or("04/15/2026")),
        system_manufacturer: hstr("Foxconn"),
        system_product: hstr("D41S"),
        system_version: hstr("1.0"),
        system_serial: Default::default(),
        baseboard_manufacturer: hstr("Foxconn"),
        baseboard_product: hstr("D41S"),
        chassis_type: ChassisType::Desktop,
        chassis_manufacturer: hstr("Foxconn"),
        processors,
        memory_devices: hvec([]),
    }
}
