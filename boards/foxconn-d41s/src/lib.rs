//! Foxconn D41S Rust board metadata.

use fstart_board_intel_pineview_ich7::PineviewIch7Board;
use fstart_device_registry::DriverInstance;
use fstart_gpio_ich as gpio;
use fstart_hda as hda;
use fstart_types::{BoardConfig, BoardInfo, BuildInfo, Platform};
use heapless::Vec as HVec;

pub const BOARD_NAME: &str = "foxconn-d41s";
pub const BOARD_PACKAGE: &str = "fstart-board-foxconn-d41s";
pub const PLATFORM: Platform = Platform::X86_64;

fn board() -> PineviewIch7Board {
    PineviewIch7Board::new(BOARD_NAME, BOARD_PACKAGE)
        .hda(d41s_hda_config())
        .gpio(d41s_gpio_config())
}

#[must_use]
pub fn board_config() -> BoardConfig {
    board().board_config()
}

#[must_use]
pub fn driver_instances() -> Vec<DriverInstance> {
    board().driver_instances()
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
                hda_pin(
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
                hda_pin_nc(0x15, 0),
                hda_pin_nc(0x16, 0),
                hda_pin(
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
                hda_pin(
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
                hda_pin(
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
                hda_pin(
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
                hda_pin_nc(0x1c, 0),
                hda_pin(
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
                hda_pin(
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
            extra_verbs: HVec::new(),
        }]),
    }
}

fn hda_pin(
    nid: u8,
    device: hda::PinDevice,
    conn: hda::PinConn,
    loc: hda::PinLoc,
    geo: hda::PinGeoLoc,
    connector: hda::PinConnector,
    color: hda::PinColor,
    misc: u8,
    group: u8,
    seq: u8,
) -> hda::PinConfig {
    hda::PinConfig {
        nid,
        nc: None,
        conn,
        loc,
        geo,
        device,
        connector,
        color,
        misc,
        group,
        seq,
    }
}

fn hda_pin_nc(nid: u8, seq: u8) -> hda::PinConfig {
    hda::PinConfig {
        nid,
        nc: Some(seq),
        conn: Default::default(),
        loc: Default::default(),
        geo: Default::default(),
        device: Default::default(),
        connector: Default::default(),
        color: Default::default(),
        misc: 0,
        group: 0,
        seq: 0,
    }
}

pub fn d41s_gpio_config() -> gpio::GpioConfig {
    gpio::GpioConfig {
        pins: hvec([
            gpio_output(0),
            gpio_output(6),
            gpio_output(7),
            gpio_output(8),
            gpio_output(9),
            gpio_output(10),
            gpio_output(12),
            gpio_output(13),
            gpio_output(14),
            gpio_output(15),
            gpio_output(24),
            gpio_output(25),
            gpio_output(26),
            gpio_output(27),
            gpio_output(28),
            gpio_input(33),
            gpio_input(34),
            gpio_input(38),
            gpio_input(39),
        ]),
    }
}

fn gpio_output(pin: u8) -> gpio::GpioPin {
    gpio::GpioPin {
        pin,
        mode: gpio::GpioMode::Gpio,
        dir: gpio::GpioDir::Output,
        level: gpio::GpioLevel::Low,
        blink: false,
        invert: false,
        reset: gpio::GpioReset::Pwrok,
    }
}

fn gpio_input(pin: u8) -> gpio::GpioPin {
    gpio::GpioPin {
        dir: gpio::GpioDir::Input,
        ..gpio_output(pin)
    }
}

fn hvec<T, const N: usize, const C: usize>(items: [T; N]) -> HVec<T, C> {
    let mut out = HVec::new();
    for item in items {
        out.push(item).ok().expect("heapless vec capacity");
    }
    out
}
