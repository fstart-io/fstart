//! Foxconn D41S UEFI Rust board metadata.

#![no_std]

#[cfg(feature = "stage")]
pub mod stage;
use fstart_board_foxconn_d41s::d41s_pineview_ich7_config;
use fstart_platform_intel_pineview_ich7::PineviewIch7Config;
use fstart_types::{x86_uefi_payload, BoardConfig, BoardInfo, BuildInfo, Platform};

pub const BOARD_NAME: &str = "foxconn-d41s-uefi";
pub const BOARD_PACKAGE: &str = "fstart-board-foxconn-d41s-uefi";
pub const PLATFORM: Platform = Platform::X86_64;

fn config() -> PineviewIch7Config {
    d41s_pineview_ich7_config(BOARD_NAME, BOARD_PACKAGE).payload(x86_uefi_payload())
}

#[must_use]
pub fn board_config() -> BoardConfig {
    config().board_config()
}

#[must_use]
pub fn board_info() -> BoardInfo {
    config().board_info()
}

#[must_use]
pub fn build_info() -> BuildInfo {
    config().build_info(["intel-pineview", "intel-ich7", "ite8721f", "i2c-ck505"])
}

#[must_use]
pub const fn board_name() -> &'static str {
    BOARD_NAME
}
