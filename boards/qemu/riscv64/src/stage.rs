//! QEMU RISC-V virt binding for the handwritten monolithic flow.

use fstart_driver_uart::ns16550::{AccessMode, Ns16550Config};
use fstart_platform_qemu::{
    QemuRiscv64VirtBoard, QemuRiscv64VirtConfig, QemuRiscv64VirtHooks,
    virt_riscv64::QEMU_RISCV64_UART_BASE,
};

use crate::Board;

/// Board-specific seams for the fixed RISC-V virt flow.
#[derive(Default)]
pub struct QemuRiscv64Hooks;

impl QemuRiscv64VirtHooks for QemuRiscv64Hooks {}

impl QemuRiscv64VirtBoard for Board {
    type Hooks = QemuRiscv64Hooks;

    const CONFIG: &'static QemuRiscv64VirtConfig = &crate::QEMU_RISCV64_VIRT;

    fn console_config() -> Ns16550Config {
        Ns16550Config {
            regs: AccessMode::Mmio {
                base: QEMU_RISCV64_UART_BASE,
                reg_shift: 0,
                reg_width: 0,
            },
            clock_freq: 3_686_400,
            baud_rate: 115_200,
        }
    }
}
