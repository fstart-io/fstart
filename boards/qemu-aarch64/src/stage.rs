//! QEMU AArch64 virt binding for the handwritten monolithic flow.

use fstart_driver_uart::pl011::Pl011Config;
use fstart_platform_qemu::{
    virt_aarch64::QEMU_AARCH64_UART_BASE, QemuAarch64Virt, QemuAarch64VirtBoard,
    QemuAarch64VirtConfig, QemuAarch64VirtHooks,
};
use fstart_stage::{payload::BuildSelectedPayload, StageBoard, StageEnvironment};

use crate::Board;

impl StageBoard for Board {
    const NAME: &'static str = crate::BOARD_NAME;
    const PLATFORM: fstart_core::Platform = crate::PLATFORM;

    fn run_stage(env: StageEnvironment, handoff: usize) -> ! {
        QemuAarch64Virt::run_stage::<Self>(env, handoff)
    }
}

/// Board-specific seams for the fixed AArch64 virt flow.
#[derive(Default)]
pub struct QemuAarch64Hooks;

impl QemuAarch64VirtHooks for QemuAarch64Hooks {}

impl QemuAarch64VirtBoard for Board {
    type Hooks = QemuAarch64Hooks;
    type Payload = BuildSelectedPayload;

    const CONFIG: &'static QemuAarch64VirtConfig = &crate::QEMU_AARCH64_VIRT;

    fn console_config() -> Pl011Config {
        Pl011Config {
            base_addr: QEMU_AARCH64_UART_BASE,
            clock_freq: 1_843_200,
            baud_rate: 115_200,
        }
    }
}
