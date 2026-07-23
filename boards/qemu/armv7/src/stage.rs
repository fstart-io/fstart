//! QEMU ARMv7 virt binding for the handwritten monolithic flow.

use fstart_driver_uart::pl011::Pl011Config;
use fstart_platform_qemu::{
    virt_armv7::QEMU_ARMV7_UART_BASE, QemuArmv7Virt, QemuArmv7VirtBoard,
    QemuArmv7VirtConfig, QemuArmv7VirtHooks,
};
use fstart_stage::{payload::BuildSelectedPayload, StageBoard, StageEnvironment};

use crate::Board;

impl StageBoard for Board {
    const NAME: &'static str = crate::BOARD_NAME;
    const PLATFORM: fstart_core::Platform = crate::PLATFORM;

    fn run_stage(env: StageEnvironment, handoff: usize) -> ! {
        QemuArmv7Virt::run_stage::<Self>(env, handoff)
    }
}

/// Board-specific seams for the fixed ARMv7 virt flow.
#[derive(Default)]
pub struct QemuArmv7Hooks;

impl QemuArmv7VirtHooks for QemuArmv7Hooks {}

impl QemuArmv7VirtBoard for Board {
    type Hooks = QemuArmv7Hooks;
    type Payload = BuildSelectedPayload;

    const CONFIG: &'static QemuArmv7VirtConfig = &crate::QEMU_ARMV7_VIRT;

    fn console_config() -> Pl011Config {
        Pl011Config {
            base_addr: QEMU_ARMV7_UART_BASE,
            clock_freq: 1_843_200,
            baud_rate: 115_200,
        }
    }
}
