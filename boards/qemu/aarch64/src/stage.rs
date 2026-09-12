//! QEMU AArch64 virt binding for the handwritten monolithic flow.

use fstart_driver_uart::pl011::Pl011Config;
use fstart_platform_qemu::{
    QemuAarch64VirtBoard, QemuAarch64VirtConfig, QemuAarch64VirtHooks,
    virt_aarch64::QEMU_AARCH64_UART_BASE,
};

use crate::Board;

/// Board-specific seams for the fixed AArch64 virt flow.
#[derive(Default)]
pub struct QemuAarch64Hooks;

impl QemuAarch64VirtHooks for QemuAarch64Hooks {}

impl QemuAarch64VirtBoard for Board {
    type Hooks = QemuAarch64Hooks;

    const CONFIG: &'static QemuAarch64VirtConfig = &crate::QEMU_AARCH64_VIRT;

    fn console_config() -> Pl011Config {
        Pl011Config {
            base_addr: QEMU_AARCH64_UART_BASE,
            clock_freq: 1_843_200,
            baud_rate: 115_200,
        }
    }
}
