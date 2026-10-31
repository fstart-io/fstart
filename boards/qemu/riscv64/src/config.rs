//! QEMU RISC-V virt hardware facts. Platform Rust owns fixed image budgets.

use fstart_platform_qemu::{
    QemuVirtConfig,
    facts::{VirtBoardFacts, VirtMachine},
};

impl VirtBoardFacts for crate::Board {
    const MACHINE: VirtMachine = VirtMachine::Riscv64;
}

pub static QEMU_RISCV64_VIRT: QemuVirtConfig = QemuVirtConfig::riscv64().build();
