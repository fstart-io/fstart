//! QEMU SBSA-ref hardware facts. Platform Rust owns fixed image budgets.

use fstart_platform_qemu::{
    QemuSbsaConfig,
    facts::{VirtBoardFacts, VirtMachine},
};

impl VirtBoardFacts for crate::Board {
    const MACHINE: VirtMachine = VirtMachine::Sbsa;
}

pub static QEMU_SBSA: QemuSbsaConfig = QemuSbsaConfig::new().build();
