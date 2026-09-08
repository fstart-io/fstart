//! QEMU ARMv7 virt hardware policy. Build geometry is metadata-owned.

use fstart_core::Platform;
#[cfg(feature = "stage")]
use fstart_platform_qemu::QemuArmv7VirtConfig;

pub const BOARD_NAME: &str = "qemu-armv7";
pub const BOARD_PACKAGE: &str = "fstart-board-qemu-armv7";
pub const PLATFORM: Platform = Platform::Armv7;

/// Retain hardware facts in ROM, not on the early-stage stack.
#[cfg(feature = "stage")]
pub static QEMU_ARMV7_VIRT: QemuArmv7VirtConfig = QemuArmv7VirtConfig::new().build();

#[must_use]
pub const fn board_name() -> &'static str {
    BOARD_NAME
}
