//! Wiring from coreboot mainboard/intel/d510mo (GPIO, PIRQ, HDA and CK505).
use fstart_core::{FlashLayout, Platform, X86LegacyFlashLayout, hstr, hvec};
use fstart_driver_intel::generic::ck505::I2cCk505Config;
use fstart_driver_intel::southbridge::pirq::{PciPin as P, PinRoute as R, Pirq as L};
use fstart_driver_intel::southbridge::{gpio_ich as gpio, hda};
use fstart_driver_superio::*;
use fstart_intel_gma::framebuffer::FramebufferConfig;
use fstart_intel_gma::{FallbackMode, OutputConfig, Port};
use fstart_platform_intel::igd::{IgdDisplayPolicy, VbtSource};
use fstart_platform_intel::pineview::*;

pub type Hardware = PineviewIch7<fstart_platform_intel::legacy_cpu::Fcbga559>;
pub const BOARD_NAME: &str = "intel-d510mo";
pub const BOARD_PACKAGE: &str = "fstart-board-intel-d510mo";
pub const PLATFORM: Platform = Platform::X86_64;
pub const UART0_PIO_BASE: u16 = 0x3f8;
pub const UART0_CLOCK_FREQ: u32 = 1_843_200;
pub const UART0_BAUD_RATE: u32 = 115_200;
pub const SUPERIO_PNP_BASE: u16 = 0x4e;
pub const HWM_BASE: u16 = 0x290;
// Stock board has a 1 MiB SPI flash, unlike the expanded D41S development ROM.
pub const FLASH_SIZE: u32 = 0x10_0000;
pub const FLASH: FlashLayout = FlashLayout::X86Legacy(X86LegacyFlashLayout { size: FLASH_SIZE });

pub const D510MO_PIRQ: PirqRouting = PirqRouting {
    bridge_routes: &[
        R::new(0, P::A, L::G),
        R::new(0, P::B, L::E),
        R::new(0, P::C, L::B),
        R::new(0, P::D, L::A),
    ],
    ..fstart_driver_intel::southbridge::pirq::ICH7_PINEVIEW_ROUTING
};

pub static D510MO_PLATFORM: PineviewIch7Platform = PineviewIch7Config::new()
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
    .pcie_port(0, true)
    .pcie_port(1, true)
    .pcie_port(2, true)
    .pcie_port(3, true)
    .pirq_routing([11; 8])
    .pirq(D510MO_PIRQ)
    .lan(false)
    .early_serial_irq(false)
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
    .hda(d510mo_hda_config())
    .gpio_pins(d510mo_gpio_pins())
    .gpe_events([Ich7Gpe::TcoSci, Ich7Gpe::Gpio(13)])
    .build();

impl fstart_platform_intel::facts::IntelBoardFacts for crate::Board {
    type Platform = Hardware;
    const CONFIG: &'static PineviewIch7Platform = &D510MO_PLATFORM;
    const FACTS: fstart_platform_intel::facts::BoardFacts =
        fstart_platform_intel::facts::BoardFacts::new(FLASH, FLASH_SIZE)
            .with_data_assets(&["data.vbt"]);
}

pub const fn d510mo_gpio_pins() -> [gpio::GpioPin; 28] {
    use gpio::GpioLevel::{High, Low};
    [
        gpio::input(0),
        gpio::output(6, High),
        gpio::output(7, Low),
        gpio::output(8, Low),
        gpio::output(9, Low),
        gpio::output(10, Low),
        gpio::output(12, Low),
        gpio::input(13).inverted(),
        gpio::input(14),
        gpio::input(15),
        gpio::input(16),
        gpio::input(19),
        gpio::output(20, Low),
        gpio::input(21),
        gpio::input(22),
        gpio::input(23),
        gpio::output(24, Low),
        gpio::output(25, Low),
        gpio::output(26, High),
        gpio::output(27, High),
        gpio::output(28, Low),
        gpio::input(33),
        gpio::input(34),
        gpio::input(35),
        gpio::input(36),
        gpio::input(37),
        gpio::input(38),
        gpio::output(39, High),
    ]
}

pub fn d510mo_superio_config() -> SuperIoConfig {
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

pub fn d510mo_ck505_config() -> I2cCk505Config {
    I2cCk505Config {
        mask: hvec([0xff; 13]),
        regs: hvec([
            0x61, 0xd9, 0xfe, 0xff, 0xff, 0x00, 0x00, 0x01, 0x03, 0x25, 0x83, 0x17, 0x0d,
        ]),
    }
}

pub const fn d510mo_hda_config() -> hda::HdaConfig {
    use hda::{
        PinColor as C, PinConn as N, PinConnector as T, PinDevice as D, PinGeoLoc as G, PinLoc as L,
    };
    hda::HdaConfig::new().verb(
        hda::HdaVerbTable::new(0x10ec_0662, 0x8086_d618)
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
            .pin(hda::pin_config(
                0x18,
                D::MicIn,
                N::Jack,
                L::External,
                G::Rear,
                T::StereoMono18,
                C::Pink,
                8,
                4,
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
                8,
                4,
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
                4,
                2,
                0,
            ))
            .pin(hda::pin_not_connected(0x1c, 0))
            // Codec-specific reserved color nibble 0xc: preserve the exact beep pin value.
            .extra_verb(hda::hda_pin_cfg(0, 0x1d, 0x4015_c603)[0])
            .extra_verb(hda::hda_pin_cfg(0, 0x1d, 0x4015_c603)[1])
            .extra_verb(hda::hda_pin_cfg(0, 0x1d, 0x4015_c603)[2])
            .extra_verb(hda::hda_pin_cfg(0, 0x1d, 0x4015_c603)[3])
            .pin(hda::pin_config(
                0x1e,
                D::SpdifOut,
                N::Integrated,
                L::Internal,
                G::Special9,
                T::AtapiInternal,
                C::ColorUnknown,
                1,
                3,
                0,
            )),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn router_matches_board_and_acpi_wiring() {
        let sb = &D510MO_PLATFORM.southbridge;
        assert!(!sb.lan);
        assert!(!sb.early_serial_irq);
        assert_eq!(sb.gpe0_en, 0x2000_0040);
        assert_eq!(sb.sata.unwrap().ports, 3);
        assert_eq!(sb.pirq_routing, [11; 8]);
        for (slot, value) in [
            (0x1f, 0x0132),
            (0x1e, 0x0146),
            (0x1d, 0x0237),
            (0x1c, 0x3201),
            (0x1b, 0x0146),
        ] {
            assert_eq!(sb.pirq.ir_value(slot), value);
        }
        assert!(sb.pirq.bridge_bus_routes().eq([
            (0, P::A, 22),
            (0, P::B, 20),
            (0, P::C, 17),
            (0, P::D, 16),
        ]));
    }

    #[test]
    fn codec_pins_preserve_reference_values() {
        let config = d510mo_hda_config();
        let table = config.verbs.get(0);
        assert!(
            table
                .pins
                .as_slice()
                .iter()
                .map(|p| (p.nid, p.encode()))
                .eq([
                    (0x14, 0x0101_4410),
                    (0x15, 0x4111_11f0),
                    (0x16, 0x4111_11f0),
                    (0x18, 0x01a1_9840),
                    (0x19, 0x02a1_9841),
                    (0x1a, 0x0181_304f),
                    (0x1b, 0x0221_4420),
                    (0x1c, 0x4111_11f0),
                    (0x1e, 0x9943_0130),
                ])
        );
        assert_eq!(
            table.extra_verbs.as_slice(),
            &hda::hda_pin_cfg(0, 0x1d, 0x4015_c603)
        );
    }
}
