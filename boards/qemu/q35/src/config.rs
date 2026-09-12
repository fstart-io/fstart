//! QEMU q35 hardware facts. Platform Rust owns fixed image budgets.

use fstart_platform_qemu::{
    QemuQ35Config,
    facts::{VirtBoardFacts, VirtMachine},
};

impl VirtBoardFacts for crate::Board {
    const MACHINE: VirtMachine = VirtMachine::Q35;
}

pub static QEMU_Q35_PLATFORM: QemuQ35Config = QemuQ35Config::new().build();
