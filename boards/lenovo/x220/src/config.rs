//! Lenovo X220 board wiring (coreboot `mainboard/lenovo/x220`, variant
//! `x220`); the chipset mechanisms live in the Sandy Bridge and bd82x6x
//! drivers.

use fstart_core::{
    ConstVec, FlashLayout, IntelIfdFlashLayout, IntelIfdRegion as Kind,
    IntelIfdRegionConfig as Region,
};
use fstart_driver_intel::gma::framebuffer::FramebufferConfig;
use fstart_driver_intel::gma::{FallbackMode, OutputConfig, Port};
use fstart_driver_intel::southbridge::{gpio_ich as gpio, hda};
use fstart_platform_intel::facts::PciSubsystemId;
use fstart_platform_intel::igd::{IgdDisplayPolicy, VbtSource};
use fstart_platform_intel::sandybridge::{
    EhciController, GpiRoute, LpcGenericIoDecode, Model206axConfig, PanelPort, PchFunction,
    PsiState, SandybridgeBd82x6xConfig, SandybridgeBd82x6xPlatform, SandybridgeDevices,
    SandybridgeIgdConfig, SataConfig, UsbPort, Vr12Phases,
};

/// The stock 8 MiB layout (coreboot `vboot-ro.fmd`): descriptor, GbE, ME,
/// then the 3 MiB BIOS region.
pub const FLASH: IntelIfdFlashLayout = {
    const FD: Region = Region {
        kind: Kind::Descriptor,
        offset: 0,
        size: 0x1000,
    };
    IntelIfdFlashLayout::new(
        ConstVec::new(FD)
            .push(FD)
            .push(Region {
                kind: Kind::Gbe,
                offset: 0x1000,
                size: 0x2000,
            })
            .push(Region {
                kind: Kind::Me,
                offset: 0x3000,
                size: 0x4fd000,
            })
            .push(Region {
                kind: Kind::Bios,
                offset: 0x500000,
                size: 0x300000,
            }),
    )
};

pub const BOARD_NAME: &str = "lenovo-x220";
pub const BOARD_PACKAGE: &str = "fstart-board-lenovo-x220";

const VBT: &str = "data.vbt";

impl fstart_platform_intel::facts::IntelBoardFacts for crate::Board {
    type Platform = fstart_platform_intel::sandybridge::SandybridgeBd82x6x;
    const OPTIONS: &'static [&'static dyn fstart_options::Describe] = &[&crate::mainboard::EC];
    const CONFIG: &'static SandybridgeBd82x6xPlatform = &X220_PLATFORM;
    const FACTS: fstart_platform_intel::facts::BoardFacts =
        fstart_platform_intel::facts::BoardFacts::new(FlashLayout::IntelIfd(FLASH), 0x800000)
            .with_data_assets(&[VBT])
            .with_memory_cache()
            .with_variable_storage();
}

/// EC SCI is GPIO1 (GPE 0x11); the EC wake GPI is GPIO13.
pub const EC_SCI_GPI: u8 = 1;
pub const EC_WAKE_GPI: u8 = 13;
/// WWAN card presence (devicetree `wwan_gpio_num`/`wwan_gpio_lvl`).
pub const WWAN_PRESENCE_GPIO: u8 = 70;
/// coreboot `DRAM_RESET_GATE_GPIO`.
pub const DRAM_RESET_GATE_GPIO: u8 = 10;

/// H8 EC transport, LPC windows and the AML surface (devicetree
/// `ec/lenovo/h8`, Kconfig `H8_HAS_BAT_*`, `mainboard_fill_gnvs`).
pub const X220_H8: fstart_driver_lenovo::h8::H8Config = {
    use fstart_driver_lenovo::{
        ec::EcPorts,
        h8::{H8BatteryThresholdBackend, H8Config, H8Resources},
    };
    H8Config {
        config: X220_H8_CONFIG,
        beep_masks: X220_H8_BEEPS,
        has_power_management_beeps: true,
        event_masks: X220_H8_EVENTS,
        has_bluetooth: true,
        has_wwan: true,
        has_thinklight: true,
        critical_temp_celsius: 100,
        passive_temp_celsius: 90,
        bat_info_extended: true,
        bat_thresholds: true,
        bat_threshold_backend: H8BatteryThresholdBackend::RegistersB0,
        ..H8Config::new(
            H8Resources::new(EcPorts::new(0x62, 0x66), 0x1600, 0x15e0),
            0x10 + EC_SCI_GPI,
            0x10 + EC_WAKE_GPI,
            "IBM0068",
        )
    }
};

/// Devicetree `config0..3`, beep masks and event masks of the H8.
pub const X220_H8_CONFIG: [u8; 4] = [0xa6, 0x01, 0xa0, 0x60];
/// Devicetree `beepmask0/1`; the user's beep options apply on top.
pub const X220_H8_BEEPS: [u8; 2] = [0x00, 0x86];
pub const X220_H8_EVENTS: [u8; 16] = [
    // variants/x220 adds eventa/eventb.
    0x00, 0x00, 0xff, 0xff, 0xd0, 0xfc, 0x00, 0x81, 0x7b, 0xff, 0x01, 0xf0, 0xff, 0xff, 0x0d, 0x00,
];

pub static X220_PLATFORM: SandybridgeBd82x6xPlatform = SandybridgeBd82x6xConfig::new()
    .laptop()
    // Sandy Bridge mobile: up to two cores, two threads each.
    .max_cpus(4)
    .pci_subsystem_ids(X220_SUBSYSTEM_IDS)
    .spd_addresses([0x50, 0, 0x51, 0])
    // DDR3-1333.
    .max_mem_clock_mhz(666)
    // cmos.layout: gfx_uma_size defaults to 32M.
    .gfx_uma_mib(32)
    .northbridge_devices(SandybridgeDevices {
        peg10: false,
        peg11: false,
        peg12: false,
        peg60: false,
        igd: true,
        dev4: false,
        dev7: false,
    })
    .igd(X220_IGD)
    .cpu(X220_CPU)
    .gpio_pins(x220_gpio_pins())
    .gpi_routing({
        let mut routing = [GpiRoute::Disabled; 16];
        routing[EC_SCI_GPI as usize] = GpiRoute::Sci;
        routing[EC_WAKE_GPI as usize] = GpiRoute::Sci;
        routing
    })
    .alt_gp_smi_en(0)
    .lpc_generic_io([
        LpcGenericIoDecode {
            base: X220_H8.resources.auxiliary_base.raw(),
            size: fstart_driver_lenovo::h8::H8Resources::AUXILIARY_SIZE,
        },
        LpcGenericIoDecode {
            base: X220_H8.resources.pmh7_base.raw(),
            size: fstart_driver_lenovo::h8::H8Resources::PMH7_SIZE as u16,
        },
        // devicetree `gen4_dec`.
        LpcGenericIoDecode {
            base: 0x06a0,
            size: 0x10,
        },
    ])
    // HDD bay, dock and mSATA at up to 6 Gb/s; cmos.default sata_mode=AHCI.
    .sata(SataConfig {
        interface_speed: 3,
        ..SataConfig::ahci(0x07)
    })
    // RP2 WLAN, RP4 ExpressCard, RP5 card reader, RP7 optional xHCI.
    .disable([
        PchFunction::Xhci,
        PchFunction::Mei2,
        PchFunction::Ider,
        PchFunction::Kt,
        PchFunction::PcieRootPort(0),
        PchFunction::PcieRootPort(2),
        PchFunction::PcieRootPort(5),
        PchFunction::PcieRootPort(7),
        PchFunction::PciBridge,
        PchFunction::Sata2,
    ])
    .pcie_hotplug(3)
    .pcie_attached(1, &WIFI_SLOT)
    .pcie_attached(3, &EXPRESSCARD)
    .pcie_attached(4, &CARD_READER)
    .pcie_port_coalesce()
    .spi_vscc(0x2005, 0x2005)
    .usb_ports(X220_USB_PORTS)
    .serirq_continuous()
    .dram_reset_gate_gpio(DRAM_RESET_GATE_GPIO)
    // cmos.default: power_on_after_fail=Disable, nmi=Enable.
    .power_on_after_fail(0)
    .nmi(true)
    .docking_supported()
    // coreboot `USBDEBUG_HCD_INDEX=2`: EHCI2 (00:1a.0).
    .ehci_debug_controller(EhciController::Ehci2)
    .hda_verb(0x14f1_506e, 0x17aa_21db, x220_hda_pins(), X220_HDA_MISC)
    .hda_verb(0x8086_2806, 0x8086_0101, hdmi_pins(), [])
    .build();

/// devicetree.cb: `smbios_slot_desc "SlotTypeM2Socket1_SD"
/// "SlotLengthShort" "WIFI" "SlotDataBusWidth1X"` on PCIe root port 2.
pub const WIFI_SLOT: fstart_core::smbios::Attached<'static> =
    fstart_core::smbios::Attached::Slot(fstart_core::smbios::SystemSlot {
        designation: "WIFI",
        slot_type: 0x15,
        data_bus_width: 0x08,
        length: 0x03,
        id: 0,
        characteristics: [1, 0],
    });

/// devicetree.cb: `smbios_slot_desc "7" "3" "ExpressCard Slot" "8"` on
/// PCIe root port 4: PCMCIA/ExpressCard, short, PCIe x1, PME and hot-plug.
pub const EXPRESSCARD: fstart_core::smbios::Attached<'static> =
    fstart_core::smbios::Attached::Slot(fstart_core::smbios::SystemSlot {
        designation: "ExpressCard Slot",
        slot_type: 0x07,
        data_bus_width: 0x08,
        length: 0x03,
        id: 0,
        characteristics: [0, 0x03],
    });

/// The Ricoh RCE822 card reader soldered behind PCIe root port 5.
pub const CARD_READER: fstart_core::smbios::Attached<'static> =
    fstart_core::smbios::Attached::Onboard(fstart_core::smbios::OnboardDevice {
        designation: "Card Reader",
        kind: fstart_core::smbios::OnboardType::Other,
    });

/// `variants/x220/overridetree.cb` `usb_port_config` (enabled, current
/// index, over-current pin).
const X220_USB_PORTS: [UsbPort; 14] = [
    UsbPort::new(0, Some(0)), // System port near VGA
    UsbPort::new(1, Some(1)), // System port near ExpressCard
    UsbPort::new(1, None),    // WLAN
    UsbPort::new(1, None),    // WWAN
    UsbPort::DISABLED,
    UsbPort::new(1, None), // ExpressCard
    UsbPort::DISABLED,
    UsbPort::DISABLED,
    UsbPort::new(1, None),    // Docking
    UsbPort::new(1, Some(5)), // Always-on system port
    UsbPort::new(1, None),    // Fingerprint reader
    UsbPort::new(1, None),    // Bluetooth
    UsbPort::new(1, Some(7)), // Docking
    UsbPort::new(1, None),    // Camera
];

/// Devicetree `chip cpu/intel/model_206ax` (vendor BIOS 1.46 values).
const X220_CPU: Model206axConfig = {
    let psi = [
        PsiState {
            phases: Vr12Phases::Two,
            current: 20,
        },
        PsiState {
            phases: Vr12Phases::All,
            current: 5,
        },
        PsiState {
            phases: Vr12Phases::All,
            current: 1,
        },
    ];
    let mut cpu = Model206axConfig::new();
    cpu.pp0_current_limit = 98;
    cpu.pp1_current_limit = 33;
    cpu.pp0_psi = psi;
    cpu.pp1_psi = psi;
    cpu.hyper_threading = true;
    cpu
};

/// Devicetree GPU registers and the ports of coreboot's `gma-mainboard.ads`,
/// in its probe order: DP1..3, HDMI1..3, VGA, then the LVDS panel.
const X220_IGD: SandybridgeIgdConfig = {
    let mut igd = SandybridgeIgdConfig::new();
    igd.dp_d_hotplug = 0x06;
    igd.panel_port = PanelPort::Lvds;
    igd.panel_power_cycle_delay = 5;
    igd.panel_power_up_delay = 300;
    igd.panel_power_down_delay = 300;
    igd.panel_backlight_on_delay = 2000;
    igd.panel_backlight_off_delay = 2000;
    igd.cpu_backlight = 0x1155;
    igd.pch_backlight = 0x0610_0610;
    igd.vbt = VbtSource::ffs(VBT);
    igd.display = Some(IgdDisplayPolicy {
        outputs: &[
            OutputConfig {
                port: Port::DpA,
                enabled: true,
            },
            OutputConfig {
                port: Port::DpB,
                enabled: true,
            },
            OutputConfig {
                port: Port::DpC,
                enabled: true,
            },
            OutputConfig {
                port: Port::HdmiA,
                enabled: true,
            },
            OutputConfig {
                port: Port::HdmiB,
                enabled: true,
            },
            OutputConfig {
                port: Port::HdmiC,
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
        ],
        framebuffer: FramebufferConfig {
            scaling: fstart_driver_intel::gma::scaler::ScalingPolicy::PreserveAspect,
            ..FramebufferConfig::edid(FallbackMode {
                width: 1366,
                height: 768,
                refresh_hz: 60,
            })
        },
    });
    igd
};

/// Devicetree `subsystemid 0x17aa 0x21db inherit`, GbE 0x21ce.
const X220_SUBSYSTEM_IDS: &[PciSubsystemId] = {
    const fn lenovo(device: u8, function: u8, id: u16) -> PciSubsystemId {
        PciSubsystemId::new(device, function, 0x17aa, id)
    }
    &[
        lenovo(0x00, 0, 0x21db),
        lenovo(0x02, 0, 0x21db),
        lenovo(0x16, 0, 0x21db),
        lenovo(0x19, 0, 0x21ce),
        lenovo(0x1a, 0, 0x21db),
        lenovo(0x1b, 0, 0x21db),
        lenovo(0x1c, 0, 0x21db),
        lenovo(0x1c, 1, 0x21db),
        lenovo(0x1c, 2, 0x21db),
        lenovo(0x1c, 3, 0x21db),
        lenovo(0x1c, 4, 0x21db),
        lenovo(0x1d, 0, 0x21db),
        lenovo(0x1f, 0, 0x21db),
        lenovo(0x1f, 2, 0x21db),
        lenovo(0x1f, 3, 0x21db),
        lenovo(0x1f, 6, 0x21db),
    ]
};

/// Coreboot x220 `hda_verb.c`: the CX20590 pin defaults.
const fn x220_hda_pins() -> [hda::PinConfig; 10] {
    use hda::{
        PinColor as C, PinConn as K, PinConnector as T, PinDevice as D, PinGeoLoc as G, PinLoc as L,
    };
    [
        hda::pin_config(
            0x19,
            D::HpOut,
            K::Jack,
            L::External,
            G::Right,
            T::StereoMono18,
            C::Black,
            0,
            4,
            0,
        ),
        hda::pin_config(
            0x1a,
            D::MicIn,
            K::Nc,
            L::SeparateChassis,
            G::Rear,
            T::StereoMono18,
            C::Pink,
            0,
            5,
            0,
        ),
        hda::pin_config(
            0x1b,
            D::MicIn,
            K::Jack,
            L::External,
            G::Right,
            T::StereoMono18,
            C::Black,
            0,
            6,
            0,
        ),
        hda::pin_config(
            0x1c,
            D::HpOut,
            K::Nc,
            L::SeparateChassis,
            G::Rear,
            T::StereoMono18,
            C::Green,
            0,
            1,
            15,
        ),
        hda::pin_config(
            0x1d,
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
            0x1e,
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
            0x1f,
            D::Speaker,
            K::Integrated,
            L::Internal,
            G::NA,
            T::OtherAnalog,
            C::ColorUnknown,
            1,
            1,
            0,
        ),
        hda::pin_config(
            0x20,
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
            0x22,
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
            0x23,
            D::MicIn,
            K::Integrated,
            L::Internal,
            G::NA,
            T::OtherDigital,
            C::ColorUnknown,
            1,
            7,
            0,
        ),
    ]
}

/// CX20590 combo-jack and port-power verbs after the pin defaults, then
/// `pc_beep_verbs` (digital PC beep gain).
const X220_HDA_MISC: [u32; 9] = [
    0x00b7_07c0,
    0x00d7_0740,
    0x0017_a200,
    0x0017_c621,
    0x0017_a208,
    0x0017_0500,
    0x0017_0500,
    0x0017_0500,
    0x0217_7a00,
];

/// The PCH HDMI codec (address 3) pin defaults.
const fn hdmi_pins() -> [hda::PinConfig; 3] {
    use hda::{
        PinColor as C, PinConn as K, PinConnector as T, PinDevice as D, PinGeoLoc as G, PinLoc as L,
    };
    [
        hda::pin_config(
            0x05,
            D::DigitalOtherOut,
            K::Jack,
            L::Internal,
            G::Special8,
            T::OtherDigital,
            C::ColorUnknown,
            0,
            1,
            0,
        ),
        hda::pin_config(
            0x06,
            D::DigitalOtherOut,
            K::Jack,
            L::Internal,
            G::Special8,
            T::OtherDigital,
            C::ColorUnknown,
            0,
            2,
            0,
        ),
        hda::pin_config(
            0x07,
            D::DigitalOtherOut,
            K::Jack,
            L::Internal,
            G::Special8,
            T::OtherDigital,
            C::ColorUnknown,
            0,
            3,
            0,
        ),
    ]
}

/// Coreboot `variants/x220/gpio.c`; unlisted pads keep native functions.
const fn x220_gpio_pins() -> [gpio::GpioPin; 41] {
    use gpio::{GpioLevel as Level, input, output};
    [
        input(0).inverted(),
        input(1).inverted(),
        input(2),
        input(3),
        input(4),
        input(5),
        input(6).inverted(),
        input(7),
        output(8, Level::High),
        output(10, Level::High),
        input(13).inverted(),
        output(15, Level::Low),
        input(16),
        input(17),
        input(21),
        output(22, Level::High),
        output(24, Level::Low),
        input(27),
        input(28),
        output(29, Level::High),
        output(33, Level::High),
        input(34),
        output(35, Level::Low),
        input(36),
        input(37),
        input(38),
        input(39),
        output(42, Level::High),
        input(48),
        input(49),
        input(50),
        output(51, Level::High),
        output(52, Level::Low),
        output(53, Level::High),
        input(54),
        output(55, Level::High),
        input(57),
        input(68),
        input(69),
        input(70),
        input(71),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flash_and_platform_facts_match_coreboot() {
        assert_eq!(FLASH.size(), 8 << 20);
        assert_eq!(FLASH.bios_region().unwrap().offset, 0x500000);
        assert_eq!(FLASH.bios_region().unwrap().size, 0x300000);
        assert_eq!(X220_PLATFORM.northbridge.spd_addresses, [0x50, 0, 0x51, 0]);
        assert_eq!(X220_PLATFORM.southbridge.dram_reset_gate_gpio, Some(10));
        assert!(X220_PLATFORM.southbridge.pcie_port_coalesce);
        assert_eq!(
            x220_hda_pins().map(|p| p.encode()),
            [
                0x0421_1040,
                0x61a1_9050,
                0x04a1_1060,
                0x6121_401f,
                0x40f0_01f0,
                0x40f0_01f0,
                0x9017_0110,
                0x40f0_01f0,
                0x40f0_01f0,
                0x90a6_0170,
            ]
        );
        assert_eq!(
            hdmi_pins().map(|p| p.encode()),
            [0x1856_0010, 0x1856_0020, 0x1856_0030]
        );
    }

    #[test]
    fn pch_functions_follow_the_devicetree() {
        let pch = &X220_PLATFORM.southbridge;
        let ports: [bool; 8] =
            core::array::from_fn(|port| pch.enabled(PchFunction::PcieRootPort(port as u8)));
        assert_eq!(ports, [false, true, false, true, true, false, true, false]);
        assert!(pch.pcie_hotplug[3]);
        // The card reader is on the board; the WLAN and ExpressCard are slots.
        assert_eq!(
            pch.pcie_onboard,
            [false, false, false, false, true, false, false, false]
        );
        assert!(!pch.enabled(PchFunction::Mei2) && pch.enabled(PchFunction::Mei1));
        assert!(pch.enabled(PchFunction::Ehci2) && pch.enabled(PchFunction::Gbe));
        assert!(X220_PLATFORM.northbridge.hda_enabled);
    }

    /// `variants/x220/gpio.c`'s set registers, for the pads this board
    /// configures.
    #[test]
    fn gpio_table_matches_gpio_c() {
        let mut mode = [0u32; 3];
        let mut input = [0u32; 3];
        let mut high = [0u32; 3];
        let mut invert = 0u32;
        for pin in x220_gpio_pins() {
            let (set, bit) = ((pin.pin / 32) as usize, 1u32 << (pin.pin % 32));
            mode[set] |= bit;
            if pin.dir == gpio::GpioDir::Input {
                input[set] |= bit;
            }
            if pin.level == gpio::GpioLevel::High {
                high[set] |= bit;
            }
            if pin.invert {
                invert |= bit;
            }
        }
        assert_eq!(mode, [0x3963_a5ff, 0x02ff_04fe, 0x0000_00f0]);
        assert_eq!(input[0], 0x9ebf_6aff & mode[0]);
        assert_eq!(input[1], 0x1f47_fbf5 & mode[1]);
        assert_eq!(input[2], 0x0000_0ff0 & mode[2]);
        assert_eq!(high[0], 0x66ff_7ffb & mode[0] & !input[0]);
        assert_eq!(high[1], 0xbdec_ff87 & mode[1] & !input[1]);
        assert_eq!(high[2], 0x0000_0fcf & mode[2] & !input[2]);
        assert_eq!(invert, 0x0000_2043);
    }
}
