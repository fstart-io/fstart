//! QEMU SBSA binding for the handwritten monolithic flow.

use fstart_platform_qemu::{QemuSbsaBoard, QemuSbsaConfig};
use fstart_platform_qemu::stage_runtime::payload::BuildSelectedPayload;

use crate::Board;

impl QemuSbsaBoard for Board {
    type Payload = BuildSelectedPayload;
    const CONFIG: &'static QemuSbsaConfig = &crate::QEMU_SBSA;
}
