//! QEMU AArch64 virt hardware facts. Platform Rust owns fixed image budgets.

use fstart_core::Platform;
use fstart_platform_qemu::{
    QemuAarch64VirtConfig,
    facts::{Aarch64BoardFacts, Aarch64ImageFacts},
};

/// Physical capacity of QEMU virt's two flash banks, checked in every graph.
pub const IMAGE_FACTS: Aarch64ImageFacts = Aarch64ImageFacts::new(0x0800_0000);
impl Aarch64BoardFacts for crate::Board {
    const IMAGE: Aarch64ImageFacts = IMAGE_FACTS;
}

pub const BOARD_NAME: &str = "qemu-aarch64";
pub const BOARD_PACKAGE: &str = "fstart-board-qemu-aarch64";
pub const PLATFORM: Platform = Platform::Aarch64;

/// Retain hardware facts in ROM, not on the early-stage stack.
pub static QEMU_AARCH64_VIRT: QemuAarch64VirtConfig = QemuAarch64VirtConfig::new().build();

#[must_use]
pub const fn board_name() -> &'static str {
    BOARD_NAME
}
