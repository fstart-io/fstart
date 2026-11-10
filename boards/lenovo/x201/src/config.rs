//! Lenovo X201 board wiring (coreboot `mainboard/lenovo/x201`); chipset
//! mechanisms remain in Ironlake/Ibex Peak.
use fstart_core::{
    ConstVec, FlashLayout, IntelIfdFlashLayout, IntelIfdRegion as Kind,
    IntelIfdRegionConfig as Region,
};
use fstart_driver_intel::southbridge::{gpio_ich as gpio, hda};
use fstart_intel_gma::framebuffer::FramebufferConfig;
use fstart_intel_gma::{FallbackMode, OutputConfig, Port};
use fstart_platform_intel::facts::PciSubsystemId;
use fstart_platform_intel::igd::{IgdDisplayPolicy, VbtSource};
use fstart_platform_intel::ironlake::{
    GpiRoute, IronlakeIbexPeakConfig, IronlakeIbexPeakPlatform, IronlakeIgdConfig,
    LpcGenericIoDecode, PanelPort, PchFunction, SataConfig, SataMode, UsbCurrent, UsbPort,
};

/// coreboot `vboot-rwa.fmd` `SI_ALL`: descriptor, GbE and ME below a
/// 3 MiB BIOS region (`MAINBOARD_USES_IFD_GBE_REGION`).
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
                size: 0x4ed000,
            })
            .push(Region {
                kind: Kind::Bios,
                offset: 0x500000,
                size: 0x300000,
            }),
    )
};

pub const BOARD_NAME: &str = "lenovo-x201";
pub const BOARD_PACKAGE: &str = "fstart-board-lenovo-x201";

const VBT: &str = "data.vbt";

pub const EHCI_CONSOLE: fstart_driver_usbdebug::Config = fstart_driver_usbdebug::Config {
    ecam_base: fstart_platform_intel::ironlake::IRONLAKE_ECAM_BASE as usize,
    // Coreboot X201 Kconfig: USBDEBUG_HCD_INDEX=2, i.e. EHCI 00:1a.0.
    bus: 0,
    device: 0x1a,
    function: 0,
    bar: fstart_platform_intel::ironlake::IBEXPEAK_EHCI_DEBUG_BAR,
};

impl fstart_platform_intel::facts::IntelBoardFacts for crate::Board {
    type Platform = fstart_platform_intel::ironlake::IronlakeIbexPeak;
    const OPTIONS: &'static [&'static dyn fstart_options::Describe] = &[&crate::mainboard::EC];
    const CONFIG: &'static IronlakeIbexPeakPlatform = &X201_PLATFORM;
    const FACTS: fstart_platform_intel::facts::BoardFacts =
        fstart_platform_intel::facts::BoardFacts::new(FlashLayout::IntelIfd(FLASH), 0x800000)
            .with_data_assets(&[VBT])
            .with_smbios_slots(X201_SLOTS)
            .with_memory_cache();
}

/// devicetree.cb: `smbios_slot_desc "7" "3" "ExpressCard Slot" "8"`
/// on PCIe root port 4 (00:1c.3): PCMCIA/ExpressCard, short, PCIe x1.
/// PME and hot-plug are supported; no voltage capabilities are inferred from
/// the PCMCIA type. This is the board-fixed bridge association; the shared
/// emitter resolves the card's actual bus/device/function at boot.
pub const X201_SLOTS: &[fstart_core::smbios::PciSlot<'static>] = &[fstart_core::smbios::PciSlot {
    bridge_segment: 0,
    bridge_bus: 0,
    bridge_devfn: fstart_pci::devfn(0x1c, 3),
    slot: fstart_core::smbios::SystemSlot {
        designation: "ExpressCard Slot",
        slot_type: 0x07,
        data_bus_width: 0x08,
        length: 0x03,
        id: 0,
        characteristics: [0, 0x03],
    },
}];

/// The FSB/BCLK word of the clock generator at SMBus 0x69 (coreboot
/// romstage `set_fsb_frequency()`).
pub const CLOCKGEN_ADDRESS: u8 = 0x69;
pub const CLOCKGEN_FSB_FREQUENCY: u16 = 62879;

/// H8 EC: transport, LPC windows and the AML surface. The EC SCI is GPIO1,
/// i.e. GPE 0x11 (devicetree `THINKPAD_EC_GPE 17`).
pub const X201_H8: fstart_driver_lenovo::h8::H8Config = {
    use fstart_driver_lenovo::{
        ec::EcPorts,
        h8::{H8BatteryThresholdBackend, H8Config, H8Resources},
    };
    H8Config {
        has_bluetooth: true,
        has_wwan: true,
        has_thinklight: true,
        second_thermal_zone: true,
        // coreboot x201 `mainboard_fill_gnvs()`.
        critical_temp_celsius: 100,
        passive_temp_celsius: 90,
        bat_info_extended: true,
        bat_thresholds: true,
        // x201 acpi/ec.asl: `H8_BAT_THRESHOLDS_BIT7`.
        bat_threshold_backend: H8BatteryThresholdBackend::Registers24 { bit7: true },
        ..H8Config::new(
            H8Resources::new(EcPorts::new(0x62, 0x66), 0x1600, 0x15e0),
            0x11,
            "IBM0068",
        )
    }
};

/// coreboot `DRAM_RESET_GATE_GPIO`.
pub const DRAM_RESET_GATE_GPIO: u8 = 10;

pub static X201_PLATFORM: IronlakeIbexPeakPlatform = IronlakeIbexPeakConfig::new()
    .laptop()
    .pci_subsystem_ids(X201_SUBSYSTEM_IDS)
    // Arrandale: two cores, two threads each.
    .max_cpus(4)
    // romstage.c `mainboard_get_spd_map()`.
    .spd_addresses([0x50, 0, 0x51, 0])
    // cmos.default: gfx_uma_size 0 = 32 MiB.
    .gfx_uma_size(0)
    // devicetree: PEG 01.0 off.
    .peg(false)
    .igd(X201_IGD)
    .gpio_pins(x201_gpio_pins())
    .gpi_routing({
        let mut routing = [GpiRoute::Disabled; 16];
        routing[1] = GpiRoute::Sci;
        routing[13] = GpiRoute::Sci;
        routing
    })
    .gpe0_en(0x2002_2046)
    .alt_gp_smi_en(0)
    .lpc_generic_io([
        LpcGenericIoDecode {
            base: X201_H8.resources.auxiliary_base.raw(),
            size: fstart_driver_lenovo::h8::H8Resources::AUXILIARY_SIZE,
        },
        LpcGenericIoDecode {
            base: X201_H8.resources.pmh7_base.raw(),
            size: fstart_driver_lenovo::h8::H8Resources::PMH7_SIZE as u16,
        },
        LpcGenericIoDecode {
            base: 0x1680,
            size: 0x20,
        },
        LpcGenericIoDecode {
            base: 0x68,
            size: 0x8,
        },
    ])
    // cmos.default: sata_mode=AHCI.
    .sata(SataConfig {
        mode: SataMode::Ahci,
        ports: 0x03,
        interface_speed: 0,
    })
    .usb_ports(X201_USB_PORTS)
    .hda_verb(0x14f1_5069, 0x17aa_2155, x201_conexant_pins(), [0u32; 0])
    .hda_verb(0x8086_2804, 0x17aa_21b5, x201_hdmi_pins(), [0u32; 0])
    // The ExpressCard slot on root port 4.
    .pcie_hotplug(3)
    .disable([
        PchFunction::Mei2,
        PchFunction::Ider,
        PchFunction::Kt,
        PchFunction::PcieRootPort(2),
        PchFunction::PcieRootPort(5),
        PchFunction::PcieRootPort(6),
        PchFunction::PcieRootPort(7),
        PchFunction::Sata2,
    ])
    .docking_supported()
    // cmos.default: power_on_after_fail=Disable, nmi=Enable.
    .power_on_after_fail(0)
    .nmi(true)
    .dram_reset_gate_gpio(DRAM_RESET_GATE_GPIO)
    .ehci_debug_bar(EHCI_CONSOLE)
    .build();

/// Devicetree IGD registers and gma-mainboard.ads ports (DP2 on the dock,
/// HDMI2, Analog, LVDS: libgfxinit's second digital port is PCH port C).
const X201_IGD: IronlakeIgdConfig = IronlakeIgdConfig {
    dp_d_hotplug: 0x06,
    panel_port: PanelPort::Lvds,
    panel_power_cycle_delay: 3,
    panel_power_up_delay: 250,
    panel_power_down_delay: 250,
    panel_backlight_on_delay: 2500,
    panel_backlight_off_delay: 2500,
    cpu_backlight: 0x58d,
    pch_backlight: 0x061a_061a,
    vbt: VbtSource::ffs(VBT),
    display: Some(IgdDisplayPolicy {
        outputs: &[
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
        ],
        framebuffer: FramebufferConfig {
            scaling: fstart_intel_gma::scaler::ScalingPolicy::PreserveAspect,
            ..FramebufferConfig::edid(FallbackMode {
                width: 1280,
                height: 800,
                refresh_hz: 60,
            })
        },
    }),
    ..IronlakeIgdConfig::new()
};

/// romstage.c `mainboard_usb_ports`.
const X201_USB_PORTS: [UsbPort; 14] = {
    use UsbCurrent::{I14B, I55F, I74B, I557};
    [
        UsbPort::new(I557, 0),
        UsbPort::new(I55F, 1),
        UsbPort::new(I74B, 3),
        UsbPort::new(I74B, 3),
        UsbPort::new(I557, 3),
        UsbPort::new(I14B, 3),
        UsbPort::new(I74B, 3),
        UsbPort::new(I74B, 3),
        UsbPort::new(I74B, 4),
        UsbPort::new(I74B, 5),
        UsbPort::new(I55F, 7),
        UsbPort::new(I55F, 7),
        UsbPort::new(I557, 7),
        UsbPort::new(I55F, 7),
    ]
};

/// Devicetree `subsystemid` entries.
const X201_SUBSYSTEM_IDS: &[PciSubsystemId] = {
    const fn lenovo(device: u8, function: u8, id: u16) -> PciSubsystemId {
        PciSubsystemId::new(device, function, 0x17aa, id)
    }
    &[
        lenovo(0x00, 0, 0x2193),
        lenovo(0x02, 0, 0x215a),
        lenovo(0x19, 0, 0x2153),
        lenovo(0x1a, 0, 0x2163),
        lenovo(0x1b, 0, 0x215e),
        lenovo(0x1d, 0, 0x2163),
        lenovo(0x1f, 0, 0x2166),
        lenovo(0x1f, 2, 0x2168),
        lenovo(0x1f, 3, 0x2167),
    ]
};

/// hda_verb.c: Conexant CX20585.
const fn x201_conexant_pins() -> [hda::PinConfig; 10] {
    use hda::{
        PinColor as C, PinConn as K, PinConnector as T, PinDevice as D, PinGeoLoc as G, PinLoc as L,
    };
    [
        // Headphone jack.
        hda::pin_config(
            0x19,
            D::HpOut,
            K::Jack,
            L::External,
            G::Right,
            T::StereoMono18,
            C::Green,
            0,
            15,
            0,
        ),
        // Dock mic jack.
        hda::pin_config(
            0x1a,
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
        // Mic jack.
        hda::pin_config(
            0x1b,
            D::MicIn,
            K::Jack,
            L::External,
            G::Right,
            T::StereoMono18,
            C::Pink,
            0,
            15,
            0,
        ),
        // Dock headphone jack.
        hda::pin_config(
            0x1c,
            D::HpOut,
            K::Nc,
            L::SeparateChassis,
            G::Rear,
            T::StereoMono18,
            C::Green,
            0,
            15,
            0,
        ),
        // EAPD detect.
        hda::pin_config(
            0x1d,
            D::Speaker,
            K::Nc,
            L::SeparateChassis,
            G::NA,
            T::OtherAnalog,
            C::ColorUnknown,
            0,
            15,
            0,
        ),
        unused(0x1e),
        hda::pin_config(
            0x1f,
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
        unused(0x20),
        unused(0x22),
        // Internal mic.
        hda::pin_config(
            0x23,
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

/// Conexant's unused pins: `0x40f001f0`.
const fn unused(nid: u8) -> hda::PinConfig {
    use hda::{
        PinColor as C, PinConn as K, PinConnector as T, PinDevice as D, PinGeoLoc as G, PinLoc as L,
    };
    hda::pin_config(
        nid,
        D::DeviceOther,
        K::Nc,
        L::External,
        G::NA,
        T::Unknown,
        C::ColorUnknown,
        1,
        15,
        0,
    )
}

/// hda_verb.c: the PCH's HDMI codec, the digital display pins.
const fn x201_hdmi_pins() -> [hda::PinConfig; 3] {
    use hda::{
        PinColor as C, PinConn as K, PinConnector as T, PinDevice as D, PinGeoLoc as G, PinLoc as L,
    };
    const fn display(nid: u8, conn: K, group: u8) -> hda::PinConfig {
        hda::pin_config(
            nid,
            D::DigitalOtherOut,
            conn,
            L::Internal,
            G::Special8,
            T::OtherDigital,
            C::ColorUnknown,
            0,
            group,
            0,
        )
    }
    [
        display(0x04, K::Nc, 1),
        display(0x05, K::Jack, 2),
        display(0x06, K::Nc, 3),
    ]
}

/// gpio.c: the pads in GPIO mode; unlisted pads keep their native function.
const fn x201_gpio_pins() -> [gpio::GpioPin; 35] {
    use gpio::{GpioLevel as Level, input, output};
    [
        input(0),
        input(1).inverted(),
        input(2).inverted(),
        input(3),
        input(4),
        input(5),
        input(6).inverted(),
        input(7).inverted(),
        output(8, Level::High),
        // DRAM_RESET_GATE_GPIO: high keeps the DIMM reset ungated.
        output(10, Level::High),
        input(13).inverted(),
        output(15, Level::High),
        input(16).inverted(),
        input(17),
        input(21),
        output(22, Level::High),
        input(24).rsmrst(),
        output(27, Level::High),
        // Dock connect, raised by the dock code.
        output(28, Level::Low),
        output(33, Level::High),
        input(34),
        output(35, Level::High),
        input(36),
        input(37),
        input(38),
        input(39),
        output(41, Level::High),
        output(42, Level::High),
        input(48),
        output(49, Level::High),
        output(50, Level::High),
        output(52, Level::High),
        output(53, Level::Low),
        output(54, Level::Low),
        input(57),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expresscard_slot_matches_coreboot_and_the_hotplug_port() {
        use fstart_platform_intel::facts::IntelBoardFacts;
        let slots = <crate::Board as IntelBoardFacts>::FACTS.smbios_slots;
        assert_eq!(slots.len(), 1);
        let binding = &slots[0];
        let slot = &binding.slot;
        assert_eq!(slot.designation, "ExpressCard Slot");
        assert_eq!(
            (slot.slot_type, slot.length, slot.data_bus_width),
            (7, 3, 8)
        );
        assert_eq!(
            (
                binding.bridge_segment,
                binding.bridge_bus,
                binding.bridge_devfn
            ),
            (0, 0, 0xe3)
        );
        assert_eq!(slot.characteristics, [0, 3]);
        assert!(X201_PLATFORM.southbridge.pcie_hotplug[3]);
    }

    #[test]
    fn flash_and_spd_facts_match_coreboot() {
        assert_eq!(FLASH.size(), 8 << 20);
        assert_eq!(FLASH.bios_region().unwrap().offset, 0x500000);
        assert_eq!(FLASH.bios_region().unwrap().size, 0x300000);
        assert_eq!(X201_PLATFORM.northbridge.spd_addresses, [0x50, 0, 0x51, 0]);
    }

    #[test]
    fn codec_pins_match_hda_verb_c() {
        assert_eq!(
            x201_conexant_pins().map(|p| p.encode()),
            [
                0x042140f0, 0x61a190f0, 0x04a190f0, 0x612140f0, 0x601700f0, 0x40f001f0, 0x901701f0,
                0x40f001f0, 0x40f001f0, 0x90a601f0,
            ]
        );
        assert_eq!(
            x201_hdmi_pins().map(|p| p.encode()),
            [0x58560010, 0x18560020, 0x58560030]
        );
    }

    /// gpio.c's set registers, for the pads this board configures.
    #[test]
    fn gpio_table_matches_gpio_c() {
        let mut mode = [0u32; 3];
        let mut input = [0u32; 3];
        let mut high = [0u32; 3];
        let mut invert = 0u32;
        let mut rsmrst = 0u32;
        for pin in x201_gpio_pins() {
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
            if pin.reset == gpio::GpioReset::Rsmrst {
                rsmrst |= bit;
            }
        }
        assert_eq!(mode, [0x1963_a5ff, 0x0277_06fe, 0]);
        assert_eq!(input[0], 0x87bf_6aff & mode[0]);
        assert_eq!(input[1], 0x1b01_f9f4 & mode[1]);
        assert_eq!(high[0], 0xefff_ffff & mode[0] & !input[0]);
        assert_eq!(high[1], 0x029f_ffff & mode[1] & !input[1]);
        assert_eq!(invert, 0x0001_20c6);
        assert_eq!(rsmrst, 0x4100_0000 & mode[0]);
    }
}
