//! QEMU AArch64 virt hardware facts. Platform Rust owns fixed image budgets.

use fstart_platform_qemu::{
    QemuVirtConfig,
    facts::{VirtBoardFacts, VirtMachine},
};

impl VirtBoardFacts for crate::Board {
    const MACHINE: VirtMachine = VirtMachine::Aarch64;
}

/// Retain hardware facts in ROM, not on the early-stage stack.
pub static QEMU_AARCH64_VIRT: QemuVirtConfig = QemuVirtConfig::aarch64().build();
