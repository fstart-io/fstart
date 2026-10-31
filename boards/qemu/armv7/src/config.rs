//! QEMU ARMv7 virt hardware facts. Platform Rust owns fixed image budgets.

use fstart_platform_qemu::{
    QemuVirtConfig,
    facts::{VirtBoardFacts, VirtMachine},
};

impl VirtBoardFacts for crate::Board {
    const MACHINE: VirtMachine = VirtMachine::Armv7;
}

/// Retain hardware facts in ROM, not on the early-stage stack.
pub static QEMU_ARMV7_VIRT: QemuVirtConfig = QemuVirtConfig::armv7().build();
