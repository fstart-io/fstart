//! QEMU ARMv7 virt binding for the handwritten monolithic flow.

use fstart_driver_uart::pl011::Pl011Config;
use fstart_platform_qemu::{
    QemuArmv7VirtBoard, QemuArmv7VirtConfig, QemuArmv7VirtHooks, virt_armv7::QEMU_ARMV7_UART_BASE,
};

use crate::Board;

/// Board-specific seams for the fixed ARMv7 virt flow.
#[derive(Default)]
pub struct QemuArmv7Hooks;

impl QemuArmv7VirtHooks for QemuArmv7Hooks {}

impl QemuArmv7VirtBoard for Board {
    type Hooks = QemuArmv7Hooks;

    const CONFIG: &'static QemuArmv7VirtConfig = &crate::QEMU_ARMV7_VIRT;

    fn console_config() -> Pl011Config {
        Pl011Config {
            base_addr: QEMU_ARMV7_UART_BASE,
            clock_freq: 1_843_200,
            baud_rate: 115_200,
        }
    }
}
