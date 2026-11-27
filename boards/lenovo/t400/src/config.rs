//! Lenovo T400 / T500 / R400 / W500 / R500 board wiring; chipset mechanisms
//! remain in GM45/ICH9.

use fstart_core::{
    ConstVec, FlashLayout, IntelIfdFlashLayout, IntelIfdRegion as Kind,
    IntelIfdRegionConfig as Region,
};
use fstart_driver_intel::gma::framebuffer::FramebufferConfig;
use fstart_driver_intel::gma::{FallbackMode, OutputConfig, Port};
use fstart_driver_intel::southbridge::pirq::{PinRoute, Pirq};
use fstart_driver_intel::southbridge::{gpio_ich as gpio, hda};
use fstart_platform_intel::facts::PciSubsystemId;
use fstart_platform_intel::gm45::{
    BacklightPwm, CState, Core2Cstates, EhciController, Gm45Ich9Config, Gm45Ich9Platform,
    Gm45IgdConfig, GpiRoute, LpcFixedIoDecode, LpcFloppyDecode, LpcGenericIoDecode,
    LpcParallelDecode, LpcSerialDecode, PciePowerLimit, SataConfig, SataMode, UsbConfig,
};
use fstart_platform_intel::igd::{IgdDisplayPolicy, VbtSource};

/// Which ThinkPad T400-series variant this build targets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Variant {
    T400,
    T500,
    R400,
    W500,
    R500,
}

pub const VARIANT: Variant = if cfg!(feature = "r500") {
    Variant::R500
} else if cfg!(feature = "w500") {
    Variant::W500
} else if cfg!(feature = "r400") {
    Variant::R400
} else if cfg!(feature = "t500") {
    Variant::T500
} else {
    Variant::T400
};

pub const IS_R500: bool = matches!(VARIANT, Variant::R500);

pub const BOARD_NAME: &str = match VARIANT {
    Variant::T400 => "lenovo-t400",
    Variant::T500 => "lenovo-t500",
    Variant::R400 => "lenovo-r400",
    Variant::W500 => "lenovo-w500",
    Variant::R500 => "lenovo-r500",
};

pub const PRODUCT_NAME: &str = match VARIANT {
    Variant::T400 => "ThinkPad T400",
    Variant::T500 => "ThinkPad T500",
    Variant::R400 => "ThinkPad R400",
    Variant::W500 => "ThinkPad W500",
    Variant::R500 => "ThinkPad R500",
};

pub const BOARD_PACKAGE: &str = "fstart-board-lenovo-t400";

/// Dock PC87384 COM1 serial console parameters (coreboot `t400/dock.c`).
pub const UART0_PIO_BASE: u16 = 0x3f8;
pub const UART0_CLOCK_FREQ: u32 = 1_843_200;
pub const UART0_BAUD_RATE: u32 = 115200;

/// 8-MiB IFD layout for T400 / T500 / R400 / W500 (coreboot `t400/vboot-rwa.fmd`).
pub const FLASH_8M: IntelIfdFlashLayout = {
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
                size: 0x5f5000,
            })
            .push(Region {
                kind: Kind::Gbe,
                offset: 0x5f6000,
                size: 0x2000,
            })
            .push(Region {
                kind: Kind::Pdr,
                offset: 0x5f8000,
                size: 0x8000,
            })
            .push(Region {
                kind: Kind::Bios,
                offset: 0x600000,
                size: 0x200000,
            }),
    )
};

/// 4-MiB IFD layout for R500 (coreboot `t400/Kconfig`: `BOARD_ROMSIZE_KB_4096`,
/// no GbE region, 2-MiB BIOS region).
pub const FLASH_4M: IntelIfdFlashLayout = {
    const FD: Region = Region {
        kind: Kind::Descriptor,
        offset: 0,
        size: 0x1000,
    };
    IntelIfdFlashLayout::new(ConstVec::new(FD).push(FD).push(Region {
        kind: Kind::Bios,
        offset: 0x200000,
        size: 0x200000,
    }))
};

pub const FLASH: IntelIfdFlashLayout = if IS_R500 { FLASH_4M } else { FLASH_8M };
pub const FLASH_SIZE: u32 = if IS_R500 { 0x40_0000 } else { 0x80_0000 };

const VBT: &str = if IS_R500 { "r500.vbt" } else { "t400.vbt" };

pub type Hardware =
    fstart_platform_intel::gm45::Gm45Ich9<fstart_platform_intel::legacy_cpu::SocketP>;

impl fstart_platform_intel::facts::IntelBoardFacts for crate::Board {
    type Platform = Hardware;
    const OPTIONS: &'static [&'static dyn fstart_options::Describe] = if IS_R500 {
        &[&crate::mainboard::EC]
    } else {
        &[&crate::mainboard::EC, &crate::mainboard::HYBRID_GRAPHICS]
    };
    const CONFIG: &'static Gm45Ich9Platform = &T400_PLATFORM;
    const FACTS: fstart_platform_intel::facts::BoardFacts =
        fstart_platform_intel::facts::BoardFacts::new(FlashLayout::IntelIfd(FLASH), FLASH_SIZE)
            .with_data_assets(&[VBT])
            .with_memory_cache();
}

/// Coreboot `t400/devicetree.cb`: root port 4 (00:1c.3) carries
/// `smbios_slot_desc "7" "3" "ExpressCard Slot" "8"`.
pub const EXPRESSCARD: fstart_core::smbios::Attached<'static> =
    fstart_core::smbios::Attached::Slot(fstart_core::smbios::SystemSlot {
        designation: "ExpressCard Slot",
        slot_type: 0x07,
        data_bus_width: 0x08,
        length: 0x03,
        id: 0,
        characteristics: [0, 0x03],
    });

/// SMBus mux (GPIO42): high reaches the SPDs, low the identity EEPROM.
pub const T400_SMBUS_MUX: u8 = 42;

/// Hybrid graphics / discrete GPU GPIOs (`variants/t400/overridetree.cb` and
/// `t400/romstage.c`).
pub const DGPU_DETECT_GPIO: u8 = 21;
pub const PANEL_MUX_GPIO: u8 = 22;
pub const BACKLIGHT_MUX_GPIO: u8 = 19;
pub const DGPU_POWER_GPIO: u8 = 49;

/// H8 EC: transport, LPC windows and the AML surface. The EC SCI is ICH9
/// GPIO1, i.e. GPE 0x11.
pub const T400_H8: fstart_driver_lenovo::h8::H8Config = {
    use fstart_driver_lenovo::{
        ec::EcPorts,
        h8::{H8BatteryThresholdBackend, H8Config, H8Resources},
    };
    H8Config {
        config: crate::ec::CONTROLS,
        beep_masks: [0xfe, 0x96],
        event_masks: crate::ec::EVENT_MASKS,
        has_power_management_beeps: true,
        has_bluetooth: true,
        has_wwan: true,
        has_uwb: true,
        has_thinklight: true,
        second_thermal_zone: true,
        critical_temp_celsius: 100,
        passive_temp_celsius: 90,
        bat_info_extended: true,
        bat_thresholds: true,
        bat_threshold_backend: H8BatteryThresholdBackend::Registers24 { bit7: false },
        ..H8Config::new(
            H8Resources::new(EcPorts::new(0x62, 0x66), 0x1600, 0x15e0),
            0x11,
            "IBM0068",
        )
    }
};

/// Conventional PCI wiring from coreboot `lenovo/t400/acpi/ich9_pci_irqs.asl`.
const PCI_BRIDGE_ROUTES: &[PinRoute] = {
    use Pirq::{A, B, C, D};
    [
        PinRoute::slot(0, [A, B, C, D]),
        PinRoute::slot(1, [B, C, D, A]),
        PinRoute::slot(2, [C, D, A, B]),
        PinRoute::slot(3, [A, B, C, D]),
        PinRoute::slot(4, [A, B, C, D]),
        PinRoute::slot(5, [B, C, D, A]),
    ]
    .as_flattened()
};

pub static T400_PLATFORM: Gm45Ich9Platform = {
    let base = Gm45Ich9Config::new()
        .laptop()
        .pci_subsystem_ids(if IS_R500 {
            R500_SUBSYSTEM_IDS
        } else {
            T400_SUBSYSTEM_IDS
        })
        // T400/T500/R400/W500: 03.0 on, 03.1..3 off. R500: all ME off.
        .disabled_me_functions(if IS_R500 { 0b1111 } else { 0b1110 })
        // R500 has a PCIe Ethernet NIC on 1c.5, not the ICH9 LAN at 19.0.
        .disable_lan(IS_R500)
        // Socket P supports quad-core Penryn CPUs (coreboot socket_p MAX_CPUS=4).
        .max_cpus(4)
        .spd_addresses([0x50, 0, 0x51, 0])
        .preserve_hardware_pirq(PCI_BRIDGE_ROUTES)
        .pirq_routing([0x0b, 0x0b, 0x0b, 0x0b, 0x80, 0x80, 0x80, 0x80])
        .gpe0_en(0x0100_0000)
        .gpi_routing([
            GpiRoute::Disabled,
            GpiRoute::Sci,
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
            GpiRoute::Disabled,
            GpiRoute::Disabled,
            GpiRoute::Disabled,
            GpiRoute::Disabled,
        ])
        .c_states(true, true, true)
        .slfm(true)
        .throttle_duty(2)
        // cmos.default: power_on_after_fail=Disable.
        .power_on_after_fail(0)
        .lpc_fixed_io(LpcFixedIoDecode {
            com_a: LpcSerialDecode::Com1,
            com_b: LpcSerialDecode::Com2,
            lpt: Some(LpcParallelDecode::Lpt378),
            fdd: Some(LpcFloppyDecode::Fdd3f0),
        })
        .lpc_generic_io([
            LpcGenericIoDecode {
                base: T400_H8.resources.auxiliary_base.raw(),
                size: fstart_driver_lenovo::h8::H8Resources::AUXILIARY_SIZE,
            },
            LpcGenericIoDecode {
                base: T400_H8.resources.pmh7_base.raw(),
                size: fstart_driver_lenovo::h8::H8Resources::PMH7_SIZE as u16,
            },
            LpcGenericIoDecode {
                base: 0x1680,
                size: 0x20,
            },
        ])
        .sata(SataConfig {
            mode: SataMode::Ahci,
            ports: 0x03,
            hotplug_map: 0,
            clock_request: IS_R500,
            traffic_monitor: false,
        })
        .usb(UsbConfig {
            ehci: [true; 2],
            uhci: [true; 6],
        })
        // coreboot `USBDEBUG_HCD_INDEX=2`: port 1 of EHCI2 (00:1a.7).
        .ehci_debug_controller(EhciController::Ehci2)
        .igd(T400_IGD)
        .cstates(T400_CSTATES)
        .hda_verb(0x14f1_5051, 0x17aa_211c, t400_hda_pins(), T400_BEEP_VERBS);

    if IS_R500 {
        // variants/r500/overridetree.cb: ports 0, 1, 3, 4, 5 enabled and slots
        // at 41 W; port 5 hotplug; port 2 off.
        base.pcie_port(0, true)
            .pcie_port(1, true)
            .pcie_port(3, true)
            .pcie_port(4, true)
            .pcie_port(5, true)
            .pcie_slot(0, R500_PCIE_POWER, false)
            .pcie_slot(1, R500_PCIE_POWER, false)
            .pcie_slot(3, R500_PCIE_POWER, false)
            .pcie_slot(4, R500_PCIE_POWER, false)
            .pcie_slot(5, R500_PCIE_POWER, true)
            .pcie_attached(3, &EXPRESSCARD)
            .gpio_pins(r500_gpio_pins())
            .build()
    } else {
        // t400/devicetree.cb: ports 0, 1, 2, 3 enabled; slots 0, 1, 3 at 10 W;
        // port 3 (ExpressCard) hot-pluggable.
        base.pcie_port(0, true)
            .pcie_port(1, true)
            .pcie_port(2, true)
            .pcie_port(3, true)
            .pcie_slot(0, MINI_PCIE_POWER, false)
            .pcie_slot(1, MINI_PCIE_POWER, false)
            .pcie_slot(3, MINI_PCIE_POWER, true)
            .pcie_attached(3, &EXPRESSCARD)
            .gpio_pins(t400_gpio_pins())
            .build()
    }
};

const fn t400_vbt(_pwm_hz: u16) -> VbtSource {
    VbtSource::ffs(VBT)
}

/// T400 / R400 (`variants/t400/malibu-3/gma-mainboard.ads`): DP2, HDMI2, Analog, LVDS.
const MALIBU3_OUTPUTS: &[OutputConfig] = &[
    OutputConfig {
        port: Port::DpB,
        enabled: true,
    },
    OutputConfig {
        port: Port::HdmiB,
        enabled: true,
    },
    OutputConfig {
        port: Port::Vga,
        enabled: true,
    },
    OutputConfig {
        port: Port::Lvds,
        enabled: true,
    },
];

/// T500 / W500 (`variants/t400/coronado-5/gma-mainboard.ads`): DP1, HDMI1, DP2, HDMI2, Analog, LVDS.
const CORONADO5_OUTPUTS: &[OutputConfig] = &[
    OutputConfig {
        port: Port::DpA,
        enabled: true,
    },
    OutputConfig {
        port: Port::HdmiA,
        enabled: true,
    },
    OutputConfig {
        port: Port::DpB,
        enabled: true,
    },
    OutputConfig {
        port: Port::HdmiB,
        enabled: true,
    },
    OutputConfig {
        port: Port::Vga,
        enabled: true,
    },
    OutputConfig {
        port: Port::Lvds,
        enabled: true,
    },
];

/// R500 (`variants/r500/gma-mainboard.ads`): DP1, HDMI1, Analog, LVDS.
const R500_OUTPUTS: &[OutputConfig] = &[
    OutputConfig {
        port: Port::DpA,
        enabled: true,
    },
    OutputConfig {
        port: Port::HdmiA,
        enabled: true,
    },
    OutputConfig {
        port: Port::Vga,
        enabled: true,
    },
    OutputConfig {
        port: Port::Lvds,
        enabled: true,
    },
];

const VARIANT_OUTPUTS: &[OutputConfig] = match VARIANT {
    Variant::T400 | Variant::R400 => MALIBU3_OUTPUTS,
    Variant::T500 | Variant::W500 => CORONADO5_OUTPUTS,
    Variant::R500 => R500_OUTPUTS,
};

/// Devicetree `gpu_panel_*` delays, `t400/blc.c` panel PWM frequencies, and
/// per-variant `gma-mainboard.ads` port lists.
const T400_IGD: Gm45IgdConfig = Gm45IgdConfig {
    vbt: t400_vbt,
    panel_power_up_delay: 250,
    panel_power_down_delay: 250,
    panel_backlight_on_delay: 2500,
    panel_backlight_off_delay: 2500,
    panel_power_cycle_delay: 3,
    backlight_pwm: &[
        // T400
        BacklightPwm {
            panel: "B141EW04 V7 ",
            hz: 110,
        },
        BacklightPwm {
            panel: "B141EW05 V3 ",
            hz: 450,
        },
        BacklightPwm {
            panel: "LP141WX3-TLR1",
            hz: 110,
        },
        BacklightPwm {
            panel: "LP141WX5-TLP2",
            hz: 450,
        },
        BacklightPwm {
            panel: "LTN141AT12401",
            hz: 450,
        },
        BacklightPwm {
            panel: "LTN141W1-L05",
            hz: 110,
        },
        BacklightPwm {
            panel: "LTN141WD-L05",
            hz: 110,
        },
        // T500
        BacklightPwm {
            panel: "LP154W02-TL06",
            hz: 110,
        },
        BacklightPwm {
            panel: "LTN154P2-L05",
            hz: 110,
        },
        BacklightPwm {
            panel: "LTN154P3-L02",
            hz: 110,
        },
        BacklightPwm {
            panel: "LTN154X3-L02",
            hz: 110,
        },
        BacklightPwm {
            panel: "LTN154U2-L05",
            hz: 110,
        },
        // R500
        BacklightPwm {
            panel: "154WX5-TLB2",
            hz: 220,
        },
    ],
    default_pwm_freq: 0,
    duty_cycle: 0,
    display: Some(IgdDisplayPolicy {
        outputs: VARIANT_OUTPUTS,
        framebuffer: FramebufferConfig {
            scaling: fstart_driver_intel::gma::scaler::ScalingPolicy::PreserveAspect,
            ..FramebufferConfig::edid(FallbackMode {
                width: 1280,
                height: 800,
                refresh_hz: 60,
            })
        },
    }),
};

/// Coreboot `t400/cstates.c`: C1, C2, C3 (MWAIT 0x00, 0x10, 0x20).
const T400_CSTATES: Core2Cstates = {
    const STATES: &[CState] = &[
        CState {
            ctype: 1,
            latency: 1,
            power: 1000,
            mwait_hint: 0x00,
        },
        CState {
            ctype: 2,
            latency: 1,
            power: 500,
            mwait_hint: 0x10,
        },
        CState {
            ctype: 3,
            latency: 0x37,
            power: 250,
            mwait_hint: 0x20,
        },
    ];
    Core2Cstates {
        ac: STATES,
        battery: STATES,
    }
};

const fn lenovo_ssid(device: u8, function: u8, id: u16) -> PciSubsystemId {
    PciSubsystemId::new(device, function, 0x17aa, id)
}

/// Devicetree `subsystemid` entries for T400 / T500 / R400 / W500.
const T400_SUBSYSTEM_IDS: &[PciSubsystemId] = &[
    lenovo_ssid(0x00, 0, 0x20e0),
    lenovo_ssid(0x02, 0, 0x20e4),
    lenovo_ssid(0x02, 1, 0x20e4),
    lenovo_ssid(0x03, 0, 0x20e6),
    lenovo_ssid(0x1a, 0, 0x20f0),
    lenovo_ssid(0x1a, 1, 0x20f0),
    lenovo_ssid(0x1a, 2, 0x20f0),
    lenovo_ssid(0x1a, 7, 0x20f1),
    lenovo_ssid(0x1b, 0, 0x20f2),
    lenovo_ssid(0x1c, 0, 0x20f3),
    lenovo_ssid(0x1c, 1, 0x20f3),
    lenovo_ssid(0x1c, 2, 0x20f3),
    lenovo_ssid(0x1c, 3, 0x20f3),
    lenovo_ssid(0x1d, 0, 0x20f0),
    lenovo_ssid(0x1d, 1, 0x20f0),
    lenovo_ssid(0x1d, 2, 0x20f0),
    lenovo_ssid(0x1d, 7, 0x20f1),
    lenovo_ssid(0x1e, 0, 0x20f4),
    lenovo_ssid(0x1f, 0, 0x20f5),
    lenovo_ssid(0x1f, 2, 0x20f8),
    lenovo_ssid(0x1f, 3, 0x20f9),
];

/// Devicetree `subsystemid` entries for R500 (no 03.0 ME, no 1c.2, adds 1c.4/5).
const R500_SUBSYSTEM_IDS: &[PciSubsystemId] = &[
    lenovo_ssid(0x00, 0, 0x20e0),
    lenovo_ssid(0x02, 0, 0x20e4),
    lenovo_ssid(0x02, 1, 0x20e4),
    lenovo_ssid(0x1a, 0, 0x20f0),
    lenovo_ssid(0x1a, 1, 0x20f0),
    lenovo_ssid(0x1a, 2, 0x20f0),
    lenovo_ssid(0x1a, 7, 0x20f1),
    lenovo_ssid(0x1b, 0, 0x20f2),
    lenovo_ssid(0x1c, 0, 0x20f3),
    lenovo_ssid(0x1c, 1, 0x20f3),
    lenovo_ssid(0x1c, 3, 0x20f3),
    lenovo_ssid(0x1c, 4, 0x20f3),
    lenovo_ssid(0x1c, 5, 0x20f3),
    lenovo_ssid(0x1d, 0, 0x20f0),
    lenovo_ssid(0x1d, 1, 0x20f0),
    lenovo_ssid(0x1d, 2, 0x20f0),
    lenovo_ssid(0x1d, 7, 0x20f1),
    lenovo_ssid(0x1e, 0, 0x20f4),
    lenovo_ssid(0x1f, 0, 0x20f5),
    lenovo_ssid(0x1f, 2, 0x20f8),
    lenovo_ssid(0x1f, 3, 0x20f9),
];

const MINI_PCIE_POWER: PciePowerLimit = PciePowerLimit {
    value: 10,
    scale: 0,
};

const R500_PCIE_POWER: PciePowerLimit = PciePowerLimit {
    value: 41,
    scale: 0,
};

/// Coreboot `t400/hda_verb.c`: Conexant CX20561 (Hermosa) pin defaults.
const fn t400_hda_pins() -> [hda::PinConfig; 8] {
    use hda::{
        PinColor as C, PinConn as K, PinConnector as T, PinDevice as D, PinGeoLoc as G, PinLoc as L,
    };
    [
        hda::pin_config(
            0x16,
            D::HpOut,
            K::Jack,
            L::External,
            G::Front,
            T::StereoMono18,
            C::Green,
            0,
            15,
            0,
        ),
        hda::pin_config(
            0x17,
            D::MicIn,
            K::Nc,
            L::SeparateChassis,
            G::Rear,
            T::StereoMono18,
            C::Pink,
            0,
            15,
            0,
        ),
        hda::pin_config(
            0x18,
            D::MicIn,
            K::Jack,
            L::External,
            G::Front,
            T::StereoMono18,
            C::Pink,
            0,
            15,
            0,
        ),
        hda::pin_config(
            0x19,
            D::DeviceOther,
            K::Nc,
            L::External,
            G::NA,
            T::Unknown,
            C::ColorUnknown,
            0,
            15,
            0,
        ),
        hda::pin_config(
            0x1a,
            D::Speaker,
            K::Integrated,
            L::Internal,
            G::NA,
            T::OtherAnalog,
            C::ColorUnknown,
            1,
            15,
            0,
        ),
        hda::pin_config(
            0x1b,
            D::DeviceOther,
            K::Nc,
            L::External,
            G::NA,
            T::Unknown,
            C::ColorUnknown,
            1,
            15,
            0,
        ),
        hda::pin_config(
            0x1c,
            D::DeviceOther,
            K::Nc,
            L::External,
            G::NA,
            T::Unknown,
            C::ColorUnknown,
            1,
            15,
            0,
        ),
        hda::pin_config(
            0x1d,
            D::MicIn,
            K::Integrated,
            L::Internal,
            G::NA,
            T::OtherDigital,
            C::ColorUnknown,
            1,
            15,
            0,
        ),
    ]
}

const T400_BEEP_VERBS: [u32; 8] = [
    0x0017_0500,
    0x0147_0500,
    0x0147_0100,
    0x0147_0740,
    0x00b3_7517,
    0x00c3_7100,
    0x00c3_b015,
    0x0143_b000,
];

/// Coreboot `variants/t400/gpio.c`. Unlisted pads select their native functions.
const fn t400_gpio_pins() -> [gpio::GpioPin; 31] {
    use gpio::{GpioLevel as Level, input, output};
    [
        input(1).inverted(),
        input(2),
        input(3),
        input(4),
        input(5),
        input(6),
        input(7),
        input(8).inverted(),
        output(9, Level::High),
        input(13),
        input(17),
        input(18),
        output(19, Level::High),
        output(20, Level::High),
        input(21),
        output(22, Level::High),
        input(24),
        output(27, Level::Low),
        output(28, Level::Low),
        output(33, Level::High),
        output(34, Level::Low),
        input(36),
        input(37),
        input(38),
        input(39),
        output(41, Level::High),
        output(42, Level::High),
        input(48),
        output(49, Level::High),
        input(56),
        input(57),
    ]
}

/// Coreboot `variants/r500/gpio.c`. Unlisted pads select their native functions.
const fn r500_gpio_pins() -> [gpio::GpioPin; 32] {
    use gpio::{GpioLevel as Level, input, output};
    [
        input(1).inverted(),
        input(2),
        input(3),
        input(4),
        input(5),
        input(6),
        input(7),
        input(8).inverted(),
        output(9, Level::High),
        output(12, Level::Low),
        input(13),
        input(17),
        output(18, Level::High),
        input(19),
        output(20, Level::High),
        input(21),
        input(22),
        output(24, Level::High),
        output(27, Level::Low),
        output(28, Level::Low),
        output(33, Level::High),
        output(34, Level::Low),
        input(36),
        input(37),
        input(38),
        input(39),
        output(41, Level::High),
        output(42, Level::High),
        input(48),
        output(49, Level::High),
        input(56),
        input(57),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn variant_flash_pcie_outputs_and_identity_match_coreboot() {
        use fstart_platform_intel::IntelPlatformConfig;
        assert_eq!(T400_PLATFORM.northbridge.spd_addresses, [0x50, 0, 0x51, 0]);
        assert_eq!(T400_PLATFORM.max_cpus(), 4);
        assert_eq!(T400_PLATFORM.pcie_attached()[3], Some(&EXPRESSCARD));
        assert!(T400_PLATFORM.southbridge.pirq.is_none());
        assert!(T400_PLATFORM.southbridge.preserve_hardware_pirq);
        assert_eq!(T400_PLATFORM.southbridge.disable_lan, IS_R500);

        match VARIANT {
            Variant::R500 => {
                assert_eq!(BOARD_NAME, "lenovo-r500");
                assert_eq!(PRODUCT_NAME, "ThinkPad R500");
                assert_eq!(FLASH.size(), 4 << 20);
                assert_eq!(FLASH.bios_region().unwrap().offset, 0x200000);
                assert_eq!(FLASH.bios_region().unwrap().size, 0x200000);
                assert_eq!(T400_PLATFORM.northbridge.disabled_me_functions, 0b1111);
                assert!(T400_PLATFORM.southbridge.sata.unwrap().clock_request);
                assert!(!T400_PLATFORM.southbridge.pcie_ports[2]);
                assert!(T400_PLATFORM.southbridge.pcie_ports[4]);
                assert!(T400_PLATFORM.southbridge.pcie_ports[5]);
                assert!(T400_PLATFORM.southbridge.pcie_hotplug[5]);
                assert_eq!(VARIANT_OUTPUTS.len(), 4);
            }
            Variant::T500 | Variant::W500 => {
                assert_eq!(FLASH.size(), 8 << 20);
                assert_eq!(FLASH.bios_region().unwrap().offset, 0x600000);
                assert_eq!(T400_PLATFORM.northbridge.disabled_me_functions, 0b1110);
                assert!(!T400_PLATFORM.southbridge.sata.unwrap().clock_request);
                assert!(T400_PLATFORM.southbridge.pcie_hotplug[3]);
                assert_eq!(VARIANT_OUTPUTS.len(), 6);
            }
            Variant::T400 | Variant::R400 => {
                assert_eq!(FLASH.size(), 8 << 20);
                assert_eq!(FLASH.bios_region().unwrap().offset, 0x600000);
                assert_eq!(T400_PLATFORM.northbridge.disabled_me_functions, 0b1110);
                assert!(!T400_PLATFORM.southbridge.sata.unwrap().clock_request);
                assert!(T400_PLATFORM.southbridge.pcie_hotplug[3]);
                assert_eq!(VARIANT_OUTPUTS.len(), 4);
            }
        }
    }

    #[test]
    fn hda_pin_defaults_match_coreboot_t400() {
        assert_eq!(
            t400_hda_pins().map(|p| p.encode()),
            [
                0x022140f0, 0x61a190f0, 0x02a190f0, 0x40f000f0, 0x901701f0, 0x40f001f0, 0x40f001f0,
                0x90a601f0,
            ]
        );
    }
}
