//! Foxconn D41S UEFI Rust board metadata.

use fstart_board_foxconn_d41s::{d41s_gpio_config, d41s_hda_config, d41s_smbios};
use fstart_platform_intel_pineview_ich7::{
    LpcGenericIoDecode, PcieRootPort, PineviewIch7Platform, SataConfig, SataMode, UsbConfig,
};
use fstart_types::{
    build_info_from_config, io16, x86_uefi_payload, BoardConfig, BoardInfo, BuildInfo, BusAddress,
    Platform,
};

pub const BOARD_NAME: &str = "foxconn-d41s-uefi";
pub const BOARD_PACKAGE: &str = "fstart-board-foxconn-d41s-uefi";
pub const PLATFORM: Platform = Platform::X86_64;

fn board() -> PineviewIch7Platform {
    PineviewIch7Platform::new(BOARD_NAME, BOARD_PACKAGE)
        .payload(x86_uefi_payload())
        .pcie_port(PcieRootPort::Port0, true)
        .pcie_port(PcieRootPort::Port1, true)
        .lpc_generic_io(LpcGenericIoDecode {
            base: 0x0a00,
            size: 0x0100,
        })
        .gpe0_en(0x441)
        .sata(SataConfig {
            mode: SataMode::Ahci,
            ports: 0x3,
        })
        .usb(UsbConfig {
            ehci: true,
            uhci: [true, true, true, true],
        })
        .hda(d41s_hda_config())
        .gpio(d41s_gpio_config())
        .superio("superio", io16(0x2e))
        .on_smbus(|smbus| {
            smbus.runtime("ck505", BusAddress::I2c(0x69));
        })
        .smbios(d41s_smbios())
}

#[must_use]
pub fn board_config() -> BoardConfig {
    board().board_config()
}

#[must_use]
pub fn board_info() -> BoardInfo {
    board().board_info()
}

#[must_use]
pub fn build_info() -> BuildInfo {
    let config = board_config();
    build_info_from_config(
        BOARD_NAME,
        BOARD_PACKAGE,
        &config,
        ["intel-pineview", "intel-ich7", "ite8721f", "i2c-ck505"],
    )
}

#[must_use]
pub const fn board_name() -> &'static str {
    BOARD_NAME
}
