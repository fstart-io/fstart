//! ASUS P5GC-MX wiring from coreboot GPIO, PIRQ, devicetree and HDA policy.
use fstart_arch::x86::cpu::intel::core2_cpu::FsbBusSelect;
use fstart_core::{FlashLayout, Platform, X86LegacyFlashLayout, hstr};
use fstart_driver_intel::southbridge::pirq::{PciPin as P, PinRoute as R, Pirq as L};
use fstart_driver_intel::southbridge::{gpio_ich as gpio, hda};
use fstart_driver_superio::w83627dhg::GpioPinConfig;
use fstart_driver_superio::{ComPortConfig, KbcConfig, MouseConfig, ParallelConfig, SuperIoConfig};
use fstart_intel_gma::framebuffer::FramebufferConfig;
use fstart_intel_gma::{FallbackMode, OutputConfig, Port};
use fstart_platform_intel::i945::*;
use fstart_platform_intel::igd::{IgdDisplayPolicy, VbtSource};

pub type Hardware = I945Ich7<fstart_platform_intel::legacy_cpu::Lga775Core2>;
pub const BOARD_NAME: &str = "asus-p5gc-mx";
pub const BOARD_PACKAGE: &str = "fstart-board-asus-p5gc-mx";
pub const PLATFORM: Platform = Platform::X86_64;
pub const UART0_PIO_BASE: u16 = 0x3f8;
pub const UART0_CLOCK_FREQ: u32 = 1_843_200;
pub const UART0_BAUD_RATE: u32 = 115_200;
pub const SUPERIO_PNP_BASE: u16 = 0x2e;
pub const HWM_BASE: u16 = 0x290;
pub const FLASH_SIZE: u32 = 0x8_0000;
pub const FLASH: FlashLayout = FlashLayout::X86Legacy(X86LegacyFlashLayout { size: FLASH_SIZE });

pub const P5GC_MX_PIRQ: PirqRouting = PirqRouting {
    bridge_routes: &[
        R::new(0, P::A, L::B),
        R::new(0, P::B, L::C),
        R::new(0, P::C, L::D),
        R::new(0, P::D, L::A),
        R::new(1, P::A, L::F),
        R::new(1, P::B, L::G),
        R::new(1, P::C, L::H),
        R::new(1, P::D, L::E),
        R::new(8, P::A, L::E),
    ],
    ..fstart_driver_intel::southbridge::pirq::ICH7_PINEVIEW_ROUTING
};

pub static P5GC_MX_PLATFORM: I945Ich7Platform = I945Ich7Config::new()
    .variant(I945Variant::DesktopGc)
    .max_cpus(4)
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
    .pirq(P5GC_MX_PIRQ)
    .ac97_audio(false)
    .ac97_modem(false)
    .lpc_fixed_io(LpcFixedIoDecode {
        com_a: LpcSerialDecode::Com1,
        com_b: LpcSerialDecode::Com2,
        lpt: Some(LpcParallelDecode::Lpt378),
        fdd: Some(LpcFloppyDecode::Fdd3f0),
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
    .gpe_events([])
    .gpio_pins(p5gc_mx_gpio_pins())
    .hda(p5gc_mx_hda_config())
    .build();

impl fstart_platform_intel::facts::IntelBoardFacts for crate::Board {
    type Platform = Hardware;
    const CONFIG: &'static I945Ich7Platform = &P5GC_MX_PLATFORM;
    const FACTS: fstart_platform_intel::facts::BoardFacts =
        fstart_platform_intel::facts::BoardFacts::new(FLASH, FLASH_SIZE)
            .with_data_assets(&["data.vbt"]);
}

/// CPU BSEL1 reaches GP33 through the board's inverted connection.
pub const fn bsel_gpio_pins(bsel: FsbBusSelect) -> [GpioPinConfig; 3] {
    [
        GpioPinConfig::output(32, bsel.bsel0_high),
        GpioPinConfig::output(33, bsel.bsel1_high).inverted(),
        GpioPinConfig::output(55, bsel.bsel2_high),
    ]
}

pub const fn p5gc_mx_gpio_pins() -> [gpio::GpioPin; 29] {
    use gpio::GpioLevel::{High, Low};
    use gpio::GpioMode::Native;
    [
        gpio::output(0, Low),
        gpio::input(6),
        gpio::input(7),
        gpio::input(8),
        gpio::input(9),
        gpio::input(10),
        gpio::output(11, High),
        gpio::input(12),
        gpio::input(13).inverted(),
        gpio::input(14).inverted(),
        gpio::input(15),
        gpio::output(16, Low),
        gpio::output(18, High),
        gpio::input(19),
        gpio::output(20, High),
        gpio::input(21),
        gpio::output(24, Low),
        gpio::output(25, High),
        gpio::output(26, Low),
        gpio::output(27, Low),
        gpio::output(28, Low),
        gpio::GpioPin {
            mode: Native,
            ..gpio::output(32, High)
        },
        gpio::GpioPin {
            mode: Native,
            ..gpio::output(33, High)
        },
        gpio::GpioPin {
            mode: Native,
            ..gpio::output(34, Low)
        },
        gpio::GpioPin {
            mode: Native,
            ..gpio::output(35, Low)
        },
        gpio::GpioPin {
            mode: Native,
            ..gpio::input(36)
        },
        gpio::GpioPin {
            mode: Native,
            ..gpio::input(37)
        },
        gpio::GpioPin {
            mode: Native,
            ..gpio::input(38)
        },
        gpio::GpioPin {
            mode: Native,
            ..gpio::input(39)
        },
    ]
}

/// Pre-RAM enables only the console, not keyboards or the GPIO4/UART-B mux.
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

pub fn p5gc_mx_superio_config() -> SuperIoConfig {
    SuperIoConfig {
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
        ..early_superio_config()
    }
}

pub const fn p5gc_mx_hda_config() -> hda::HdaConfig {
    use hda::{
        PinColor as C, PinConn as N, PinConnector as T, PinDevice as D, PinGeoLoc as G, PinLoc as L,
    };
    // The reference calls this ALC662 but identifies an ALC883; use its ID.
    hda::HdaConfig::new().verb(
        hda::HdaVerbTable::new(0x10ec_0883, 0x1043_82c7)
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
            .pin(hda::pin_not_connected(0x17, 0))
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
                0,
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
            .pin(hda::pin_config(
                0x1e,
                D::DigitalOtherOut,
                N::Jack,
                L::Internal,
                G::Special8,
                T::OtherDigital,
                C::Black,
                1,
                3,
                0,
            ))
            .pin(hda::pin_not_connected(0x1f, 0))
            // Reference beep node uses reserved color 0xc and codec-specific misc.
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
    fn cpu_strap_wiring_and_console_phases_are_explicit() {
        assert_eq!(
            bsel_gpio_pins(FsbBusSelect {
                bsel0_high: true,
                bsel1_high: false,
                bsel2_high: true
            }),
            [
                GpioPinConfig::output(32, true),
                GpioPinConfig::output(33, false).inverted(),
                GpioPinConfig::output(55, true),
            ]
        );
        assert!(early_superio_config().com2.is_none());
        assert!(early_superio_config().keyboard.is_none());
        assert_eq!(p5gc_mx_superio_config().com2.unwrap().irq, 3);
        assert_eq!(P5GC_MX_PLATFORM.southbridge.pirq_routing, [0x80; 8]);
        assert_eq!(P5GC_MX_PLATFORM.southbridge.gpe0_en, 0);
        assert_eq!(P5GC_MX_PLATFORM.southbridge.sata.unwrap().ports, 15);
    }

    #[test]
    fn audio_policy_preserves_the_reference_pin_values() {
        let hda = p5gc_mx_hda_config();
        let codec = hda.verbs.get(0);
        for (nid, value) in [
            (0x14, 0x0101_4010),
            (0x18, 0x01a1_9840),
            (0x19, 0x02a1_9850),
            (0x1a, 0x0181_304f),
            (0x1b, 0x0221_4020),
            (0x1c, 0x5933_01f0),
            (0x1e, 0x1856_1130),
        ] {
            let pin = codec
                .pins
                .as_slice()
                .iter()
                .find(|pin| pin.nid == nid)
                .unwrap();
            assert_eq!(pin.encode(), value);
        }
        assert_eq!(codec.vendor_id, 0x10ec_0883);
    }
}
