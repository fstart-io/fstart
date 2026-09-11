//! QEMU SiFive U binding for the direct DRAM-resident flow.

use fstart_driver_uart::sifive::SifiveUartConfig;
use fstart_platform_qemu::{
    QemuSifiveUBoard, QemuSifiveUBuildSelectedPayload, QemuSifiveUConfig, QemuSifiveUHooks,
};

use crate::Board;

/// Board-specific seams for QEMU's fixed SiFive U flow.
#[derive(Default)]
pub struct QemuSifiveUBoardHooks;

impl QemuSifiveUHooks for QemuSifiveUBoardHooks {}

impl QemuSifiveUBoard for Board {
    type Hooks = QemuSifiveUBoardHooks;
    type Payload = QemuSifiveUBuildSelectedPayload;

    const CONFIG: &'static QemuSifiveUConfig = &crate::QEMU_SIFIVE_U;
    const CONSOLE_CONFIG: &'static SifiveUartConfig = &crate::QEMU_SIFIVE_U_UART;
}
