//! QEMU AArch64 virt hardware facts. Platform Rust owns fixed image budgets.

use fstart_platform_qemu::{
    QemuAarch64VirtConfig,
    facts::{VirtBoardFacts, VirtMachine},
};

impl VirtBoardFacts for crate::Board {
    const MACHINE: VirtMachine = VirtMachine::Aarch64;
}

/// Retain hardware facts in ROM, not on the early-stage stack.
pub static QEMU_AARCH64_VIRT: QemuAarch64VirtConfig = QemuAarch64VirtConfig::new().build();
