//! MinnowBoard MAX wiring, from coreboot `mb/intel/minnowmax` (devicetree,
//! GPIO table and flash map).

use fstart_core::{
    ConstVec, FlashLayout, IntelIfdFlashLayout, IntelIfdRegion as Kind,
    IntelIfdRegionConfig as Region,
};
use fstart_platform_intel::baytrail::*;
use fstart_platform_intel::igd::VbtSource;

pub type Hardware = Baytrail;
pub const BOARD_NAME: &str = "intel-minnowmax";
pub const UART0_PIO_BASE: u16 = 0x3f8;
pub const UART0_CLOCK_FREQ: u32 = 1_843_200;
pub const UART0_BAUD_RATE: u32 = 115_200;
pub const FLASH_SIZE: u32 = 0x80_0000;

/// 8 MiB Winbond W25Q64: descriptor, TXE, then the 4 MiB BIOS region.
pub const FLASH: IntelIfdFlashLayout = {
    const DESCRIPTOR: Region = Region {
        kind: Kind::Descriptor,
        offset: 0,
        size: 0x1000,
    };
    IntelIfdFlashLayout::new(
        ConstVec::new(DESCRIPTOR)
            .push(DESCRIPTOR)
            .push(Region {
                kind: Kind::Me,
                offset: 0x1000,
                size: 0x3f_f000,
            })
            .push(Region {
                kind: Kind::Bios,
                offset: 0x40_0000,
                size: 0x40_0000,
            }),
    )
};

pub static MINNOWMAX_PLATFORM: BaytrailPlatform = BaytrailConfig::new()
    .max_cpus(4)
    .spd(SpdSource::Board)
    .gfx(GfxConfig {
        vbt: Some(VbtSource::ffs("data.vbt")),
        ..GfxConfig::new()
    })
    .disable_all([
        Function::Mmc,
        Function::Sdio,
        Function::Mmc45,
        Function::I2c(1),
        Function::I2c(3),
        Function::I2c(4),
        Function::I2c(5),
        Function::Hda,
    ])
    .sata(SataConfig {
        port_map: 0x1,
        ahci: true,
        ide_legacy_combined: false,
    })
    .usb(UsbConfig {
        route_to_xhci: false,
        usb2_port_disable_mask: 0,
        usb3_port_disable_mask: 0,
        usb2_comp_bg: 0x4700,
        usb2_phy: [
            Usb2Phy {
                lane: 0x0004_9a09,
                rcomp_hs_pullup: 0x0300_401d,
            },
            Usb2Phy {
                lane: 0x0004_9a09,
                rcomp_hs_pullup: 0x0300_401d,
            },
            Usb2Phy {
                lane: 0x0004_9209,
                rcomp_hs_pullup: 0x0100_4015,
            },
            Usb2Phy {
                lane: 0x0004_9a09,
                rcomp_hs_pullup: 0x0300_401d,
            },
        ],
    })
    .pcie_wake(true)
    .disable_slp_x_stretch_sus_fail(true)
    .gpio(GpioConfig {
        ncore: &crate::gpio::NCORE,
        score: &crate::gpio::SCORE,
        ssus: &crate::gpio::SSUS,
        ..GpioConfig::new()
    })
    .build();

impl fstart_platform_intel::facts::IntelBoardFacts for crate::Board {
    type Platform = Hardware;
    const CONFIG: &'static BaytrailPlatform = &MINNOWMAX_PLATFORM;
    const FACTS: fstart_platform_intel::facts::BoardFacts =
        fstart_platform_intel::facts::BoardFacts::new(FlashLayout::IntelIfd(FLASH), FLASH_SIZE)
            .with_data_assets(&["data.vbt"]);
}
