//! T60/R60/Z61t wiring from coreboot mainboard/lenovo/t60, including variants.
use fstart_core::{FlashLayout, Platform, X86LegacyFlashLayout, hvec};
use fstart_driver_intel::{
    generic::ck505::I2cCk505Config,
    southbridge::{
        gpio_ich as gpio, hda,
        pirq::{PciPin as P, PinRoute as R, Pirq as L, SlotPins},
    },
};
use fstart_driver_lenovo::t6::Model;
use fstart_intel_gma::{FallbackMode, OutputConfig, Port, framebuffer::FramebufferConfig};
use fstart_platform_intel::{
    i945::{
        GpiRoute, HdaConfig, HdaVerbTable, I945Ich7Config, I945Ich7Platform, I945IgdConfig,
        I945Variant, IdeConfig, LpcFixedIoDecode, LpcGenericIoDecode, LpcSerialDecode, PirqRouting,
        SataConfig, SataMode, UsbConfig,
    },
    igd::{IgdDisplayPolicy, VbtSource},
};

pub type Hardware = fstart_platform_intel::i945::I945Ich7<
    fstart_platform_intel::legacy_cpu::SocketM<0xfef0_0000, 0x80000>,
>;
pub const MODEL: Model = if cfg!(feature = "z61t") {
    Model::Z61t
} else if cfg!(feature = "r60") {
    Model::R60
} else {
    Model::T60
};
pub const BOARD_NAME: &str = match MODEL {
    Model::T60 => "lenovo-t60",
    Model::R60 => "lenovo-r60",
    Model::Z61t => "lenovo-z61t",
};
pub const PRODUCT_NAME: &str = match MODEL {
    Model::T60 => "ThinkPad T60",
    Model::R60 => "ThinkPad R60",
    Model::Z61t => "ThinkPad Z61t",
};
pub const BOARD_PACKAGE: &str = "fstart-board-lenovo-t60";
pub const PLATFORM: Platform = Platform::X86;
pub const UART0_NODE: &str = "dock_superio/com1";
pub const UART0_PIO_BASE: u16 = 0x3f8;
pub const UART0_CLOCK_FREQ: u32 = 1_843_200;
pub const UART0_BAUD_RATE: u32 = 115200;
const VBT: &str = if cfg!(feature = "z61t") {
    "z61t.vbt"
} else {
    "data.vbt"
};
pub const FLASH_SIZE: u32 = 0x20_0000;
pub const FLASH: FlashLayout = FlashLayout::X86Legacy(X86LegacyFlashLayout { size: FLASH_SIZE });
impl fstart_platform_intel::facts::IntelBoardFacts for crate::Board {
    type Platform = Hardware;
    const CONFIG: &'static I945Ich7Platform = &T6_PLATFORM;
    const FACTS: fstart_platform_intel::facts::BoardFacts =
        fstart_platform_intel::facts::BoardFacts::new(FLASH, FLASH_SIZE)
            .with_data_assets(&[VBT])
            .with_protected_mode();
}
pub const EC_SCI_GPIO: u8 = 12;
pub const T6_H8: fstart_driver_lenovo::h8::H8Config =
    fstart_driver_lenovo::t6::h8_config(EC_SCI_GPIO + 16);
const DISPLAY: IgdDisplayPolicy = IgdDisplayPolicy {
    outputs: &[
        OutputConfig {
            port: Port::Vga,
            enabled: true,
        },
        OutputConfig {
            port: Port::Lvds,
            enabled: true,
        },
    ],
    framebuffer: FramebufferConfig {
        scaling: fstart_intel_gma::scaler::ScalingPolicy::PreserveAspect,
        ..FramebufferConfig::edid(FallbackMode {
            width: 1024,
            height: 768,
            refresh_hz: 60,
        })
    },
};

pub const T6_PIRQ: PirqRouting = PirqRouting {
    slot_pins: &[
        SlotPins {
            slot: 0x1b,
            functions: [Some(P::B), None, None, None, None, None, None, None],
        },
        SlotPins {
            slot: 0x1c,
            functions: [
                Some(P::A),
                Some(P::B),
                Some(P::C),
                Some(P::D),
                None,
                None,
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
                Some(P::D),
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
                Some(P::C),
                Some(P::B),
                Some(P::A),
                None,
                None,
                None,
                None,
            ],
        },
    ],
    pin_routes: &[
        R::new(2, P::A, L::A),
        R::new(2, P::B, L::B),
        R::new(0x1b, P::A, L::A),
        R::new(0x1b, P::B, L::B),
        R::new(0x1b, P::C, L::A),
        R::new(0x1b, P::D, L::A),
        R::new(0x1c, P::A, L::E),
        R::new(0x1c, P::B, L::F),
        R::new(0x1c, P::C, L::G),
        R::new(0x1c, P::D, L::H),
        R::new(0x1d, P::A, L::A),
        R::new(0x1d, P::B, L::B),
        R::new(0x1d, P::C, L::C),
        R::new(0x1d, P::D, L::D),
        R::new(0x1e, P::A, L::G),
        R::new(0x1e, P::B, L::H),
        R::new(0x1e, P::C, L::A),
        R::new(0x1e, P::D, L::A),
        R::new(0x1f, P::A, L::H),
        R::new(0x1f, P::B, L::A),
        R::new(0x1f, P::C, L::A),
        R::new(0x1f, P::D, L::B),
    ],
    bridge_routes: &[
        R::new(0, P::A, L::A),
        R::new(0, P::B, L::B),
        R::new(0, P::C, L::C),
        R::new(1, P::A, L::A),
        R::new(2, P::A, L::F),
        R::new(2, P::B, L::G),
        R::new(8, P::A, L::E),
    ],
};

pub static T6_PLATFORM: I945Ich7Platform = {
    let mut platform = I945Ich7Config::new()
        .variant(I945Variant::Mobile)
        .igd(igd_config())
        .max_cpus(2)
        .gfx_gms(4)
        .pci_mmio_size(768)
        .spd_addresses([0x50, 0, 0x51, 0])
        .pcie_port(0, true)
        .pcie_port(1, true)
        .pcie_port(2, true)
        .pcie_port(3, true)
        .lan(false)
        .ac97_audio(false)
        .ac97_modem(false)
        .pirq_routing([0x0b; 8])
        .pirq(T6_PIRQ)
        // H8SCI is SMI until the board's ACPI command transfers ownership to SCI.
        .gpi_routing([
            GpiRoute::Disabled,
            GpiRoute::Disabled,
            GpiRoute::Disabled,
            GpiRoute::Disabled,
            GpiRoute::Disabled,
            GpiRoute::Disabled,
            GpiRoute::Disabled,
            GpiRoute::Disabled,
            GpiRoute::Sci,
            GpiRoute::Disabled,
            GpiRoute::Disabled,
            GpiRoute::Disabled,
            GpiRoute::Smi,
            GpiRoute::Sci,
            GpiRoute::Disabled,
            GpiRoute::Disabled,
        ])
        .gpe0_en(0x1100_0006)
        .c4_on_c3(true)
        .lpc_fixed_io(LpcFixedIoDecode {
            com_a: LpcSerialDecode::Com1,
            com_b: LpcSerialDecode::Com2,
            lpt: None,
            fdd: None,
        })
        .lpc_generic_io([
            LpcGenericIoDecode {
                base: T6_H8.resources.auxiliary_base.raw(),
                size: fstart_driver_lenovo::h8::H8Resources::AUXILIARY_SIZE,
            },
            LpcGenericIoDecode {
                base: T6_H8.resources.pmh7_base.raw(),
                size: fstart_driver_lenovo::h8::H8Resources::PMH7_SIZE as u16,
            },
            LpcGenericIoDecode {
                base: 0x1680,
                size: 0x20,
            },
        ])
        .ide(IdeConfig {
            enable_primary: false,
            enable_secondary: false,
        })
        .sata(SataConfig {
            mode: SataMode::Ahci,
            ports: 1,
        })
        .usb(UsbConfig {
            ehci: true,
            uhci: [true; 4],
        })
        .hda(HdaConfig::new().verb(hda_verbs()))
        .gpio_pins(gpio_pins())
        .build();
    platform.southbridge.c3_latency = 35;
    platform
};

pub const fn igd_config() -> I945IgdConfig {
    I945IgdConfig {
        vbt: VbtSource::ffs(VBT),
        panel: Some(fstart_driver_intel::i945::I945PanelConfig {
            panel_power_up_delay: 250,
            panel_backlight_on_delay: 2380,
            panel_power_down_delay: 250,
            panel_backlight_off_delay: 2380,
            panel_power_cycle_delay: 2,
            default_pwm_freq: 275,
        }),
        display: Some(DISPLAY),
    }
}

pub const fn hda_verbs() -> HdaVerbTable {
    use hda::{
        PinColor as C, PinConn as N, PinConnector as T, PinDevice as D, PinGeoLoc as G,
        PinLoc as Loc, pin_config as pin,
    };
    HdaVerbTable::new(0x11d4_1981, 0x17aa_2025)
        .pin(pin(
            5,
            D::LineOut,
            N::JackAndIntegrated,
            Loc::External,
            G::Left,
            T::StereoMono18,
            C::Green,
            1,
            1,
            0,
        ))
        .pin(pin(
            6,
            D::HpOut,
            N::Nc,
            Loc::External,
            G::Front,
            T::StereoMono18,
            C::Green,
            0,
            1,
            15,
        ))
        .pin(pin(
            7,
            D::Speaker,
            N::Nc,
            Loc::Internal,
            G::Special9,
            T::AtapiInternal,
            C::Black,
            1,
            15,
            0,
        ))
        .pin(pin(
            8,
            D::MicIn,
            N::JackAndIntegrated,
            Loc::External,
            G::Left,
            T::StereoMono18,
            C::Red,
            0,
            2,
            0,
        ))
        .pin(pin(
            9,
            D::LineIn,
            N::Nc,
            Loc::External,
            G::Rear,
            T::StereoMono18,
            C::Blue,
            0,
            2,
            1,
        ))
        .pin(pin(
            0x0a,
            D::SpdifOut,
            N::Jack,
            Loc::External,
            G::Rear,
            T::Rca,
            C::Yellow,
            0,
            15,
            0,
        ))
        .pin(pin(
            0x16,
            D::DeviceOther,
            N::Nc,
            Loc::Internal,
            G::Special9,
            T::AtapiInternal,
            C::Black,
            1,
            15,
            0,
        ))
        .pin(pin(
            0x17,
            D::Aux,
            N::Nc,
            Loc::Internal,
            G::Special9,
            T::AtapiInternal,
            C::Black,
            1,
            2,
            2,
        ))
        .pin(pin(
            0x18,
            D::MicIn,
            N::Nc,
            Loc::External,
            G::Rear,
            T::StereoMono18,
            C::Pink,
            0,
            2,
            3,
        ))
        .pin(pin(
            0x19,
            D::Cd,
            N::Integrated,
            Loc::Internal,
            G::Special9,
            T::AtapiInternal,
            C::White,
            1,
            2,
            14,
        ))
}

pub const fn gpio_pins() -> [gpio::GpioPin; 24] {
    use gpio::{GpioLevel::*, input, output};
    [
        input(1).inverted(),
        input(6).inverted(),
        input(7).inverted(),
        input(8).inverted(),
        input(9),
        input(10),
        input(12).inverted(),
        input(13).inverted(),
        input(14),
        input(15),
        output(19, High),
        if cfg!(feature = "r60") {
            input(21)
        } else {
            gpio::GpioPin {
                mode: gpio::GpioMode::Native,
                ..input(21)
            }
        },
        if cfg!(feature = "r60") {
            output(22, Low)
        } else {
            input(22)
        },
        output(24, High),
        output(25, High),
        output(26, Low),
        output(27, High),
        output(28, High),
        if cfg!(feature = "r60") {
            input(34)
        } else {
            gpio::GpioPin {
                mode: gpio::GpioMode::Native,
                ..input(34)
            }
        },
        input(36),
        input(37),
        input(38),
        input(39),
        output(48, High),
    ]
}

pub fn ck505_config() -> I2cCk505Config {
    if cfg!(feature = "z61t") {
        I2cCk505Config {
            mask: hvec([0xff; 8]),
            regs: hvec([0x6d, 0xff, 0xff, 0x20, 0x41, 0x7f, 0x18, 0]),
        }
    } else {
        I2cCk505Config {
            mask: hvec([0xff; 12]),
            regs: hvec([0x2e, 0xf7, 0x3c, 0x20, 1, 0, 0x1b, 1, 0x54, 0xff, 0xff, 7]),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn routing_and_codec_match_the_family_reference() {
        for (slot, ir) in [
            (0x1b, 0x0010),
            (0x1c, 0x7654),
            (0x1d, 0x3210),
            (0x1e, 0x0076),
            (0x1f, 0x1007),
        ] {
            assert_eq!(T6_PIRQ.ir_value(slot), ir);
        }
        for (slot, ip) in [
            (0x1b, 2),
            (0x1c, 0x4321),
            (0x1d, 0x4000_4321),
            (0x1f, 0x1230),
        ] {
            assert_eq!(T6_PIRQ.ip_value(slot), ip);
        }
        assert_eq!(T6_PLATFORM.southbridge.c3_latency, 35);
        assert_eq!(hda_verbs().pins.len(), 10);
        let expected = [
            0xc3014110, 0x4221401f, 0x591311f0, 0xc3a15020, 0x41813021, 0x014470f0, 0x59f311f0,
            0x59931122, 0x41a19023, 0x9933e12e,
        ];
        assert!(
            hda_verbs()
                .pins
                .as_slice()
                .iter()
                .zip(expected)
                .all(|(pin, value)| pin.encode() == value)
        );
        assert_eq!(T6_H8.ec_gpe, 0x1c);
    }
    #[test]
    fn variant_gpio_clock_and_identity_are_selected_together() {
        let pins = gpio_pins();
        let wp = pins.iter().find(|p| p.pin == 22).unwrap();
        assert_eq!(
            wp.dir,
            if cfg!(feature = "r60") {
                gpio::GpioDir::Output
            } else {
                gpio::GpioDir::Input
            }
        );
        assert_eq!(
            ck505_config().regs.len(),
            if cfg!(feature = "z61t") { 8 } else { 12 }
        );
        assert_eq!(
            VBT,
            if cfg!(feature = "z61t") {
                "z61t.vbt"
            } else {
                "data.vbt"
            }
        );
    }
}
