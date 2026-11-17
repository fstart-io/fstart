//! T61/T61p wiring and factory flash geometry.

use fstart_core::hvec;
use fstart_driver_intel::{
    gma::{FallbackMode, OutputConfig, Port, framebuffer::FramebufferConfig},
    southbridge::{gpio_ich as gpio, hda, pirq},
};
use fstart_platform_intel::gm965::*;
use fstart_platform_intel::igd::{IgdDisplayPolicy, VbtSource};

pub const BOARD_NAME: &str = "lenovo-t61";
pub const BOARD_PACKAGE: &str = "fstart-board-lenovo-t61";
pub const PLATFORM: fstart_core::Platform = fstart_core::Platform::X86_64;
pub const UART0_NODE: &str = "dock/com1";
pub const UART0_PIO_BASE: u16 = 0x3f8;
pub const UART0_CLOCK_FREQ: u32 = 1_843_200;
pub const UART0_BAUD_RATE: u32 = 115_200;

pub type Hardware = Gm965Ich8<fstart_platform_intel::legacy_cpu::SocketP<0xfef0_0000, 0x80000>>;

pub const FLASH: fstart_core::IntelIfdFlashLayout = {
    use fstart_core::{
        ConstVec, IntelIfdFlashLayout, IntelIfdRegion as Kind, IntelIfdRegionConfig as Region,
    };
    const FD: Region = Region {
        kind: Kind::Descriptor,
        offset: 0,
        size: 0x1000,
    };
    IntelIfdFlashLayout::new(
        ConstVec::new(FD)
            .push(FD)
            .push(Region {
                kind: Kind::Me,
                offset: 0x1000,
                size: 0x27d000,
            })
            .push(Region {
                kind: Kind::Gbe,
                offset: 0x27e000,
                size: 0x2000,
            })
            .push(Region {
                kind: Kind::Bios,
                offset: 0x280000,
                size: 0x180000,
            }),
    )
};

impl fstart_platform_intel::facts::IntelBoardFacts for crate::Board {
    type Platform = Hardware;
    const CONFIG: &'static Gm965Ich8Platform = &T61_PLATFORM;
    const FACTS: fstart_platform_intel::facts::BoardFacts =
        fstart_platform_intel::facts::BoardFacts::new(
            fstart_core::FlashLayout::IntelIfd(FLASH),
            0x400000,
        )
        .with_data_assets(&["data.vbt"])
        .with_memory_cache();
}

pub const T61_H8: fstart_driver_lenovo::h8::H8Config = fstart_driver_lenovo::t6::h8_config(0x12);

pub const T61_PIRQ: PirqRouting = {
    const SLOTS: [pirq::SlotPins; 7] = {
        let base = pirq::ICH8_ROUTING.slot_pins;
        let mut slots = [
            base[0], base[1], base[2], base[3], base[4], base[5], base[6],
        ];
        slots[3].functions[4] = Some(pirq::PciPin::A);
        slots
    };
    PirqRouting {
        slot_pins: &SLOTS,
        bridge_routes: &[
            pirq::PinRoute::new(0, pirq::PciPin::A, pirq::Pirq::A),
            pirq::PinRoute::new(0, pirq::PciPin::B, pirq::Pirq::B),
            pirq::PinRoute::new(0, pirq::PciPin::C, pirq::Pirq::C),
            pirq::PinRoute::new(0, pirq::PciPin::D, pirq::Pirq::D),
            pirq::PinRoute::new(1, pirq::PciPin::A, pirq::Pirq::A),
            pirq::PinRoute::new(2, pirq::PciPin::A, pirq::Pirq::F),
            pirq::PinRoute::new(2, pirq::PciPin::B, pirq::Pirq::G),
            pirq::PinRoute::new(8, pirq::PciPin::A, pirq::Pirq::E),
        ],
        ..pirq::ICH8_ROUTING
    }
};

pub static T61_PLATFORM: Gm965Ich8Platform = Gm965Ich8Config::new()
    .max_cpus(2)
    .pirq(T61_PIRQ)
    .peg(true)
    .igd(igd_config())
    .pcie_port(0, true)
    .pcie_port(1, true)
    .pcie_port(2, true)
    .pcie_port(3, true)
    .pcie_port(4, true)
    .lpc_fixed_io(LpcFixedIoDecode {
        com_a: LpcSerialDecode::Com1,
        com_b: LpcSerialDecode::Com2,
        lpt: None,
        fdd: None,
    })
    .lpc_generic_io([
        LpcGenericIoDecode {
            base: T61_H8.resources.auxiliary_base.raw(),
            size: fstart_driver_lenovo::h8::H8Resources::AUXILIARY_SIZE,
        },
        LpcGenericIoDecode {
            base: T61_H8.resources.pmh7_base.raw(),
            size: fstart_driver_lenovo::h8::H8Resources::PMH7_SIZE as u16,
        },
        LpcGenericIoDecode {
            base: 0x1680,
            size: 0x20,
        },
    ])
    .gpe0_en(0x0104_0046)
    .gpi_routing({
        let mut routes = [gpio::GpiRoute::Disabled; 16];
        routes[2] = gpio::GpiRoute::Sci;
        routes[8] = gpio::GpiRoute::Sci;
        routes[13] = gpio::GpiRoute::Sci;
        routes
    })
    .ide(IdeConfig {
        enable_primary: false,
        enable_secondary: false,
    })
    .sata(SataConfig {
        mode: SataMode::Ahci,
        ports: 0x05,
        hotplug_map: 0,
        clock_request: false,
        traffic_monitor: false,
    })
    .usb(UsbConfig {
        ehci: [true; 2],
        uhci: [true; 6],
    })
    // W25X32: opcode 0x20, 4 KiB blocks. SST-equipped boards need 0x2009.
    .spi_vscc(0x2005)
    .hda_verb(0x11d4_1984, 0x17aa_20d7, hda_pins(), hda_extra())
    .gpio_pins(gpio_pins())
    .build();

pub const fn igd_config() -> Gm965IgdConfig {
    Gm965IgdConfig {
        vbt: VbtSource::ffs("data.vbt"),
        panel_power_up_delay: 250,
        panel_power_down_delay: 250,
        panel_backlight_on_delay: 2500,
        panel_backlight_off_delay: 2500,
        panel_power_cycle_delay: 3,
        default_pwm_freq: 220,
        display: Some(IgdDisplayPolicy {
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
                scaling: fstart_driver_intel::gma::scaler::ScalingPolicy::PreserveAspect,
                ..FramebufferConfig::edid(FallbackMode {
                    width: 1024,
                    height: 768,
                    refresh_hz: 60,
                })
            },
        }),
        ..Gm965IgdConfig::new()
    }
}

/// GPIO42 selects the SPD/clock branch high and identity EEPROM low.
pub const SMBUS_MUX: gpio::GpioPin = gpio::output(42, gpio::GpioLevel::Low);

pub const fn gpio_pins() -> [gpio::GpioPin; 35] {
    use gpio::{
        GpioLevel::{High, Low},
        input as i, output as o,
    };
    const fn native_inverted(pin: u8) -> gpio::GpioPin {
        gpio::GpioPin {
            mode: gpio::GpioMode::Native,
            ..gpio::input(pin).inverted()
        }
    }
    [
        native_inverted(0),
        i(1).inverted(),
        i(2).inverted(),
        i(3).inverted(),
        i(4).inverted(),
        i(5).inverted(),
        i(6).inverted(),
        i(7).inverted(),
        i(8).inverted(),
        o(9, Low),
        native_inverted(11),
        native_inverted(12),
        native_inverted(13),
        i(17),
        o(18, High).blink(),
        i(19),
        o(20, High),
        i(21),
        i(22),
        o(24, Low),
        o(27, Low),
        o(28, Low),
        o(29, High),
        i(30),
        i(31),
        o(33, High),
        o(34, Low),
        i(36),
        i(37),
        i(38),
        i(39),
        o(41, High),
        SMBUS_MUX,
        o(43, Low),
        i(48),
    ]
}

pub fn ck505_config() -> fstart_driver_intel::generic::ck505::I2cCk505Config {
    fstart_driver_intel::generic::ck505::I2cCk505Config {
        mask: hvec([0xff]),
        regs: hvec([0x51]),
    }
}

const fn hda_pins() -> [hda::PinConfig; 7] {
    use hda::{
        PinColor::*, PinConn::*, PinConnector::*, PinDevice::*, PinGeoLoc::*, PinLoc::*,
        pin_config as p,
    };
    [
        p(
            0x11,
            HpOut,
            Jack,
            External,
            Front,
            StereoMono18,
            Green,
            0,
            1,
            15,
        ),
        p(
            0x12,
            Speaker,
            Integrated,
            Internal,
            NA,
            OtherAnalog,
            ColorUnknown,
            1,
            1,
            0,
        ),
        p(
            0x14,
            MicIn,
            Jack,
            External,
            Front,
            StereoMono18,
            Red,
            0,
            2,
            1,
        ),
        p(
            0x15,
            MicIn,
            Integrated,
            Internal,
            NA,
            OtherAnalog,
            ColorUnknown,
            1,
            2,
            14,
        ),
        p(
            0x1a,
            DeviceOther,
            Integrated,
            Internal,
            Rear,
            AtapiInternal,
            Black,
            1,
            15,
            0,
        ),
        p(
            0x1b,
            SpdifOut,
            Jack,
            SeparateChassis,
            Rear,
            Rca,
            Black,
            1,
            10,
            0,
        ),
        p(
            0x1c,
            MicIn,
            Jack,
            SeparateChassis,
            Rear,
            StereoMono18,
            Red,
            0,
            2,
            0,
        ),
    ]
}

const fn hda_extra() -> [u32; 16] {
    let pins = [
        (0x13, 0x511301f0),
        (0x16, 0x593301f0),
        (0x17, 0x55a601f0),
        (0x18, 0x55a601f0),
    ];
    let mut verbs = [0; 16];
    let mut i = 0;
    while i < pins.len() {
        let pin = hda::hda_pin_cfg(0, pins[i].0, pins[i].1);
        let mut j = 0;
        while j < 4 {
            verbs[4 * i + j] = pin[j];
            j += 1;
        }
        i += 1;
    }
    verbs
}
