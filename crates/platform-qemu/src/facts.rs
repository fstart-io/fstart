//! Host-clean selection of supported QEMU virt machines.
//! Flash mapping and all normal image reservations are machine invariants owned
//! by the platform. Runtime geometry remains the validated linked descriptor.

#[derive(Debug, Clone, Copy)]
pub enum VirtMachine {
    Riscv64,
    Armv7,
    Aarch64,
}

pub trait VirtBoardFacts {
    const MACHINE: VirtMachine;
}
