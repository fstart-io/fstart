//! QEMU q35 with 32-bit protected-mode stages, run as `-cpu coreduo`.

use fstart_platform_qemu::{
    QemuQ35Config,
    facts::{VirtBoardFacts, VirtMachine},
};

impl VirtBoardFacts for crate::Board {
    const MACHINE: VirtMachine = VirtMachine::Q35ProtectedMode;
}

pub static QEMU_Q35_PLATFORM: QemuQ35Config = QemuQ35Config::new().build();
