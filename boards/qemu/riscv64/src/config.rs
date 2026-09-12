//! QEMU RISC-V virt hardware facts. Platform Rust owns fixed image budgets.

use fstart_platform_qemu::{
    QemuRiscv64VirtConfig,
    facts::{VirtBoardFacts, VirtMachine},
};

impl VirtBoardFacts for crate::Board {
    const MACHINE: VirtMachine = VirtMachine::Riscv64;
}

pub static QEMU_RISCV64_VIRT: QemuRiscv64VirtConfig = QemuRiscv64VirtConfig::new().build();
