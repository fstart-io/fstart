//! Shared S2L/S2C wiring, expressed as chipset and attached-device policy.
use fstart_core::{FlashLayout, Platform, X86LegacyFlashLayout, hstr};
use fstart_driver_intel::southbridge::pirq::{PciPin as P, PinRoute as R, Pirq as L};
use fstart_driver_intel::southbridge::{gpio_ich as gpio, hda};
use fstart_driver_superio::ite_env::{
    EnvironmentConfig, FanCurve, FanPolarity, FanPolicy, TemperatureConfig, TemperatureInput,
    TemperatureMode,
};
use fstart_driver_superio::ite8718f::{
    GpioDirection as Direction, GpioFunction as Function, GpioPinConfig as Pin, PullUp,
};
use fstart_driver_superio::{
    ComPortConfig, EcConfig, KbcConfig, MouseConfig, ParallelConfig, SuperIoConfig,
};
use fstart_intel_gma::framebuffer::FramebufferConfig;
use fstart_intel_gma::{FallbackMode, OutputConfig, Port};
use fstart_platform_intel::i945::*;
use fstart_platform_intel::igd::{IgdDisplayPolicy, VbtSource};

pub type Hardware = I945Ich7<fstart_platform_intel::legacy_cpu::Lga775Core2>;
pub const BOARD_NAME: &str = if cfg!(feature = "s2c") {
    "gigabyte-ga-945gcm-s2c"
} else {
    "gigabyte-ga-945gcm-s2l"
};
pub const PRODUCT_NAME: &str = if cfg!(feature = "s2c") {
    "GA-945GCM-S2C"
} else {
    "GA-945GCM-S2L"
};
pub const BOARD_PACKAGE: &str = "fstart-board-gigabyte-ga-945gcm-s2l";
pub const PLATFORM: Platform = Platform::X86_64;
pub const UART0_PIO_BASE: u16 = 0x3f8;
pub const UART0_CLOCK_FREQ: u32 = 1_843_200;
pub const UART0_BAUD_RATE: u32 = 115_200;
pub const SUPERIO_PNP_BASE: u16 = 0x2e;
pub const HWM_BASE: u16 = 0x290;
pub const SUPERIO_GPIO_BASE: u16 = 0x800;
pub const FLASH_SIZE: u32 = 0x8_0000;
pub const FLASH: FlashLayout = FlashLayout::X86Legacy(X86LegacyFlashLayout { size: FLASH_SIZE });

pub const GA945GCM_PIRQ: PirqRouting = PirqRouting {
    bridge_routes: &[
        R::new(0, P::A, L::E),
        R::new(0, P::B, L::D),
        R::new(0, P::C, L::C),
        R::new(0, P::D, L::A),
        R::new(1, P::A, L::D),
        R::new(1, P::B, L::C),
        R::new(1, P::C, L::A),
        R::new(1, P::D, L::E),
    ],
    ..fstart_driver_intel::southbridge::pirq::ICH7_PINEVIEW_ROUTING
};

pub static GA945GCM_PLATFORM: I945Ich7Platform = I945Ich7Config::new()
    .variant(I945Variant::DesktopGc)
    .max_cpus(2)
    .spd_addresses([0x50, 0x51, 0x52, 0x53])
    .igd(I945IgdConfig {
        vbt: VbtSource::ffs("data.vbt"),
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
    .pirq_routing([0x80; 8])
    .pirq(GA945GCM_PIRQ)
    .gpi_routing([
        GpiRoute::Smi,
        GpiRoute::Smi,
        GpiRoute::Smi,
        GpiRoute::Smi,
        GpiRoute::Smi,
        GpiRoute::Smi,
        GpiRoute::Smi,
        GpiRoute::Smi,
        GpiRoute::Smi,
        GpiRoute::Smi,
        GpiRoute::Smi,
        GpiRoute::Smi,
        GpiRoute::Smi,
        GpiRoute::Sci,
        GpiRoute::Smi,
        GpiRoute::Smi,
    ])
    .gpe_events([])
    .ac97_audio(false)
    .ac97_modem(false)
    .lpc_fixed_io(LpcFixedIoDecode {
        com_a: LpcSerialDecode::Com1,
        com_b: LpcSerialDecode::Com2,
        lpt: Some(LpcParallelDecode::Lpt378),
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
    .sata(SataConfig::new(SataMode::Ide, [0, 1, 2, 3]))
    .usb(UsbConfig {
        ehci: true,
        uhci: [true; 4],
    })
    .gpio_pins(ga945gcm_gpio_pins())
    .hda(ga945gcm_hda_config())
    .build();

impl fstart_platform_intel::facts::IntelBoardFacts for crate::Board {
    type Platform = Hardware;
    const CONFIG: &'static I945Ich7Platform = &GA945GCM_PLATFORM;
    const FACTS: fstart_platform_intel::facts::BoardFacts =
        fstart_platform_intel::facts::BoardFacts::new(FLASH, FLASH_SIZE)
            .with_data_assets(&["data.vbt"]);
}

pub const fn ga945gcm_gpio_pins() -> [gpio::GpioPin; 26] {
    use gpio::GpioLevel::{High, Low};
    [
        gpio::input(0).inverted(),
        gpio::input(6).inverted(),
        gpio::input(7).inverted(),
        gpio::input(8).inverted(),
        gpio::input(9),
        gpio::input(10),
        gpio::input(12).inverted(),
        gpio::input(13).inverted(),
        gpio::input(14),
        gpio::input(15),
        gpio::output(16, Low),
        gpio::output(18, High),
        gpio::output(20, High),
        gpio::output(24, Low),
        gpio::output(25, High),
        gpio::output(26, Low),
        gpio::output(27, Low),
        gpio::output(28, Low),
        gpio::output(32, Low),
        gpio::output(33, High),
        gpio::output(34, Low),
        gpio::GpioPin {
            mode: gpio::GpioMode::Native,
            ..gpio::input(35)
        },
        gpio::GpioPin {
            mode: gpio::GpioMode::Native,
            ..gpio::input(36)
        },
        gpio::GpioPin {
            mode: gpio::GpioMode::Native,
            ..gpio::input(37)
        },
        gpio::input(38),
        gpio::input(39),
    ]
}

/// Physical pins selected by the reference. Unlisted banks/latches are retained.
/// UART2's coupled pads are unused GPIO inputs, not an enabled COM2 port.
pub const SUPERIO_PINS: [Pin; 38] = [
    Pin::peripheral(10)
        .direction(Direction::Input)
        .pull_up(PullUp::Disabled),
    Pin::peripheral(11)
        .direction(Direction::Input)
        .pull_up(PullUp::Disabled),
    Pin::peripheral(12)
        .direction(Direction::Input)
        .pull_up(PullUp::Disabled),
    Pin::peripheral(13)
        .direction(Direction::Input)
        .pull_up(PullUp::Disabled),
    Pin::peripheral(14)
        .direction(Direction::Input)
        .pull_up(PullUp::Disabled),
    Pin::new(16, Function::Alternate)
        .direction(Direction::Input)
        .pull_up(PullUp::Disabled),
    Pin::peripheral(17)
        .direction(Direction::Input)
        .pull_up(PullUp::Enabled),
    Pin::new(20, Function::Alternate).inverted(true),
    Pin::new(21, Function::Alternate).inverted(false),
    Pin::new(22, Function::Alternate).inverted(false),
    Pin::new(23, Function::Alternate).inverted(false),
    Pin::new(24, Function::Alternate).inverted(false),
    Pin::new(25, Function::Alternate).inverted(false),
    Pin::peripheral(26).inverted(false),
    Pin::peripheral(27).inverted(false),
    Pin::new(40, Function::Alternate)
        .direction(Direction::Input)
        .pull_up(PullUp::Disabled),
    Pin::peripheral(41)
        .direction(Direction::Input)
        .pull_up(PullUp::Disabled),
    Pin::peripheral(42)
        .direction(Direction::Input)
        .pull_up(PullUp::Disabled),
    Pin::peripheral(43)
        .direction(Direction::Input)
        .pull_up(PullUp::Disabled),
    Pin::peripheral(44)
        .direction(Direction::Input)
        .pull_up(PullUp::Disabled),
    Pin::new(46, Function::Alternate)
        .direction(Direction::Input)
        .pull_up(PullUp::Enabled),
    Pin::peripheral(47)
        .direction(Direction::Input)
        .pull_up(PullUp::Disabled),
    Pin::peripheral(50),
    Pin::peripheral(51),
    Pin::peripheral(52),
    Pin::new(53, Function::Alternate),
    Pin::peripheral(54),
    Pin::peripheral(55),
    Pin::peripheral(56),
    Pin::peripheral(57),
    Pin::peripheral(60),
    Pin::peripheral(61),
    Pin::peripheral(62),
    Pin::input(63),
    Pin::input(64),
    Pin::input(65),
    Pin::input(66),
    Pin::input(67),
];

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
pub fn cooling_superio_config() -> SuperIoConfig {
    SuperIoConfig {
        env_controller: Some(EcConfig {
            io_base: HWM_BASE,
            io_ext: 0,
        }),
        ..early_superio_config()
    }
}
pub fn ga945gcm_superio_config() -> SuperIoConfig {
    SuperIoConfig {
        parallel: Some(ParallelConfig {
            io_base: 0x378,
            irq: 7,
        }),
        keyboard: Some(KbcConfig {
            io_base: 0x60,
            io_ext: 0x64,
            irq: 1,
        }),
        mouse: Some(MouseConfig { irq: 12 }),
        acpi_name: Some(hstr("SIO0")),
        ..cooling_superio_config()
    }
}

/// Both populated fans use the CPU diode; the unused third output is preserved.
/// Full-speed temperature is nominal, not an unconditional deadline with filters.
pub const FAN_CURVE: FanCurve = FanCurve::new(30, 35, 75, 0).hysteresis(3).smoothing(true);
pub const ENVIRONMENT: EnvironmentConfig = EnvironmentConfig {
    temperatures: [
        TemperatureConfig::new(TemperatureMode::Resistor),
        TemperatureConfig::new(TemperatureMode::Resistor),
        TemperatureConfig::new(TemperatureMode::Diode),
    ],
    voltage_inputs: [true; 8],
    fans: [
        Some(FanPolicy::Automatic {
            input: TemperatureInput::Three,
            curve: FAN_CURVE,
        }),
        Some(FanPolicy::Automatic {
            input: TemperatureInput::Three,
            curve: FAN_CURVE,
        }),
        None,
    ],
    fan_polarity: FanPolarity::ActiveHigh,
};

pub const fn ga945gcm_hda_config() -> hda::HdaConfig {
    use hda::{
        PinColor as C, PinConn as N, PinConnector as T, PinDevice as D, PinGeoLoc as G, PinLoc as L,
    };
    hda::HdaConfig::new().verb(
        hda::HdaVerbTable::new(0x10ec_0662, 0x1458_a002)
            .pin(hda::pin_config(
                0x14,
                D::LineOut,
                N::Jack,
                L::External,
                G::Rear,
                T::StereoMono18,
                C::Green,
                0,
                1,
                0,
            ))
            .pin(hda::pin_not_connected(0x15, 0))
            .pin(hda::pin_not_connected(0x16, 0))
            .pin(hda::pin_config(
                0x18,
                D::MicIn,
                N::Jack,
                L::External,
                G::Rear,
                T::StereoMono18,
                C::Pink,
                8,
                3,
                0,
            ))
            .pin(hda::pin_config(
                0x19,
                D::MicIn,
                N::Jack,
                L::External,
                G::Front,
                T::StereoMono18,
                C::Pink,
                12,
                3,
                1,
            ))
            .pin(hda::pin_config(
                0x1a,
                D::LineIn,
                N::Jack,
                L::External,
                G::Rear,
                T::StereoMono18,
                C::Blue,
                0,
                3,
                15,
            ))
            .pin(hda::pin_config(
                0x1b,
                D::HpOut,
                N::Jack,
                L::External,
                G::Front,
                T::StereoMono18,
                C::Green,
                12,
                1,
                15,
            ))
            .pin(hda::pin_config(
                0x1c,
                D::Cd,
                N::Nc,
                L::Internal,
                G::Special9,
                T::AtapiInternal,
                C::ColorUnknown,
                1,
                15,
                0,
            ))
            .pin(hda::pin_config(
                0x1e,
                D::SpdifOut,
                N::Jack,
                L::External,
                G::Rear,
                T::Combination,
                C::Orange,
                1,
                2,
                0,
            ))
            // Reserved color 0xc on the codec-specific beep node: retain its verbs.
            .extra_verb(hda::hda_pin_cfg(0, 0x1d, 0x4005_c603)[0])
            .extra_verb(hda::hda_pin_cfg(0, 0x1d, 0x4005_c603)[1])
            .extra_verb(hda::hda_pin_cfg(0, 0x1d, 0x4005_c603)[2])
            .extra_verb(hda::hda_pin_cfg(0, 0x1d, 0x4005_c603)[3]),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn resources_and_variant_identity_match_the_selected_board() {
        assert_eq!(
            PRODUCT_NAME,
            if cfg!(feature = "s2c") {
                "GA-945GCM-S2C"
            } else {
                "GA-945GCM-S2L"
            }
        );
        assert!(early_superio_config().keyboard.is_none());
        assert!(early_superio_config().env_controller.is_none());
        assert_eq!(
            cooling_superio_config().env_controller.unwrap().io_base,
            HWM_BASE
        );
        let sio = ga945gcm_superio_config();
        assert!(sio.com2.is_none());
        assert_eq!(sio.mouse.unwrap().irq, 12);
        assert_eq!(sio.env_controller.unwrap().io_base, HWM_BASE);
        assert_eq!(GA945GCM_PLATFORM.southbridge.pirq_routing, [0x80; 8]);
        assert_eq!(GA945GCM_PLATFORM.southbridge.gpe0_en, 0);
        assert_eq!(GA945GCM_PLATFORM.southbridge.gpi_routing[13], GpiRoute::Sci);
        assert!(matches!(
            ENVIRONMENT.temperatures[2].mode,
            TemperatureMode::Diode
        ));
        assert!(matches!(
            ENVIRONMENT.fans[0],
            Some(FanPolicy::Automatic {
                input: TemperatureInput::Three,
                ..
            })
        ));
    }
    #[test]
    fn audio_pins_match_the_reference() {
        let hda = ga945gcm_hda_config();
        let codec = hda.verbs.get(0);
        assert_eq!(codec.vendor_id, 0x10ec_0662);
        for (nid, value) in [
            (0x14, 0x0101_4010),
            (0x18, 0x01a1_9830),
            (0x19, 0x02a1_9c31),
            (0x1a, 0x0181_303f),
            (0x1b, 0x0221_4c1f),
            (0x1c, 0x5933_01f0),
            (0x1e, 0x014b_6120),
        ] {
            assert_eq!(
                codec
                    .pins
                    .as_slice()
                    .iter()
                    .find(|pin| pin.nid == nid)
                    .unwrap()
                    .encode(),
                value
            );
        }
    }
}
