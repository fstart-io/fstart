//! Lenovo ThinkPad X60 chipset and attached-device configuration.
//!
//! Facts ported from coreboot `mainboard/lenovo/x60`: `devicetree.cb`,
//! `gpio.c`, `early_init.c` (LPC decode, RCBA interrupt routing, SPD map),
//! `hda_verb.c`, `acpi/ich7_pci_irqs.asl` and `cmos.default`.

use fstart_core::{FlashLayout, Platform, X86LegacyFlashLayout, hvec};
use fstart_driver_intel::generic::ck505::I2cCk505Config;
use fstart_driver_intel::southbridge::gpio_ich as gpio;
use fstart_driver_intel::southbridge::hda;
use fstart_driver_intel::southbridge::pirq::{PciPin as P, PinRoute as R, Pirq as L, SlotPins};
use fstart_intel_gma::framebuffer::FramebufferConfig;
use fstart_intel_gma::{FallbackMode, OutputConfig, Port};
use fstart_platform_intel::i945::{
    HdaConfig, HdaVerbTable, I945Ich7Config, I945Ich7Platform, I945IgdConfig, I945Variant,
    IdeConfig, LpcFixedIoDecode, LpcGenericIoDecode, LpcParallelDecode, LpcSerialDecode,
    PirqRouting, SataConfig, SataMode, UsbConfig,
};
use fstart_platform_intel::igd::{IgdDisplayPolicy, VbtSource};

/// Socket M; the CAR window matches the X61's budgeted Core 2 setup.
pub type Hardware = fstart_platform_intel::i945::I945Ich7<
    fstart_platform_intel::legacy_cpu::SocketM<0xfef0_0000, 0x80000>,
>;

pub const BOARD_NAME: &str = "lenovo-x60";
pub const BOARD_PACKAGE: &str = "fstart-board-lenovo-x60";
pub const PLATFORM: Platform = Platform::X86_64;
pub const UART0_NODE: &str = "dock_superio/com1";
pub const UART0_PIO_BASE: u16 = 0x3f8;
pub const UART0_CLOCK_FREQ: u32 = 1_843_200;
pub const UART0_BAUD_RATE: u32 = 115_200;

/// Board VBT: packaged as a verified FFS data asset under this name and read
/// back by the same name when the OpRegion is published.
const X60_VBT: &str = "data.vbt";

/// 2 MiB SPI flash; ICH7 has no flash descriptor (`BOARD_ROMSIZE_KB_2048`).
pub const FLASH_SIZE: u32 = 0x0020_0000;
pub const FLASH: FlashLayout = FlashLayout::X86Legacy(X86LegacyFlashLayout { size: FLASH_SIZE });

impl fstart_platform_intel::facts::IntelBoardFacts for crate::Board {
    type Platform = Hardware;
    const CONFIG: &'static I945Ich7Platform = &X60_PLATFORM;
    const FACTS: fstart_platform_intel::facts::BoardFacts =
        fstart_platform_intel::facts::BoardFacts::new(FLASH, FLASH_SIZE)
            .with_data_assets(&[X60_VBT]);
}

/// The H8 EC SCI is ICH7 GPIO12 (`H8SCI#`), i.e. GPE 0x1c.
pub const EC_SCI_GPIO: u8 = 12;

/// One source for EC transport, LPC windows, and AML resources/capabilities.
pub const X60_H8: fstart_driver_lenovo::h8::H8Config =
    fstart_driver_lenovo::x6::h8_config(EC_SCI_GPIO + 16);

/// Probe VGA and the internal LVDS panel, mirroring one shared framebuffer.
const X60_DISPLAY: IgdDisplayPolicy = IgdDisplayPolicy {
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

/// RCBA interrupt router (coreboot `mainboard_late_rcba_config()`) and the
/// devices behind the 0:1e.0 bridge (`acpi/ich7_pci_irqs.asl`): the Ricoh
/// multifunction device and the UltraBase dock slots.
pub const X60_PIRQ: PirqRouting = PirqRouting {
    slot_pins: &[
        // D27IP = 0x00000002
        SlotPins {
            slot: 0x1b,
            functions: [Some(P::B), None, None, None, None, None, None, None],
        },
        // D28IP = 0x00004321
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
        // D29IP = 0x40004321
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
        // D30IP keeps its reset value: AC'97 audio INTA, modem INTB.
        SlotPins {
            slot: 0x1e,
            functions: [None, None, Some(P::A), Some(P::B), None, None, None, None],
        },
        // D31IP = 0x00001230: IDE INTC, SATA INTB, SMBus INTA.
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
        // Integrated graphics: outside the router, 1:1.
        R::new(0x02, P::A, L::A),
        R::new(0x02, P::B, L::B),
        // D27IR = 0x0010
        R::new(0x1b, P::A, L::A),
        R::new(0x1b, P::B, L::B),
        R::new(0x1b, P::C, L::A),
        R::new(0x1b, P::D, L::A),
        // D28IR = 0x7654
        R::new(0x1c, P::A, L::E),
        R::new(0x1c, P::B, L::F),
        R::new(0x1c, P::C, L::G),
        R::new(0x1c, P::D, L::H),
        // D29IR = 0x3210
        R::new(0x1d, P::A, L::A),
        R::new(0x1d, P::B, L::B),
        R::new(0x1d, P::C, L::C),
        R::new(0x1d, P::D, L::D),
        // D30IR = 0x0076
        R::new(0x1e, P::A, L::G),
        R::new(0x1e, P::B, L::H),
        R::new(0x1e, P::C, L::A),
        R::new(0x1e, P::D, L::A),
        // D31IR = 0x1007
        R::new(0x1f, P::A, L::H),
        R::new(0x1f, P::B, L::A),
        R::new(0x1f, P::C, L::A),
        R::new(0x1f, P::D, L::B),
    ],
    bridge_routes: &[
        R::new(0x00, P::A, L::A),
        R::new(0x00, P::B, L::B),
        R::new(0x00, P::C, L::C),
        R::new(0x01, P::A, L::A),
        R::new(0x02, P::A, L::F),
        R::new(0x02, P::B, L::G),
        R::new(0x08, P::A, L::E),
    ],
};

pub static X60_PLATFORM: I945Ich7Platform = I945Ich7Config::new()
    .variant(I945Variant::Mobile)
    .igd(x60_igd_config())
    .max_cpus(2)
    // 16 MiB UMA: room for a VGA monitor mode mirrored with the panel.
    .gfx_gms(4)
    .pci_mmio_size(768)
    // Channel 0 and channel 1 slot 0 only (`mainboard_get_spd_map`).
    .spd_addresses([0x50, 0x00, 0x51, 0x00])
    // 1c.0 Ethernet, 1c.1 WLAN, 1c.2/1c.3 ExpressCard and dock.
    .pcie_port(0, true)
    .pcie_port(1, true)
    .pcie_port(2, true)
    .pcie_port(3, true)
    .lan(false)
    .ac97_audio(false)
    .ac97_modem(false)
    .pirq_routing([0x0b; 8])
    .pirq(X60_PIRQ)
    // GPIO8 H8_WAKE# and GPIO13 dock: SCI; GPIO12 H8SCI#: SMI until ACPI.
    .gpi_routing([0, 0, 0, 0, 0, 0, 0, 0, 2, 0, 0, 0, 1, 2, 0, 0])
    .gpe0_en(0x1100_0006)
    .c4_on_c3(true)
    .lpc_fixed_io(LpcFixedIoDecode {
        com_a: LpcSerialDecode::Com1,
        com_b: LpcSerialDecode::Com2,
        lpt: Some(LpcParallelDecode::Lpt3bc),
        fdd: None,
    })
    .lpc_generic_io([
        LpcGenericIoDecode {
            base: X60_H8.resources.auxiliary_base.raw(),
            size: fstart_driver_lenovo::h8::H8Resources::AUXILIARY_SIZE,
        },
        LpcGenericIoDecode {
            base: X60_H8.resources.pmh7_base.raw(),
            size: fstart_driver_lenovo::h8::H8Resources::PMH7_SIZE as u16,
        },
        LpcGenericIoDecode {
            base: 0x1680,
            size: 0x0020,
        },
    ])
    .ide(IdeConfig {
        // Enabled dynamically only for a populated, connected dock UltraBay.
        enable_primary: false,
        enable_secondary: false,
    })
    .sata(SataConfig {
        mode: SataMode::Ahci,
        ports: 0x01,
    })
    .usb(UsbConfig {
        ehci: true,
        uhci: [true; 4],
    })
    .hda(HdaConfig::new().verb(x60_hda_verbs()))
    .gpio_pins(x60_gpio_pins())
    .build();

pub const fn x60_igd_config() -> I945IgdConfig {
    I945IgdConfig {
        vbt: VbtSource::ffs(X60_VBT),
        panel: Some(fstart_driver_intel::i945::I945PanelConfig {
            panel_power_up_delay: 250,
            panel_backlight_on_delay: 2380,
            panel_power_down_delay: 250,
            panel_backlight_off_delay: 2380,
            panel_power_cycle_delay: 2,
            default_pwm_freq: 180,
        }),
        display: Some(X60_DISPLAY),
    }
}

/// Analog Devices AD1981HD pin configuration (coreboot `hda_verb.c`).
pub const fn x60_hda_verbs() -> HdaVerbTable {
    use hda::{PinColor as C, PinConn as N, PinConnector as T, PinDevice as D, PinGeoLoc as G};
    use hda::{PinLoc as Loc, pin_config as pin};
    HdaVerbTable::new(0x11d4_1981, 0x17aa_2025)
        .pin(pin(
            0x05,
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
            0x06,
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
            0x07,
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
            0x08,
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
            0x09,
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

/// GPIO pads from coreboot `gpio.c`; unlisted pins keep their native function.
pub const fn x60_gpio_pins() -> [gpio::GpioPin; 22] {
    use gpio::{GpioLevel::*, input, output};
    [
        input(1).inverted(), // HDD_PRESENCE#
        input(6).inverted(),
        input(7).inverted(), // BDC_PRESENCE#
        input(8).inverted(), // H8_WAKE#
        input(9),            // RTC_BAT_IN#
        input(10),
        input(12).inverted(), // H8SCI#
        input(13).inverted(), // SLICE_ON_3M# (dock present)
        input(14),
        input(15),
        input(22), // FWH_WP#
        output(24, High),
        output(25, High), // MDC_KILL#
        output(26, Low),
        output(27, High),
        output(28, High),
        output(33, High), // HDD_PRESENCE_2#
        input(36),        // PLANARID0..3
        input(37),
        input(38),
        input(39),
        output(48, High), // FWH_TBL#
    ]
}

/// CK505 clock generator programming (coreboot devicetree `drivers/i2c/ck505`).
pub fn x60_ck505_config() -> I2cCk505Config {
    I2cCk505Config {
        mask: hvec([0xff; 12]),
        regs: hvec([
            0x2e, 0xf7, 0x3c, 0x20, 0x01, 0x00, 0x1b, 0x01, 0x54, 0xff, 0xff, 0x07,
        ]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hda_pin_configs_match_coreboot_values() {
        let expected = [
            (0x05, 0xc301_4110),
            (0x06, 0x4221_401f),
            (0x07, 0x5913_11f0),
            (0x08, 0xc3a1_5020),
            (0x09, 0x4181_3021),
            (0x0a, 0x0144_70f0),
            (0x16, 0x59f3_11f0),
            (0x17, 0x5993_1122),
            (0x18, 0x41a1_9023),
            (0x19, 0x9933_e12e),
        ];
        let table = x60_hda_verbs();
        let pins = table.pins.as_slice();
        assert_eq!(pins.len(), expected.len());
        for (pin, (nid, value)) in pins.iter().zip(expected) {
            assert_eq!((pin.nid, pin.encode()), (nid, value));
        }
    }

    #[test]
    fn interrupt_router_matches_coreboot_rcba_values() {
        for (slot, ir) in [
            (0x1b, 0x0010),
            (0x1c, 0x7654),
            (0x1d, 0x3210),
            (0x1e, 0x0076),
            (0x1f, 0x1007),
        ] {
            assert_eq!(X60_PIRQ.ir_value(slot), ir);
        }
        for (slot, ip) in [
            (0x1b, 0x0000_0002),
            (0x1c, 0x0000_4321),
            (0x1d, 0x4000_4321),
            (0x1f, 0x0000_1230),
        ] {
            assert_eq!(X60_PIRQ.ip_value(slot), ip);
        }
    }

    #[test]
    fn ec_sci_is_gpe_28() {
        assert_eq!(X60_H8.ec_gpe, 0x1c);
    }
}
