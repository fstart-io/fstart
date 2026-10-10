//! Board wiring, not Super I/O or RCBA register encodings.
use fstart_core::{FlashLayout, Platform, X86LegacyFlashLayout, hstr};
use fstart_driver_intel::southbridge::{gpio_ich as gpio, hda};
use fstart_driver_superio::ite8720f::{BusSelect, GpioDirection, GpioPinConfig, PullUp};
use fstart_driver_superio::*;
use fstart_intel_gma::framebuffer::FramebufferConfig;
use fstart_intel_gma::{FallbackMode, OutputConfig, Port};
use fstart_platform_intel::igd::{IgdDisplayPolicy, VbtSource};
use fstart_platform_intel::pineview::*;

pub type Hardware = PineviewIch7<fstart_platform_intel::legacy_cpu::Fcbga559>;
pub const BOARD_NAME: &str = "gigabyte-ga-d510ud";
pub const BOARD_PACKAGE: &str = "fstart-board-gigabyte-ga-d510ud";
pub const PLATFORM: Platform = Platform::X86_64;
pub const UART0_PIO_BASE: u16 = 0x3f8;
pub const UART0_CLOCK_FREQ: u32 = 1_843_200;
pub const UART0_BAUD_RATE: u32 = 115_200;
pub const SUPERIO_PNP_BASE: u16 = 0x2e;
pub const HWM_BASE: u16 = 0x290;
pub const SUPERIO_GPIO_BASE: u16 = 0x800;
pub const FLASH_SIZE: u32 = 0x8_0000;
pub const FLASH: FlashLayout = FlashLayout::X86Legacy(X86LegacyFlashLayout { size: FLASH_SIZE });

pub static GA_D510UD_PLATFORM: PineviewIch7Platform = PineviewIch7Config::new()
    .max_cpus(4)
    .igd(PineviewIgdConfig {
        use_crt: true,
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
        ..PineviewIgdConfig::new()
    })
    // Realtek GbE and JMB363 respectively; the other root ports are unwired.
    .pcie_port(0, true)
    .pcie_port(1, true)
    .pirq_routing([11; 8])
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
    .sata(SataConfig::new(SataMode::Ahci, [0, 1]))
    .usb(UsbConfig {
        ehci: true,
        uhci: [true; 4],
    })
    .gpio_pins(ga_d510ud_gpio_pins())
    .gpe_events([Ich7Gpe::Thermal, Ich7Gpe::TcoSci, Ich7Gpe::BatteryLow])
    .hda(ga_d510ud_hda_config())
    .build();

impl fstart_platform_intel::facts::IntelBoardFacts for crate::Board {
    type Platform = Hardware;
    const CONFIG: &'static PineviewIch7Platform = &GA_D510UD_PLATFORM;
    const FACTS: fstart_platform_intel::facts::BoardFacts =
        fstart_platform_intel::facts::BoardFacts::new(FLASH, FLASH_SIZE)
            .with_data_assets(&["data.vbt"]);
}

pub const fn ga_d510ud_gpio_pins() -> [gpio::GpioPin; 20] {
    use gpio::GpioLevel::{High, Low};
    [
        gpio::input(6).inverted(),
        gpio::input(7).inverted(),
        gpio::input(8).inverted(),
        gpio::input(9),
        gpio::input(10),
        gpio::input(11).inverted(),
        gpio::input(12).inverted(),
        gpio::input(13).inverted(),
        gpio::input(14),
        gpio::input(15),
        gpio::output(20, High),
        gpio::output(24, Low),
        gpio::input(25),
        gpio::output(26, Low),
        gpio::output(27, Low),
        gpio::output(28, Low),
        gpio::output(33, High),
        gpio::output(34, Low),
        gpio::GpioPin {
            level: High,
            ..gpio::input(38)
        },
        gpio::GpioPin {
            level: High,
            ..gpio::input(39)
        },
    ]
}

pub const GA_D510UD_SUPERIO_GPIO: [GpioPinConfig; 23] = [
    // Retain peripheral functions; park the simple-I/O side as inputs without pull-ups.
    GpioPinConfig::peripheral(10)
        .direction(GpioDirection::Input)
        .pull_up(PullUp::Disabled),
    GpioPinConfig::peripheral(11)
        .direction(GpioDirection::Input)
        .pull_up(PullUp::Disabled),
    GpioPinConfig::peripheral(12)
        .direction(GpioDirection::Input)
        .pull_up(PullUp::Disabled),
    GpioPinConfig::peripheral(13)
        .direction(GpioDirection::Input)
        .pull_up(PullUp::Disabled),
    GpioPinConfig::peripheral(14)
        .direction(GpioDirection::Input)
        .pull_up(PullUp::Disabled),
    GpioPinConfig::peripheral(15)
        .direction(GpioDirection::Input)
        .pull_up(PullUp::Disabled),
    GpioPinConfig::peripheral(16)
        .direction(GpioDirection::Input)
        .pull_up(PullUp::Disabled),
    GpioPinConfig::peripheral(17)
        .direction(GpioDirection::Input)
        .pull_up(PullUp::Disabled),
    // GP22/23 replace SPI SI/SCK with simple I/O, retaining strap directions.
    // The remaining GP2x pins keep their UART-B peripheral functions.
    GpioPinConfig::peripheral(20),
    GpioPinConfig::peripheral(21),
    GpioPinConfig::simple_io(22),
    GpioPinConfig::simple_io(23),
    GpioPinConfig::peripheral(24),
    GpioPinConfig::peripheral(25),
    GpioPinConfig::peripheral(26),
    GpioPinConfig::peripheral(27),
    // GP40/46 become inputs instead of 3VSBSW#/IRRX; GP45 is reserved.
    GpioPinConfig::input(40),
    GpioPinConfig::peripheral(41).direction(GpioDirection::Input),
    GpioPinConfig::peripheral(42).direction(GpioDirection::Input),
    GpioPinConfig::peripheral(43).direction(GpioDirection::Input),
    GpioPinConfig::peripheral(44).direction(GpioDirection::Input),
    GpioPinConfig::input(46),
    GpioPinConfig::peripheral(47).direction(GpioDirection::Input),
];

pub const GA_D510UD_BSEL_PRESET: BusSelect = BusSelect {
    bsel0_high: true,
    bsel1_high: false,
    bsel2_high: false,
};

pub fn ga_d510ud_superio_config() -> SuperIoConfig {
    SuperIoConfig {
        com1: Some(ComPortConfig {
            io_base: UART0_PIO_BASE,
            irq: 4,
            baud_rate: UART0_BAUD_RATE,
        }),
        com2: Some(ComPortConfig {
            io_base: 0x2f8,
            irq: 3,
            baud_rate: UART0_BAUD_RATE,
        }),
        parallel: Some(ParallelConfig {
            io_base: 0x378,
            irq: 7,
        }),
        env_controller: Some(EcConfig {
            io_base: HWM_BASE,
            io_ext: 0,
        }),
        keyboard: Some(KbcConfig {
            io_base: 0x60,
            io_ext: 0x64,
            irq: 1,
        }),
        mouse: Some(MouseConfig { irq: 12 }),
        acpi_name: Some(hstr("SIO0")),
        ..SuperIoConfig::default()
    }
}

pub const fn ga_d510ud_hda_config() -> hda::HdaConfig {
    use hda::{
        PinColor as C, PinConn as N, PinConnector as T, PinDevice as D, PinGeoLoc as G, PinLoc as L,
    };
    hda::HdaConfig::new().verb(
        hda::HdaVerbTable::new(0x10ec_0887, 0x1458_a002)
            .pin(hda::pin_not_connected(0x11, 0))
            .pin(hda::pin_not_connected(0x12, 0))
            .pin(hda::pin_config(
                0x14,
                D::LineOut,
                N::Jack,
                L::External,
                G::Rear,
                T::StereoMono18,
                C::Green,
                4,
                1,
                0,
            ))
            .pin(hda::pin_not_connected(0x15, 0))
            .pin(hda::pin_not_connected(0x16, 0))
            .pin(hda::pin_not_connected(0x17, 0))
            .pin(hda::pin_config(
                0x18,
                D::MicIn,
                N::Jack,
                L::External,
                G::Rear,
                T::StereoMono18,
                C::Pink,
                12,
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
                5,
                0,
            ))
            .pin(hda::pin_config(
                0x1a,
                D::LineIn,
                N::Jack,
                L::External,
                G::Rear,
                T::StereoMono18,
                C::Blue,
                4,
                4,
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
                2,
                0,
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
            // Beep uses reserved color nibble 0xc, preserved verbatim.
            .extra_verb(hda::hda_pin_cfg(2, 0x1d, 0x4005_c603)[0])
            .extra_verb(hda::hda_pin_cfg(2, 0x1d, 0x4005_c603)[1])
            .extra_verb(hda::hda_pin_cfg(2, 0x1d, 0x4005_c603)[2])
            .extra_verb(hda::hda_pin_cfg(2, 0x1d, 0x4005_c603)[3])
            .pin(hda::pin_not_connected(0x1e, 0))
            .pin(hda::pin_not_connected(0x1f, 0)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_and_sata_policy_matches_reference_encodings() {
        let sb = &GA_D510UD_PLATFORM.southbridge;
        assert_eq!(sb.gpe0_en, 0x441);
        assert_eq!(sb.sata.unwrap().ports, 3);
        assert_eq!(sb.pirq_routing, [11; 8]);
    }

    #[test]
    fn codec_pins_preserve_reference_values() {
        let config = ga_d510ud_hda_config();
        let table = config.verbs.get(0);
        assert!(
            table
                .pins
                .as_slice()
                .iter()
                .map(|p| (p.nid, p.encode()))
                .eq([
                    (0x11, 0x4111_11f0),
                    (0x12, 0x4111_11f0),
                    (0x14, 0x0101_4410),
                    (0x15, 0x4111_11f0),
                    (0x16, 0x4111_11f0),
                    (0x17, 0x4111_11f0),
                    (0x18, 0x01a1_9c30),
                    (0x19, 0x02a1_9c50),
                    (0x1a, 0x0181_344f),
                    (0x1b, 0x0221_4c20),
                    (0x1c, 0x5933_01f0),
                    (0x1e, 0x4111_11f0),
                    (0x1f, 0x4111_11f0),
                ])
        );
        assert_eq!(
            table.extra_verbs.as_slice(),
            &hda::hda_pin_cfg(2, 0x1d, 0x4005_c603)
        );
    }
}
