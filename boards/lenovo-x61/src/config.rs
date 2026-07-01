//! Lenovo ThinkPad X61 board metadata and build policy.

use fstart_driver_i2c_ck505::I2cCk505Config;
use fstart_driver_intel_gm965::Gm965IgdConfig;
use fstart_driver_nsc_pc87382 as pc87382;
use fstart_driver_nsc_pc87392 as pc87392;
use fstart_gpio_ich as gpio;
use fstart_hda as hda;
use fstart_platform_intel_gm965_ich8::{
    Gm965Ich8Config, IdeConfig, IoTrapAccess, IoTrapConfig, LpcFixedIoDecode, LpcGenericIoDecode,
    LpcParallelDecode, LpcSerialDecode, PcieRootPort, SataConfig, SataMode, UsbConfig,
};
use fstart_types::smbios::{
    ChassisType, MemoryDeviceType, ProcessorFamily, SmbiosMemoryDevice, SmbiosProcessor,
};
use fstart_types::{
    hstr, hvec, io16, x86_uefi_payload, BoardConfig, BoardInfo, BuildInfo, BusAddress, FlashLayout,
    IntelIfdFlashLayout, IntelIfdRegion, IntelIfdRegionConfig, Platform, SmbiosConfig,
};

pub const BOARD_NAME: &str = "lenovo-x61";
pub const BOARD_PACKAGE: &str = "fstart-board-lenovo-x61";
pub const PLATFORM: Platform = Platform::X86_64;
pub const UART0_NODE: &str = "dock_superio/com1";
pub const UART0_PIO_BASE: u16 = 0x3f8;
pub const UART0_CLOCK_FREQ: u32 = 1_843_200;
pub const UART0_BAUD_RATE: u32 = 115_200;

pub fn gm965_ich8_config() -> Gm965Ich8Config {
    Gm965Ich8Config::new(BOARD_NAME, BOARD_PACKAGE)
        .payload(x86_uefi_payload())
        .flash_layout(Some(x61_flash_layout()))
        .igd(x61_igd_config())
        .pcie_port(PcieRootPort::Port1, true)
        .pcie_port(PcieRootPort::Port2, true)
        .acpi_print_hex(true)
        .lpc_fixed_io(LpcFixedIoDecode {
            com_a: LpcSerialDecode::Com1,
            com_b: LpcSerialDecode::Com2,
            lpt: Some(LpcParallelDecode::Lpt3bc),
            fdd: None,
        })
        .lpc_generic_io(LpcGenericIoDecode {
            base: 0x1600,
            size: 0x0080,
        })
        .lpc_generic_io(LpcGenericIoDecode {
            base: 0x15e0,
            size: 0x0010,
        })
        .lpc_generic_io(LpcGenericIoDecode {
            base: 0x1680,
            size: 0x0020,
        })
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
        .hda(x61_hda_config())
        .gpio(x61_gpio_config())
        .io_trap(IoTrapConfig {
            index: 3,
            base: 0x0800,
            size: 0x10,
            access: IoTrapAccess::Any,
        })
        .superio("dlpc_superio", io16(0x164e), true)
        .superio("dock_superio", io16(0x2e), false)
        .on_smbus(|smbus| {
            smbus.runtime_enabled("ck505", BusAddress::I2c(0x69), false);
        })
        .mainboard()
        .smbios(x61_smbios())
}

#[must_use]
pub fn board_config() -> BoardConfig {
    gm965_ich8_config().board_config()
}

#[must_use]
pub fn board_info() -> BoardInfo {
    gm965_ich8_config().board_info()
}

#[must_use]
pub fn build_info() -> BuildInfo {
    gm965_ich8_config().build_info([
        "intel-gm965",
        "intel-ich8",
        "nsc-pc87382",
        "nsc-pc87392",
        "i2c-ck505",
    ])
}

#[must_use]
pub const fn board_name() -> &'static str {
    BOARD_NAME
}

pub fn x61_flash_layout() -> FlashLayout {
    FlashLayout::IntelIfd(IntelIfdFlashLayout {
        base: 0xFFC0_0000,
        size: 0x0040_0000,
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
    })
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

pub fn x61_igd_config() -> Gm965IgdConfig {
    Gm965IgdConfig {
        enable_vga: true,
        enable_pipe_b: true,
        gtt_mmio_base: 0xFEB0_0000,
        stolen_memory_mb: 32,
        vbt_file: Some(hstr("data.vbt")),
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

pub fn x61_hda_config() -> hda::HdaConfig {
    hda::HdaConfig {
        verbs: hvec([hda::HdaVerbTable {
            vendor_id: 0x11d4_1984,
            subsystem_id: 0x17aa_20d6,
            pins: hvec([
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
            ]),
            extra_verbs: hvec([
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
            ]),
        }]),
    }
}

pub fn x61_gpio_config() -> gpio::GpioConfig {
    gpio::GpioConfig {
        pins: hvec([
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
        ]),
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn x61_build_features_do_not_select_deleted_mainboard_crate() {
        let build = build_info();
        assert!(!build
            .features
            .iter()
            .any(|feature| feature.as_str() == "lenovo-x61-mainboard"));
    }

    #[test]
    fn x61_board_config_keeps_board_owned_mainboard_hook() {
        let config = board_config();
        let mainboard = config
            .devices
            .iter()
            .find(|device| device.name.as_str() == "mainboard")
            .expect("X61 board hook device is declared");
        assert!(mainboard.enabled);
    }
}
