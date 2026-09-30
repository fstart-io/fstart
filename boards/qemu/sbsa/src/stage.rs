//! QEMU SBSA binding for the handwritten monolithic flow.

use fstart_platform_qemu::{QemuSbsaBoard, QemuSbsaConfig};

use crate::Board;

impl QemuSbsaBoard for Board {
    const CONFIG: &'static QemuSbsaConfig = &crate::QEMU_SBSA;
}
