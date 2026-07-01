//! Host Rust board metadata crate for `qemu-aarch64`.
//!
//! This mainboard crate only names the board package and selects the reusable
//! QEMU AArch64 `virt` defaults. Runtime driver wiring lives in the board-owned
//! stage package.

#![cfg_attr(feature = "stage", no_std)]

pub mod facts;
#[cfg(feature = "stage")]
pub mod stage;

#[cfg(not(feature = "stage"))]
use fstart_board_qemu_virt::QemuAarch64VirtConfig;
use fstart_types::Platform;
#[cfg(not(feature = "stage"))]
use fstart_types::{BoardConfig, BoardInfo, BuildInfo};

/// Stable fstart board name.
pub const BOARD_NAME: &str = facts::BOARD_NAME;
/// Cargo package containing this board.
pub const BOARD_PACKAGE: &str = facts::BOARD_PACKAGE;
/// Platform declared by Cargo discovery metadata.
pub const PLATFORM: Platform = Platform::Aarch64;

/// Returns the stable fstart board name.
pub const fn board_name() -> &'static str {
    BOARD_NAME
}

#[cfg(not(feature = "stage"))]
fn config() -> QemuAarch64VirtConfig {
    QemuAarch64VirtConfig::new(BOARD_NAME, BOARD_PACKAGE)
}

/// Complete board facts for runtime hardware, stage policy, and packaging.
#[cfg(not(feature = "stage"))]
pub fn board_config() -> BoardConfig {
    config().board_config()
}

/// Runtime hardware facts for static typed board mode.
#[cfg(not(feature = "stage"))]
pub fn board_info() -> BoardInfo {
    config().board_info()
}

/// Host build/package facts for this board.
#[cfg(not(feature = "stage"))]
pub fn build_info() -> BuildInfo {
    config().build_info()
}
