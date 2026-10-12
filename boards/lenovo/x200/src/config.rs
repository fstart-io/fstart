//! Lenovo X200 board wiring; chipset mechanisms remain in GM45/ICH9.
use fstart_core::{
    ConstVec, FlashLayout, IntelIfdFlashLayout, IntelIfdRegion as Kind,
    IntelIfdRegionConfig as Region,
};
use fstart_driver_intel::southbridge::{gpio_ich as gpio, hda};
use fstart_platform_intel::gm45::{
    Gm45Ich9Config, Gm45Ich9Platform, GpiRoute, LpcFixedIoDecode, LpcFloppyDecode,
    LpcGenericIoDecode, LpcSerialDecode, SataConfig, SataMode, UsbConfig,
};

/// Verified from Lenovo's 6D BIOS stock update: `bcpvpd` unpacked FL1
/// followed by `ifdtool -d`. This is NOT the 7X/4 MiB
/// X200 layout and does not bundle vendor descriptor, ME, GbE or PDR bytes.
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

const VBT: &str = "data_led.vbt";
pub const EHCI_CONSOLE: fstart_driver_usbdebug::Config = fstart_driver_usbdebug::Config {
    ecam_base: fstart_platform_intel::gm45::GM45_ECAM_BASE as usize,
    // Coreboot X200 Kconfig: USBDEBUG_HCD_INDEX=2, port 1 of EHCI2.
    bus: 0,
    device: 0x1a,
    function: 7,
    bar: 0xfed0_4000,
};

impl fstart_platform_intel::facts::IntelBoardFacts for crate::Board {
    type Platform = fstart_platform_intel::gm45::Gm45Ich9;
    const CONFIG: &'static Gm45Ich9Platform = &X200_PLATFORM;
    const FACTS: fstart_platform_intel::facts::BoardFacts =
        fstart_platform_intel::facts::BoardFacts::new(FlashLayout::IntelIfd(FLASH), 0x800000)
            .with_data_assets(&[VBT]);
}

/// The X200 SPD mux is initially on the DIMM sockets; GPIO42 selects the
/// board EEPROM only after memory initialization (coreboot romstage.c).
pub static X200_PLATFORM: Gm45Ich9Platform = Gm45Ich9Config::new()
    .max_cpus(2)
    .spd_addresses([0x50, 0, 0x51, 0])
    // coreboot i82801ix leaves DxxIP/DxxIR as found and reads DxxIR
    // when building _PRT; preserve that mapping instead of inventing one.
    .preserve_hardware_pirq()
    .pirq_routing([0x0b, 0x0b, 0x0b, 0x0b, 0x80, 0x80, 0x80, 0x80])
    .pcie_port(0, true)
    .pcie_port(1, true)
    .pcie_port(2, true)
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
    .lpc_fixed_io(LpcFixedIoDecode {
        com_a: LpcSerialDecode::Com1,
        com_b: LpcSerialDecode::Com2,
        lpt: None,
        fdd: Some(LpcFloppyDecode::Fdd3f0),
    })
    .lpc_generic_io([
        LpcGenericIoDecode {
            base: 0x1600,
            size: 0x80,
        },
        LpcGenericIoDecode {
            base: 0x15e0,
            size: 0x10,
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
        clock_request: false,
        traffic_monitor: false,
    })
    .usb(UsbConfig {
        ehci: [true; 2],
        uhci: [true; 6],
    })
    .ehci_debug_bar(EHCI_CONSOLE)
    .hda_verb(0x14f1_5051, 0x17aa_20ff, x200_hda_pins(), X200_BEEP_VERBS)
    .gpio_pins(x200_gpio_pins())
    .build();

/// Coreboot x200/hda_verb.c: retain the exact codec pin defaults.
const fn x200_hda_pins() -> [hda::PinConfig; 8] {
    use hda::{
        PinColor as C, PinConn as K, PinConnector as T, PinDevice as D, PinGeoLoc as G, PinLoc as L,
    };
    [
        hda::pin_config(
            0x16,
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
            G::Right,
            T::StereoMono18,
            C::Pink,
            0,
            15,
            0,
        ),
        hda::pin_config(
            0x19,
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
const X200_BEEP_VERBS: [u32; 8] = [
    0x0017_0500,
    0x0147_0500,
    0x0147_0100,
    0x0147_0740,
    0x00b3_7517,
    0x00c3_7100,
    0x00c3_b015,
    0x0143_b000,
];

/// Coreboot variants/x200/gpio.c. Unlisted pads select their native functions.
const fn x200_gpio_pins() -> [gpio::GpioPin; 31] {
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn vendor_ifd_and_codec_facts_match_x200_6d() {
        assert_eq!(FLASH.size(), 8 << 20);
        assert_eq!(FLASH.bios_region().unwrap().offset, 0x600000);
        assert_eq!(FLASH.bios_region().unwrap().size, 0x200000);
        assert_eq!(X200_PLATFORM.northbridge.spd_addresses, [0x50, 0, 0x51, 0]);
        assert!(X200_PLATFORM.southbridge.pirq.is_none());
        assert!(X200_PLATFORM.southbridge.preserve_hardware_pirq);
        assert_eq!(
            x200_hda_pins().map(|p| p.encode()),
            [
                0x042140f0, 0x61a190f0, 0x04a190f0, 0x612140f0, 0x901701f0, 0x40f001f0, 0x40f001f0,
                0x90a601f0,
            ]
        );
    }
}
