//! Foxconn D41S UEFI Rust board metadata.

use fstart_board_foxconn_d41s::d41s_smbios;
use fstart_platform_intel_pineview_ich7::{PcieRootPort, PineviewIch7Board};
use fstart_types::{
    build_info_from_config, io16, x86_uefi_payload, BoardConfig, BoardInfo, BuildInfo, BusAddress,
    Platform,
};

pub const BOARD_NAME: &str = "foxconn-d41s-uefi";
pub const BOARD_PACKAGE: &str = "fstart-board-foxconn-d41s-uefi";
pub const PLATFORM: Platform = Platform::X86_64;

fn board() -> PineviewIch7Board {
    PineviewIch7Board::new(BOARD_NAME, BOARD_PACKAGE)
        .payload(x86_uefi_payload())
        .pcie_port(PcieRootPort::Port0, true)
        .pcie_port(PcieRootPort::Port1, true)
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
