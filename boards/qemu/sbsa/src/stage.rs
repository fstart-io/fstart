use fstart_platform_qemu::{QemuSbsa, QemuSbsaBoard, QemuSbsaConfig};
use fstart_stage::{payload::BuildSelectedPayload, StageBoard, StageEnvironment};

use crate::Board;

impl StageBoard for Board {
    const NAME: &'static str = crate::BOARD_NAME;
    const PLATFORM: fstart_core::Platform = fstart_core::Platform::Aarch64;

    fn run_stage(env: StageEnvironment, handoff: usize) -> ! {
        QemuSbsa::run_stage::<Self>(env, handoff)
    }
}

impl QemuSbsaBoard for Board {
    type Payload = BuildSelectedPayload;
    const CONFIG: &'static QemuSbsaConfig = &crate::QEMU_SBSA;
}
