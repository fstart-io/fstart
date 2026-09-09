//! Lenovo ThinkPad X61 chipset and attached-device configuration.

use fstart_core::{Platform, hvec};
use fstart_driver_intel::generic::ck505::I2cCk505Config;
use fstart_driver_intel::southbridge::gpio_ich as gpio;
use fstart_driver_intel::southbridge::hda;
use fstart_driver_superio::pc87382;
use fstart_driver_superio::pc87392;
use fstart_platform_intel::gm965::{
    Gm965Ich8Config, Gm965IgdConfig, IdeConfig, IoTrapAccess, IoTrapConfig, LpcFixedIoDecode,
    LpcGenericIoDecode, LpcParallelDecode, LpcSerialDecode, SataConfig, SataMode, UsbConfig,
};

/// Factory 4-MiB SPI image map, const-validated in the real board source.
pub const FLASH: fstart_core::IntelIfdFlashLayout = {
    use fstart_core::{
        ConstVec, IntelIfdFlashLayout, IntelIfdRegion as Kind, IntelIfdRegionConfig as Region,
    };
    const DESCRIPTOR: Region = Region {
        kind: Kind::Descriptor,
        offset: 0,
        size: 0x1000,
    };
    IntelIfdFlashLayout::new(
        ConstVec::new(DESCRIPTOR)
            .push(DESCRIPTOR)
            .push(Region {
                kind: Kind::Gbe,
                offset: 0x1000,
                size: 0x2000,
            })
            .push(Region {
                kind: Kind::Me,
                offset: 0x3000,
                size: 0x27d000,
            })
            .push(Region {
                kind: Kind::Bios,
                offset: 0x280000,
                size: 0x180000,
            }),
    )
};

impl fstart_platform_intel::facts::IntelBoardFacts for crate::Board {
    const FACTS: fstart_platform_intel::facts::BoardFacts =
        fstart_platform_intel::facts::BoardFacts::new(
            fstart_core::FlashLayout::IntelIfd(FLASH),
            0x400000,
            2,
            fstart_platform_intel::facts::Chipset::Gm965Ich8,
        );
}

pub const BOARD_NAME: &str = "lenovo-x61";
pub const BOARD_PACKAGE: &str = "fstart-board-lenovo-x61";
pub const PLATFORM: Platform = Platform::X86_64;
pub const UART0_NODE: &str = "dock_superio/com1";
pub const UART0_PIO_BASE: u16 = 0x3f8;
pub const UART0_CLOCK_FREQ: u32 = 1_843_200;
pub const UART0_BAUD_RATE: u32 = 115_200;
pub static X61_PLATFORM: Gm965Ich8Config = Gm965Ich8Config::new()
    .igd(x61_igd_config())
    .pcie_port(0, true)
    .pcie_port(1, true)
    .lpc_fixed_io(LpcFixedIoDecode {
        com_a: LpcSerialDecode::Com1,
        com_b: LpcSerialDecode::Com2,
        lpt: Some(LpcParallelDecode::Lpt3bc),
        fdd: None,
    })
    .lpc_generic_io([
        LpcGenericIoDecode {
            base: 0x1600,
            size: 0x0080,
        },
        LpcGenericIoDecode {
            base: 0x15e0,
            size: 0x0010,
        },
        LpcGenericIoDecode {
            base: 0x1680,
            size: 0x0020,
        },
    ])
    .gpe0_en(0x0104_0046)
    .gpi_routing([0, 0, 2, 0, 0, 0, 0, 0, 2, 0, 0, 0, 0, 2, 0, 0])
    .ide(IdeConfig {
        enable_primary: true,
        enable_secondary: false,
    })
    .sata(SataConfig {
        mode: SataMode::Ahci,
        ports: 0x01,
        hotplug_map: 0,
        clock_request: false,
        traffic_monitor: false,
    })
    .usb(UsbConfig {
        ehci: [true, true],
        uhci: [true, true, true, true, true, true],
    })
    .hda_verb(
        0x11d4_1984,
        0x17aa_20d6,
        x61_hda_pins(),
        x61_hda_extra_verbs(),
    )
    .gpio_pins(x61_gpio_pins())
    .io_traps([IoTrapConfig {
        index: 3,
        base: 0x0800,
        size: 0x10,
        access: IoTrapAccess::Any,
    }])
    .build();

// ---------------------------------------------------------------------------
// Board device configuration facts
// ---------------------------------------------------------------------------

pub const fn x61_igd_config() -> Gm965IgdConfig {
    Gm965IgdConfig {
        enable_vga: true,
        enable_pipe_b: true,
        gtt_mmio_base: 0xFEB0_0000,
        stolen_memory_mb: 32,
        vbt_file: Some("data.vbt"),
        vbt_addr: None,
        vbt_size: 0,
        legacy_vbt_probe: Some(0x000C_0000),
        panel_power_up_delay: 2000,
        panel_power_down_delay: 2000,
        panel_backlight_on_delay: 2000,
        panel_backlight_off_delay: 2000,
        panel_power_cycle_delay: 6,
        default_pwm_freq: 0,
        duty_cycle: 100,
    }
}

pub const fn x61_hda_pins() -> [hda::PinConfig; 11] {
    [
        hda::pin_config(
            0x11,
            hda::PinDevice::HpOut,
            hda::PinConn::Jack,
            hda::PinLoc::External,
            hda::PinGeoLoc::Right,
            hda::PinConnector::StereoMono18,
            hda::PinColor::Green,
            0,
            1,
            15,
        ),
        hda::pin_config(
            0x12,
            hda::PinDevice::Speaker,
            hda::PinConn::Integrated,
            hda::PinLoc::Internal,
            hda::PinGeoLoc::NA,
            hda::PinConnector::OtherAnalog,
            hda::PinColor::ColorUnknown,
            1,
            1,
            0,
        ),
        hda::pin_not_connected(0x13, 0),
        hda::pin_config(
            0x14,
            hda::PinDevice::MicIn,
            hda::PinConn::Jack,
            hda::PinLoc::External,
            hda::PinGeoLoc::Right,
            hda::PinConnector::StereoMono18,
            hda::PinColor::Red,
            0,
            2,
            1,
        ),
        hda::pin_config(
            0x15,
            hda::PinDevice::MicIn,
            hda::PinConn::Integrated,
            hda::PinLoc::Internal,
            hda::PinGeoLoc::NA,
            hda::PinConnector::OtherAnalog,
            hda::PinColor::ColorUnknown,
            1,
            2,
            14,
        ),
        hda::pin_not_connected(0x16, 1),
        hda::pin_not_connected(0x17, 2),
        hda::pin_not_connected(0x18, 3),
        hda::pin_not_connected(0x1a, 4),
        hda::pin_not_connected(0x1b, 5),
        hda::pin_config(
            0x1c,
            hda::PinDevice::MicIn,
            hda::PinConn::Jack,
            hda::PinLoc::SeparateChassis,
            hda::PinGeoLoc::Rear,
            hda::PinConnector::StereoMono18,
            hda::PinColor::Red,
            0,
            2,
            0,
        ),
    ]
}

pub const fn x61_hda_extra_verbs() -> [u32; 16] {
    [
        0x00c3_b027,
        0x00d3_b027,
        0x0073_7100,
        0x00a3_7100,
        0x0203_7318,
        0x0213_b01f,
        0x0113_b000,
        0x0123_b000,
        0x0037_0500,
        0x0047_0500,
        0x0057_0500,
        0x0067_0500,
        0x0087_0500,
        0x0097_0500,
        0x0197_0500,
        0x0127_0c02,
    ]
}

pub const fn x61_gpio_pins() -> [gpio::GpioPin; 35] {
    [
        gpio::GpioPin {
            pin: 0,
            mode: gpio::GpioMode::Native,
            dir: gpio::GpioDir::Output,
            level: gpio::GpioLevel::High,
            blink: false,
            invert: true,
            reset: gpio::GpioReset::Pwrok,
        },
        gpio::GpioPin {
            pin: 1,
            mode: gpio::GpioMode::Gpio,
            dir: gpio::GpioDir::Input,
            level: gpio::GpioLevel::High,
            blink: false,
            invert: true,
            reset: gpio::GpioReset::Pwrok,
        },
        gpio::GpioPin {
            pin: 2,
            mode: gpio::GpioMode::Gpio,
            dir: gpio::GpioDir::Input,
            level: gpio::GpioLevel::High,
            blink: false,
            invert: true,
            reset: gpio::GpioReset::Pwrok,
        },
        gpio::GpioPin {
            pin: 3,
            mode: gpio::GpioMode::Gpio,
            dir: gpio::GpioDir::Input,
            level: gpio::GpioLevel::High,
            blink: false,
            invert: true,
            reset: gpio::GpioReset::Pwrok,
        },
        gpio::GpioPin {
            pin: 4,
            mode: gpio::GpioMode::Gpio,
            dir: gpio::GpioDir::Input,
            level: gpio::GpioLevel::High,
            blink: false,
            invert: true,
            reset: gpio::GpioReset::Pwrok,
        },
        gpio::GpioPin {
            pin: 5,
            mode: gpio::GpioMode::Gpio,
            dir: gpio::GpioDir::Input,
            level: gpio::GpioLevel::High,
            blink: false,
            invert: true,
            reset: gpio::GpioReset::Pwrok,
        },
        gpio::GpioPin {
            pin: 6,
            mode: gpio::GpioMode::Gpio,
            dir: gpio::GpioDir::Input,
            level: gpio::GpioLevel::High,
            blink: false,
            invert: true,
            reset: gpio::GpioReset::Pwrok,
        },
        gpio::GpioPin {
            pin: 7,
            mode: gpio::GpioMode::Gpio,
            dir: gpio::GpioDir::Input,
            level: gpio::GpioLevel::High,
            blink: false,
            invert: true,
            reset: gpio::GpioReset::Pwrok,
        },
        gpio::GpioPin {
            pin: 8,
            mode: gpio::GpioMode::Gpio,
            dir: gpio::GpioDir::Input,
            level: gpio::GpioLevel::High,
            blink: false,
            invert: true,
            reset: gpio::GpioReset::Pwrok,
        },
        gpio::GpioPin {
            pin: 9,
            mode: gpio::GpioMode::Gpio,
            dir: gpio::GpioDir::Output,
            level: gpio::GpioLevel::Low,
            blink: false,
            invert: false,
            reset: gpio::GpioReset::Pwrok,
        },
        gpio::GpioPin {
            pin: 11,
            mode: gpio::GpioMode::Native,
            dir: gpio::GpioDir::Input,
            level: gpio::GpioLevel::High,
            blink: false,
            invert: true,
            reset: gpio::GpioReset::Pwrok,
        },
        gpio::GpioPin {
            pin: 12,
            mode: gpio::GpioMode::Native,
            dir: gpio::GpioDir::Output,
            level: gpio::GpioLevel::High,
            blink: false,
            invert: true,
            reset: gpio::GpioReset::Pwrok,
        },
        gpio::GpioPin {
            pin: 13,
            mode: gpio::GpioMode::Native,
            dir: gpio::GpioDir::Input,
            level: gpio::GpioLevel::Low,
            blink: false,
            invert: true,
            reset: gpio::GpioReset::Pwrok,
        },
        gpio::GpioPin {
            pin: 17,
            mode: gpio::GpioMode::Gpio,
            dir: gpio::GpioDir::Input,
            level: gpio::GpioLevel::High,
            blink: false,
            invert: false,
            reset: gpio::GpioReset::Pwrok,
        },
        gpio::GpioPin {
            pin: 18,
            mode: gpio::GpioMode::Gpio,
            dir: gpio::GpioDir::Input,
            level: gpio::GpioLevel::Low,
            blink: false,
            invert: false,
            reset: gpio::GpioReset::Pwrok,
        },
        gpio::GpioPin {
            pin: 19,
            mode: gpio::GpioMode::Gpio,
            dir: gpio::GpioDir::Input,
            level: gpio::GpioLevel::Low,
            blink: false,
            invert: false,
            reset: gpio::GpioReset::Pwrok,
        },
        gpio::GpioPin {
            pin: 20,
            mode: gpio::GpioMode::Gpio,
            dir: gpio::GpioDir::Output,
            level: gpio::GpioLevel::High,
            blink: false,
            invert: false,
            reset: gpio::GpioReset::Pwrok,
        },
        gpio::GpioPin {
            pin: 21,
            mode: gpio::GpioMode::Gpio,
            dir: gpio::GpioDir::Input,
            level: gpio::GpioLevel::Low,
            blink: false,
            invert: false,
            reset: gpio::GpioReset::Pwrok,
        },
        gpio::GpioPin {
            pin: 22,
            mode: gpio::GpioMode::Gpio,
            dir: gpio::GpioDir::Input,
            level: gpio::GpioLevel::Low,
            blink: false,
            invert: false,
            reset: gpio::GpioReset::Pwrok,
        },
        gpio::GpioPin {
            pin: 24,
            mode: gpio::GpioMode::Gpio,
            dir: gpio::GpioDir::Input,
            level: gpio::GpioLevel::High,
            blink: false,
            invert: false,
            reset: gpio::GpioReset::Pwrok,
        },
        gpio::GpioPin {
            pin: 27,
            mode: gpio::GpioMode::Gpio,
            dir: gpio::GpioDir::Output,
            level: gpio::GpioLevel::Low,
            blink: false,
            invert: false,
            reset: gpio::GpioReset::Pwrok,
        },
        gpio::GpioPin {
            pin: 28,
            mode: gpio::GpioMode::Gpio,
            dir: gpio::GpioDir::Output,
            level: gpio::GpioLevel::Low,
            blink: false,
            invert: false,
            reset: gpio::GpioReset::Pwrok,
        },
        gpio::GpioPin {
            pin: 29,
            mode: gpio::GpioMode::Gpio,
            dir: gpio::GpioDir::Output,
            level: gpio::GpioLevel::High,
            blink: false,
            invert: false,
            reset: gpio::GpioReset::Pwrok,
        },
        gpio::GpioPin {
            pin: 30,
            mode: gpio::GpioMode::Gpio,
            dir: gpio::GpioDir::Input,
            level: gpio::GpioLevel::High,
            blink: false,
            invert: false,
            reset: gpio::GpioReset::Pwrok,
        },
        gpio::GpioPin {
            pin: 31,
            mode: gpio::GpioMode::Gpio,
            dir: gpio::GpioDir::Input,
            level: gpio::GpioLevel::High,
            blink: false,
            invert: false,
            reset: gpio::GpioReset::Pwrok,
        },
        gpio::GpioPin {
            pin: 33,
            mode: gpio::GpioMode::Gpio,
            dir: gpio::GpioDir::Output,
            level: gpio::GpioLevel::High,
            blink: false,
            invert: false,
            reset: gpio::GpioReset::Pwrok,
        },
        gpio::GpioPin {
            pin: 34,
            mode: gpio::GpioMode::Gpio,
            dir: gpio::GpioDir::Output,
            level: gpio::GpioLevel::Low,
            blink: false,
            invert: false,
            reset: gpio::GpioReset::Pwrok,
        },
        gpio::GpioPin {
            pin: 36,
            mode: gpio::GpioMode::Gpio,
            dir: gpio::GpioDir::Input,
            level: gpio::GpioLevel::High,
            blink: false,
            invert: false,
            reset: gpio::GpioReset::Pwrok,
        },
        gpio::GpioPin {
            pin: 37,
            mode: gpio::GpioMode::Gpio,
            dir: gpio::GpioDir::Input,
            level: gpio::GpioLevel::High,
            blink: false,
            invert: false,
            reset: gpio::GpioReset::Pwrok,
        },
        gpio::GpioPin {
            pin: 38,
            mode: gpio::GpioMode::Gpio,
            dir: gpio::GpioDir::Input,
            level: gpio::GpioLevel::High,
            blink: false,
            invert: false,
            reset: gpio::GpioReset::Pwrok,
        },
        gpio::GpioPin {
            pin: 39,
            mode: gpio::GpioMode::Gpio,
            dir: gpio::GpioDir::Input,
            level: gpio::GpioLevel::Low,
            blink: false,
            invert: false,
            reset: gpio::GpioReset::Pwrok,
        },
        gpio::GpioPin {
            pin: 41,
            mode: gpio::GpioMode::Gpio,
            dir: gpio::GpioDir::Output,
            level: gpio::GpioLevel::High,
            blink: false,
            invert: false,
            reset: gpio::GpioReset::Pwrok,
        },
        gpio::GpioPin {
            pin: 42,
            mode: gpio::GpioMode::Gpio,
            dir: gpio::GpioDir::Output,
            level: gpio::GpioLevel::High,
            blink: false,
            invert: false,
            reset: gpio::GpioReset::Pwrok,
        },
        gpio::GpioPin {
            pin: 43,
            mode: gpio::GpioMode::Gpio,
            dir: gpio::GpioDir::Output,
            level: gpio::GpioLevel::Low,
            blink: false,
            invert: false,
            reset: gpio::GpioReset::Pwrok,
        },
        gpio::GpioPin {
            pin: 48,
            mode: gpio::GpioMode::Gpio,
            dir: gpio::GpioDir::Input,
            level: gpio::GpioLevel::Low,
            blink: false,
            invert: false,
            reset: gpio::GpioReset::Pwrok,
        },
    ]
}

pub fn x61_dlpc_superio_config() -> pc87382::Pc87382Config {
    pc87382::Pc87382Config(pc87382::SuperIoConfig {
        com2: Some(pc87382::ComPortConfig {
            io_base: 0x2f8,
            irq: 0,
            baud_rate: 115200,
        }),
        cir: Some(pc87382::CirConfig {
            io_base: 0x200,
            irq: 5,
        }),
        gpio: Some(pc87382::GpioConfig { io_base: 0x1680 }),
        ..Default::default()
    })
}

pub fn x61_dock_superio_config() -> pc87392::Pc87392Config {
    pc87392::Pc87392Config(pc87392::SuperIoConfig {
        com1: Some(pc87392::ComPortConfig {
            io_base: 0x3f8,
            irq: 4,
            baud_rate: 115200,
        }),
        parallel: Some(pc87392::ParallelConfig {
            io_base: 0x3bc,
            irq: 7,
        }),
        gpio: Some(pc87392::GpioConfig { io_base: 0x1620 }),
        ..Default::default()
    })
}

pub fn x61_ck505_config() -> I2cCk505Config {
    I2cCk505Config {
        mask: hvec([0xff, 0, 0, 0, 0]),
        regs: hvec([0x11, 0, 0, 0, 0]),
    }
}
