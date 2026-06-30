//! Host Rust board metadata crate for `qemu-aarch64`.
//!
//! This mainboard crate only names the board package and selects the reusable
//! QEMU AArch64 `virt` defaults. Runtime driver wiring lives in the board-owned
//! stage package.

use fstart_board_qemu_aarch64_facts as facts;
use fstart_board_qemu_virt::QemuAarch64Virt;
use fstart_types::{BoardConfig, BoardInfo, BuildInfo, Platform};

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

fn board() -> QemuAarch64Virt {
    QemuAarch64Virt::new(BOARD_NAME, BOARD_PACKAGE)
}

/// Complete board facts for runtime hardware, stage policy, and packaging.
pub fn board_config() -> BoardConfig {
    board().board_config()
}

/// Runtime hardware facts for static typed board mode.
pub fn board_info() -> BoardInfo {
    board().board_info()
}

/// Host build/package facts for this board.
pub fn build_info() -> BuildInfo {
    board().build_info()
}
