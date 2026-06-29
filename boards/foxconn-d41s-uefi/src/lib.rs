//! Foxconn D41S UEFI Rust board metadata.

use fstart_board_foxconn_d41s::{
    d41s_ck505_config, d41s_gpio_config, d41s_hda_config, d41s_smbios, d41s_superio_config,
};
use fstart_device_registry::DriverInstance;
use fstart_platform_intel_pineview_ich7::{uefi_payload, PineviewIch7Platform};
use fstart_types::{BoardConfig, BoardInfo, BuildInfo, Platform};

pub const BOARD_NAME: &str = "foxconn-d41s-uefi";
pub const BOARD_PACKAGE: &str = "fstart-board-foxconn-d41s-uefi";
pub const PLATFORM: Platform = Platform::X86_64;

fn board() -> PineviewIch7Platform {
    PineviewIch7Platform::new(BOARD_NAME, BOARD_PACKAGE)
        .payload(uefi_payload())
        .hda(d41s_hda_config())
        .gpio(d41s_gpio_config())
        .superio(d41s_superio_config())
        .clock_generator(d41s_ck505_config())
        .smbios(d41s_smbios())
}

#[must_use]
pub fn board_config() -> BoardConfig {
    board().board_config()
}

#[must_use]
pub fn driver_instances() -> Vec<DriverInstance> {
    board().driver_instances()
}

#[must_use]
pub fn board_info() -> BoardInfo {
    board().board_info()
}

#[must_use]
pub fn build_info() -> BuildInfo {
    board().build_info()
}

#[must_use]
pub const fn board_name() -> &'static str {
    BOARD_NAME
}
