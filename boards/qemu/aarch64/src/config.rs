//! QEMU AArch64 virt hardware policy. Geometry is metadata-owned.

use fstart_core::Platform;
#[cfg(feature = "stage")]
use fstart_platform_qemu::QemuAarch64VirtConfig;

pub const BOARD_NAME: &str = "qemu-aarch64";
pub const BOARD_PACKAGE: &str = "fstart-board-qemu-aarch64";
pub const PLATFORM: Platform = Platform::Aarch64;

/// Retain hardware facts in ROM, not on the early-stage stack.
#[cfg(feature = "stage")]
pub static QEMU_AARCH64_VIRT: QemuAarch64VirtConfig = QemuAarch64VirtConfig::new().build();

#[must_use]
pub const fn board_name() -> &'static str {
    BOARD_NAME
}
