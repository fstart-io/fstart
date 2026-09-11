//! QEMU `sifive_u` hardware facts. Platform Rust owns fixed image budgets.

use fstart_core::mmio32;
use fstart_driver_uart::sifive::SifiveUartConfig;
use fstart_platform_qemu::{
    QemuSifiveUConfig,
    facts::{VirtBoardFacts, VirtMachine},
};

impl VirtBoardFacts for crate::Board {
    const MACHINE: VirtMachine = VirtMachine::SifiveU;
}

/// QEMU `sifive_u` platform defaults, retained in ROM.
pub static QEMU_SIFIVE_U: QemuSifiveUConfig = QemuSifiveUConfig::new().build();

/// QEMU's FU740-compatible UART wiring and precomputed divisor.
pub static QEMU_SIFIVE_U_UART: SifiveUartConfig =
    SifiveUartConfig::new(mmio32(0x1001_0000), 500_000_000, 115_200);
