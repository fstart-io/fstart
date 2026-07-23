//! Lenovo ThinkPad X61 board metadata and build policy.

use fstart_core::smbios::{
    ChassisType, MemoryDeviceType, ProcessorFamily, SmbiosMemoryDevice, SmbiosProcessor,
};
#[cfg(feature = "host")]
use fstart_core::{
    dev_security_config, AcpiConfig, AcpiPlatform, BoardBuildPolicy, BoardConfig, SmmConfig,
};
use fstart_core::{
    hstr, hvec, FlashLayout, IntelIfdFlashLayout, IntelIfdRegion, IntelIfdRegionConfig, Platform,
    SmbiosConfig,
};
use fstart_driver_intel::generic::ck505::I2cCk505Config;
use fstart_driver_intel::southbridge::gpio_ich as gpio;
use fstart_driver_superio::pc87382;
use fstart_driver_superio::pc87392;
use fstart_driver_intel::southbridge::hda;
#[cfg(feature = "host")]
use fstart_platform_intel::gm965::{gm965_ich8_memory, gm965_ich8_microcode, gm965_ich8_stages};
use fstart_platform_intel::gm965::{
    Gm965Ich8Config, Gm965IgdConfig, IdeConfig, IoTrapAccess, IoTrapConfig, LpcFixedIoDecode,
    LpcGenericIoDecode, LpcParallelDecode, LpcSerialDecode, SataConfig, SataMode, UsbConfig,
};

pub const BOARD_NAME: &str = "lenovo-x61";
pub const BOARD_PACKAGE: &str = "fstart-board-lenovo-x61";
pub const PLATFORM: Platform = Platform::X86_64;
pub const UART0_NODE: &str = "dock_superio/com1";
pub const UART0_PIO_BASE: u16 = 0x3f8;
pub const UART0_CLOCK_FREQ: u32 = 1_843_200;
pub const UART0_BAUD_RATE: u32 = 115_200;
pub static X61_PLATFORM: Gm965Ich8Config = Gm965Ich8Config::new()
    .max_cpus(2)
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

#[cfg(feature = "host")]
#[must_use]
pub fn board_config() -> BoardConfig {
    let flash_layout = x61_flash_layout();

    BoardConfig {
        name: hstr(BOARD_NAME),
        platform: PLATFORM,
        memory: gm965_ich8_memory(Some(flash_layout)),
        stages: gm965_ich8_stages(&X61_PLATFORM),
        security: dev_security_config("keys/dev-signing.pub"),
        payload: None,
        microcode: Some(gm965_ich8_microcode()),
        soc_image_format: Default::default(),
        full_flash_image: true,
        acpi: Some(AcpiConfig {
            print_hex: true,
            platform: AcpiPlatform::X86,
        }),
        smbios: Some(x61_smbios()),
        smm: Some(SmmConfig::default()),
        build: BoardBuildPolicy {
            cpu_feature: Some(hstr("cpu-intel-core2")),
            ..Default::default()
        },
        boot_hart_id: 0,
    }
}

#[must_use]
pub const fn board_name() -> &'static str {
    BOARD_NAME
}

pub fn x61_flash_layout() -> FlashLayout {
    FlashLayout::IntelIfd(x61_ifd_flash_layout())
}

pub fn x61_ifd_flash_layout() -> IntelIfdFlashLayout {
    IntelIfdFlashLayout {
        regions: hvec([
            IntelIfdRegionConfig {
                kind: IntelIfdRegion::Descriptor,
                offset: 0x000000,
                size: 0x001000,
                file: None,
            },
            IntelIfdRegionConfig {
                kind: IntelIfdRegion::Gbe,
                offset: 0x001000,
                size: 0x002000,
                file: None,
            },
            IntelIfdRegionConfig {
                kind: IntelIfdRegion::Me,
                offset: 0x003000,
                size: 0x27D000,
                file: None,
            },
            IntelIfdRegionConfig {
                kind: IntelIfdRegion::Bios,
                offset: 0x280000,
                size: 0x180000,
                file: None,
            },
        ]),
    }
}

pub fn x61_smbios() -> SmbiosConfig {
    SmbiosConfig {
        bios_vendor: hstr("fstart"),
        bios_version: hstr("0.1.0"),
        bios_release_date: hstr(option_env!("FSTART_SMBIOS_DATE").unwrap_or("04/15/2026")),
        system_manufacturer: hstr("LENOVO"),
        system_product: hstr("ThinkPad X61"),
        system_version: hstr("1.0"),
        system_serial: hstr(""),
        baseboard_manufacturer: hstr("LENOVO"),
        baseboard_product: hstr("ThinkPad X61"),
        chassis_type: ChassisType::Other,
        chassis_manufacturer: hstr("LENOVO"),
        processors: hvec([SmbiosProcessor {
            socket: hstr("Socket M"),
            manufacturer: hstr("Intel"),
            processor_family: ProcessorFamily::X86_64,
            max_speed_mhz: None,
            core_count: None,
            thread_count: None,
            caches: hvec([]),
        }]),
        memory_devices: hvec([
            SmbiosMemoryDevice {
                locator: hstr("DIMM0"),
                size_mb: None,
                speed_mhz: None,
                memory_type: Some(MemoryDeviceType::Unknown),
            },
            SmbiosMemoryDevice {
                locator: hstr("DIMM1"),
                size_mb: None,
                speed_mhz: None,
                memory_type: Some(MemoryDeviceType::Unknown),
            },
        ]),
    }
}

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
