// SPDX-License-Identifier: MIT OR Apache-2.0
//! Peppy platform facts, flash layout, native Haswell GMA display policy, and PCH wiring.

use fstart_core::{
    ConstVec, FlashLayout, IntelIfdFlashLayout, IntelIfdRegion as Kind,
    IntelIfdRegionConfig as Region,
};
use fstart_driver_intel::gma::{
    FallbackMode, OutputConfig, Port, framebuffer::FramebufferConfig, panel::PanelPowerDelays,
    scaler::ScalingPolicy,
};
use fstart_platform_intel::{
    facts::{BoardFacts, IntelBoardFacts},
    haswell::{self as hsw, Location, Usb2Port, Usb3Port},
    igd::{IgdDisplayPolicy, VbtSource},
};

/// Native Haswell GMA display policy (coreboot `slippy/gma-mainboard.ads`:
/// eDP internal 1366x768 panel on DDI-A + HDMI1 external port on DDI-B).
pub const DISPLAY: IgdDisplayPolicy = IgdDisplayPolicy {
    outputs: &[
        OutputConfig {
            port: Port::Edp,
            enabled: true,
        },
        OutputConfig {
            port: Port::HdmiA,
            enabled: true,
        },
    ],
    framebuffer: FramebufferConfig {
        scaling: ScalingPolicy::PreserveAspect,
        ..FramebufferConfig::edid(FallbackMode {
            width: 1366,
            height: 768,
            refresh_hz: 60,
        })
    },
};

impl IntelBoardFacts for crate::Board {
    type Platform = hsw::HaswellUlt;
    const CONFIG: &'static hsw::Config<true> = &PLATFORM;
    const OPTIONS: &'static [&'static dyn fstart_options::Describe] = &[&crate::options::DEVICES];
    const FACTS: BoardFacts = {
        // chromeos.fmd, SI_ALL and SI_BIOS only. No ChromeOS FMAP or GbE.
        const FD: Region = Region {
            kind: Kind::Descriptor,
            offset: 0,
            size: 0x1000,
        };
        let layout = IntelIfdFlashLayout::new(
            ConstVec::new(FD)
                .push(FD)
                .push(Region {
                    kind: Kind::Me,
                    offset: 0x1000,
                    size: 0x1ff000,
                })
                .push(Region {
                    kind: Kind::Bios,
                    offset: 0x200000,
                    size: 0x600000,
                }),
        );
        BoardFacts::new(FlashLayout::IntelIfd(layout), 0x800000)
            .with_data_assets(&["data.vbt"])
            .with_memory_cache()
    };
}

pub static PLATFORM: hsw::Config<true> = {
    let mut c = crate::slippy::config(&crate::pads::PADS);
    c.northbridge.select_spd = Some(crate::spd::select);
    c.northbridge.igd.vbt = Some(VbtSource::ffs("data.vbt"));
    c.northbridge.igd.display = Some(DISPLAY);
    c.northbridge.igd.panel.delays = Some(PanelPowerDelays {
        power_up_us: 40_000,
        power_down_us: 15_000,
        power_cycle_us: 400_000,
        power_up_to_backlight_on_us: 210_000,
        backlight_off_to_power_down_us: 210_000,
    });
    c.northbridge.igd.panel.backlight_pwm_hz = 200;
    c.northbridge.igd.panel.backlight_active_low = false;
    c.northbridge.igd.panel.ddi_e_connected = false;
    // Empty list selects full 0..=100% granularity in ACPI `_BCL`.
    c.northbridge.igd.brightness_levels = &[];
    c.southbridge.pcie_ports = 0b11; // fitted RP2 is gated by lte_ngff before enumeration
    c.southbridge.sata_gen3_dtle = [Some(5); 2];
    c.southbridge.icc_clock_disable = 0x013c_0000;
    c.southbridge.hda = Some(&crate::hda::ALC283);
    c.southbridge.usb2 = &USB2;
    c.southbridge.usb3 = &USB3;
    c.build()
};

const USB2: [Usb2Port; 8] = {
    use Location::*;
    const fn port(length: u16, oc_pin: Option<u8>, location: Location) -> Usb2Port {
        Usb2Port {
            length,
            enabled: !matches!(location, Location::Skip),
            oc_pin,
            location,
        }
    }
    [
        port(0x150, None, MiniPcie),
        port(0x40, Some(0), BackPanel),
        port(0x80, None, Internal),
        port(0x40, None, MiniPcie),
        port(0x40, Some(2), BackPanel),
        port(0, None, Internal),
        port(0x150, None, Flex),
        port(0, None, Skip),
    ]
};

const USB3: [Usb3Port; 4] = {
    let mut ports = [Usb3Port {
        enabled: false,
        oc_pin: None,
    }; 4];
    ports[0] = Usb3Port {
        enabled: true,
        oc_pin: Some(0),
    };
    ports
};
