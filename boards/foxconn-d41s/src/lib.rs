//! Foxconn D41S Rust board metadata.

#![no_std]

use fstart_gpio_ich as gpio;
use fstart_hda as hda;
use fstart_platform_intel_pineview_ich7::{
    IgdConfig, LpcGenericIoDecode, PcieRootPort, PineviewIch7Config, SataConfig, SataMode,
    UsbConfig,
};
use fstart_types::smbios::{ChassisType, ProcessorFamily, SmbiosProcessor};
use fstart_types::{
    hstr, hvec, io16, BoardConfig, BoardInfo, BuildInfo, BusAddress, Platform, SmbiosConfig,
};

pub const BOARD_NAME: &str = "foxconn-d41s";
pub const BOARD_PACKAGE: &str = "fstart-board-foxconn-d41s";
pub const PLATFORM: Platform = Platform::X86_64;

pub fn d41s_pineview_ich7_config(
    board_name: &'static str,
    board_package: &'static str,
) -> PineviewIch7Config {
    PineviewIch7Config::new(board_name, board_package)
        .pineview(|pineview| {
            pineview.igd = Some(IgdConfig {
                use_crt: true,
                use_lvds: false,
                spread_spectrum: false,
                vbt_file: Some(hstr("data.vbt")),
            });
            pineview.spd_addresses = [0x50, 0x51, 0, 0];
            pineview.ck505_pre_raminit = true;
            pineview.acpi_name = Some(hstr("MCHC"));
        })
        .ich7(|ich7| {
            ich7.pirq_routing = [0x0b; 8];
            ich7.pata = false;
            ich7.smbus_base = 0x0400;
            ich7.acpi_name = Some(hstr("LPCB"));
            ich7.c3_latency = 85;
            ich7.power_on_after_fail = 0;
        })
        .pcie_port(PcieRootPort::Port0, true)
        .pcie_port(PcieRootPort::Port1, true)
        .lpc_generic_io(LpcGenericIoDecode {
            base: 0x0a00,
            size: 0x0100,
        })
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
        .gpe0_en(0x441)
        .superio("superio", io16(0x2e))
        .on_smbus(|smbus| {
            smbus.runtime("ck505", BusAddress::I2c(0x69));
        })
        .smbios(d41s_smbios())
}

pub fn pineview_ich7_config() -> PineviewIch7Config {
    d41s_pineview_ich7_config(BOARD_NAME, BOARD_PACKAGE)
}

#[must_use]
pub fn board_config() -> BoardConfig {
    pineview_ich7_config().board_config()
}

#[must_use]
pub fn board_info() -> BoardInfo {
    pineview_ich7_config().board_info()
}

#[must_use]
pub fn build_info() -> BuildInfo {
    pineview_ich7_config().build_info(["intel-pineview", "intel-ich7", "ite8721f", "i2c-ck505"])
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
