//! Host-clean selection of supported QEMU machines.
//! Flash mapping and all normal image reservations are machine invariants owned
//! by the platform. Runtime geometry remains the validated linked descriptor.

#[derive(Debug, Clone, Copy)]
pub enum VirtMachine {
    Riscv64,
    Armv7,
    Aarch64,
    Q35,
    /// q35 with 32-bit protected-mode stages (`-cpu coreduo`), the test
    /// vehicle for boards built with `Platform::X86`.
    Q35ProtectedMode,
    /// q35 with two-stage RO/A/B layout over 8 MiB `pflash_cfi01` (1.5 MiB
    /// read-only RO bank + 6.5 MiB RW bank) + TPM 2.0 rollback floor + CrabEFI
    /// Capsule-on-Disk updates.
    Q35Ab,
    Sbsa,
    SifiveU,
}

impl VirtMachine {
    /// Either monolithic q35 flavour: same machine, differing only in CPU mode.
    #[must_use]
    pub const fn is_q35(self) -> bool {
        matches!(self, Self::Q35 | Self::Q35ProtectedMode)
    }
}

pub trait VirtBoardFacts {
    const MACHINE: VirtMachine;
}
