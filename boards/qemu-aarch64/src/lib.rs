//! Rust board metadata crate for `qemu-aarch64`.
//!
//! This mainboard crate only names the board package and selects the reusable
//! QEMU AArch64 `virt` defaults. Emulator memory maps, UART wiring, fixed stage
//! flow, and default LinuxBoot/ATF placement live in `fstart-board-qemu-virt`.

#![cfg_attr(not(feature = "host"), no_std)]

#[cfg(feature = "host")]
#[cfg(feature = "host")]
use fstart_board_qemu_virt::QemuAarch64Virt;
#[cfg(feature = "host")]
use fstart_types::{BoardConfig, BoardInfo, BuildInfo, Platform};

/// Stable fstart board name.
pub const BOARD_NAME: &str = "qemu-aarch64";
/// Cargo package containing this board.
pub const BOARD_PACKAGE: &str = "fstart-board-qemu-aarch64";

/// Returns the stable fstart board name.
pub const fn board_name() -> &'static str {
    BOARD_NAME
}

#[cfg(feature = "host")]
fn board() -> QemuAarch64Virt {
    QemuAarch64Virt::new(BOARD_NAME, BOARD_PACKAGE)
}

/// Complete board facts for runtime hardware, stage policy, and packaging.
#[cfg(feature = "host")]
pub fn board_config() -> BoardConfig {
    board().board_config()
}

/// Runtime hardware facts for static typed board mode.
#[cfg(feature = "host")]
pub fn board_info() -> BoardInfo {
    board().board_info()
}

/// Host build/package facts for this board.
#[cfg(feature = "host")]
pub fn build_info() -> BuildInfo {
    board().build_info()
}

/// Platform declared by Cargo discovery metadata.
#[cfg(feature = "host")]
pub const PLATFORM: Platform = Platform::Aarch64;
