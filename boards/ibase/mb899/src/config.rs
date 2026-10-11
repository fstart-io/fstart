//! MB899 wiring/policy from coreboot's board sources, not register scripts.
use fstart_core::{FlashLayout, Platform, X86LegacyFlashLayout, hstr};
use fstart_driver_intel::southbridge::gpio_ich as gpio;
use fstart_driver_intel::southbridge::pirq::{PciPin as P, PinRoute as R, Pirq as L, SlotPins};
use fstart_driver_superio::w83627ehg::GpioPinConfig as Pin;
use fstart_driver_superio::winbond_hwm::{
    FanOutputMode, FullSpeedCooling, MonitorConfig, TemperatureConfig, TemperatureSensor,
};
use fstart_driver_superio::{ComPortConfig, KbcConfig, MouseConfig, SuperIoConfig};
use fstart_intel_gma::framebuffer::FramebufferConfig;
use fstart_intel_gma::{FallbackMode, OutputConfig, Port};
use fstart_platform_intel::i945::*;
use fstart_platform_intel::igd::{IgdDisplayPolicy, VbtSource};

pub type Hardware = I945Ich7<fstart_platform_intel::legacy_cpu::SocketM<0xfef0_0000, 0x80000>>;
pub const BOARD_NAME: &str = "ibase-mb899";
pub const BOARD_PACKAGE: &str = "fstart-board-ibase-mb899";
pub const PLATFORM: Platform = Platform::X86;
pub const UART0_PIO_BASE: u16 = 0x3f8;
pub const UART0_CLOCK_FREQ: u32 = 1_843_200;
pub const UART0_BAUD_RATE: u32 = 115_200;
pub const SUPERIO_PNP_BASE: u16 = 0x4e;
pub const HWM_BASE: u16 = 0x290;
pub const FLASH_SIZE: u32 = 0x8_0000;
pub const FLASH: FlashLayout = FlashLayout::X86Legacy(X86LegacyFlashLayout { size: FLASH_SIZE });

// RCBA pin assignments from early_init.c; unchanged blocks use reset values.
// PCI-bridge wiring uses the board's ACPI _PRT, not contradictory legacy tables.
pub const MB899_PIRQ: PirqRouting = PirqRouting {
    slot_pins: &[
        SlotPins {
            slot: 0x1b,
            functions: [Some(P::A), None, None, None, None, None, None, None],
        },
        SlotPins {
            slot: 0x1c,
            functions: [
                Some(P::A),
                Some(P::B),
                Some(P::C),
                Some(P::D),
                Some(P::A),
                Some(P::B),
                None,
                None,
            ],
        },
        SlotPins {
            slot: 0x1d,
            functions: [
                Some(P::A),
                Some(P::B),
                Some(P::C),
                Some(P::D),
                None,
                None,
                None,
                Some(P::A),
            ],
        },
        SlotPins {
            slot: 0x1e,
            functions: [None, None, Some(P::A), Some(P::B), None, None, None, None],
        },
        SlotPins {
            slot: 0x1f,
            functions: [
                None,
                Some(P::A),
                Some(P::B),
                Some(P::B),
                Some(P::D),
                None,
                None,
                None,
            ],
        },
    ],
    pin_routes: &[
        R::new(0x02, P::A, L::A),
        R::new(0x02, P::B, L::B),
        R::new(0x1b, P::A, L::G),
        R::new(0x1b, P::B, L::E),
        R::new(0x1b, P::C, L::B),
        R::new(0x1b, P::D, L::A),
        R::new(0x1c, P::A, L::B),
        R::new(0x1c, P::B, L::A),
        R::new(0x1c, P::C, L::C),
        R::new(0x1c, P::D, L::D),
        R::new(0x1d, P::A, L::H),
        R::new(0x1d, P::B, L::D),
        R::new(0x1d, P::C, L::C),
        R::new(0x1d, P::D, L::A),
        R::new(0x1e, P::A, L::G),
        R::new(0x1e, P::B, L::E),
        R::new(0x1e, P::C, L::B),
        R::new(0x1e, P::D, L::A),
        R::new(0x1f, P::A, L::C),
        R::new(0x1f, P::B, L::D),
        R::new(0x1f, P::C, L::B),
        R::new(0x1f, P::D, L::A),
    ],
    bridge_routes: &[
        R::new(0, P::A, L::A),
        R::new(1, P::A, L::E),
        R::new(1, P::B, L::F),
        R::new(1, P::C, L::G),
        R::new(1, P::D, L::H),
        R::new(2, P::A, L::F),
        R::new(2, P::B, L::G),
        R::new(2, P::C, L::H),
        R::new(2, P::D, L::E),
        R::new(3, P::A, L::G),
        R::new(3, P::B, L::H),
        R::new(3, P::C, L::E),
        R::new(3, P::D, L::F),
        R::new(4, P::A, L::H),
        R::new(4, P::B, L::E),
        R::new(4, P::C, L::F),
        R::new(4, P::D, L::G),
        R::new(5, P::A, L::D),
        R::new(5, P::B, L::C),
        R::new(5, P::C, L::B),
        R::new(5, P::D, L::A),
        R::new(6, P::A, L::C),
        R::new(6, P::B, L::B),
        R::new(6, P::C, L::A),
        R::new(6, P::D, L::D),
        R::new(9, P::A, L::F),
        R::new(9, P::B, L::G),
        R::new(9, P::C, L::H),
        R::new(9, P::D, L::E),
    ],
};

pub static MB899_PLATFORM: I945Ich7Platform = I945Ich7Config::new()
    .variant(I945Variant::Mobile)
    .max_cpus(2)
    .pci_mmio_size(768)
    // The reference does not override i945's four-address SPD map.
    .spd_addresses([0x50, 0x51, 0x52, 0x53])
    .igd(I945IgdConfig {
        vbt: VbtSource::NONE,
        display: Some(IgdDisplayPolicy {
            outputs: &[OutputConfig {
                port: Port::Vga,
                enabled: true,
            }],
            framebuffer: FramebufferConfig::edid(FallbackMode {
                width: 1024,
                height: 768,
                refresh_hz: 60,
            }),
        }),
        ..I945IgdConfig::new()
    })
    .pcie_port(0, true)
    .pcie_port(1, true)
    .pcie_port(2, true)
    .pcie_port(3, true)
    .lan(false)
    .pirq_routing([5, 7, 5, 7, 0x80, 0x80, 0x80, 6])
    .pirq(MB899_PIRQ)
    .gpi_routing([
        GpiRoute::Disabled,
        GpiRoute::Disabled,
        GpiRoute::Disabled,
        GpiRoute::Disabled,
        GpiRoute::Disabled,
        GpiRoute::Disabled,
        GpiRoute::Disabled,
        GpiRoute::Disabled,
        GpiRoute::Disabled,
        GpiRoute::Disabled,
        GpiRoute::Disabled,
        GpiRoute::Disabled,
        GpiRoute::Disabled,
        GpiRoute::Smi,
        GpiRoute::Disabled,
        GpiRoute::Disabled,
    ])
    .gpe_events([])
    .ac97_audio(true)
    .ac97_modem(true)
    .lpc_fixed_io(LpcFixedIoDecode {
        com_a: LpcSerialDecode::Com1,
        com_b: LpcSerialDecode::Com2,
        lpt: None,
        fdd: None,
    })
    .lpc_generic_io([LpcGenericIoDecode {
        base: HWM_BASE,
        size: 8,
    }])
    .ide(IdeConfig {
        enable_primary: true,
        enable_secondary: false,
    })
    .sata(SataConfig::new(SataMode::Ide, [0, 2]))
    .usb(UsbConfig {
        ehci: true,
        uhci: [true; 4],
    })
    .gpio_pins(mb899_gpio_pins())
    .build();

impl fstart_platform_intel::facts::IntelBoardFacts for crate::Board {
    type Platform = Hardware;
    const CONFIG: &'static I945Ich7Platform = &MB899_PLATFORM;
    const FACTS: fstart_platform_intel::facts::BoardFacts =
        fstart_platform_intel::facts::BoardFacts::new(FLASH, FLASH_SIZE).with_protected_mode();
}

pub const fn mb899_gpio_pins() -> [gpio::GpioPin; 28] {
    use gpio::GpioLevel::{High, Low};
    [
        gpio::input(6),
        gpio::input(7).inverted(),
        gpio::input(8).inverted(),
        gpio::input(9),
        gpio::input(10),
        gpio::output(12, Low),
        gpio::input(13).inverted(),
        gpio::input(14),
        gpio::input(15),
        gpio::output(16, High),
        gpio::output(17, High),
        gpio::output(18, High),
        gpio::input(19),
        gpio::output(20, High),
        gpio::output(24, High),
        gpio::output(25, High),
        gpio::output(26, Low),
        gpio::output(27, High),
        gpio::output(28, Low),
        gpio::output(32, High),
        gpio::output(33, Low),
        gpio::output(34, High),
        gpio::output(35, Low),
        gpio::input(36),
        gpio::input(37),
        gpio::output(38, Low),
        gpio::output(39, Low),
        gpio::output(48, High),
    ]
}

// Only the physically valid GPIO3 pads are configured. The reference drives
// GP32 low before RAM and high through its late devicetree; no role is inferred.
pub const fn superio_gpio_pins(after_ram: bool) -> [Pin; 3] {
    [Pin::output(32, after_ram), Pin::input(33), Pin::input(34)]
}

pub fn early_superio_config() -> SuperIoConfig {
    SuperIoConfig {
        com1: Some(ComPortConfig {
            io_base: UART0_PIO_BASE,
            irq: 4,
            baud_rate: UART0_BAUD_RATE,
        }),
        ..SuperIoConfig::default()
    }
}
pub fn mb899_superio_config() -> SuperIoConfig {
    SuperIoConfig {
        com2: Some(ComPortConfig {
            io_base: 0x2f8,
            irq: 3,
            baud_rate: UART0_BAUD_RATE,
        }),
        keyboard: Some(KbcConfig {
            io_base: 0x60,
            io_ext: 0x64,
            irq: 1,
        }),
        mouse: Some(MouseConfig { irq: 12 }),
        acpi_name: Some(hstr("SIO1")),
        ..early_superio_config()
    }
}

/// Reference CR04=0 selects PWM for SYSFANOUT and CPUFANOUT0.
/// Explicit maximum output replaces reliance on power-on duty values; no cruise.
pub const COOLING: FullSpeedCooling = FullSpeedCooling {
    system: Some(FanOutputMode::Pwm),
    cpu_primary: Some(FanOutputMode::Pwm),
    auxiliary: None,
    cpu_secondary: None,
};
pub const MONITOR: MonitorConfig = MonitorConfig {
    system: TemperatureConfig {
        sensor: TemperatureSensor::Diode,
        offset_c: -15,
    },
    cpu: TemperatureConfig {
        sensor: TemperatureSensor::PentiumIIDiode,
        offset_c: 25,
    },
    auxiliary: TemperatureConfig {
        sensor: TemperatureSensor::Diode,
        offset_c: -4,
    },
    battery_voltage: true,
    cooling: COOLING,
};

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn resource_and_interrupt_policy_is_consistent() {
        assert!(early_superio_config().com2.is_none());
        assert!(early_superio_config().keyboard.is_none());
        let sio = mb899_superio_config();
        assert_eq!(sio.com2.unwrap().irq, 3);
        assert_eq!(sio.mouse.unwrap().irq, 12);
        assert!(sio.env_controller.is_none()); // HWM has a separate single-base allocator.
        assert_eq!(
            MB899_PLATFORM.southbridge.pirq_routing,
            [5, 7, 5, 7, 0x80, 0x80, 0x80, 6]
        );
        assert_eq!(MB899_PLATFORM.southbridge.gpi_routing[13], GpiRoute::Smi);
        assert_eq!(MB899_PLATFORM.southbridge.gpe0_en, 0);
        assert!(MB899_PLATFORM.northbridge.igd.vbt.file.is_none());
        assert_eq!(FLASH_SIZE, 512 * 1024);
    }
    #[test]
    fn interrupt_register_values_match_the_early_reference() {
        assert_eq!(MB899_PIRQ.ip_value(0x1f), 0x0004_2210);
        assert_eq!(MB899_PIRQ.ip_value(0x1c), 0x0021_4321);
        assert_eq!(MB899_PIRQ.ip_value(0x1d), 0x1000_4321);
        for (slot, value) in [
            (0x1f, 0x0132),
            (0x1e, 0x0146),
            (0x1d, 0x0237),
            (0x1c, 0x3201),
            (0x1b, 0x0146),
        ] {
            assert_eq!(MB899_PIRQ.ir_value(slot), value);
        }
    }
    #[test]
    fn cooling_and_board_gpio_follow_explicit_policy() {
        assert_eq!(COOLING.system, Some(FanOutputMode::Pwm));
        assert_eq!(COOLING.cpu_primary, Some(FanOutputMode::Pwm));
        assert_eq!(MONITOR.cpu.offset_c, 25);
        assert_eq!(MONITOR.system.offset_c, -15);
        assert_eq!(MONITOR.auxiliary.offset_c, -4);
        assert_eq!(MONITOR.cpu.sensor, TemperatureSensor::PentiumIIDiode);
        let pins = mb899_gpio_pins();
        assert_eq!(
            pins.iter()
                .filter(|p| p.invert)
                .map(|p| p.pin)
                .collect::<std::vec::Vec<_>>(),
            [7, 8, 13]
        );
        assert_eq!(
            pins.iter().find(|p| p.pin == 48).unwrap().level,
            gpio::GpioLevel::High
        );
        assert_ne!(superio_gpio_pins(false)[0], superio_gpio_pins(true)[0]);
    }
}
